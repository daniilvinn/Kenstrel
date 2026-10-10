//! Syntax highlighting with syntect + two-face's extended syntax set.
//!
//! The `SyntaxSet` is loaded lazily, once. Themes are built per request from the caller's
//! [`Palette`] (cheap), so `ThemeSet` is not needed.

use std::path::Path;
use std::str::FromStr;
use std::sync::OnceLock;
use std::time::Instant;

use syntect::easy::HighlightLines;
use syntect::highlighting::{
    Color as SyColor, FontStyle, ScopeSelectors, StyleModifier, Theme, ThemeItem, ThemeSettings,
};
use syntect::parsing::{SyntaxReference, SyntaxSet};

use crate::model::{Color, Rgb, SpanStyle, StyledLine};
use crate::options::{Limits, Palette};
use crate::sanitize::Sanitizer;

static SYNTAXES: OnceLock<SyntaxSet> = OnceLock::new();

/// The shared syntax set (loads on first use, roughly tens of ms).
pub fn syntax_set() -> &'static SyntaxSet {
    SYNTAXES.get_or_init(two_face::syntax::extra_newlines)
}

/// Load the syntax set now. Call from a background thread at startup so the first
/// preview does not pay for it.
pub fn warm_up() {
    let _ = syntax_set();
}

/// Find a syntax by file name / extension, falling back to the first line (shebang).
pub fn find_syntax(path: &Path, first_line: &str) -> Option<&'static SyntaxReference> {
    let ss = syntax_set();
    let name = path.file_name()?.to_string_lossy();
    let by_name = ss.find_syntax_by_extension(&name);
    let ext = path.extension().map(|e| e.to_string_lossy());
    let found = by_name
        .or_else(|| ext.as_deref().and_then(|e| ss.find_syntax_by_extension(e)))
        .or_else(|| ss.find_syntax_by_token(&name))
        .or_else(|| alias(&name, ext.as_deref()).and_then(|a| ss.find_syntax_by_extension(a)))
        .or_else(|| ss.find_syntax_by_first_line(first_line));
    found.filter(|s| s.name != "Plain Text")
}

/// Extensions / names the bundled syntaxes do not know but that map onto one they do.
fn alias(name: &str, ext: Option<&str>) -> Option<&'static str> {
    if name.eq_ignore_ascii_case("Cargo.lock") || name.eq_ignore_ascii_case("poetry.lock") {
        return Some("toml");
    }
    match ext?.to_ascii_lowercase().as_str() {
        "jsx" | "mjs" | "cjs" => Some("js"),
        "cmd" => Some("bat"),
        "psm1" | "psd1" | "ps1" => Some("sh"),
        "mts" | "cts" => Some("ts"),
        "jsonc" | "json5" => Some("json"),
        _ => None,
    }
}

fn sy(c: Rgb) -> SyColor {
    SyColor {
        r: c.r,
        g: c.g,
        b: c.b,
        a: 255,
    }
}

fn item(scopes: &str, color: Rgb, font: Option<FontStyle>) -> ThemeItem {
    ThemeItem {
        scope: ScopeSelectors::from_str(scopes).unwrap_or_default(),
        style: StyleModifier {
            foreground: Some(sy(color)),
            background: None,
            font_style: font,
        },
    }
}

/// Build a syntect theme from the palette.
pub fn build_theme(p: &Palette) -> Theme {
    let scopes = vec![
        item(
            "comment, punctuation.definition.comment",
            p.comment,
            Some(FontStyle::ITALIC),
        ),
        item("string, punctuation.definition.string", p.string, None),
        item("constant.character.escape", p.operator, None),
        item("constant", p.constant, None),
        item(
            "constant.numeric, constant.language.numeric",
            p.number,
            None,
        ),
        item("keyword, storage, keyword.control", p.keyword, None),
        item("keyword.operator", p.operator, None),
        item(
            "entity.name.function, support.function, meta.function-call variable.function",
            p.function,
            None,
        ),
        item(
            "entity.name.type, entity.name.class, entity.name.struct, entity.name.enum, \
             entity.name.trait, entity.other.inherited-class, support.type, support.class",
            p.type_,
            None,
        ),
        item("entity.name.tag, markup.heading", p.keyword, None),
        item(
            "entity.other.attribute-name, variable.parameter",
            p.type_,
            None,
        ),
        item("punctuation", p.punctuation, None),
        item("storage.type.string", p.string, None),
    ];
    Theme {
        name: Some("kestrel".into()),
        author: None,
        settings: ThemeSettings {
            foreground: Some(sy(p.fg)),
            ..ThemeSettings::default()
        },
        scopes,
    }
}

