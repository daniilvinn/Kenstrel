//! A single-line text field with the usual editing keys, shared by the palette and the
//! help filter. Pure data; the UI draws it.

use crate::key::{Key, KeyChord, Modifiers};

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct TextInput {
    text: String,
    /// Byte offset of the cursor; always on a char boundary.
    cursor: usize,
}

impl TextInput {
    pub fn text(&self) -> &str {
        &self.text
    }

    pub fn is_empty(&self) -> bool {
        self.text.is_empty()
    }

    /// Byte offset of the cursor in [`text`](Self::text).
    pub fn cursor(&self) -> usize {
        self.cursor
    }

    pub fn clear(&mut self) {
        self.text.clear();
        self.cursor = 0;
    }

    pub fn set(&mut self, text: &str) {
        self.text = text.to_owned();
        self.cursor = self.text.len();
    }

    /// Applies an editing key. Returns `Some(changed)` when the key was an editing key
    /// (`changed` says whether the text itself differs), `None` when it is not for the field.
    pub fn handle_key(&mut self, chord: KeyChord) -> Option<bool> {
        let ctrl = chord.mods.contains(Modifiers::CTRL);
        let alt = chord.mods.contains(Modifiers::ALT);
        match chord.key {
            Key::Char(c) if !ctrl && !alt => {
                self.text.insert(self.cursor, c);
                self.cursor += c.len_utf8();
                Some(true)
            }
            Key::Char('a') if ctrl => Some(self.move_to(0)),
            Key::Char('e') if ctrl => Some(self.move_to(self.text.len())),
            Key::Char('b') if ctrl => Some(self.move_left()),
            Key::Char('f') if ctrl => Some(self.move_right()),
            Key::Char('b') if alt => Some(self.move_to(self.word_start())),
            Key::Char('f') if alt => Some(self.move_to(self.word_end())),
            Key::Char('u') if ctrl => Some(self.delete_range(0, self.cursor)),
            Key::Char('k') if ctrl => Some(self.delete_range(self.cursor, self.text.len())),
            Key::Char('w') if ctrl => Some(self.delete_range(self.word_start(), self.cursor)),
            Key::Backspace if alt => Some(self.delete_range(self.word_start(), self.cursor)),
            Key::Backspace => {
                let start = self.prev_boundary();
                Some(self.delete_range(start, self.cursor))
            }
            Key::Delete => {
                let end = self.next_boundary();
                Some(self.delete_range(self.cursor, end))
            }
            Key::Left if ctrl || alt => Some(self.move_to(self.word_start())),
            Key::Right if ctrl || alt => Some(self.move_to(self.word_end())),
            Key::Left => Some(self.move_left()),
            Key::Right => Some(self.move_right()),
            Key::Home => Some(self.move_to(0)),
            Key::End => Some(self.move_to(self.text.len())),
            _ => None,
        }
    }

    fn move_to(&mut self, at: usize) -> bool {
        self.cursor = at;
        false
    }

    fn move_left(&mut self) -> bool {
        self.cursor = self.prev_boundary();
        false
    }

    fn move_right(&mut self) -> bool {
        self.cursor = self.next_boundary();
        false
    }

    fn prev_boundary(&self) -> usize {
        self.text[..self.cursor]
            .char_indices()
            .next_back()
            .map_or(0, |(i, _)| i)
    }

    fn next_boundary(&self) -> usize {
        self.text[self.cursor..]
            .chars()
            .next()
            .map_or(self.cursor, |c| self.cursor + c.len_utf8())
    }

    fn word_start(&self) -> usize {
        let before = &self.text[..self.cursor];
        let trimmed = before.trim_end_matches(char::is_whitespace);
        match trimmed.rfind(char::is_whitespace) {
            Some(i) => i + trimmed[i..].chars().next().map_or(1, char::len_utf8),
            None => 0,
        }
    }

    fn word_end(&self) -> usize {
        let after = &self.text[self.cursor..];
        let skipped = after.len() - after.trim_start_matches(char::is_whitespace).len();
        let rest = &after[skipped..];
        let word = rest.find(char::is_whitespace).unwrap_or(rest.len());
        self.cursor + skipped + word
    }

    /// Removes `start..end` and puts the cursor at `start`. True when something went.
    fn delete_range(&mut self, start: usize, end: usize) -> bool {
        if start >= end {
            return false;
        }
        self.text.replace_range(start..end, "");
        self.cursor = start;
        true
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn press(input: &mut TextInput, key: &str) -> Option<bool> {
        input.handle_key(key.parse().unwrap())
    }

    fn typed(text: &str) -> TextInput {
        let mut input = TextInput::default();
        for c in text.chars() {
            input.handle_key(KeyChord::char(c));
        }
        input
    }

    #[test]
    fn typing_and_backspace_report_text_changes() {
        let mut input = typed("héllo");
        assert_eq!(input.text(), "héllo");
        assert_eq!(press(&mut input, "backspace"), Some(true));
        assert_eq!(input.text(), "héll");
        press(&mut input, "home");
        assert_eq!(
            press(&mut input, "backspace"),
            Some(false),
            "nothing to delete"
        );
        assert_eq!(press(&mut input, "delete"), Some(true));
        assert_eq!(input.text(), "éll");
    }

    #[test]
    fn cursor_moves_over_multibyte_characters() {
        let mut input = typed("aéb");
        press(&mut input, "left");
        press(&mut input, "left");
        assert_eq!(input.cursor(), 1);
        press(&mut input, "x");
        assert_eq!(input.text(), "axéb");
        assert_eq!(press(&mut input, "left"), Some(false));
        assert_eq!(press(&mut input, "end"), Some(false));
        assert_eq!(input.cursor(), input.text().len());
    }

    #[test]
    fn word_and_line_deletion() {
        let mut input = typed("one two  three");
        press(&mut input, "ctrl-w");
        assert_eq!(input.text(), "one two  ");
        press(&mut input, "ctrl-w");
        assert_eq!(input.text(), "one ");
        press(&mut input, "ctrl-u");
        assert_eq!(input.text(), "");

        let mut input = typed("abc def");
        press(&mut input, "home");
        press(&mut input, "alt-f");
        assert_eq!(input.cursor(), 3);
        press(&mut input, "ctrl-k");
        assert_eq!(input.text(), "abc");
    }

    #[test]
    fn non_editing_keys_are_not_consumed() {
        let mut input = typed("a");
        assert_eq!(press(&mut input, "enter"), None);
        assert_eq!(press(&mut input, "esc"), None);
        assert_eq!(press(&mut input, "ctrl-z"), None);
        assert_eq!(press(&mut input, "up"), None);
    }
}
