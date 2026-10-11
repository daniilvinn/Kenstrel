//! Where Kestrel keeps its files. Injectable so tests never touch the real
//! `%APPDATA%`.

use std::path::{Path, PathBuf};

use anyhow::{anyhow, Result};
use directories::BaseDirs;

const APP_DIR: &str = "kestrel";
/// Overrides the configuration directory.
pub const CONFIG_DIR_ENV: &str = "KESTREL_CONFIG_DIR";
/// Overrides the state directory (bookmarks, frecency, logs).
pub const STATE_DIR_ENV: &str = "KESTREL_STATE_DIR";

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct AppPaths {
    /// `%APPDATA%\kestrel`
    config_dir: PathBuf,
    /// `%LOCALAPPDATA%\kestrel\state`
    state_dir: PathBuf,
}

impl AppPaths {
    /// The platform locations. On Windows these are `%APPDATA%\kestrel` and
    /// `%LOCALAPPDATA%\kestrel\state`.
    ///
    /// `KESTREL_CONFIG_DIR` and `KESTREL_STATE_DIR` replace them (a portable install, or a
    /// throwaway profile for demos and tests).
    pub fn discover() -> Result<Self> {
        Self::discover_with(|name| std::env::var_os(name).filter(|v| !v.is_empty()))
    }

    fn discover_with(var: impl Fn(&str) -> Option<std::ffi::OsString>) -> Result<Self> {
        let config_dir = var(CONFIG_DIR_ENV).map(PathBuf::from);
        let state_dir = var(STATE_DIR_ENV).map(PathBuf::from);
        if let (Some(config_dir), Some(state_dir)) = (&config_dir, &state_dir) {
            return Ok(Self {
                config_dir: config_dir.clone(),
                state_dir: state_dir.clone(),
            });
        }
        let base = BaseDirs::new().ok_or_else(|| anyhow!("cannot determine the home directory"))?;
        Ok(Self {
            config_dir: config_dir.unwrap_or_else(|| base.config_dir().join(APP_DIR)),
            state_dir: state_dir
                .unwrap_or_else(|| base.data_local_dir().join(APP_DIR).join("state")),
        })
    }

    /// Everything below `root` (for tests).
    pub fn under(root: &Path) -> Self {
        Self {
            config_dir: root.join("config"),
            state_dir: root.join("state"),
        }
    }

    pub fn config_dir(&self) -> &Path {
        &self.config_dir
    }

    pub fn config_file(&self) -> PathBuf {
        self.config_dir.join("config.toml")
    }

    pub fn themes_dir(&self) -> PathBuf {
        self.config_dir.join("themes")
    }

    /// `%LOCALAPPDATA%\kestrel\state`: bookmarks, frecency, logs.
    pub fn state_dir(&self) -> &Path {
        &self.state_dir
    }

    pub fn log_dir(&self) -> PathBuf {
        self.state_dir.join("logs")
    }
}

/// The user's home directory, if known.
pub fn home_dir() -> Option<PathBuf> {
    BaseDirs::new().map(|b| b.home_dir().to_owned())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn layout_under_a_root() {
        let paths = AppPaths::under(Path::new("root"));
        assert_eq!(paths.config_file(), Path::new("root/config/config.toml"));
        assert_eq!(paths.themes_dir(), Path::new("root/config/themes"));
        assert_eq!(paths.log_dir(), Path::new("root/state/logs"));
    }

    #[test]
    fn environment_variables_override_each_directory() {
        let both = AppPaths::discover_with(|name| match name {
            CONFIG_DIR_ENV => Some("C:/portable/config".into()),
            STATE_DIR_ENV => Some("C:/portable/state".into()),
            _ => None,
        })
        .unwrap();
        assert_eq!(both.config_dir(), Path::new("C:/portable/config"));
        assert_eq!(both.log_dir(), Path::new("C:/portable/state/logs"));
        let one = AppPaths::discover_with(|name| {
            (name == STATE_DIR_ENV).then(|| "C:/portable/state".into())
        })
        .unwrap();
        assert!(one.config_dir().ends_with("kestrel"));
        assert_eq!(one.log_dir(), Path::new("C:/portable/state/logs"));
    }

    #[test]
    fn discovered_paths_use_the_kestrel_directory() {
        let paths = AppPaths::discover().unwrap();
        assert!(paths.config_dir().ends_with("kestrel"));
        assert!(paths.log_dir().ends_with("kestrel/state/logs"));
    }
}
