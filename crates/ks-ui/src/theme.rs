//! Theme tokens (PLAN §4.4), theme files and colour-depth handling.

use std::fmt;
use std::path::PathBuf;

use ratatui::style::{Color, Modifier, Style};
use serde::{Deserialize, Deserializer};
use thiserror::Error;

use crate::syntax::{DeriveFrom, SyntaxFile, SyntaxTheme};

const EMBER: &str = include_str!("../../../assets/themes/ember.toml");
const PAPER: &str = include_str!("../../../assets/themes/paper.toml");

/// Built-in themes: `(name, toml source)`.
const BUILTIN: &[(&str, &str)] = &[("ember", EMBER), ("paper", PAPER)];

/// Name of the theme used when nothing else is available.
pub const FALLBACK_THEME: &str = "ember";

/// How many colours the terminal can show.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ColorDepth {
    TrueColor,
    /// The fixed xterm 256-colour palette.
    Indexed256,
}

impl ColorDepth {
    /// Truecolor when `COLORTERM` is `truecolor`/`24bit`, or when running inside Windows
    /// Terminal (`WT_SESSION`), which always supports it. `env` is injectable for tests.
    pub fn detect(env: impl Fn(&str) -> Option<String>) -> Self {
        let colorterm = env("COLORTERM").unwrap_or_default().to_ascii_lowercase();
        if colorterm == "truecolor" || colorterm == "24bit" || env("WT_SESSION").is_some() {
            ColorDepth::TrueColor
        } else {
            ColorDepth::Indexed256
        }
    }

    /// Detection from the real process environment.
    pub fn detect_from_env() -> Self {
        Self::detect(|key| std::env::var(key).ok())
    }
}

/// A colour as written in a theme file.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ThemeColor {
    /// The terminal's own default colour (`"reset"`).
    Reset,
    Rgb(u8, u8, u8),
}

impl ThemeColor {
    fn parse(text: &str) -> Result<Self, String> {
        let text = text.trim();
        if text.eq_ignore_ascii_case("reset") || text.eq_ignore_ascii_case("default") {
            return Ok(ThemeColor::Reset);
        }
        let hex = text
            .strip_prefix('#')
            .ok_or_else(|| format!("colour `{text}` must be `#rrggbb` or `reset`"))?;
        let expand = |c: char| c.to_digit(16).map(|d| (d * 17) as u8);
        let byte = |s: &str| u8::from_str_radix(s, 16).ok();
        let rgb = match hex.len() {
            6 if hex.is_ascii() => byte(&hex[0..2]).zip(byte(&hex[2..4])).zip(byte(&hex[4..6])),
            3 => {
                let mut it = hex.chars().map(expand);
                it.next()
                    .flatten()
                    .zip(it.next().flatten())
                    .zip(it.next().flatten())
            }
            _ => None,
        };
        rgb.map(|((r, g), b)| ThemeColor::Rgb(r, g, b))
            .ok_or_else(|| format!("invalid colour `{text}`"))
    }

    /// Converts to a terminal colour for the given depth.
    pub fn to_color(self, depth: ColorDepth) -> Color {
        match (self, depth) {
            (ThemeColor::Reset, _) => Color::Reset,
            (ThemeColor::Rgb(r, g, b), ColorDepth::TrueColor) => Color::Rgb(r, g, b),
            (ThemeColor::Rgb(r, g, b), ColorDepth::Indexed256) => {
                Color::Indexed(rgb_to_xterm256(r, g, b))
            }
        }
    }
}

impl<'de> Deserialize<'de> for ThemeColor {
    fn deserialize<D: Deserializer<'de>>(d: D) -> Result<Self, D::Error> {
        let text = String::deserialize(d)?;
        ThemeColor::parse(&text).map_err(serde::de::Error::custom)
    }
}

