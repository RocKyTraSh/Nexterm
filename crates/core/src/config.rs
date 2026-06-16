//! Application configuration (UI preferences, defaults). Contains **no secrets**.

use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

use crate::error::{CoreError, Result};

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(rename_all = "snake_case")]
pub enum ThemeMode {
    #[default]
    System,
    Light,
    Dark,
}

/// Top-level user configuration, persisted as TOML.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct AppConfig {
    #[serde(default)]
    pub theme: ThemeMode,
    /// Default SSH port suggested in the UI.
    #[serde(default = "default_ssh_port")]
    pub default_ssh_port: u16,
    /// `tracing` filter applied if `RUST_LOG` is not set.
    #[serde(default = "default_log_filter")]
    pub log_filter: String,
    /// Confirm before sending flagged commands in multi-exec mode.
    #[serde(default = "default_true")]
    pub confirm_dangerous_commands: bool,
}

fn default_ssh_port() -> u16 {
    22
}
fn default_log_filter() -> String {
    "info".to_string()
}
fn default_true() -> bool {
    true
}

impl Default for AppConfig {
    fn default() -> Self {
        Self {
            theme: ThemeMode::default(),
            default_ssh_port: default_ssh_port(),
            log_filter: default_log_filter(),
            confirm_dangerous_commands: true,
        }
    }
}

impl AppConfig {
    /// Standard config file path: `<config_dir>/config.toml`.
    pub fn default_path() -> PathBuf {
        rrs_platform::config_dir().join("config.toml")
    }

    /// Load config from `path`, returning [`AppConfig::default`] if absent.
    pub async fn load_or_default(path: &Path) -> Result<Self> {
        match tokio::fs::read_to_string(path).await {
            Ok(text) => Ok(toml::from_str(&text)?),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(Self::default()),
            Err(e) => Err(CoreError::Io(e)),
        }
    }

    /// Persist config to `path`, creating parent directories as needed.
    pub async fn save(&self, path: &Path) -> Result<()> {
        if let Some(parent) = path.parent() {
            tokio::fs::create_dir_all(parent).await?;
        }
        let text = toml::to_string_pretty(self)?;
        tokio::fs::write(path, text).await?;
        Ok(())
    }
}
