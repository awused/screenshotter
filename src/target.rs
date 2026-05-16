use std::cell::LazyCell;
use std::cmp::Reverse;
use std::collections::{HashMap, HashSet};
use std::convert;
use std::ffi::{OsStr, OsString};
use std::os::unix::ffi::{OsStrExt, OsStringExt};
use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::LazyLock;
use std::time::Instant;

use color_eyre::eyre::{OptionExt, eyre};
use color_eyre::{Result, Section, SectionExt};
use constcat::concat;
use regex::{Regex, bytes};
use strfmt::strfmt_map;
use swayipc::Node;
use sysinfo::{Pid, System};

use crate::ENV_VARS;
use crate::config::{CONFIG, Override, Transform};
use crate::selection::Region;


const PREFIX: &str = "SCREENSHOTTER_";
const APP_ID: &str = concat!(PREFIX, "APP_ID");
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

#[instrument(level = "error")]
fn get_process(name: String, pid: u32) -> (OsString, Option<OsString>, u32) {
    let mut pid = Pid::from_u32(pid);
    let mut name = OsString::from(name);
    let mut system = System::new();
    let mut cli = None;

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
        return (name, cli, pid.as_u32());
    }

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

#[instrument(level = "error", skip_all)]
pub fn application_for(region: Region, window: Option<Node>) -> Result<Application> {
    let mut env = ENV_VARS.lock().unwrap();
    env.insert(GEOMETRY, region.to_string().into());

    let mut application = Application::default();
    let mut cli = None;

    if let Some(window) = window {
        let pid = window.pid.ok_or_eyre("Unreachable")?;
        if let Some(wm_name) = window.name {
            env.insert(WM_NAME, wm_name.into());
        };

        let name = if let Some(app_id) = window.app_id {
            debug!("Got app_id from window \"{app_id}\"");
            let name = app_id.rsplit_once('.').map_or(&*app_id, |(_left, right)| right);
            let name = convert_application_name(name);
            env.insert(APP_ID, app_id.into());
            name
        } else if let Some(props) = window.window_properties
            && let Some(class) = props.class
        {
            let name = class.rsplit_once('.').map_or(&*class, |(_left, right)| right);
            let name = convert_application_name(name);
            env.insert(APP_ID, class.into());
            name
        } else {
            convert_application_name(&CONFIG.fallback)
        };

        env.insert(WINDOW_PID, pid.to_string().into());
        let (name, cmd, pid) = get_process(name, pid as u32);
        cli = cmd;

        let mut dir = CONFIG.screenshot_dir.clone();
        dir.push(&name);
        application.relative_dir = name.clone().into();
        env.insert(NAME, name);
        env.insert(DIR, dir.into());
        env.insert(PID, pid.to_string().into());
        env.insert(WINDOW_ID, window.id.to_string().into());
    } else {
        let name = convert_application_name(&CONFIG.fallback);
        application.relative_dir = name.clone().into();
        env.insert(NAME, name.clone().into());
        let mut dir = CONFIG.screenshot_dir.clone();
        dir.push(name);
        env.insert(DIR, dir.into());
    }
    // Delegates could take a long time to run, could parallelize them.
    drop(env);

    for over in &CONFIG.overrides {
        if run_override(&mut application, &cli, over)? {
            let mut env = ENV_VARS.lock().unwrap();
            env.insert(NAME, application.relative_dir.clone().into());

            let mut dir = CONFIG.screenshot_dir.clone();
            dir.push(&application.relative_dir);
            env.insert(DIR, dir.into());


            drop(env);

            break;
        }
    }

    debug!("Determined application to be {application:?}");
    Ok(application)
}

#[instrument(level = "error", skip(app, cli))]
fn run_override(
    app: &mut Application,
    cli: &Option<OsString>,
    over: &'static Override,
) -> Result<bool> {
    debug!("Testing override");
    if let Some(name) = &over.name
        && Path::new(name) != app.relative_dir
    {
        trace!("Name didn't match");
        return Ok(false);
    }

    let caps = if let Some(re) = &over.regex {
        let re = bytes::Regex::new(re)?;

        if let Some(cli) = cli
            && let Some(cap) = re.captures(cli.as_bytes())
        {
            Some(cap)
        } else {
            trace!("Regex didn't match");
            return Ok(false);
        }
    } else {
        None
    };

    // Delegate exiting with a failure is not fatal, but means it didn't match

    match &over.transform {
        Some(Transform::Format(template)) => {
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

            app.relative_dir = convert_application_name(&new_name).into();
        }
        Some(Transform::Delegate(delegate)) => match run_delegate(delegate) {
            Ok(Some(path)) => {
                debug!("Delegate matched with output: {path:?}");
                app.relative_dir = path;
            }
            Ok(None) => {
                debug!("Delegate matched with no output");
            }
            Err(_) => {
                debug!("Delegate didn't match");
                return Ok(false);
            }
        },
        None => {}
    }

    app.yearly = over.yearly;
    app.monthly = over.monthly;
    app.callback = over.callback.as_deref();

    Ok(true)
}

#[instrument(level = "error", skip_all, err(level = "debug", Debug))]
fn run_delegate(delegate: &Path) -> Result<Option<PathBuf>> {
    let env = ENV_VARS.lock().unwrap();
    trace!("Running delegate with env: {:#?}", env);

    let mut cmd = Command::new(delegate);
    cmd.envs(env.iter());
    drop(env);
    let output = cmd.output()?;

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