/// Nearest entry of the xterm 256-colour palette (6x6x6 cube or grayscale ramp).
pub fn rgb_to_xterm256(r: u8, g: u8, b: u8) -> u8 {
    const LEVELS: [i32; 6] = [0, 95, 135, 175, 215, 255];
    let nearest_level = |v: u8| {
        (0..6)
            .min_by_key(|&i| (LEVELS[i] - i32::from(v)).abs())
            .unwrap_or(0)
    };
    let dist = |a: (i32, i32, i32)| {
        let (r, g, b) = (i32::from(r), i32::from(g), i32::from(b));
        (a.0 - r).pow(2) + (a.1 - g).pow(2) + (a.2 - b).pow(2)
    };

    let (ri, gi, bi) = (nearest_level(r), nearest_level(g), nearest_level(b));
    let cube = (LEVELS[ri], LEVELS[gi], LEVELS[bi]);
    let cube_index = 16 + 36 * ri + 6 * gi + bi;

    let avg = (i32::from(r) + i32::from(g) + i32::from(b)) / 3;
    let gray_step = ((avg - 8 + 5) / 10).clamp(0, 23);
    let gray_level = 8 + 10 * gray_step;
    let gray = (gray_level, gray_level, gray_level);

    if dist(gray) < dist(cube) {
        (232 + gray_step) as u8
    } else {
        cube_index as u8
    }
}

macro_rules! theme_tokens {
    ($($field:ident => $key:literal),+ $(,)?) => {
        /// Colours as written in a theme file; every token is optional so a user theme can
        /// override just a few and inherit the rest from its `base`.
        #[derive(Clone, Debug, Default, Deserialize)]
        #[serde(deny_unknown_fields)]
        struct Palette {
            $(#[serde(rename = $key)] $field: Option<ThemeColor>,)+
        }

        impl Palette {
            /// Fills unset tokens from `base`.
            fn over(self, base: &Theme) -> Palette {
                Palette { $($field: self.$field.or(base.raw.$field),)+ }
            }

            fn missing(&self) -> Vec<&'static str> {
                let mut out = Vec::new();
                $(if self.$field.is_none() { out.push($key); })+
                out
            }
        }

        /// A resolved theme: every token is a terminal colour for the detected depth.
        #[derive(Clone, Debug)]
        pub struct Theme {
            pub name: String,
            $(pub $field: Color,)+
            /// Syntax highlighting colours (the optional `[syntax]` table).
            pub syntax: SyntaxTheme,
            depth: ColorDepth,
            raw: Palette,
        }

        impl Theme {
            fn from_palette(name: String, palette: Palette, syntax: SyntaxFile, depth: ColorDepth) -> Result<Theme, ThemeError> {
                let missing = palette.missing();
                if !missing.is_empty() {
                    return Err(ThemeError::MissingTokens { name, tokens: missing.join(", ") });
                }
                let token = |color: Option<ThemeColor>| color.unwrap_or(ThemeColor::Reset);
                let derive = DeriveFrom {
                    fg: token(palette.fg),
                    accent: token(palette.accent),
                    success: token(palette.success),
                    fg_muted: token(palette.fg_muted),
                    fg_dim: token(palette.fg_dim),
                    dir: token(palette.dir),
                    warning: token(palette.warning),
                    symlink: token(palette.symlink),
                    git_untracked: token(palette.git_untracked),
                };
                Ok(Theme {
                    name,
                    $($field: palette.$field.unwrap_or(ThemeColor::Reset).to_color(depth),)+
                    syntax: SyntaxTheme::resolve(syntax, derive),
                    depth,
                    raw: palette,
                })
            }

            /// Names of all tokens, for documentation and tests.
            pub const TOKEN_NAMES: &'static [&'static str] = &[$($key,)+];
        }
    };
}

