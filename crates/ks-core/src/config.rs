//! The user configuration model (PLAN §9).
//!
//! Pure data and parsing; locating, reading and writing the file is the application's job.
//! Every field has a default, so an empty file is a valid config.

use std::collections::BTreeMap;

use serde::Deserialize;
use thiserror::Error;

/// A fully commented-out config written on first run. Uncommenting every setting yields
/// exactly [`Config::default`] (enforced by a test).
pub const DEFAULT_CONFIG_TOML: &str = include_str!("../assets/default_config.toml");

#[derive(Debug, Error)]
pub enum ConfigError {
    #[error("{0}")]
    Parse(#[from] toml::de::Error),
    #[error("invalid value: {0}")]
    Invalid(String),
}

/// Which glyph set to draw icons with.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum IconSet {
    #[default]
    Nerd,
    Unicode,
    Ascii,
}

/// Sort key for directory listings.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum SortKey {
    #[default]
    Name,
    Size,
    Mtime,
    Ext,
}

#[derive(Clone, Debug, PartialEq, Eq, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct SortConfig {
    pub key: SortKey,
    pub reverse: bool,
    pub dirs_first: bool,
}

impl Default for SortConfig {
    fn default() -> Self {
        Self {
            key: SortKey::Name,
            reverse: false,
            dirs_first: true,
        }
    }
}

/// Visibility toggles (PLAN §6.2, §7).
#[derive(Clone, Debug, Default, PartialEq, Eq, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct FilesConfig {
    /// Show dotfiles and hidden/system files.
    pub show_hidden: bool,
    /// Include gitignored files in the finder and searches.
    pub show_ignored: bool,
    /// Divisor for human sizes: 1024 (KiB-style) or 1000 (SI).
    pub size_base: SizeBase,
}

/// Base for human readable sizes.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum SizeBase {
    Si,
    #[default]
    Binary,
}

impl SizeBase {
    pub const fn divisor(self) -> u64 {
        match self {
            SizeBase::Si => 1000,
            SizeBase::Binary => 1024,
        }
    }
}

impl<'de> Deserialize<'de> for SizeBase {
    fn deserialize<D: serde::Deserializer<'de>>(d: D) -> Result<Self, D::Error> {
        match u32::deserialize(d)? {
            1000 => Ok(SizeBase::Si),
            1024 => Ok(SizeBase::Binary),
            other => Err(serde::de::Error::custom(format!(
                "size_base must be 1000 or 1024, got {other}"
            ))),
        }
    }
}

/// Which terminal graphics protocol draws image previews.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum ImageProtocol {
    /// Ask the terminal (device attributes query); halfblocks when it does not answer.
    #[default]
    Auto,
    Halfblocks,
    Sixel,
    Kitty,
    Iterm2,
}

#[derive(Clone, Debug, PartialEq, Eq, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct PreviewConfig {
    pub enabled: bool,
    /// Image protocol; `auto` asks the terminal at start-up.
    pub image_protocol: ImageProtocol,
    /// Largest number of bytes read for a text preview.
    pub max_bytes: u64,
    /// Largest number of lines shown for a text preview.
    pub max_lines: usize,
}

