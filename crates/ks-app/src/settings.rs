//! Loading `config.toml`, the keymap and the theme into a ready-to-run session.
//!
//! Nothing here may fail the application: every problem falls back to defaults and is
//! reported through the status line.

use std::fs;
use std::io::ErrorKind;
use std::path::{Path, PathBuf};
use std::time::SystemTime;

use ks_core::config::DEFAULT_CONFIG_TOML;
use ks_core::{AppState, Config, Keymap, Level, Msg, Places, Size, StartParams};
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

/// The well-known folders behind the `g` jump keys. Downloads and Desktop come from the
/// shell's known-folder lookup, so a relocated folder is found where it really is.
pub fn discover_places(paths: &AppPaths, home: Option<PathBuf>) -> Places {
    let user = directories::UserDirs::new();
    let folder = |pick: fn(&directories::UserDirs) -> Option<&Path>| {
        user.as_ref().and_then(pick).map(Path::to_path_buf)
    };
    Places {
        home,
        downloads: folder(directories::UserDirs::download_dir),
        desktop: folder(directories::UserDirs::desktop_dir),
        config: Some(paths.config_dir().to_path_buf()),
    }
}

/// Rebinds the Claude focus toggle to `[claude] focus_key` in the contexts where it lives. An
/// explicit `[keymap.*]` entry in the same config still wins (it is applied afterwards).
pub fn apply_claude_focus_key(keymap: &mut Keymap, focus_key: &str) {
    use ks_core::config::DEFAULT_CLAUDE_FOCUS_KEY;
    if focus_key == DEFAULT_CLAUDE_FOCUS_KEY {
        return;
    }
    let tables = ["normal", "claude"].map(|ctx| {
        (
            ctx.to_owned(),
            std::collections::BTreeMap::from([
                (DEFAULT_CLAUDE_FOCUS_KEY.to_owned(), "none".to_owned()),
                (focus_key.to_owned(), "claude.focus".to_owned()),
            ]),
        )
    });
    let issues = keymap.apply_overrides(&tables.into_iter().collect());
    for issue in issues {
        tracing::warn!(issue, "claude focus key rejected");
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
    pub places: Places,
    pub size: Size,
    pub depth: ColorDepth,
    pub now: SystemTime,
    pub utc_offset_secs: i32,
}

/// The default keymap with the config's `[keymap.*]` overrides applied. Every problem is
/// logged and returned; none is fatal.
pub fn build_keymap(config: &Config) -> (Keymap, Vec<String>) {
    let mut keymap = Keymap::defaults();
    apply_claude_focus_key(&mut keymap, &config.claude.focus_key);
    let issues = keymap.apply_overrides(&config.keymap);
    for issue in &issues {
        tracing::warn!(issue, "keymap override rejected");
    }
    (keymap, issues)
}

/// Loads config, keymap and theme; collects every problem into one error message.
pub fn build_session(inputs: SessionInputs<'_>) -> Session {
    let loaded = load_config(inputs.paths);
    let mut problems: Vec<String> = loaded.problem.into_iter().collect();

    let (keymap, issues) = build_keymap(&loaded.config);
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

    // The first message lists the start directory; the notice (if any) follows.
    let mut startup = vec![Msg::Start];
    if !problems.is_empty() {
        startup.push(Msg::Notify {
            level: Level::Error,
            text: problems.join("; "),
        });
    }
    let state = AppState::new(StartParams {
        cwd: inputs.cwd,
        home: inputs.home,
        places: inputs.places,
        config: loaded.config,
        keymap,
        size: inputs.size,
        now: inputs.now,
        utc_offset_secs: inputs.utc_offset_secs,
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

    fn inputs(paths: &AppPaths, depth: ColorDepth) -> SessionInputs<'_> {
        SessionInputs {
            paths,
            cwd: PathBuf::from("/tmp"),
            home: None,
            places: Places::default(),
            size: Size::new(80, 24),
            depth,
            now: SystemTime::UNIX_EPOCH,
            utc_offset_secs: 0,
        }
    }

    #[test]
    fn places_include_the_config_directory_and_the_given_home() {
        let (_dir, paths) = paths();
        let places = discover_places(&paths, Some(PathBuf::from("/home/me")));
        assert_eq!(places.home.as_deref(), Some(Path::new("/home/me")));
        assert_eq!(places.config.as_deref(), Some(paths.config_dir()));
    }

    #[test]
    fn session_state_carries_the_inputs() {
        let (_dir, paths) = paths();
        let mut input = inputs(&paths, ColorDepth::TrueColor);
        input.utc_offset_secs = 3600;
        let session = build_session(input);
        assert_eq!(session.state.utc_offset_secs, 3600);
        assert_eq!(session.state.cwd, Path::new("/tmp"));
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
        let session = build_session(inputs(&paths, ColorDepth::TrueColor));
        assert_eq!(session.theme.name, "ember");
        let [Msg::Start, Msg::Notify { level, text }] = session.startup.as_slice() else {
            panic!(
                "expected start and one notification, got {:?}",
                session.startup
            );
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
    fn clean_config_only_starts_the_listing() {
        let (_dir, paths) = paths();
        let session = build_session(inputs(&paths, ColorDepth::Indexed256));
        assert_eq!(session.startup, [Msg::Start]);
        assert_eq!(session.theme.name, "ember");
    }

    #[test]
    fn the_claude_focus_key_is_configurable() {
        use ks_core::config::DEFAULT_CLAUDE_FOCUS_KEY;
        use ks_core::keymap::{Invocation, Resolution};

        let (_dir, paths) = paths();
        write_config(
            &paths,
            "[claude]
focus_key = \"ctrl-]\"
",
        );
        let session = build_session(inputs(&paths, ColorDepth::TrueColor));
        assert_eq!(session.startup, [Msg::Start], "the config is valid");
        let focus = Resolution::Action(Invocation {
            action: Action::ClaudeFocus,
            arg: None,
        });
        let keymap = &session.state.keymap;
        for ctx in [KeyContext::Normal, KeyContext::Claude] {
            assert_eq!(keymap.resolve(ctx, &[], "ctrl-]".parse().unwrap()), focus);
            // The old key is free again (in the pane it reaches Claude).
            assert_eq!(
                keymap.resolve(ctx, &[], DEFAULT_CLAUDE_FOCUS_KEY.parse().unwrap()),
                Resolution::NoMatch
            );
        }
        // Unchanged default: nothing is rewritten.
        let defaults = Keymap::defaults();
        let mut same = Keymap::defaults();
        apply_claude_focus_key(&mut same, DEFAULT_CLAUDE_FOCUS_KEY);
        assert_eq!(
            same.bindings(KeyContext::Claude).len(),
            defaults.bindings(KeyContext::Claude).len()
        );
    }

    #[test]
    fn toml_errors_collapse_to_one_line() {
        let err = Config::from_toml("theme = \n").unwrap_err().to_string();
        assert!(err.contains('\n'), "premise: toml errors span lines");
        assert!(!one_line(&err).contains('\n'));
    }

    #[test]
    fn an_unparsable_key_string_is_an_error_level_notice() {
        let (_dir, paths) = paths();
        write_config(
            &paths,
            "[keymap.normal]
\"ctrl-bogus-x\" = \"quit\"
",
        );
        let session = build_session(inputs(&paths, ColorDepth::TrueColor));
        let [Msg::Start, Msg::Notify { level, text }] = session.startup.as_slice() else {
            panic!("expected one notification, got {:?}", session.startup);
        };
        assert_eq!(*level, Level::Error);
        assert!(text.contains("ctrl-bogus-x"), "{text}");
    }
}