theme_tokens! {
    bg => "bg",
    fg => "fg",
    fg_dim => "fg_dim",
    fg_muted => "fg_muted",
    accent => "accent",
    accent_fg => "accent_fg",
    selection_bg => "selection_bg",
    border => "border",
    border_focus => "border_focus",
    dir => "dir",
    exec => "exec",
    symlink => "symlink",
    hidden => "hidden",
    success => "success",
    warning => "warning",
    error => "error",
    git_added => "git_added",
    git_modified => "git_modified",
    git_untracked => "git_untracked",
    git_ignored => "git_ignored",
    git_conflict => "git_conflict",
    match_ => "match",
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct ThemeFile {
    /// Built-in theme to inherit unset tokens from (default `ember`).
    #[serde(default)]
    base: Option<String>,
    #[serde(default)]
    colors: Palette,
    /// Optional syntax highlighting colours; unset keys are derived from `colors`.
    #[serde(default)]
    syntax: SyntaxFile,
}

#[derive(Debug, Error)]
pub enum ThemeError {
    #[error("theme `{0}` not found")]
    NotFound(String),
    #[error("invalid theme name `{0}` (use letters, digits, `-` and `_`)")]
    BadName(String),
    #[error("cannot read theme file {path}: {source}")]
    Io {
        path: PathBuf,
        source: std::io::Error,
    },
    #[error("theme `{name}`: {message}")]
    Parse { name: String, message: String },
    #[error("theme `{name}` does not define: {tokens}")]
    MissingTokens { name: String, tokens: String },
    #[error("`{0}` is not a built-in theme and cannot be used as `base`")]
    BadBase(String),
}

impl Theme {
    /// A built-in theme by name.
    pub fn builtin(name: &str, depth: ColorDepth) -> Result<Theme, ThemeError> {
        let source = BUILTIN
            .iter()
            .find(|(n, _)| *n == name)
            .map(|(_, s)| *s)
            .ok_or_else(|| ThemeError::NotFound(name.to_owned()))?;
        Self::parse(name, source, depth, false)
    }

    /// Names of the built-in themes.
    pub fn builtin_names() -> impl Iterator<Item = &'static str> {
        BUILTIN.iter().map(|(name, _)| *name)
    }

    /// Parses a theme file. User themes (`inherit`) fill unset tokens from their `base`;
    /// built-ins must define every token themselves.
    fn parse(
        name: &str,
        source: &str,
        depth: ColorDepth,
        inherit: bool,
    ) -> Result<Theme, ThemeError> {
        let file: ThemeFile = toml::from_str(source).map_err(|e| ThemeError::Parse {
            name: name.to_owned(),
            message: e.to_string(),
        })?;
        let (palette, syntax) = if inherit {
            let base_name = file.base.as_deref().unwrap_or(FALLBACK_THEME);
            let base = Theme::builtin(base_name, depth)
                .map_err(|_| ThemeError::BadBase(base_name.to_owned()))?;
            (
                file.colors.over(&base),
                file.syntax.over(&base.syntax.explicit),
            )
        } else {
            (file.colors, file.syntax)
        };
        Theme::from_palette(name.to_owned(), palette, syntax, depth)
    }

    /// The colour depth this theme was resolved for.
    pub fn depth(&self) -> ColorDepth {
        self.depth
    }

    /// The palette for the preview engine's syntax highlighter.
    pub fn syntax_palette(&self) -> ks_preview::Palette {
        self.syntax.palette()
    }

    /// A concrete RGB colour from the preview engine at this theme's colour depth.
    pub fn rgb(&self, rgb: ks_preview::Rgb) -> Color {
        ThemeColor::Rgb(rgb.r, rgb.g, rgb.b).to_color(self.depth)
    }

    /// Foreground on the theme background.
    pub fn base_style(&self) -> Style {
        Style::new().fg(self.fg).bg(self.bg)
    }

    pub fn dim(&self) -> Style {
        Style::new().fg(self.fg_dim)
    }

    pub fn muted(&self) -> Style {
        Style::new().fg(self.fg_muted)
    }

    pub fn accent_style(&self) -> Style {
        Style::new().fg(self.accent)
    }

    pub fn bold_accent(&self) -> Style {
        self.accent_style().add_modifier(Modifier::BOLD)
    }
}

impl fmt::Display for Theme {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.name)
    }
}

/// Finds themes: user files first, then built-ins.
#[derive(Clone, Debug)]
pub struct ThemeLoader {
    user_dir: Option<PathBuf>,
    depth: ColorDepth,
}

impl ThemeLoader {
    pub fn new(user_dir: Option<PathBuf>, depth: ColorDepth) -> Self {
        Self { user_dir, depth }
    }

    pub fn depth(&self) -> ColorDepth {
        self.depth
    }

