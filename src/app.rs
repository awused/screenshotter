use std::cell::LazyCell;
use std::cmp::Reverse;
use std::ffi::{OsStr, OsString};
use std::fs::{DirBuilder, File};
use std::os::unix::ffi::{OsStrExt, OsStringExt};
use std::os::unix::fs::DirBuilderExt;
use std::path::{Path, PathBuf};
use std::sync::LazyLock;
use std::time::Instant;

use color_eyre::eyre::{bail, eyre};
use color_eyre::{Result, Section, SectionExt};
use constcat::concat;
use image::RgbImage;
use image::codecs::png::PngEncoder;
use image::codecs::webp::WebPEncoder;
use path_clean::PathClean;
use regex::{Regex, bytes};
use strfmt::strfmt_map;
use sysinfo::{Pid, System};
use time::macros::format_description;
use tokio::process::Command;
use tokio::task::{JoinHandle, spawn_blocking};

use crate::config::{CONFIG, FileFormat, Override, Reformatter};
use crate::wayland::Selected;
use crate::{ENV_VARS, OPTIONS, TIME};


const PREFIX: &str = "SCREENSHOTTER_";
const CLASS: &str = concat!(PREFIX, "CLASS");
const NAME: &str = concat!(PREFIX, "NAME");
const WINDOW_ID: &str = concat!(PREFIX, "WINDOW_ID");
const WINDOW_PID: &str = concat!(PREFIX, "WINDOW_PID");
const PID: &str = concat!(PREFIX, "PID");
const DIR: &str = concat!(PREFIX, "DIR");
pub const MODE: &str = concat!(PREFIX, "MODE");
const WM_NAME: &str = concat!(PREFIX, "WM_NAME");
const GEOMETRY: &str = concat!(PREFIX, "GEOMETRY");

#[derive(Debug, Default)]
pub struct Application {
    pub relative_dir: PathBuf,
    pub yearly: bool,
    pub monthly: bool,
    pub callback: Option<&'static Path>,
}

impl Application {
    pub fn save_file(self, combined: RgbImage) -> Result<PathBuf> {
        let dir = &CONFIG.screenshot_dir.join(self.relative_dir);
        if !dir.clean().starts_with(&CONFIG.screenshot_dir) {
            bail!("Computed directory {dir:?} not in configured screenshot dir");
        }

        let mut path = if self.yearly && self.monthly {
            dir.join(TIME.format(format_description!("[year]"))?)
                .join(TIME.format(format_description!("[month]"))?)
                .join(TIME.format(format_description!("[day]_[hour]-[minute]-[second]"))?)
        } else if self.yearly {
            dir.join(TIME.format(format_description!("[year]"))?)
                .join(TIME.format(format_description!("[month]-[day]_[hour]-[minute]-[second]"))?)
        } else if self.monthly {
            dir.join(TIME.format(format_description!("[year]-[month]"))?)
                .join(TIME.format(format_description!("[day]_[hour]-[minute]-[second]"))?)
        } else {
            dir.join(
                TIME.format(format_description!("[year]-[month]-[day]_[hour]-[minute]-[second]"))?,
            )
        };

        if OPTIONS.dry_run {
            info!("Dry run, would have written to {path:?}");
            return Ok(path);
        }

        DirBuilder::new().recursive(true).mode(0o755).create(path.parent().unwrap())?;

        let start = Instant::now();
        match CONFIG.format {
            FileFormat::Png => {
                path.add_extension("png");
                let enc = PngEncoder::new_with_quality(
                    File::create_new(&path)?,
                    image::codecs::png::CompressionType::Level(CONFIG.compression),
                    image::codecs::png::FilterType::Adaptive,
                );
                combined.write_with_encoder(enc)?;
            }
            FileFormat::Webp => {
                path.add_extension("webp");
                let enc = WebPEncoder::new_lossless(File::create_new(&path)?);
                combined.write_with_encoder(enc)?;
            }
        }
        info!("Saved file {path:?} in {:?}", start.elapsed());


        debug!("Wrote file in {:?}", start.elapsed());


        if let Some(callback) = self.callback
            && let Err(e) = run_callback(callback, &path)
        {
            error!("Override callback failed with error {e:?}");
        }

        if let Some(callback) = &CONFIG.callback
            && let Err(e) = run_callback(callback, &path)
        {
            error!("Callback failed with error {e:?}");
        }

        Ok(path)
    }
}

#[derive(Debug, Default)]
pub struct Finder {
    system: Option<JoinHandle<System>>,
}


