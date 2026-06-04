use std::collections::HashMap;
use std::ffi::OsString;
use std::path::PathBuf;
use std::sync::{LazyLock, Mutex};
use std::thread::available_parallelism;

use clap::Parser;
use color_eyre::Result;
use color_eyre::eyre::bail;
use futures::future::{Either, select};
use notify_rust::{Notification, Urgency};
use rayon::ThreadPoolBuilder;
use serde_json::Value;
use time::OffsetDateTime;
use tokio::pin;

use crate::app::{Finder, MODE};
use crate::config::CONFIG;
use crate::ipc::Window;
use crate::wayland::conn::Conn;
use crate::wayland::{Mode, Selected};

#[macro_use]
extern crate tracing;

mod app;
mod config;
mod elapsedlogger;
mod img;
mod ipc;
mod util;
mod wayland;

const CLICK_TIME_MS: u32 = 100;

pub static TIME: LazyLock<OffsetDateTime> =
    LazyLock::new(|| OffsetDateTime::now_local().unwrap_or_else(|_| OffsetDateTime::now_utc()));

#[derive(Debug, Parser)]
enum Command {
    /// Take a screenshot of the active window
    Window,
    /// Take a screenshot of the entire desktop
    Desktop {
        /// Do not scale the screenshots.
        /// Positioning of screenshots for each monitor is consistent but based on a naive
        /// algorithm.
        #[arg(long)]
        unscaled: bool,
    },
    /// Prompt to select a region using slurp before taking a screenshot.
    /// The name and directory will be based on the center of the selected region.
    Region,
    /// Gets the output name and directory for a screenshot without actually taking the screenshot.
    /// Intended for debugging configs.
    Name,
    /// Behaves roughly like xprop with json output.
    /// Output is compositor dependent and should match hypctrl -j clients or equivalent.
    Prop,
    /// Dump data about all visible windows including their visible rectangles.
    /// Only considers portions of
    /// Output is compositor dependent and should match hypctrl -j clients or equivalent.
    /// Windows should be ordered from front to back.
    /// `visible_region` will be in the form {x: int, y: int, width: int, height: int}
    VisibleWindows,
}

#[derive(Debug, Parser)]
#[clap(
    name = "screenshotter",
    about = "Tool for taking screenshots and organizing them, or replacing xprop"
)]
pub struct Opt {
    #[arg(short, long, value_parser)]
    /// Override the selected config.
    awconf: Option<PathBuf>,

    #[command(subcommand)]
    cmd: Command,
}

// Just being lazy about passing this around
pub static ENV_VARS: LazyLock<Mutex<HashMap<&'static str, OsString>>> =
    LazyLock::new(Mutex::default);

pub static OPTIONS: LazyLock<Opt> = LazyLock::new(Opt::parse);

#[tokio::main(flavor = "current_thread")]
async fn main() -> Result<()> {
    elapsedlogger::init_logging();
    color_eyre::install().unwrap();
    LazyLock::force(&TIME);
    trace!("Starting main");


    ENV_VARS.lock().unwrap().insert(MODE, OPTIONS.cmd.str().into());

    match &OPTIONS.cmd {
        Command::Window => window().await,
        Command::Desktop { unscaled } => desktop(*unscaled).await,
        Command::Region => region().await,
        Command::Name => name().await,
        Command::Prop => prop().await,
        Command::VisibleWindows => visible().await,
    }
}

#[instrument(level = "error", skip_all)]
async fn window() -> Result<()> {
    trace!("Starting window");
    LazyLock::force(&CONFIG);

    init_threadpool();

    let mut con = Conn::init(Mode::ScreenshotOnly)?;
    let finder = app::Finder::init();

    let windows = while_polling(ipc::visible_windows(true), &mut con).await?;
    if windows.len() != 1 {
        bail!("No active window found");
    }
    let window = windows.into_iter().next().unwrap();

    let selection = con.run(Vec::new()).await?;
    if !matches!(selection, Selected::Nothing) {
        bail!("Wrong selection, expected nothing, got {selection:?}");
    }
    let app = finder.application_for_spawned(Selected::Window(window.clone()));

    let screenshots = con.screenshot_window(window)?;
    let combined = img::combine(screenshots, true);

    let app = app.await??;

    app.save_file(combined)?;

    Ok(())
}

