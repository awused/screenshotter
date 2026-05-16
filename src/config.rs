use std::ffi::OsString;
use std::path::{Path, PathBuf};
use std::sync::LazyLock;

use serde::{Deserialize, Deserializer};

use crate::OPTIONS;

#[derive(Debug, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Transform {
    Format(String),
    Delegate(PathBuf),
}


#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)] // Break if both format and delegate are defined
pub struct Override {
    #[serde(default)]
    pub name: Option<String>,
    #[serde(default)]
    pub regex: Option<String>,
    #[serde(default, flatten)]
    pub transform: Option<Transform>,
    #[serde(default)]
    pub yearly: bool,
    #[serde(default)]
    pub monthly: bool,
    #[serde(default)]
    pub callback: Option<PathBuf>,
}

#[derive(Debug, Deserialize)]
pub struct Config {
    pub screenshot_dir: PathBuf,

    pub fallback: String,

    #[serde(default)]
    pub overrides: Vec<Override>,

    #[serde(default)]
    pub ignored_parents: Vec<String>,

    pub compression: usize,

    #[serde(default, deserialize_with = "empty_path_is_none")]
    pub callback: Option<PathBuf>,

    #[serde(default, deserialize_with = "empty_path_is_none")]
    pub slurp: Option<PathBuf>,
}


// Serde seems broken with OsString for some reason
fn empty_path_is_none<'de, D, T>(deserializer: D) -> Result<Option<T>, D::Error>
where
    D: Deserializer<'de>,
    T: From<PathBuf>,
{
    let s = PathBuf::deserialize(deserializer)?;
    if s.as_os_str().is_empty() { Ok(None) } else { Ok(Some(s.into())) }
}

pub static CONFIG: LazyLock<Config> = LazyLock::new(|| {
    let (config, _) =
        awconf::load_config::<Config>("screenshotter", OPTIONS.awconf.as_ref(), None::<&str>)
            .expect("Error loading config");
    assert!(
        config.screenshot_dir.is_absolute(),
        "Screenshot directory {:?} is not absolute",
        config.screenshot_dir
    );
    assert!(
        config.screenshot_dir.is_dir(),
        "Screenshot directory {:?} is not a directory",
        config.screenshot_dir
    );

    config
});

pub static SLURP: LazyLock<&'static Path> = LazyLock::new(|| {
    let slurp = CONFIG.slurp.as_deref().unwrap_or_else(|| Path::new("slurp"));
    trace!("Slurp command: {slurp:?}");
    slurp
});