impl Finder {
    pub fn init() -> Self {
        let system = spawn_blocking(|| {
            let mut system = System::new();
            trace!("Attempting to get info for processes");
            // Getting information on the processes is ~100ms
            if system.refresh_processes_specifics(
                sysinfo::ProcessesToUpdate::All,
                true,
                sysinfo::ProcessRefreshKind::nothing()
                    .with_cmd(sysinfo::UpdateKind::Always)
                    .with_exe(sysinfo::UpdateKind::Always),
            ) == 0
            {
                warn!("Failed to get process info for any processes");
            }
            trace!("Finished getting info for processes");
            system
        });

        Self { system: Some(system) }
    }

    pub const fn empty() -> Self {
        Self { system: None }
    }

    // Can't meaningfully avoid spinning up another thread here, but if spawn_blocking with a
    // single threaded executor ensures there are no entirely wasted threads.
    pub fn application_for_spawned(self, selection: Selected) -> JoinHandle<Result<Application>> {
        spawn_blocking(move || self.async_main(selection))
    }

    #[tokio::main(flavor = "current_thread")]
    async fn async_main(self, selection: Selected) -> Result<Application> {
        self.application_for(&selection).await
    }

    #[allow(clippy::await_holding_lock)]
    #[instrument(level = "error", skip_all)]
    pub async fn application_for(mut self, selection: &Selected) -> Result<Application> {
        let mut env = ENV_VARS.lock().unwrap();
        if let Some(region) = selection.int_region() {
            env.insert(GEOMETRY, region.to_string().into());
        }

        let mut application = Application::default();
        let mut cli = None;

        if let Some(window) = selection.window() {
            let pid = window.pid();
            if let Some(wm_name) = window.name() {
                env.insert(WM_NAME, wm_name.into());
            };

            let name = if let Some(class) = window.class() {
                debug!("Got application (class) from window \"{class}\"");
                let name = class.rsplit_once('.').map_or(class, |(_left, right)| right);
                let name = convert_application_name(name);
                env.insert(CLASS, class.into());
                name
            } else {
                convert_application_name(&CONFIG.fallback)
            };

            env.insert(WINDOW_PID, pid.to_string().into());
            // system should always be available if the selection includes a window
            let system = self.system.take().unwrap().await?;
            let (name, cmd, pid) = get_process(system, name, pid as u32);
            cli = cmd;

            let mut dir = CONFIG.screenshot_dir.clone();
            dir.push(&name);
            application.relative_dir = name.clone().into();
            env.insert(NAME, name);
            env.insert(DIR, dir.into());
            env.insert(PID, pid.to_string().into());
            env.insert(WINDOW_ID, window.id().into());
        } else {
            let name = convert_application_name(&CONFIG.fallback);
            application.relative_dir = name.clone().into();
            env.insert(NAME, name.clone().into());
            let mut dir = CONFIG.screenshot_dir.clone();
            dir.push(name);
            env.insert(DIR, dir.into());
        }

        // Delegates _could_ take a long time to run, could parallelize them.
        drop(env);

        for over in &CONFIG.overrides {
            let (matched, path) = check_override(&application, &cli, over).await?;
            if !matched {
                continue;
            }

            if let Some(path) = path {
                application.relative_dir = path;

                let mut env = ENV_VARS.lock().unwrap();
                env.insert(NAME, application.relative_dir.clone().into());

                let mut dir = CONFIG.screenshot_dir.clone();
                dir.push(&application.relative_dir);
                env.insert(DIR, dir.into());
            }

            application.yearly = over.yearly;
            application.monthly = over.monthly;
            application.callback = over.callback.as_deref();

            break;
        }

        debug!("Determined application to be {application:?}");
        Ok(application)
    }
}

#[instrument(level = "error", skip(system))]
fn get_process(system: System, name: String, pid: u32) -> (OsString, Option<OsString>, u32) {
    let mut pid = Pid::from_u32(pid);
    let mut name = OsString::from(name);
    let mut cli = None;

    let Some(mut process) = system.process(pid) else {
        error!("Could not find process");
        return (name, cli, pid.as_u32());
    };

    let processes: LazyCell<Vec<_>, _> = LazyCell::new(|| {
        // Filter out threads and sort processes by reverse creation time/pid
        let mut processes: Vec<_> =
            system.processes().values().filter(|p| p.thread_kind().is_none()).collect();
        processes.sort_unstable_by_key(|p| Reverse((p.start_time(), p.pid().as_u32())));
        processes
    });


    // Max 100 tries/depth
    'outer: for _ in 0..100 {
        let new_name = process.exe().and_then(Path::file_name).unwrap_or_else(|| process.name());
        if new_name.is_empty() {
            error!("Empty process name");
            return (name, cli, pid.as_u32());
        }
        name = convert_application_name(&new_name.to_string_lossy()).into();
        pid = process.pid();
        let args = process.cmd().iter().map(|s| s.as_bytes());
        // Not going to execute this, just match a regex against it
        cli = shlex::bytes::try_join(args).map(OsString::from_vec).ok();
        debug!("Got process info: {pid} {name:?} {cli:?}");

        if !CONFIG.ignored_parents.iter().any(|p| OsStr::new(p) == name) {
            break;
        }

        if let Some(child) = processes.iter().find(|p| p.parent() == Some(pid)) {
            process = child;
            continue 'outer;
        }
        break;
    }

    (name, cli, pid.as_u32())
}