#[instrument(level = "error", skip_all)]
async fn desktop(unscaled: bool) -> Result<()> {
    trace!("Starting desktop");
    LazyLock::force(&CONFIG);

    init_threadpool();

    let mut con = Conn::init(Mode::ScreenshotOnly)?;
    let app = Finder::empty().application_for_spawned(Selected::Nothing);
    let selection = con.run(Vec::new()).await?;
    if !matches!(selection, Selected::Nothing) {
        bail!("Wrong selection, expected nothing, got {selection:?}");
    }

    let screenshots = con.selected_screenshot()?;
    let combined = img::combine(screenshots, !unscaled);

    let app = app.await??;

    app.save_file(combined)?;

    Ok(())
}

#[instrument(level = "error", skip_all)]
async fn region() -> Result<()> {
    trace!("Starting region");
    LazyLock::force(&CONFIG);

    init_threadpool();

    let mut con = Conn::init(Mode::Region)?;
    let finder = app::Finder::init();
    let windows = while_polling(ipc::visible_windows(false), &mut con).await?;

    let selection = con.run(windows).await?;
    if matches!(selection, Selected::Nothing) {
        bail!("Wrong selection, expected something, got nothing");
    }
    debug!("Selected region {:?}", selection.int_region());
    let app = finder.application_for_spawned(selection.clone());

    let screenshots = con.selected_screenshot()?;
    let combined = img::combine(screenshots, true);

    let app = app.await??;

    app.save_file(combined)?;

    Ok(())
}


#[instrument(level = "error", skip_all)]
async fn name() -> Result<()> {
    trace!("Starting name");
    LazyLock::force(&CONFIG);

    let mut con = Conn::init(Mode::PickWindow)?;
    let finder = app::Finder::init();
    let windows = while_polling(ipc::visible_windows(false), &mut con).await?;


    let selection = con.run(windows).await?;
    let Selected::Window(window) = selection else {
        bail!("No window selected");
    };

    debug!("Found window {window:?}");
    let app = finder.application_for(selection).await?;

    let target = app.relative_dir.to_string_lossy();

    println!("{target}");

    Notification::new()
        .summary("application name")
        .appname("screenshotter")
        .body(&target)
        .urgency(Urgency::Low)
        .show()?;


    Ok(())
}

#[instrument(level = "error", skip_all)]
async fn prop() -> Result<()> {
    trace!("Starting prop");
    let mut con = Conn::init(Mode::PickWindow)?;
    let windows = while_polling(ipc::visible_windows(false), &mut con).await?;

    let selection = con.run(windows).await?;
    if let Selected::Window(w) = selection {
        w.dump();
    }

    Ok(())
}

#[instrument(level = "error", skip_all)]
async fn visible() -> Result<()> {
    let windows = ipc::visible_windows(false).await?;

    let json = Value::Array(windows.iter().map(Window::to_json).collect::<Vec<_>>());

    println!("{json}");

    Ok(())
}

// Only spin this up if we might need it. Only used for scaling
fn init_threadpool() {
    ThreadPoolBuilder::new()
        .num_threads(available_parallelism().map_or(4, |p| p.get() / 2))
        .build_global()
        .unwrap();
}

// If we need to do more than this, it'd be better to spawn() and use channels
async fn while_polling<T>(fut: impl Future<Output = Result<T>>, con: &mut Conn) -> Result<T> {
    pin! {
        let right = fut;
        let poll = con.poll();
    };

    match select(right, poll).await {
        Either::Left((right, _)) => right,
        Either::Right((Err(e), _)) => Err(e),
        Either::Right((Ok(_), _)) => unreachable!(),
    }
}


impl Command {
    const fn str(&self) -> &'static str {
        match self {
            Self::Window => "window",
            Self::Desktop { .. } => "desktop",
            Self::Region => "region",
            Self::Name => "name",
            Self::Prop => "prop",
            Self::VisibleWindows => "visible_windows",
        }
    }
}
