//! Loading `config.toml`, the keymap and the theme into a ready-to-run session.
//!
//! Nothing here may fail the application: every problem falls back to defaults and is
//! reported through the status line.

use std::fs;
use std::io::ErrorKind;
use std::path::PathBuf;

use ks_core::config::DEFAULT_CONFIG_TOML;
use ks_core::{AppState, Config, Keymap, Level, Msg, Size, StartParams};
use ks_ui::{ColorDepth, Theme, ThemeLoader};

use crate::paths::AppPaths;

/// Result of reading the config file.
#[derive(Debug)]
pub struct LoadedConfig {
    pub config: Config,
    /// Something went wrong; `config` is the default.
    pub problem: Option<String>,
    /// A fresh default file was written.
    pub created: bool,
}

/// Reads `config.toml`, writing a commented default on first run.
pub fn load_config(paths: &AppPaths) -> LoadedConfig {
    let file = paths.config_file();
    let fallback = |problem: String| LoadedConfig {
        config: Config::default(),
        problem: Some(problem),
        created: false,
    };
    match fs::read_to_string(&file) {
        Ok(text) => match Config::from_toml(&text) {
            Ok(config) => LoadedConfig {
                config,
                problem: None,
                created: false,
            },
            Err(e) => {
                tracing::warn!(path = %file.display(), error = %e, "invalid config");
                fallback(format!("config.toml: {}", one_line(&e.to_string())))
            }
        },
        Err(e) if e.kind() == ErrorKind::NotFound => {
            let written = fs::create_dir_all(paths.config_dir())
                .and_then(|()| fs::write(&file, DEFAULT_CONFIG_TOML));
            match written {
                Ok(()) => {
                    tracing::info!(path = %file.display(), "wrote default config");
                    LoadedConfig {
                        config: Config::default(),
                        problem: None,
                        created: true,
                    }
                }
                Err(e) => {
                    tracing::warn!(path = %file.display(), error = %e, "cannot write default config");
                    fallback(format!("cannot write {}: {e}", file.display()))
                }
            }
        }
        Err(e) => {
            tracing::warn!(path = %file.display(), error = %e, "cannot read config");
            fallback(format!("cannot read {}: {e}", file.display()))
        }
    }
}

/// Collapses a multi-line TOML error into `first line: last line`.
fn one_line(message: &str) -> String {
    let mut lines = message.lines().map(str::trim).filter(|l| !l.is_empty());
    match (lines.next(), lines.next_back()) {
        (Some(first), Some(last)) => format!("{first}: {last}"),
        (Some(only), None) => only.to_owned(),
        _ => "invalid config".to_owned(),
    }
}

/// Everything needed to run, plus start-up messages for the status line.
pub struct Session {
    pub state: AppState,
    pub theme: Theme,
    pub startup: Vec<Msg>,
}

pub struct SessionInputs<'a> {
    pub paths: &'a AppPaths,
    pub cwd: PathBuf,
    pub home: Option<PathBuf>,
    pub size: Size,
    pub depth: ColorDepth,
}

/// Loads config, keymap and theme; collects every problem into one error message.
pub fn build_session(inputs: SessionInputs<'_>) -> Session {
    let loaded = load_config(inputs.paths);
    let mut problems: Vec<String> = loaded.problem.into_iter().collect();

    let mut keymap = Keymap::defaults();
    let issues = keymap.apply_overrides(&loaded.config.keymap);
    for issue in &issues {
        tracing::warn!(issue, "keymap override rejected");
    }
    if let Some(first) = issues.first() {
        let more = issues.len() - 1;
        problems.push(if more == 0 {
            first.clone()
        } else {
            format!("{first} (+{more} more)")
        });
    }

    let loader = ThemeLoader::new(Some(inputs.paths.themes_dir()), inputs.depth);
    let (theme, theme_problem) = loader.load_or_fallback(&loaded.config.theme);
    if let Some(problem) = theme_problem {
        tracing::warn!(problem, "theme fallback");
        problems.push(problem);
    }

    let startup = if problems.is_empty() {
        Vec::new()
    } else {
        vec![Msg::Notify {
            level: Level::Error,
            text: problems.join("; "),
        }]
    };
    let state = AppState::new(StartParams {
        cwd: inputs.cwd,
        home: inputs.home,
        config: loaded.config,
        keymap,
        size: inputs.size,
    });
    Session {
        state,
        theme,
        startup,
    }
}

