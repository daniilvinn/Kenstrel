//! Input modes and the overlay stack (PLAN §3.3).

use std::fmt;
use std::str::FromStr;

/// What the prompt line is currently collecting.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum PromptKind {
    Command,
    Rename,
    Create,
    Shell,
}

/// The base input mode. Overlays sit on top of it.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum Mode {
    #[default]
    Normal,
    Visual,
    Prompt(PromptKind),
    Filter,
}

/// A modal layer drawn above the main screen. The topmost overlay receives keys first.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Overlay {
    Finder,
    Grep,
    Palette,
    Help,
    Confirm,
    Jobs,
    Claude,
    Bookmarks,
    Jump,
    /// A job waits for an answer to "destination exists" (P6).
    Conflict,
    /// The `!` command output pager (P6).
    Pager,
    /// A small choice list: `Y` copy-as, `O` open-with (P6).
    Menu,
    ClaudeChanges,
}

/// Selects which keymap table handles a key press.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum KeyContext {
    Normal,
    Visual,
    Prompt,
    Filter,
    Finder,
    Grep,
    Palette,
    Help,
    Confirm,
    Jobs,
    Claude,
    Bookmarks,
    Jump,
    Conflict,
    Pager,
    Menu,
    ClaudeChanges,
    /// Line selection in the text preview (`V`).
    PreviewLines,
}

impl KeyContext {
    pub const ALL: &'static [KeyContext] = &[
        KeyContext::Normal,
        KeyContext::Visual,
        KeyContext::Prompt,
        KeyContext::Filter,
        KeyContext::Finder,
        KeyContext::Grep,
        KeyContext::Palette,
        KeyContext::Help,
        KeyContext::Confirm,
        KeyContext::Jobs,
        KeyContext::Claude,
        KeyContext::Bookmarks,
        KeyContext::Jump,
        KeyContext::Conflict,
        KeyContext::Pager,
        KeyContext::Menu,
        KeyContext::ClaudeChanges,
        KeyContext::PreviewLines,
    ];

    /// Name used for the `[keymap.<name>]` table in `config.toml`.
    pub const fn name(self) -> &'static str {
        match self {
            KeyContext::Normal => "normal",
            KeyContext::Visual => "visual",
            KeyContext::Prompt => "prompt",
            KeyContext::Filter => "filter",
            KeyContext::Finder => "finder",
            KeyContext::Grep => "grep",
            KeyContext::Palette => "palette",
            KeyContext::Help => "help",
            KeyContext::Confirm => "confirm",
            KeyContext::Jobs => "jobs",
            KeyContext::Claude => "claude",
            KeyContext::Bookmarks => "bookmarks",
            KeyContext::Jump => "jump",
            KeyContext::Conflict => "conflict",
            KeyContext::Pager => "pager",
            KeyContext::Menu => "menu",
            KeyContext::ClaudeChanges => "claude_changes",
            KeyContext::PreviewLines => "preview_lines",
        }
    }

    /// Context consulted when the primary one has no binding for a key.
    pub const fn fallback(self) -> Option<KeyContext> {
        match self {
            KeyContext::Visual => Some(KeyContext::Normal),
            _ => None,
        }
    }
}

impl fmt::Display for KeyContext {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.name())
    }
}

impl FromStr for KeyContext {
    type Err = String;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        KeyContext::ALL
            .iter()
            .copied()
            .find(|c| c.name() == s)
            .ok_or_else(|| format!("unknown keymap context `{s}`"))
    }
}

impl From<Mode> for KeyContext {
    fn from(mode: Mode) -> Self {
        match mode {
            Mode::Normal => KeyContext::Normal,
            Mode::Visual => KeyContext::Visual,
            Mode::Prompt(_) => KeyContext::Prompt,
            Mode::Filter => KeyContext::Filter,
        }
    }
}

impl From<Overlay> for KeyContext {
    fn from(overlay: Overlay) -> Self {
        match overlay {
            Overlay::Finder => KeyContext::Finder,
            Overlay::Grep => KeyContext::Grep,
            Overlay::Palette => KeyContext::Palette,
            Overlay::Help => KeyContext::Help,
            Overlay::Confirm => KeyContext::Confirm,
            Overlay::Jobs => KeyContext::Jobs,
            Overlay::Claude => KeyContext::Claude,
            Overlay::Bookmarks => KeyContext::Bookmarks,
            Overlay::Jump => KeyContext::Jump,
            Overlay::Conflict => KeyContext::Conflict,
            Overlay::Pager => KeyContext::Pager,
            Overlay::Menu => KeyContext::Menu,
            Overlay::ClaudeChanges => KeyContext::ClaudeChanges,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn context_names_round_trip() {
        for &ctx in KeyContext::ALL {
            assert_eq!(ctx.name().parse::<KeyContext>(), Ok(ctx));
        }
        assert!("bogus".parse::<KeyContext>().is_err());
    }
}
