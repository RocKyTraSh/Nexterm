//! Platform abstraction layer.
//!
//! Linux-first. All OS-specific path/identity logic lives here so the rest of
//! the workspace stays platform-agnostic. Windows/macOS specifics will be added
//! behind the same functions (and, where needed, `#[cfg(...)]`) without
//! touching callers.

use std::path::PathBuf;

/// Canonical application name used for config/data directories and service ids.
pub const APP_NAME: &str = "rust-remote-suite";

/// Detected operating system family.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Os {
    Linux,
    Windows,
    MacOs,
    Other,
}

/// Returns the OS family this binary was compiled for.
pub fn current_os() -> Os {
    if cfg!(target_os = "linux") {
        Os::Linux
    } else if cfg!(target_os = "windows") {
        Os::Windows
    } else if cfg!(target_os = "macos") {
        Os::MacOs
    } else {
        Os::Other
    }
}

/// Directory for user configuration (`AppConfig`). Never stores secrets.
///
/// Linux: `$XDG_CONFIG_HOME/rust-remote-suite` (usually `~/.config/...`).
pub fn config_dir() -> PathBuf {
    base_or_cwd(dirs::config_dir()).join(APP_NAME)
}

/// Directory for application data (profile database, logs).
///
/// Linux: `$XDG_DATA_HOME/rust-remote-suite` (usually `~/.local/share/...`).
pub fn data_dir() -> PathBuf {
    base_or_cwd(dirs::data_dir()).join(APP_NAME)
}

/// Default OpenSSH `known_hosts` location, if a home directory is known.
pub fn default_known_hosts() -> Option<PathBuf> {
    dirs::home_dir().map(|h| h.join(".ssh").join("known_hosts"))
}

fn base_or_cwd(base: Option<PathBuf>) -> PathBuf {
    // Falling back to the current directory keeps the app usable in odd
    // environments (CI, containers) instead of panicking.
    base.unwrap_or_else(|| PathBuf::from("."))
}
