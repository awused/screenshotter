use std::collections::{HashMap, HashSet};
use std::ffi::OsString;
use std::path::{Path, PathBuf};
use std::sync::LazyLock;

use color_eyre::Result;
use color_eyre::eyre::OptionExt;
use regex::Regex;
use swayipc::Node;

use crate::DELEGATE_ENV;
use crate::config::CONFIG;
use crate::selection::Region;


#[derive(Debug, Default)]
pub struct Application {
    pub relative_dir: PathBuf,
    pub yearly: bool,
    pub monthly: bool,
    pub callback: Option<&'static Path>,
}

fn maybe_get_child_process(name: String, pid: i32) -> Result<(String, i32)> {
    todo!()
}

#[instrument(level = "error", skip_all)]
pub fn application_for(region: Region, window: Option<Node>) -> Result<Application> {
    let mut delegate_env = DELEGATE_ENV.lock().unwrap();
    delegate_env.insert("SCREENSHOTTER_GEOMETRY", region.to_string().into());

    let mut application = Application::default();

    if let Some(window) = window {
        let pid = window.pid.ok_or_eyre("Unreachable")?;
        if let Some(wm_name) = window.name {
            delegate_env.insert("SCREENSHOTTER_WM_NAME", wm_name.clone().into());
        };

        let name = if let Some(app_id) = window.app_id {
            debug!("Got app_id from window \"{app_id}\"");
            let name = app_id.rsplit_once('.').map_or(&*app_id, |(_left, right)| right);
            let name = convert_application_name(name);
            delegate_env.insert("SCREENHOTTER_APP_ID", app_id.into());
            name
        } else {
            convert_application_name(&CONFIG.fallback)
        };

        delegate_env.insert("SCREENSHOTTER_NAME", name.clone().into());
        delegate_env.insert("SCREENSHOTTER_DIR", name.into());
        delegate_env.insert("SCREENSHOTTER_PID", pid.to_string().into());
        delegate_env.insert("SCREENSHOTTER_WINDOW_PID", pid.to_string().into());
        delegate_env.insert("SCREENSHOTTER_WINDOW_ID", window.id.to_string().into());
    } else {
        delegate_env.insert("SCREENSHOTTER_NAME", CONFIG.fallback.clone().into());
        delegate_env.insert("SCREENSHOTTER_DIR", CONFIG.fallback.clone().into());
    }

    Ok(application)
}

static SAFE_FILENAME: LazyLock<Regex> = LazyLock::new(|| Regex::new(r"[^\pL\pN\-_+=]+").unwrap());
static HYPHENS: LazyLock<Regex> = LazyLock::new(|| Regex::new(r"--+").unwrap());

fn convert_application_name(input: &str) -> String {
    let name = input.to_lowercase();
    let name = SAFE_FILENAME.replace_all(&name, "-");
    let name = HYPHENS.replace_all(&name, "-");
    let name = name.trim_matches('-');
    debug!("Converted \"{input}\" to \"{name}\"");
    name.to_string()
}
