use std::path::PathBuf;

use directories::BaseDirs;
use serde::{Deserialize, Serialize};

use crate::error::CurlyError;

pub const CONFIG_VERSION: u32 = 1;

#[derive(Debug, Clone)]
pub struct StoragePaths {
    pub config_root: PathBuf,
    pub data_root: PathBuf,
    pub config_file: PathBuf,
    pub requests_dir: PathBuf,
    pub history_db: PathBuf,
}

impl StoragePaths {
    pub fn discover() -> Result<Self, CurlyError> {
        let base = BaseDirs::new().ok_or_else(|| {
            CurlyError::Config("cannot determine platform configuration directories".to_string())
        })?;
        let config_root = std::env::var_os("CURLY_CONFIG_DIR")
            .map(PathBuf::from)
            .unwrap_or_else(|| base.config_dir().join("curly"));
        let data_root = std::env::var_os("CURLY_DATA_DIR")
            .map(PathBuf::from)
            .unwrap_or_else(|| base.data_local_dir().join("curly"));
        Ok(Self {
            config_file: config_root.join("config.toml"),
            requests_dir: config_root.join("requests"),
            history_db: data_root.join("history.sqlite3"),
            config_root,
            data_root,
        })
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(default)]
pub struct AppConfig {
    pub version: u32,
}

impl Default for AppConfig {
    fn default() -> Self {
        Self {
            version: CONFIG_VERSION,
        }
    }
}

impl AppConfig {
    pub fn load(paths: &StoragePaths) -> Result<Self, CurlyError> {
        if !paths.config_file.exists() {
            return Ok(Self::default());
        }
        let text = std::fs::read_to_string(&paths.config_file).map_err(|err| {
            CurlyError::Config(format!(
                "cannot read config {}: {err}",
                paths.config_file.display()
            ))
        })?;
        let config: Self = toml::from_str(&text).map_err(|err| {
            CurlyError::Config(format!(
                "invalid config {}: {err}",
                paths.config_file.display()
            ))
        })?;
        if config.version != CONFIG_VERSION {
            return Err(CurlyError::Config(format!(
                "unsupported config version {}; expected {}",
                config.version, CONFIG_VERSION
            )));
        }
        Ok(config)
    }
}