impl Default for PreviewConfig {
    fn default() -> Self {
        Self {
            enabled: true,
            image_protocol: ImageProtocol::Auto,
            max_bytes: 256 * 1024,
            max_lines: 2000,
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct LayoutConfig {
    /// Width of the list pane as a percentage of the body (10..=90).
    pub list_percent: u16,
    /// Show the parent directory column (Miller columns).
    pub parent_column: bool,
}

impl Default for LayoutConfig {
    fn default() -> Self {
        Self {
            list_percent: 40,
            parent_column: false,
        }
    }
}

/// Whether the editor runs inside the terminal (so the TUI must be suspended while it runs).
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum EditorTerminal {
    /// Decide by the program name: vim, nvim, hx, nano, micro, ... are terminal editors,
    /// everything else (code, notepad, ...) is a GUI editor started detached.
    #[default]
    Auto,
    /// Always suspend the TUI and run the editor attached to the terminal.
    Always,
    /// Never suspend; start the editor detached.
    Never,
}

/// Command templates for external editors. `{file}` and `{line}` are substituted.
#[derive(Clone, Debug, PartialEq, Eq, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct EditorConfig {
    /// Command to open a file. Empty means `$VISUAL`, then `$EDITOR`, then `code`, then
    /// `notepad`.
    pub open: String,
    /// Command to open a file at a line (content search hits).
    pub open_at_line: String,
    /// Terminal or GUI editor; see [`EditorTerminal`].
    pub terminal: EditorTerminal,
}

impl Default for EditorConfig {
    fn default() -> Self {
        Self {
            open: String::new(),
            open_at_line: "code -g {file}:{line}".to_owned(),
            terminal: EditorTerminal::Auto,
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct UiConfig {
    /// Delay before the which-key popup appears after a prefix key.
    pub which_key_delay_ms: u64,
}

impl Default for UiConfig {
    fn default() -> Self {
        Self {
            which_key_delay_ms: 300,
        }
    }
}

/// Where the embedded Claude pane sits relative to the file list.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum PanePosition {
    /// Below the list.
    #[default]
    Bottom,
    /// To the right of the list.
    Right,
}

impl PanePosition {
    pub const fn toggled(self) -> Self {
        match self {
            PanePosition::Bottom => PanePosition::Right,
            PanePosition::Right => PanePosition::Bottom,
        }
    }
}

/// Environment variables removed from every `claude` child so the subscription login is used
/// (PLAN §10.1). Kept in sync with `ks_claude::DEFAULT_STRIP_ENV` by a test in `ks-app`.
pub const DEFAULT_CLAUDE_STRIP_ENV: &[&str] = &[
    "ANTHROPIC_API_KEY",
    "ANTHROPIC_AUTH_TOKEN",
    "CLAUDE_CODE_OAUTH_TOKEN",
    "CLAUDE_CODE_USE_BEDROCK",
    "CLAUDE_CODE_USE_VERTEX",
    "CLAUDE_CODE_USE_FOUNDRY",
];

/// The default key that moves keyboard focus between the explorer and the Claude pane.
pub const DEFAULT_CLAUDE_FOCUS_KEY: &str = "ctrl-\\";

/// Claude Code integration (PLAN §10).
#[derive(Clone, Debug, PartialEq, Eq, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct ClaudeConfig {
    /// Path of the `claude` executable. Empty means: look it up on `PATH`.
    pub binary: String,
    /// Environment variables removed from the `claude` child process.
    pub strip_env: Vec<String>,
    /// Where the embedded pane is drawn.
    pub pane_position: PanePosition,
    /// Share of the body the pane takes, in percent (15..=85).
    pub pane_percent: u16,
    /// Key that toggles focus between the explorer and the pane.
    pub focus_key: String,
}

impl Default for ClaudeConfig {
    fn default() -> Self {
        Self {
            binary: String::new(),
            strip_env: DEFAULT_CLAUDE_STRIP_ENV
                .iter()
                .map(|s| (*s).to_owned())
                .collect(),
            pane_position: PanePosition::Bottom,
            pane_percent: 40,
            focus_key: DEFAULT_CLAUDE_FOCUS_KEY.to_owned(),
        }
    }
}

/// `[keymap.<context>]` tables: key sequence to action id (or `"none"`).
pub type KeymapOverrides = BTreeMap<String, BTreeMap<String, String>>;

/// The whole `config.toml`.
#[derive(Clone, Debug, PartialEq, Eq, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct Config {
    /// Theme name: a file in `themes/` or a built-in (`ember`, `paper`).
    pub theme: String,
    pub icons: IconSet,
    pub sort: SortConfig,
    pub files: FilesConfig,
    pub preview: PreviewConfig,
    pub layout: LayoutConfig,
    pub editor: EditorConfig,
    pub ui: UiConfig,
    pub claude: ClaudeConfig,
    pub keymap: KeymapOverrides,
}

impl Default for Config {
    fn default() -> Self {
        Self {
            theme: "ember".to_owned(),
            icons: IconSet::default(),
            sort: SortConfig::default(),
            files: FilesConfig::default(),
            preview: PreviewConfig::default(),
            layout: LayoutConfig::default(),
            editor: EditorConfig::default(),
            ui: UiConfig::default(),
            claude: ClaudeConfig::default(),
            keymap: KeymapOverrides::new(),
        }
    }
}

impl Config {
    /// Parses and validates a config file's text.
    pub fn from_toml(text: &str) -> Result<Self, ConfigError> {
        let config: Config = toml::from_str(text)?;
        config.validate()?;
        Ok(config)
    }

    fn validate(&self) -> Result<(), ConfigError> {
        if !(10..=90).contains(&self.layout.list_percent) {
            return Err(ConfigError::Invalid(format!(
                "layout.list_percent must be between 10 and 90, got {}",
                self.layout.list_percent
            )));
        }
        if self.preview.max_bytes == 0 || self.preview.max_lines == 0 {
            return Err(ConfigError::Invalid(
                "preview.max_bytes and preview.max_lines must be greater than 0".to_owned(),
            ));
        }
        if self.theme.trim().is_empty() {
            return Err(ConfigError::Invalid("theme must not be empty".to_owned()));
        }
        let claude = &self.claude;
        if !(15..=85).contains(&claude.pane_percent) {
            return Err(ConfigError::Invalid(format!(
                "claude.pane_percent must be between 15 and 85, got {}",
                claude.pane_percent
            )));
        }
        if claude.focus_key.parse::<crate::key::KeyChord>().is_err() {
            return Err(ConfigError::Invalid(format!(
                "claude.focus_key `{}` is not a valid key",
                claude.focus_key
            )));
        }
        if let Some(bad) = claude
            .strip_env
            .iter()
            .find(|n| n.is_empty() || n.contains('='))
        {
            return Err(ConfigError::Invalid(format!(
                "claude.strip_env entry `{bad}` is not a variable name"
            )));
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn editor_terminal_parses_and_rejects_unknown_values() {
        for (text, want) in [
            ("auto", EditorTerminal::Auto),
            ("always", EditorTerminal::Always),
            ("never", EditorTerminal::Never),
        ] {
            let config = Config::from_toml(&format!(
                "[editor]
terminal = \"{text}\""
            ))
            .unwrap();
            assert_eq!(config.editor.terminal, want);
        }
        assert!(Config::from_toml(
            "[editor]
terminal = \"sometimes\""
        )
        .is_err());
    }

    #[test]
    fn empty_file_is_the_default_config() {
        assert_eq!(Config::from_toml("").unwrap(), Config::default());
    }

    #[test]
    fn parses_overrides() {
        let config = Config::from_toml(
            r#"
            theme = "paper"
            icons = "ascii"

            [sort]
            key = "mtime"
            reverse = true

            [files]
            show_hidden = true
            size_base = 1000

            [layout]
            list_percent = 30

            [keymap.normal]
            "g g" = "cursor.top"
            q = "none"
            "#,
        )
        .unwrap();
        assert_eq!(config.theme, "paper");
        assert_eq!(config.icons, IconSet::Ascii);
        assert_eq!(config.sort.key, SortKey::Mtime);
        assert!(config.sort.reverse);
        assert!(config.sort.dirs_first, "unspecified fields keep defaults");
        assert!(config.files.show_hidden);
        assert_eq!(config.files.size_base.divisor(), 1000);
        assert_eq!(config.layout.list_percent, 30);
        assert_eq!(config.keymap["normal"]["q"], "none");
    }

    #[test]
    fn image_protocol_parses_and_rejects_unknown() {
        assert_eq!(
            Config::default().preview.image_protocol,
            ImageProtocol::Auto
        );
        for (name, want) in [
            ("auto", ImageProtocol::Auto),
            ("halfblocks", ImageProtocol::Halfblocks),
            ("sixel", ImageProtocol::Sixel),
            ("kitty", ImageProtocol::Kitty),
            ("iterm2", ImageProtocol::Iterm2),
        ] {
            let config = Config::from_toml(&format!(
                "[preview]
image_protocol = \"{name}\""
            ))
            .unwrap();
            assert_eq!(config.preview.image_protocol, want);
        }
        let err = Config::from_toml(
            "[preview]
image_protocol = \"blit\"",
        )
        .unwrap_err()
        .to_string();
        assert!(err.contains("blit"), "{err}");
    }

    #[test]
    fn rejects_bad_input_with_a_useful_message() {
        for (text, needle) in [
            ("theme = ", "expected"),
            ("icons = \"emoji\"", "emoji"),
            ("bogus = 1", "bogus"),
            ("[files]\nsize_base = 999", "1000 or 1024"),
            ("[layout]\nlist_percent = 5", "list_percent"),
            ("[preview]\nmax_lines = 0", "max_lines"),
            ("theme = \"\"", "theme"),
            ("[claude]\npane_percent = 99", "pane_percent"),
            ("[claude]\nfocus_key = \"ctrl-wat\"", "focus_key"),
            ("[claude]\nstrip_env = [\"A=B\"]", "strip_env"),
            ("[claude]\npane_position = \"top\"", "top"),
        ] {
            let err = Config::from_toml(text).unwrap_err().to_string();
            assert!(err.contains(needle), "`{text}` -> {err}");
        }
    }

    #[test]
    fn shipped_default_config_matches_the_defaults() {
        // Uncomment every `# key = value` line and compare. The keymap example at the end
        // is illustrative and intentionally not part of the defaults.
        let settings = DEFAULT_CONFIG_TOML
            .split("# [keymap.normal]")
            .next()
            .unwrap();
        let uncommented: String = settings
            .lines()
            .map(|line| match line.strip_prefix("# ") {
                Some(rest) if rest.contains('=') => rest,
                _ => line,
            })
            .collect::<Vec<_>>()
            .join("\n");
        let parsed = Config::from_toml(&uncommented).unwrap();
        assert_eq!(parsed, Config::default());
        assert!(
            Config::from_toml(DEFAULT_CONFIG_TOML).unwrap() == Config::default(),
            "the commented file must also be valid as written"
        );
    }
}
