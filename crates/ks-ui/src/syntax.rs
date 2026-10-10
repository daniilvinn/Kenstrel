//! Syntax highlighting colours of a theme (the optional `[syntax]` table).
//!
//! Every key is optional. A key that is not set is derived from the regular tokens, so a
//! theme written before syntax colours existed still gets a coherent palette:
//!
//! | key | default |
//! |---|---|
//! | `keyword` | `accent` |
//! | `string` | `success` |
//! | `comment` | `fg_muted` |
//! | `function` | `dir` |
//! | `type` | `warning` |
//! | `number` | `symlink` |
//! | `constant` | `git_untracked` |
//! | `operator`, `punctuation` | `fg_dim` |

use ks_preview::{Palette, Rgb};
use serde::Deserialize;

use crate::theme::ThemeColor;

/// The `[syntax]` table as written in a theme file.
#[derive(Clone, Debug, Default, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct SyntaxFile {
    pub keyword: Option<ThemeColor>,
    pub string: Option<ThemeColor>,
    pub comment: Option<ThemeColor>,
    pub function: Option<ThemeColor>,
    #[serde(rename = "type")]
    pub type_: Option<ThemeColor>,
    pub number: Option<ThemeColor>,
    pub constant: Option<ThemeColor>,
    pub operator: Option<ThemeColor>,
    pub punctuation: Option<ThemeColor>,
}

impl SyntaxFile {
    /// Keys set here win; the rest come from `base`.
    pub fn over(self, base: &SyntaxFile) -> SyntaxFile {
        SyntaxFile {
            keyword: self.keyword.or(base.keyword),
            string: self.string.or(base.string),
            comment: self.comment.or(base.comment),
            function: self.function.or(base.function),
            type_: self.type_.or(base.type_),
            number: self.number.or(base.number),
            constant: self.constant.or(base.constant),
            operator: self.operator.or(base.operator),
            punctuation: self.punctuation.or(base.punctuation),
        }
    }
}

/// The regular tokens the defaults are derived from.
#[derive(Clone, Copy, Debug)]
pub struct DeriveFrom {
    pub fg: ThemeColor,
    pub accent: ThemeColor,
    pub success: ThemeColor,
    pub fg_muted: ThemeColor,
    pub fg_dim: ThemeColor,
    pub dir: ThemeColor,
    pub warning: ThemeColor,
    pub symlink: ThemeColor,
    pub git_untracked: ThemeColor,
}

/// Resolved syntax colours plus the explicitly set keys (kept for `base` inheritance).
#[derive(Clone, Debug)]
pub struct SyntaxTheme {
    pub explicit: SyntaxFile,
    pub fg: ThemeColor,
    pub keyword: ThemeColor,
    pub string: ThemeColor,
    pub comment: ThemeColor,
    pub function: ThemeColor,
    pub type_: ThemeColor,
    pub number: ThemeColor,
    pub constant: ThemeColor,
    pub operator: ThemeColor,
    pub punctuation: ThemeColor,
}

impl SyntaxTheme {
    pub fn resolve(file: SyntaxFile, from: DeriveFrom) -> Self {
        Self {
            fg: from.fg,
            keyword: file.keyword.unwrap_or(from.accent),
            string: file.string.unwrap_or(from.success),
            comment: file.comment.unwrap_or(from.fg_muted),
            function: file.function.unwrap_or(from.dir),
            type_: file.type_.unwrap_or(from.warning),
            number: file.number.unwrap_or(from.symlink),
            constant: file.constant.unwrap_or(from.git_untracked),
            operator: file.operator.unwrap_or(from.fg_dim),
            punctuation: file.punctuation.unwrap_or(from.fg_dim),
            explicit: file,
        }
    }

    /// The palette handed to the preview engine. `reset` colours (the terminal's own) have
    /// no RGB value, so the engine's dark palette stands in for them.
    pub fn palette(&self) -> Palette {
        let fallback = Palette::dark();
        let rgb = |color: ThemeColor, default: Rgb| match color {
            ThemeColor::Rgb(r, g, b) => Rgb::new(r, g, b),
            ThemeColor::Reset => default,
        };
        Palette {
            fg: rgb(self.fg, fallback.fg),
            keyword: rgb(self.keyword, fallback.keyword),
            string: rgb(self.string, fallback.string),
            comment: rgb(self.comment, fallback.comment),
            function: rgb(self.function, fallback.function),
            type_: rgb(self.type_, fallback.type_),
            number: rgb(self.number, fallback.number),
            constant: rgb(self.constant, fallback.constant),
            operator: rgb(self.operator, fallback.operator),
            punctuation: rgb(self.punctuation, fallback.punctuation),
        }
    }
}