#[instrument(level = "error", skip(app, cli))]
async fn check_override(
    app: &Application,
    cli: &Option<OsString>,
    over: &'static Override,
) -> Result<(bool, Option<PathBuf>)> {
    debug!("Testing override");
    if let Some(name) = &over.name
        && Path::new(name) != app.relative_dir
    {
        trace!("Name didn't match");
        return Ok((false, None));
    }

    let caps = if let Some(re) = &over.regex {
        let re = bytes::Regex::new(re)?;

        if let Some(cli) = cli
            && let Some(cap) = re.captures(cli.as_bytes())
        {
            Some(cap)
        } else {
            trace!("Regex didn't match");
            return Ok((false, None));
        }
    } else {
        None
    };

    // Delegate exiting with a failure is not fatal, but means it didn't match

    let mut dir = None;

    match &over.transform {
        Some(Reformatter::Format(template)) => {
            let new_name = strfmt_map(template, |mut f| {
                if let Some(caps) = &caps
                    && let Ok(g) = f.key.parse::<usize>()
                    && let Some(caps) = caps.get(g)
                {
                    f.str(&OsStr::from_bytes(caps.as_bytes()).to_string_lossy())
                } else {
                    error!("Bad formatting identifier: \"{}\"", f.key);
                    f.skip()
                }
            })?;

            dir = Some(convert_application_name(&new_name).into());
        }
        Some(Reformatter::Delegate(delegate)) => match run_delegate(delegate).await {
            Ok(Some(path)) => {
                debug!("Delegate matched with output: {path:?}");
                dir = Some(path);
            }
            Ok(None) => {
                debug!("Delegate matched with no output");
            }
            Err(_) => {
                debug!("Delegate didn't match");
                return Ok((false, None));
            }
        },
        None => {}
    }

    Ok((true, dir))
}

#[instrument(level = "error", skip_all, err(level = "debug", Debug))]
#[allow(clippy::await_holding_lock)] // false positive
async fn run_delegate(delegate: &Path) -> Result<Option<PathBuf>> {
    let env = ENV_VARS.lock().unwrap();
    trace!("Running delegate with env: {:#?}", env);

    let mut cmd = Command::new(delegate);
    cmd.envs(env.iter());
    drop(env);
    let output = cmd.output().await?;

    if !output.status.success() {
        let out = String::from_utf8_lossy(&output.stdout).to_string().header("Stdout");
        let err = String::from_utf8_lossy(&output.stderr).to_string().header("Stderr");
        let e = eyre!("Delegate status code: {:?}", output.status.code())
            .section(out)
            .section(err);
        return Err(e);
    }

    let mut path = PathBuf::new();

    let out = String::from_utf8(output.stdout)?;
    trace!("Delegate output: {out:?}");
    for line in out.lines() {
        let line = line.trim();
        if line.is_empty() {
            continue;
        }

        let dir = convert_application_name(line);
        path.push(dir);
    }

    if path.as_os_str().is_empty() { Ok(None) } else { Ok(Some(path)) }
}

#[instrument(level = "error", err(level = "debug", Debug))]
fn run_callback(callback: &Path, output_path: &Path) -> Result<()> {
    let env = ENV_VARS.lock().unwrap();
    trace!("Running callback with path: {output_path:?} and env: {:#?}", env);

    let mut cmd = std::process::Command::new(callback);
    cmd.envs(env.iter());
    cmd.arg(output_path);
    drop(env);
    let output = cmd.output()?;

    if !output.status.success() {
        let out = String::from_utf8_lossy(&output.stdout).to_string().header("Stdout");
        let err = String::from_utf8_lossy(&output.stderr).to_string().header("Stderr");
        let e = eyre!("Output status code: {:?}", output.status.code())
            .section(out)
            .section(err);
        return Err(e);
    }

    Ok(())
}

static SAFE_FILENAME: LazyLock<Regex> = LazyLock::new(|| Regex::new(r"[^\pL\pN\-_+=]+").unwrap());
static HYPHENS: LazyLock<Regex> = LazyLock::new(|| Regex::new(r"--+").unwrap());

fn convert_application_name(input: &str) -> String {
    let name = input.to_lowercase();
    let name = SAFE_FILENAME.replace_all(&name, "-");
    let name = HYPHENS.replace_all(&name, "-");
    let name = name.trim_matches('-');
    trace!("Converted \"{input}\" to \"{name}\"");
    name.to_string()
}
