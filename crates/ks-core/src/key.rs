//! Terminal-independent key model: [`Key`], [`Modifiers`] and [`KeyChord`].
//!
//! The UI layer converts backend key events into [`KeyChord`]s; the keymap works only with
//! these types, which keeps `ks-core` free of terminal dependencies.

use std::fmt;
use std::str::FromStr;

use thiserror::Error;

/// A key without modifiers.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum Key {
    Char(char),
    Enter,
    Esc,
    Tab,
    BackTab,
    Backspace,
    Delete,
    Insert,
    Up,
    Down,
    Left,
    Right,
    Home,
    End,
    PageUp,
    PageDown,
    F(u8),
}

/// A set of modifier keys.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct Modifiers(u8);

impl Modifiers {
    pub const NONE: Self = Self(0);
    pub const CTRL: Self = Self(1);
    pub const ALT: Self = Self(2);
    pub const SHIFT: Self = Self(4);

    /// True when every modifier in `other` is also set in `self`.
    pub const fn contains(self, other: Self) -> bool {
        self.0 & other.0 == other.0
    }

    pub const fn is_empty(self) -> bool {
        self.0 == 0
    }

    const fn without(self, other: Self) -> Self {
        Self(self.0 & !other.0)
    }
}

impl std::ops::BitOr for Modifiers {
    type Output = Self;

    fn bitor(self, rhs: Self) -> Self {
        Self(self.0 | rhs.0)
    }
}

/// A key plus modifiers, normalised so that equal physical presses compare equal.
///
/// Normalisation rules (applied by [`KeyChord::new`]):
/// * `shift` is dropped for character keys (the character already carries its case, so
///   `G` is `Char('G')` without modifiers) and for `BackTab`;
/// * with `ctrl` or `alt` held, ASCII letters are lower-cased, so `ctrl-A` equals `ctrl-a`.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct KeyChord {
    pub key: Key,
    pub mods: Modifiers,
}

impl KeyChord {
    pub fn new(key: Key, mods: Modifiers) -> Self {
        let mut mods = mods;
        let mut key = key;
        match key {
            Key::Char(c) => {
                mods = mods.without(Modifiers::SHIFT);
                if !mods.is_empty() {
                    key = Key::Char(c.to_ascii_lowercase());
                }
            }
            Key::BackTab => mods = mods.without(Modifiers::SHIFT),
            _ => {}
        }
        Self { key, mods }
    }

    /// A chord without modifiers.
    pub fn plain(key: Key) -> Self {
        Self::new(key, Modifiers::NONE)
    }

    /// `ctrl-<c>`.
    pub fn ctrl(c: char) -> Self {
        Self::new(Key::Char(c), Modifiers::CTRL)
    }

    /// A plain character key.
    pub fn char(c: char) -> Self {
        Self::plain(Key::Char(c))
    }
}

/// Failure to parse a key chord from its textual form.
#[derive(Clone, Debug, PartialEq, Eq, Error)]
pub enum KeyParseError {
    #[error("empty key")]
    Empty,
    #[error("unknown key name `{0}`")]
    UnknownKey(String),
}

const NAMED_KEYS: &[(&str, Key)] = &[
    ("space", Key::Char(' ')),
    ("enter", Key::Enter),
    ("esc", Key::Esc),
    ("tab", Key::Tab),
    ("backtab", Key::BackTab),
    ("backspace", Key::Backspace),
    ("delete", Key::Delete),
    ("insert", Key::Insert),
    ("up", Key::Up),
    ("down", Key::Down),
    ("left", Key::Left),
    ("right", Key::Right),
    ("home", Key::Home),
    ("end", Key::End),
    ("pageup", Key::PageUp),
    ("pagedown", Key::PageDown),
];

const ALIASES: &[(&str, &str)] = &[
    ("escape", "esc"),
    ("return", "enter"),
    ("bs", "backspace"),
    ("del", "delete"),
    ("ins", "insert"),
    ("pgup", "pageup"),
    ("pgdn", "pagedown"),
];

/// Splits leading `ctrl-`, `alt-` and `shift-` prefixes off `s`.
fn split_modifiers(s: &str) -> (Modifiers, &str) {
    let mut mods = Modifiers::NONE;
    let mut rest = s;
    loop {
        let lower = rest.to_ascii_lowercase();
        let (flag, len) = if lower.starts_with("ctrl-") {
            (Modifiers::CTRL, 5)
        } else if lower.starts_with("alt-") {
            (Modifiers::ALT, 4)
        } else if lower.starts_with("shift-") {
            (Modifiers::SHIFT, 6)
        } else {
            break;
        };
        if rest.len() == len {
            break; // nothing follows the prefix, so it is a (malformed) key name
        }
        mods = mods | flag;
        rest = &rest[len..];
    }
    (mods, rest)
}