    /// Names of every theme that can be loaded: built-ins first, then user files
    /// (`themes/*.toml` with a valid name) that are not built-in names, sorted.
    pub fn available(&self) -> Vec<String> {
        let mut names: Vec<String> = Theme::builtin_names().map(str::to_owned).collect();
        let mut user = Vec::new();
        if let Some(entries) = self
            .user_dir
            .as_ref()
            .and_then(|dir| std::fs::read_dir(dir).ok())
        {
            for entry in entries.flatten() {
                let path = entry.path();
                if path.extension().and_then(|e| e.to_str()) != Some("toml") {
                    continue;
                }
                let Some(stem) = path.file_stem().and_then(|s| s.to_str()) else {
                    continue;
                };
                let valid = !stem.is_empty()
                    && stem
                        .chars()
                        .all(|c| c.is_ascii_alphanumeric() || c == '-' || c == '_');
                if valid && !names.iter().any(|n| n == stem) && !user.iter().any(|n| n == stem) {
                    user.push(stem.to_owned());
                }
            }
        }
        user.sort();
        names.extend(user);
        names
    }

    /// Loads `name` from `<user_dir>/<name>.toml` if present, else from the built-ins.
    pub fn load(&self, name: &str) -> Result<Theme, ThemeError> {
        if name.is_empty()
            || !name
                .chars()
                .all(|c| c.is_ascii_alphanumeric() || c == '-' || c == '_')
        {
            return Err(ThemeError::BadName(name.to_owned()));
        }
        if let Some(dir) = &self.user_dir {
            let path = dir.join(format!("{name}.toml"));
            match std::fs::read_to_string(&path) {
                Ok(source) => return Theme::parse(name, &source, self.depth, true),
                Err(e) if e.kind() == std::io::ErrorKind::NotFound => {}
                Err(source) => return Err(ThemeError::Io { path, source }),
            }
        }
        Theme::builtin(name, self.depth)
    }

    /// Like [`load`](Self::load) but never fails: falls back to the built-in default and
    /// returns the reason as a message.
    pub fn load_or_fallback(&self, name: &str) -> (Theme, Option<String>) {
        match self.load(name) {
            Ok(theme) => (theme, None),
            Err(e) => {
                let theme = Theme::builtin(FALLBACK_THEME, self.depth)
                    .or_else(|_| Theme::builtin(FALLBACK_THEME, ColorDepth::Indexed256))
                    .expect("the built-in fallback theme is valid (covered by tests)");
                (theme, Some(e.to_string()))
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn available_lists_builtins_then_user_themes() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join("zebra.toml"), "").unwrap();
        std::fs::write(dir.path().join("apple.toml"), "").unwrap();
        // A user file named like a built-in shadows it but is listed once.
        std::fs::write(dir.path().join("paper.toml"), "").unwrap();
        std::fs::write(dir.path().join("notes.txt"), "").unwrap();
        std::fs::write(dir.path().join("bad name.toml"), "").unwrap();
        let loader = ThemeLoader::new(Some(dir.path().to_owned()), ColorDepth::TrueColor);
        assert_eq!(loader.available(), ["ember", "paper", "apple", "zebra"]);

        let none = ThemeLoader::new(Some(dir.path().join("missing")), ColorDepth::TrueColor);
        assert_eq!(none.available(), ["ember", "paper"]);
    }

    #[test]
    fn builtin_themes_are_complete_and_resolve_in_both_depths() {
        for name in Theme::builtin_names() {
            for depth in [ColorDepth::TrueColor, ColorDepth::Indexed256] {
                let theme = Theme::builtin(name, depth).unwrap();
                assert_eq!(theme.name, name);
            }
        }
        assert_eq!(Theme::TOKEN_NAMES.len(), 22);
    }

    #[test]
    fn bg_defaults_to_the_terminal_default() {
        for name in Theme::builtin_names() {
            let theme = Theme::builtin(name, ColorDepth::TrueColor).unwrap();
            assert_eq!(theme.bg, Color::Reset, "{name} must not paint a background");
        }
    }

    #[test]
    fn colour_parsing() {
        assert_eq!(
            ThemeColor::parse("#d97757"),
            Ok(ThemeColor::Rgb(0xd9, 0x77, 0x57))
        );
        assert_eq!(
            ThemeColor::parse("#fff"),
            Ok(ThemeColor::Rgb(255, 255, 255))
        );
        assert_eq!(ThemeColor::parse("Reset"), Ok(ThemeColor::Reset));
        assert!(ThemeColor::parse("red").is_err());
        assert!(ThemeColor::parse("#12345").is_err());
        assert!(ThemeColor::parse("#gggggg").is_err());
    }

    #[test]
    fn detects_truecolor() {
        let env = |pairs: &'static [(&'static str, &'static str)]| {
            move |k: &str| {
                pairs
                    .iter()
                    .find(|(key, _)| *key == k)
                    .map(|(_, v)| (*v).to_owned())
            }
        };
        assert_eq!(
            ColorDepth::detect(env(&[("COLORTERM", "truecolor")])),
            ColorDepth::TrueColor
        );
        assert_eq!(
            ColorDepth::detect(env(&[("COLORTERM", "24bit")])),
            ColorDepth::TrueColor
        );
        assert_eq!(
            ColorDepth::detect(env(&[("WT_SESSION", "abc")])),
            ColorDepth::TrueColor
        );
        assert_eq!(ColorDepth::detect(env(&[])), ColorDepth::Indexed256);
        assert_eq!(
            ColorDepth::detect(env(&[("COLORTERM", "yes")])),
            ColorDepth::Indexed256
        );
    }

