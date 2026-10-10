//! A single-line text input (query fields of the filter, find file and content search).

use crate::key::{Key, KeyChord, Modifiers};

/// What an edit key did.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Edit {
    /// Not a text-editing key.
    Ignored,
    /// Only the cursor moved.
    Moved,
    /// The text changed.
    Changed,
}

/// A line of text with a cursor (a character index, not a byte index).
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct LineInput {
    text: String,
    cursor: usize,
}

impl LineInput {
    pub fn new(text: impl Into<String>) -> Self {
        let text = text.into();
        let cursor = text.chars().count();
        Self { text, cursor }
    }

    pub fn text(&self) -> &str {
        &self.text
    }

    /// Cursor position in characters.
    pub fn cursor(&self) -> usize {
        self.cursor
    }

    pub fn is_empty(&self) -> bool {
        self.text.is_empty()
    }

    pub fn clear(&mut self) {
        self.text.clear();
        self.cursor = 0;
    }

    fn byte_index(&self, chars: usize) -> usize {
        self.text
            .char_indices()
            .nth(chars)
            .map_or(self.text.len(), |(i, _)| i)
    }

    fn len_chars(&self) -> usize {
        self.text.chars().count()
    }

    /// Applies an editing key: printable characters, backspace, delete, left/right,
    /// home/end and `ctrl-u` (clear).
    pub fn apply(&mut self, key: KeyChord) -> Edit {
        let plain = key.mods.is_empty();
        match key.key {
            Key::Char(c) if plain && !c.is_control() => {
                let at = self.byte_index(self.cursor);
                self.text.insert(at, c);
                self.cursor += 1;
                Edit::Changed
            }
            Key::Char('u') if key.mods == Modifiers::CTRL => {
                if self.text.is_empty() {
                    Edit::Ignored
                } else {
                    self.clear();
                    Edit::Changed
                }
            }
            Key::Backspace if plain => {
                if self.cursor == 0 {
                    return Edit::Moved;
                }
                let start = self.byte_index(self.cursor - 1);
                let end = self.byte_index(self.cursor);
                self.text.replace_range(start..end, "");
                self.cursor -= 1;
                Edit::Changed
            }
            Key::Delete if plain => {
                if self.cursor >= self.len_chars() {
                    return Edit::Moved;
                }
                let start = self.byte_index(self.cursor);
                let end = self.byte_index(self.cursor + 1);
                self.text.replace_range(start..end, "");
                Edit::Changed
            }
            Key::Left if plain => {
                self.cursor = self.cursor.saturating_sub(1);
                Edit::Moved
            }
            Key::Right if plain => {
                self.cursor = (self.cursor + 1).min(self.len_chars());
                Edit::Moved
            }
            Key::Home if plain => {
                self.cursor = 0;
                Edit::Moved
            }
            Key::End if plain => {
                self.cursor = self.len_chars();
                Edit::Moved
            }
            _ => Edit::Ignored,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn press(input: &mut LineInput, key: &str) -> Edit {
        input.apply(key.parse().unwrap())
    }

    #[test]
    fn typing_inserts_at_the_cursor() {
        let mut input = LineInput::default();
        for k in ["a", "b", "c"] {
            assert_eq!(press(&mut input, k), Edit::Changed);
        }
        press(&mut input, "left");
        press(&mut input, "x");
        assert_eq!(input.text(), "abxc");
        assert_eq!(input.cursor(), 3);
    }

    #[test]
    fn backspace_delete_and_home_end() {
        let mut input = LineInput::new("héllo");
        assert_eq!(input.cursor(), 5);
        press(&mut input, "backspace");
        assert_eq!(input.text(), "héll");
        press(&mut input, "home");
        assert_eq!(press(&mut input, "backspace"), Edit::Moved);
        press(&mut input, "right");
        press(&mut input, "delete");
        assert_eq!(input.text(), "hll");
        press(&mut input, "end");
        assert_eq!(input.cursor(), 3);
    }

    #[test]
    fn ctrl_u_clears_and_other_chords_are_ignored() {
        let mut input = LineInput::new("abc");
        assert_eq!(press(&mut input, "ctrl-u"), Edit::Changed);
        assert!(input.is_empty());
        assert_eq!(press(&mut input, "ctrl-u"), Edit::Ignored);
        assert_eq!(press(&mut input, "ctrl-x"), Edit::Ignored);
        assert_eq!(press(&mut input, "alt-a"), Edit::Ignored);
        assert_eq!(press(&mut input, "enter"), Edit::Ignored);
        assert_eq!(press(&mut input, "esc"), Edit::Ignored);
    }

    #[test]
    fn multibyte_characters_are_single_steps() {
        let mut input = LineInput::default();
        for c in "привет".chars() {
            input.apply(KeyChord::char(c));
        }
        press(&mut input, "left");
        press(&mut input, "backspace");
        assert_eq!(input.text(), "привт");
    }
}