impl FromStr for KeyChord {
    type Err = KeyParseError;

    /// Parses tokens such as `j`, `G`, `ctrl-p`, `alt-.`, `shift-tab`, `space`, `f5`.
    fn from_str(s: &str) -> Result<Self, Self::Err> {
        if s.is_empty() {
            return Err(KeyParseError::Empty);
        }
        let (mods, rest) = split_modifiers(s);

        let mut chars = rest.chars();
        if let (Some(c), None) = (chars.next(), chars.next()) {
            // `shift-a` means the uppercase letter.
            let c = if mods.contains(Modifiers::SHIFT) {
                c.to_ascii_uppercase()
            } else {
                c
            };
            return Ok(Self::new(Key::Char(c), mods));
        }

        let mut name = rest.to_ascii_lowercase();
        if let Some((_, target)) = ALIASES.iter().find(|(alias, _)| *alias == name) {
            (*target).clone_into(&mut name);
        }
        let key = if let Some((_, key)) = NAMED_KEYS.iter().find(|(n, _)| *n == name) {
            if *key == Key::Tab && mods.contains(Modifiers::SHIFT) {
                Key::BackTab
            } else {
                *key
            }
        } else if let Some(n) = name
            .strip_prefix('f')
            .and_then(|d| d.parse::<u8>().ok())
            .filter(|n| (1..=24).contains(n))
        {
            Key::F(n)
        } else {
            return Err(KeyParseError::UnknownKey(rest.to_owned()));
        };
        Ok(Self::new(key, mods))
    }
}

impl fmt::Display for Key {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Key::Char(' ') => f.write_str("space"),
            Key::Char(c) => write!(f, "{c}"),
            Key::F(n) => write!(f, "f{n}"),
            other => {
                let name = NAMED_KEYS
                    .iter()
                    .find(|(_, k)| k == other)
                    .map_or("?", |(n, _)| n);
                f.write_str(name)
            }
        }
    }
}

impl fmt::Display for KeyChord {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        if self.mods.contains(Modifiers::CTRL) {
            f.write_str("ctrl-")?;
        }
        if self.mods.contains(Modifiers::ALT) {
            f.write_str("alt-")?;
        }
        if self.mods.contains(Modifiers::SHIFT) {
            f.write_str("shift-")?;
        }
        write!(f, "{}", self.key)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn parse(s: &str) -> KeyChord {
        s.parse().unwrap()
    }

    #[test]
    fn parses_plain_and_modified_keys() {
        assert_eq!(parse("j"), KeyChord::char('j'));
        assert_eq!(parse("G"), KeyChord::char('G'));
        assert_eq!(parse("ctrl-p"), KeyChord::ctrl('p'));
        assert_eq!(parse("Ctrl-P"), KeyChord::ctrl('p'));
        assert_eq!(parse("space"), KeyChord::char(' '));
        assert_eq!(parse("esc"), KeyChord::plain(Key::Esc));
        assert_eq!(parse("escape"), KeyChord::plain(Key::Esc));
        assert_eq!(parse("shift-tab"), KeyChord::plain(Key::BackTab));
        assert_eq!(parse("shift-a"), KeyChord::char('A'));
        assert_eq!(parse("f5"), KeyChord::plain(Key::F(5)));
        assert_eq!(parse("-"), KeyChord::char('-'));
        assert_eq!(
            parse("ctrl--"),
            KeyChord::new(Key::Char('-'), Modifiers::CTRL)
        );
        assert_eq!(
            parse("alt-."),
            KeyChord::new(Key::Char('.'), Modifiers::ALT)
        );
    }

    #[test]
    fn rejects_garbage() {
        assert_eq!("".parse::<KeyChord>(), Err(KeyParseError::Empty));
        assert!(matches!(
            "wat".parse::<KeyChord>(),
            Err(KeyParseError::UnknownKey(_))
        ));
        assert!("f99".parse::<KeyChord>().is_err());
    }

    #[test]
    fn normalisation_drops_shift_on_chars() {
        let shifted = KeyChord::new(Key::Char('G'), Modifiers::SHIFT);
        assert_eq!(shifted, KeyChord::char('G'));
        let ctrl_upper = KeyChord::new(Key::Char('A'), Modifiers::CTRL | Modifiers::SHIFT);
        assert_eq!(ctrl_upper, KeyChord::ctrl('a'));
    }

    #[test]
    fn display_round_trips() {
        for text in [
            "j", "G", "ctrl-p", "alt-.", "space", "enter", "backtab", "f12", "~",
        ] {
            assert_eq!(parse(text).to_string(), text);
        }
    }
}