    #[test]
    fn xterm256_mapping() {
        assert_eq!(rgb_to_xterm256(0, 0, 0), 16);
        assert_eq!(rgb_to_xterm256(255, 255, 255), 231);
        assert_eq!(rgb_to_xterm256(255, 0, 0), 196);
        assert_eq!(rgb_to_xterm256(0, 255, 0), 46);
        // Mid grays use the grayscale ramp (232..=255).
        let gray = rgb_to_xterm256(128, 128, 128);
        assert!((232..=255).contains(&gray), "{gray}");
        // The terracotta accent lands on a warm cube entry.
        assert_eq!(rgb_to_xterm256(0xd9, 0x77, 0x57), 173);
    }

    #[test]
    fn indexed_depth_produces_indexed_colours() {
        let theme = Theme::builtin("ember", ColorDepth::Indexed256).unwrap();
        assert!(matches!(theme.accent, Color::Indexed(_)));
        let theme = Theme::builtin("ember", ColorDepth::TrueColor).unwrap();
        assert!(matches!(theme.accent, Color::Rgb(..)));
    }

    #[test]
    fn user_theme_inherits_from_base_and_overrides() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(
            dir.path().join("mine.toml"),
            "base = \"paper\"\n[colors]\naccent = \"#112233\"\n",
        )
        .unwrap();
        let loader = ThemeLoader::new(Some(dir.path().to_owned()), ColorDepth::TrueColor);
        let mine = loader.load("mine").unwrap();
        let paper = Theme::builtin("paper", ColorDepth::TrueColor).unwrap();
        assert_eq!(mine.accent, Color::Rgb(0x11, 0x22, 0x33));
        assert_eq!(mine.fg, paper.fg);
        assert_eq!(mine.name, "mine");
    }

    fn ember_without_syntax() -> String {
        let cut = EMBER.find("[syntax]").expect("ember has a [syntax] table");
        EMBER[..cut].to_owned()
    }

    fn rgb(r: u8, g: u8, b: u8) -> ThemeColor {
        ThemeColor::Rgb(r, g, b)
    }

    #[test]
    fn missing_syntax_table_derives_from_the_regular_tokens() {
        let theme = Theme::parse(
            "plain",
            &ember_without_syntax(),
            ColorDepth::TrueColor,
            false,
        )
        .unwrap();
        // ember: accent #d97757, success #92c07f, fg_muted #716b64, dir #7fa8c9.
        assert_eq!(theme.syntax.keyword, rgb(0xd9, 0x77, 0x57));
        assert_eq!(theme.syntax.string, rgb(0x92, 0xc0, 0x7f));
        assert_eq!(theme.syntax.comment, rgb(0x71, 0x6b, 0x64));
        assert_eq!(theme.syntax.function, rgb(0x7f, 0xa8, 0xc9));
        assert_eq!(theme.syntax.operator, theme.syntax.punctuation);
    }

    #[test]
    fn syntax_palette_maps_every_role_to_its_token() {
        let theme = Theme::parse(
            "plain",
            &ember_without_syntax(),
            ColorDepth::TrueColor,
            false,
        )
        .unwrap();
        let p = theme.syntax_palette();
        let s = &theme.syntax;
        let want = |c: ThemeColor| match c {
            ThemeColor::Rgb(r, g, b) => ks_preview::Rgb::new(r, g, b),
            ThemeColor::Reset => panic!("ember has no reset syntax colours"),
        };
        assert_eq!(p.fg, want(s.fg));
        assert_eq!(p.keyword, want(s.keyword));
        assert_eq!(p.string, want(s.string));
        assert_eq!(p.comment, want(s.comment));
        assert_eq!(p.function, want(s.function));
        assert_eq!(p.type_, want(s.type_));
        assert_eq!(p.number, want(s.number));
        assert_eq!(p.constant, want(s.constant));
        assert_eq!(p.operator, want(s.operator));
        assert_eq!(p.punctuation, want(s.punctuation));
        // And the six themed tokens differ from each other, so a mix-up would show.
        assert_ne!(p.keyword, p.string);
        assert_ne!(p.function, p.type_);
    }

    #[test]
    fn user_theme_inherits_the_base_syntax_colours_and_overrides_some() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(
            dir.path().join("mine.toml"),
            "base = \"paper\"
[syntax]
keyword = \"#010203\"
",
        )
        .unwrap();
        let loader = ThemeLoader::new(Some(dir.path().to_owned()), ColorDepth::TrueColor);
        let mine = loader.load("mine").unwrap();
        let paper = Theme::builtin("paper", ColorDepth::TrueColor).unwrap();
        assert_eq!(mine.syntax.keyword, rgb(1, 2, 3));
        assert_eq!(mine.syntax.string, paper.syntax.string);
        assert_eq!(mine.syntax.comment, paper.syntax.comment);
    }

    #[test]
    fn an_invalid_syntax_colour_is_rejected() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(
            dir.path().join("bad.toml"),
            "[syntax]
keyword = \"orange\"
",
        )
        .unwrap();
        std::fs::write(
            dir.path().join("typo.toml"),
            "[syntax]
keywrd = \"#000000\"
",
        )
        .unwrap();
        let loader = ThemeLoader::new(Some(dir.path().to_owned()), ColorDepth::TrueColor);
        assert!(matches!(loader.load("bad"), Err(ThemeError::Parse { .. })));
        assert!(matches!(loader.load("typo"), Err(ThemeError::Parse { .. })));
    }

    #[test]
    fn user_theme_can_shadow_a_builtin() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(
            dir.path().join("ember.toml"),
            "[colors]\nfg = \"#010203\"\n",
        )
        .unwrap();
        let loader = ThemeLoader::new(Some(dir.path().to_owned()), ColorDepth::TrueColor);
        assert_eq!(loader.load("ember").unwrap().fg, Color::Rgb(1, 2, 3));
    }

    #[test]
    fn loader_errors_and_fallback() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(
            dir.path().join("broken.toml"),
            "[colors]\naccent = \"red\"\n",
        )
        .unwrap();
        std::fs::write(dir.path().join("badbase.toml"), "base = \"nope\"\n").unwrap();
        let loader = ThemeLoader::new(Some(dir.path().to_owned()), ColorDepth::TrueColor);

        assert!(matches!(
            loader.load("missing"),
            Err(ThemeError::NotFound(_))
        ));
        assert!(matches!(loader.load("../x"), Err(ThemeError::BadName(_))));
        assert!(matches!(
            loader.load("broken"),
            Err(ThemeError::Parse { .. })
        ));
        assert!(matches!(
            loader.load("badbase"),
            Err(ThemeError::BadBase(_))
        ));

        let (theme, problem) = loader.load_or_fallback("broken");
        assert_eq!(theme.name, FALLBACK_THEME);
        assert!(problem.unwrap().contains("broken"));
        let (theme, problem) = loader.load_or_fallback("paper");
        assert_eq!(theme.name, "paper");
        assert!(problem.is_none());
    }

    #[test]
    fn the_documented_example_theme_is_valid() {
        // docs/theme.example.toml is shown in the README; it must keep parsing and must
        // define every token and syntax key.
        let example = include_str!("../../../docs/theme.example.toml");
        let theme = Theme::parse("example", example, ColorDepth::TrueColor, true).unwrap();
        assert_eq!(theme.accent, Color::Rgb(0x6a, 0xa5, 0xd9));
        let file: ThemeFile = toml::from_str(example).unwrap();
        assert!(
            file.colors.missing().is_empty(),
            "{:?}",
            file.colors.missing()
        );
        assert!(file.syntax.keyword.is_some() && file.syntax.punctuation.is_some());
    }
}
