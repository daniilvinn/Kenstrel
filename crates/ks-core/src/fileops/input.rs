//! A one-line text input with a cursor, used by the rename / create / shell prompts, the
//! typed delete confirmation and the inline rename of the conflict dialog.

use crate::key::{Key, KeyChord, Modifiers};

/// Text plus a cursor position (counted in characters).
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct PromptInput {
    text: String,
    cursor: usize,
}

impl PromptInput {
    /// Empty input.
    pub fn new() -> Self {
        Self::default()
    }

    /// Input with `text`, the cursor placed after `cursor` characters (clamped).
    pub fn with_text(text: impl Into<String>, cursor: usize) -> Self {
        let text = text.into();
        let cursor = cursor.min(text.chars().count());
        Self { text, cursor }
    }

    /// Input with `text` and the cursor at its end.
    pub fn with_text_at_end(text: impl Into<String>) -> Self {
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

    fn byte_index(&self, chars: usize) -> usize {
        self.text
            .char_indices()
            .nth(chars)
            .map_or(self.text.len(), |(i, _)| i)
    }

    pub fn insert(&mut self, c: char) {
        let at = self.byte_index(self.cursor);
        self.text.insert(at, c);
        self.cursor += 1;
    }

    fn backspace(&mut self) {
        if self.cursor == 0 {
            return;
        }
        let start = self.byte_index(self.cursor - 1);
        let end = self.byte_index(self.cursor);
        self.text.replace_range(start..end, "");
        self.cursor -= 1;
    }

    fn delete(&mut self) {
        let start = self.byte_index(self.cursor);
        if start >= self.text.len() {
            return;
        }
        let end = self.byte_index(self.cursor + 1);
        self.text.replace_range(start..end, "");
    }

    /// Deletes the word before the cursor (`ctrl-w`).
    fn delete_word(&mut self) {
        let chars: Vec<char> = self.text.chars().collect();
        let mut start = self.cursor;
        while start > 0 && chars[start - 1].is_whitespace() {
            start -= 1;
        }
        while start > 0 && !chars[start - 1].is_whitespace() {
            start -= 1;
        }
        let from = self.byte_index(start);
        let to = self.byte_index(self.cursor);
        self.text.replace_range(from..to, "");
        self.cursor = start;
    }

    /// Applies an editing key. Returns whether the key was an editing key (and so consumed).
    /// `enter`, `esc` and `tab` are not editing keys.
    pub fn handle_key(&mut self, chord: KeyChord) -> bool {
        let ctrl = chord.mods.contains(Modifiers::CTRL);
        let alt = chord.mods.contains(Modifiers::ALT);
        let len = self.text.chars().count();
        match chord.key {
            Key::Char(c) if !ctrl && !alt => self.insert(c),
            Key::Char('a') if ctrl => self.cursor = 0,
            Key::Char('e') if ctrl => self.cursor = len,
            Key::Char('b') if ctrl => self.cursor = self.cursor.saturating_sub(1),
            Key::Char('f') if ctrl => self.cursor = (self.cursor + 1).min(len),
            Key::Char('u') if ctrl => {
                let at = self.byte_index(self.cursor);
                self.text.replace_range(..at, "");
                self.cursor = 0;
            }
            Key::Char('k') if ctrl => {
                let at = self.byte_index(self.cursor);
                self.text.truncate(at);
            }
            Key::Char('w') if ctrl => self.delete_word(),
            Key::Backspace => self.backspace(),
            Key::Delete => self.delete(),
            Key::Left => self.cursor = self.cursor.saturating_sub(1),
            Key::Right => self.cursor = (self.cursor + 1).min(len),
            Key::Home => self.cursor = 0,
            Key::End => self.cursor = len,
            _ => return false,
        }
        true
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn key(text: &str) -> KeyChord {
        text.parse().unwrap()
    }

    #[test]
    fn typing_and_editing() {
        let mut input = PromptInput::new();
        for c in "héllo".chars() {
            input.handle_key(KeyChord::char(c));
        }
        assert_eq!(input.text(), "héllo");
        assert_eq!(input.cursor(), 5);
        input.handle_key(key("left"));
        input.handle_key(key("left"));
        input.handle_key(key("backspace"));
        assert_eq!(input.text(), "hélo");
        input.handle_key(key("delete"));
        assert_eq!(input.text(), "héo");
        input.handle_key(key("home"));
        input.handle_key(KeyChord::char('X'));
        assert_eq!(input.text(), "Xhéo");
        input.handle_key(key("ctrl-u"));
        assert_eq!(input.text(), "héo");
        input.handle_key(key("end"));
        input.handle_key(key("ctrl-w"));
        assert!(input.is_empty());
    }

    #[test]
    fn word_delete_and_kill() {
        let mut input = PromptInput::with_text_at_end("git status --short");
        input.handle_key(key("ctrl-w"));
        assert_eq!(input.text(), "git status ");
        input.handle_key(key("ctrl-a"));
        input.handle_key(key("right"));
        input.handle_key(key("ctrl-k"));
        assert_eq!(input.text(), "g");
    }

    #[test]
    fn non_editing_keys_are_not_consumed() {
        let mut input = PromptInput::new();
        assert!(!input.handle_key(key("enter")));
        assert!(!input.handle_key(key("esc")));
        assert!(!input.handle_key(key("ctrl-p")));
        assert!(input.is_empty());
    }

    #[test]
    fn cursor_is_clamped() {
        let input = PromptInput::with_text("ab", 10);
        assert_eq!(input.cursor(), 2);
    }
}
