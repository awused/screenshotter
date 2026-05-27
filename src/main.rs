use std::collections::HashMap;
use std::ffi::OsString;
use std::path::PathBuf;
use std::sync::{LazyLock, Mutex};

use clap::Parser;
use color_eyre::Result;
use futures::future::{Either, select};
use notify_rust::{Notification, Urgency};
use serde_json::Value;
use tokio::pin;

use crate::config::CONFIG;
use crate::ipc::Window;
use crate::target::MODE;
use crate::wayland::Conn;

#[macro_use]
extern crate tracing;

mod config;
mod elapsedlogger;
mod ipc;
mod selection;
mod target;
mod wayland;

#[derive(Debug, Parser)]
enum Command {
    /// Take a screenshot of the active window
    Window,
    /// Take a screenshot of the entire desktop
    Desktop,
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
    about = "Tool for taking screenshots and organizing them"
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
    trace!("Starting main");


    ENV_VARS.lock().unwrap().insert(MODE, OPTIONS.cmd.str().into());

    match &OPTIONS.cmd {
        Command::Window => window().await,
        Command::Desktop => desktop().await,
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

    let mut con = wayland::Conn::init(true, false)?;


    con.poll().await?;
    Ok(())
}

#[instrument(level = "error", skip_all)]
async fn desktop() -> Result<()> {
    trace!("Starting desktop");
    LazyLock::force(&CONFIG);

    let mut con = wayland::Conn::init(true, false)?;


    con.poll().await?;
    Ok(())
}

#[instrument(level = "error", skip_all)]
async fn region() -> Result<()> {
    trace!("Starting region");
    LazyLock::force(&CONFIG);

    let mut con = wayland::Conn::init(true, true)?;


    con.poll().await?;
    Ok(())
}


#[instrument(level = "error", skip_all)]
async fn name() -> Result<()> {
    trace!("Starting name");
    LazyLock::force(&CONFIG);
    let mut finder = target::ApplicationFinder::init();
    let mut con = wayland::Conn::init(false, true)?;
    let windows = while_polling(ipc::visible_windows(), &mut con).await?;


    let region = selection::region(&windows)?;
    let window = region.best_window(windows);
    debug!("Found window {window:?}");
    let app = finder.application_for(region, window).await?;
    let target = app.relative_dir.to_string_lossy();

    Notification::new()
        .summary("application name")
        .appname("screenshotter")
        .body(&target)
        .urgency(Urgency::Low)
        .show()?;

    println!("{target}");

    Ok(())
}

#[instrument(level = "error", skip_all)]
async fn prop() -> Result<()> {
    trace!("Starting prop");
    let mut con = wayland::Conn::init(false, true)?;
    let windows = while_polling(ipc::visible_windows(), &mut con).await?;

    let region = selection::region(&windows)?;
    if let Some(window) = region.best_window(windows) {
        window.dump();
    }

    Ok(())
}

#[instrument(level = "error", skip_all)]
async fn visible() -> Result<()> {
    let windows = ipc::visible_windows().await?;

    let json = Value::Array(windows.iter().map(Window::to_json).collect::<Vec<_>>());

    println!("{json}");

    Ok(())
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
            Self::Desktop => "desktop",
            Self::Region => "region",
            Self::Name => "name",
            Self::Prop => "prop",
            Self::VisibleWindows => "visible_windows",
        }
    }
}