#[cfg(test)]
mod tests {
    use ks_core::{Action, KeyContext};

    use super::*;

    fn paths() -> (tempfile::TempDir, AppPaths) {
        let dir = tempfile::tempdir().unwrap();
        let paths = AppPaths::under(dir.path());
        (dir, paths)
    }

    fn write_config(paths: &AppPaths, text: &str) {
        fs::create_dir_all(paths.config_dir()).unwrap();
        fs::write(paths.config_file(), text).unwrap();
    }

    #[test]
    fn first_run_writes_a_commented_default_config() {
        let (_dir, paths) = paths();
        let loaded = load_config(&paths);
        assert!(loaded.created);
        assert!(loaded.problem.is_none());
        assert_eq!(loaded.config, Config::default());
        let written = fs::read_to_string(paths.config_file()).unwrap();
        assert_eq!(written, DEFAULT_CONFIG_TOML);

        // The second run reads it back without rewriting.
        let again = load_config(&paths);
        assert!(!again.created && again.problem.is_none());
    }

    #[test]
    fn existing_config_is_parsed_and_never_overwritten() {
        let (_dir, paths) = paths();
        write_config(&paths, "theme = \"paper\"\n");
        let loaded = load_config(&paths);
        assert_eq!(loaded.config.theme, "paper");
        assert!(!loaded.created);
        assert_eq!(
            fs::read_to_string(paths.config_file()).unwrap(),
            "theme = \"paper\"\n"
        );
    }

    #[test]
    fn bad_config_falls_back_to_defaults_with_a_message() {
        let (_dir, paths) = paths();
        write_config(&paths, "theme = \n");
        let loaded = load_config(&paths);
        assert_eq!(loaded.config, Config::default());
        let problem = loaded.problem.unwrap();
        assert!(problem.starts_with("config.toml: "), "{problem}");
        assert!(!problem.contains('\n'));
        // The broken file is left alone for the user to fix.
        assert_eq!(
            fs::read_to_string(paths.config_file()).unwrap(),
            "theme = \n"
        );
    }

    #[test]
    fn unwritable_config_dir_is_reported_not_fatal() {
        let (dir, _) = paths();
        // A file where the config directory should be.
        let blocker = dir.path().join("blocker");
        fs::write(&blocker, "").unwrap();
        let loaded = load_config(&AppPaths::under(&blocker));
        assert_eq!(loaded.config, Config::default());
        assert!(loaded.problem.is_some());
    }

    #[test]
    fn session_collects_config_keymap_and_theme_problems() {
        let (_dir, paths) = paths();
        write_config(
            &paths,
            "theme = \"nope\"\n[keymap.normal]\nx = \"no.such.action\"\nz = \"quit\"\n",
        );
        let session = build_session(SessionInputs {
            paths: &paths,
            cwd: PathBuf::from("/tmp"),
            home: None,
            size: Size::new(80, 24),
            depth: ColorDepth::TrueColor,
        });
        assert_eq!(session.theme.name, "ember");
        let [Msg::Notify { level, text }] = session.startup.as_slice() else {
            panic!("expected one notification, got {:?}", session.startup);
        };
        assert_eq!(*level, Level::Error);
        assert!(text.contains("no.such.action"), "{text}");
        assert!(text.contains("theme `nope` not found"), "{text}");
        // The valid keymap line still applied.
        assert!(session
            .state
            .keymap
            .keys_for(KeyContext::Normal, Action::Quit)
            .contains(&"z".to_owned()));
    }

    #[test]
    fn clean_config_yields_no_startup_messages() {
        let (_dir, paths) = paths();
        let session = build_session(SessionInputs {
            paths: &paths,
            cwd: PathBuf::from("/tmp"),
            home: None,
            size: Size::new(80, 24),
            depth: ColorDepth::Indexed256,
        });
        assert!(session.startup.is_empty());
        assert_eq!(session.theme.name, "ember");
    }

    #[test]
    fn toml_errors_collapse_to_one_line() {
        let err = Config::from_toml("theme = \n").unwrap_err().to_string();
        assert!(err.contains('\n'), "premise: toml errors span lines");
        assert!(!one_line(&err).contains('\n'));
    }
}
