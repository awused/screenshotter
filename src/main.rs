use std::collections::{HashMap, VecDeque};
use std::ffi::OsString;
use std::path::PathBuf;
use std::sync::{LazyLock, Mutex};

use clap::Parser;
use color_eyre::Result;
use notify_rust::{Notification, Urgency};
use swayipc::{Connection, Node};
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

    let con = Connection::new()?;

    ENV_VARS.lock().unwrap().insert(MODE, OPTIONS.cmd.str().into());

    LazyLock::force(&CONFIG);
    trace!("Config: {:?}", CONFIG);

    match &OPTIONS.cmd {
        Command::Window => todo!(),
        Command::Desktop => todo!(),
        Command::Region => todo!(),
        Command::Name => name(con),
    }
}


#[instrument(level = "error", skip_all)]
fn name(mut con: Connection) -> Result<()> {
    let windows = visible_windows(&mut con)?;
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
fn visible_windows(con: &mut Connection) -> Result<Vec<Node>> {
    let root = con.get_tree()?;

    let mut queue: VecDeque<_> = vec![root].into();
    let mut out = Vec::new();

    while let Some(mut node) = queue.pop_front() {
        // Floating nodes are ordered from bottom to top, we want top first.
        while let Some(child) = node.floating_nodes.pop() {
            queue.push_back(child);
        }
        for child in node.nodes.drain(..) {
            queue.push_back(child);
        }
        if node.pid.is_some() && node.visible.unwrap_or(false) {
            out.push(node);
        }
    }

    Ok(out)
}

impl Command {
    const fn str(&self) -> &'static str {
        match self {
            Self::Window => "window",
            Self::Desktop => "desktop",
            Self::Region => "region",
            Self::Name => "name",
        }
    }
}
