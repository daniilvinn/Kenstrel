//! Where Kestrel keeps its files (PLAN §9). Injectable so tests never touch the real
//! `%APPDATA%`.

use std::path::{Path, PathBuf};

use anyhow::{anyhow, Result};
use directories::BaseDirs;

const APP_DIR: &str = "kestrel";

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
    pub fn discover() -> Result<Self> {
        let base = BaseDirs::new().ok_or_else(|| anyhow!("cannot determine the home directory"))?;
        Ok(Self {
            config_dir: base.config_dir().join(APP_DIR),
            state_dir: base.data_local_dir().join(APP_DIR).join("state"),
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
    fn discovered_paths_use_the_kestrel_directory() {
        let paths = AppPaths::discover().unwrap();
        assert!(paths.config_dir().ends_with("kestrel"));
        assert!(paths.log_dir().ends_with("kestrel/state/logs"));
    }
}
