//! Runtime changes to the configuration: re-reading `config.toml` and switching themes.
//!
//! `update` only requests these ([`Effect::ReloadConfig`](ks_core::Effect),
//! [`Effect::SetTheme`](ks_core::Effect)); the event loop asks a [`Reloader`] to do the
//! small bit of file reading and feeds the outcome back as ordinary messages.

use ks_core::{Level, Msg};
use ks_ui::{ColorDepth, Theme, ThemeLoader};

use crate::paths::AppPaths;
use crate::settings::{build_keymap, load_config};

pub struct Reloader {
    paths: AppPaths,
    loader: ThemeLoader,
}

impl Reloader {
    pub fn new(paths: AppPaths, depth: ColorDepth) -> Self {
        let loader = ThemeLoader::new(Some(paths.themes_dir()), depth);
        Self { paths, loader }
    }

    /// Themes that can be switched to: built-ins plus `themes/*.toml`.
    pub fn themes(&self) -> Vec<String> {
        self.loader.available()
    }

    /// Loads a theme, or explains why not (the running theme is then kept).
    pub fn load_theme(&self, name: &str) -> Result<Theme, String> {
        self.loader.load(name).map_err(|e| {
            tracing::warn!(theme = name, error = %e, "cannot switch theme");
            e.to_string()
        })
    }

    /// Re-reads `config.toml`. A file that cannot be read or parsed leaves the running
    /// settings alone and only reports the problem.
    pub fn reload_config(&self) -> Msg {
        let loaded = load_config(&self.paths);
        if let Some(problem) = loaded.problem {
            return Msg::Notify {
                level: Level::Error,
                text: format!("{problem} (keeping the current settings)"),
            };
        }
        let (keymap, issues) = build_keymap(&loaded.config);
        tracing::info!(issues = issues.len(), "config reloaded");
        Msg::ConfigReloaded {
            config: Box::new(loaded.config),
            keymap: Box::new(keymap),
            issues,
        }
    }
}

#[cfg(test)]
mod tests {
    use std::fs;

    use ks_core::Keymap;

    use super::*;

    fn reloader() -> (tempfile::TempDir, Reloader) {
        let dir = tempfile::tempdir().unwrap();
        let paths = AppPaths::under(dir.path());
        fs::create_dir_all(paths.config_dir()).unwrap();
        (dir, Reloader::new(paths, ColorDepth::TrueColor))
    }

    fn write(r: &Reloader, text: &str) {
        fs::write(r.paths.config_file(), text).unwrap();
    }

    #[test]
    fn reload_picks_up_keymap_and_icon_changes() {
        let (_dir, r) = reloader();
        write(
            &r,
            "icons = \"ascii\"\n[keymap.normal]\n\"T\" = \"file.trash\"\n",
        );
        let Msg::ConfigReloaded {
            config,
            keymap,
            issues,
        } = r.reload_config()
        else {
            panic!("expected a reload");
        };
        assert!(issues.is_empty(), "{issues:?}");
        assert_eq!(config.icons, ks_core::config::IconSet::Ascii);
        assert_ne!(*keymap, Keymap::defaults());
    }

    #[test]
    fn keymap_problems_are_carried_not_fatal() {
        let (_dir, r) = reloader();
        write(
            &r,
            "[keymap.normal]\n\"g\" = \"go.home\"\n\"x\" = \"nope\"\n",
        );
        let Msg::ConfigReloaded { issues, .. } = r.reload_config() else {
            panic!("expected a reload");
        };
        assert_eq!(issues.len(), 2, "{issues:?}");
    }

    #[test]
    fn a_broken_file_keeps_the_running_settings() {
        let (_dir, r) = reloader();
        write(&r, "theme = \n");
        let Msg::Notify { level, text } = r.reload_config() else {
            panic!("expected a notification");
        };
        assert_eq!(level, Level::Error);
        assert!(text.contains("keeping the current settings"), "{text}");
    }

    #[test]
    fn themes_come_from_builtins_and_the_user_directory() {
        let (_dir, r) = reloader();
        fs::create_dir_all(r.paths.themes_dir()).unwrap();
        fs::write(
            r.paths.themes_dir().join("mine.toml"),
            "base = \"paper\"\n[colors]\naccent = \"#ff0000\"\n",
        )
        .unwrap();
        assert_eq!(r.themes(), ["ember", "paper", "mine"]);
        let theme = r.load_theme("mine").unwrap();
        assert_eq!(theme.name, "mine");
        assert!(r.load_theme("missing").unwrap_err().contains("not found"));
    }
}