/// Outcome of highlighting one line.
fn style_of(s: &syntect::highlighting::Style, fg: Rgb) -> SpanStyle {
    let rgb = Rgb::new(s.foreground.r, s.foreground.g, s.foreground.b);
    SpanStyle {
        fg: if rgb == fg {
            None
        } else {
            Some(Color::Rgb(rgb))
        },
        bold: s.font_style.contains(FontStyle::BOLD),
        italic: s.font_style.contains(FontStyle::ITALIC),
        underline: s.font_style.contains(FontStyle::UNDERLINE),
        dim: false,
    }
}

/// Line-by-line highlighter with a time budget. Lines that cannot be highlighted
/// (budget exhausted, overlong, parser error) come back plain.
pub struct LineHighlighter<'a> {
    inner: HighlightLines<'a>,
    fg: Rgb,
    started: Instant,
    limits: Limits,
    tab_width: usize,
    exhausted: bool,
}

impl<'a> LineHighlighter<'a> {
    pub fn new(
        syntax: &'a SyntaxReference,
        theme: &'a Theme,
        palette: &Palette,
        limits: Limits,
        tab_width: usize,
    ) -> Self {
        Self {
            inner: HighlightLines::new(syntax, theme),
            fg: palette.fg,
            started: Instant::now(),
            limits,
            tab_width,
            exhausted: false,
        }
    }

    /// Highlight one line (without line terminator).
    pub fn line(&mut self, text: &str) -> StyledLine {
        let mut san = Sanitizer::new(self.tab_width);
        let mut out = StyledLine::default();
        if !self.exhausted && self.started.elapsed() > self.limits.highlight_budget {
            self.exhausted = true;
            tracing::debug!("highlight budget exhausted; rendering the rest plain");
        }
        if self.exhausted {
            let mut s = String::with_capacity(text.len());
            san.push_str(text, &mut s);
            out.push(s, SpanStyle::default());
            return out;
        }
        // Overlong lines: still feed the head to the parser so multi-line state (block
        // comments, strings) stays in sync; the tail is rendered plain.
        let mut cut = text.len().min(self.limits.max_highlight_line);
        while !text.is_char_boundary(cut) {
            cut -= 1;
        }
        let (text, tail) = text.split_at(cut);
        // The newline-aware syntax set needs the terminator.
        let mut with_nl = String::with_capacity(text.len() + 1);
        with_nl.push_str(text);
        with_nl.push('\n');
        match self.inner.highlight_line(&with_nl, syntax_set()) {
            Ok(regions) => {
                for (style, piece) in regions {
                    let piece = piece.strip_suffix('\n').unwrap_or(piece);
                    let mut s = String::with_capacity(piece.len());
                    san.push_str(piece, &mut s);
                    out.push(s, style_of(&style, self.fg));
                }
            }
            Err(err) => {
                tracing::debug!(%err, "syntect failed on a line; rendering plain");
                let mut s = String::with_capacity(text.len());
                san.push_str(text, &mut s);
                out.push(s, SpanStyle::default());
            }
        }
        if !tail.is_empty() {
            let mut s = String::with_capacity(tail.len());
            san.push_str(tail, &mut s);
            out.push(s, SpanStyle::default());
        }
        out
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn rust_keywords_get_palette_colour() {
        let pal = Palette::dark();
        let theme = build_theme(&pal);
        let syn = find_syntax(Path::new("a.rs"), "").expect("rust syntax");
        let mut hl = LineHighlighter::new(syn, &theme, &pal, Limits::default(), 4);
        let line = hl.line("fn main() {}");
        let first = &line.spans[0];
        assert_eq!(first.text, "fn");
        assert_eq!(first.style.fg, Some(Color::Rgb(pal.keyword)));
    }

    #[test]
    fn overlong_line_keeps_parser_state_in_sync() {
        let pal = Palette::dark();
        let theme = build_theme(&pal);
        let syn = find_syntax(Path::new("a.rs"), "").expect("rust syntax");
        let limits = Limits {
            max_highlight_line: 50,
            ..Limits::default()
        };
        let mut hl = LineHighlighter::new(syn, &theme, &pal, limits, 4);
        // Block comment opens in the head of an overlong line and spans onwards.
        let long = format!("/* {}", "x".repeat(200));
        let l1 = hl.line(&long);
        assert_eq!(l1.text(), long, "all text is kept");
        let l2 = hl.line("fn still_comment() {}");
        assert!(
            l2.spans
                .iter()
                .all(|s| s.style.fg == Some(Color::Rgb(pal.comment))),
            "{l2:?}"
        );
    }

    #[test]
    fn extended_syntaxes_present() {
        for f in ["a.toml", "a.ts", "a.tsx", "Cargo.lock", "a.jsx", "a.kt"] {
            assert!(find_syntax(Path::new(f), "").is_some(), "{f}");
        }
    }
}
