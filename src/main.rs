use std::collections::{HashMap, VecDeque};
use std::ffi::OsString;
use std::path::PathBuf;
use std::process::exit;
use std::sync::{LazyLock, Mutex};

use clap::Parser;
use color_eyre::Result;
use notify_rust::{Notification, Urgency};
use tracing::Level;
use tracing_error::ErrorLayer;
use tracing_subscriber::EnvFilter;
use tracing_subscriber::layer::SubscriberExt;
use tracing_subscriber::util::SubscriberInitExt;

use crate::config::CONFIG;
use crate::target::MODE;

#[macro_use]
extern crate tracing;

mod config;
mod ipc;
mod selection;
mod target;

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
    /// Behaves roughly like xprop with json output. Comparable to hyprctrl -j
    Prop,
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

fn main() -> Result<()> {
    let filter_layer =
        EnvFilter::builder().with_default_directive(Level::INFO.into()).from_env_lossy();
    tracing_subscriber::registry()
        .with(filter_layer)
        .with(tracing_subscriber::fmt::layer())
        .with(ErrorLayer::default())
        .init();
    color_eyre::install().unwrap();


    ENV_VARS.lock().unwrap().insert(MODE, OPTIONS.cmd.str().into());

    match &OPTIONS.cmd {
        Command::Window => todo!(),
        Command::Desktop => todo!(),
        Command::Region => todo!(),
        Command::Name => name(),
        Command::Prop => prop(),
    }
}


#[instrument(level = "error", skip_all)]
fn name() -> Result<()> {
    LazyLock::force(&CONFIG);

    let windows = ipc::visible_windows()?;
    let region = selection::region(&windows)?;
    let window = region.best_window(windows);
    debug!("Found window {window:?}");
    let app = target::application_for(region, window)?;
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
fn prop() -> Result<()> {
    let windows = ipc::visible_windows()?;
    let region = selection::region(&windows)?;
    if let Some(window) = region.best_window(windows) {
        window.dump();
    }

    Ok(())
}


impl Command {
    const fn str(&self) -> &'static str {
        match self {
            Self::Window => "window",
            Self::Desktop => "desktop",
            Self::Region => "region",
            Self::Name => "name",
            Self::Prop => "prop",
        }
    }
}
