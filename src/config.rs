use std::path::PathBuf;
use std::sync::LazyLock;

use serde::{Deserialize, Deserializer};

use crate::OPTIONS;

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)] // Break if both format and delegate are defined
pub struct Override {
    #[serde(default)]
    pub name: Option<String>,
    #[serde(default)]
    pub regex: Option<String>,

    #[serde(default)]
    pub format: Option<String>,
    #[serde(default)]
    pub delegate: Option<PathBuf>,

    #[serde(default)]
    pub yearly: bool,
    #[serde(default)]
    pub monthly: bool,
    #[serde(default)]
    pub callback: Option<PathBuf>,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct MonitorPosition {
    pub id: String,
    pub x: i32,
    pub y: i32,
}

#[derive(Debug, Default, Deserialize, Clone, Copy)]
#[serde(rename_all = "lowercase")]
pub enum FileFormat {
    #[default]
    Png,
    Webp,
}

impl FileFormat {
    pub const fn extension(self) -> &'static str {
        match self {
            Self::Png => "png",
            Self::Webp => "webp",
        }
    }
}

#[derive(Debug, Deserialize)]
pub struct Config {
    pub screenshot_dir: PathBuf,

    pub fallback: String,

    #[serde(default)]
    pub ignored_parents: Vec<String>,

    #[serde(default)]
    pub format: FileFormat,

    pub compression: u8,

    #[serde(default, deserialize_with = "empty_path_is_none")]
    pub callback: Option<PathBuf>,

    #[serde(default)]
    pub timeout: u64,

    #[serde(default)]
    pub monitor_positions: Vec<MonitorPosition>,

    #[serde(default)]
    pub overrides: Vec<Override>,
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

    let dir = &config.screenshot_dir;
    assert!(dir.is_absolute(), "Screenshot directory {dir:?} is not absolute",);
    assert!(dir.is_dir(), "Screenshot directory {dir:?} is not a directory",);

    config
});
