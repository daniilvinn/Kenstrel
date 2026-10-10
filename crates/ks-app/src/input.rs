//! Translating crossterm events into core messages.
//!
//! On Windows crossterm reports both key presses and releases, so without filtering every key
//! would fire twice. Only `Press` and `Repeat` are accepted.

use crossterm::event::{Event, KeyCode, KeyEvent, KeyEventKind, KeyModifiers};
use ks_core::{Key, KeyChord, Modifiers, Msg, Size};

/// Whether a key event of this kind should be handled at all.
pub fn is_actionable(kind: KeyEventKind) -> bool {
    matches!(kind, KeyEventKind::Press | KeyEventKind::Repeat)
}

/// Converts a key event into a chord, or `None` for releases and unsupported keys.
pub fn translate_key(event: KeyEvent) -> Option<KeyChord> {
    if !is_actionable(event.kind) {
        return None;
    }
    let key = match event.code {
        KeyCode::Char(c) => Key::Char(c),
        KeyCode::Enter => Key::Enter,
        KeyCode::Esc => Key::Esc,
        KeyCode::Tab => Key::Tab,
        KeyCode::BackTab => Key::BackTab,
        KeyCode::Backspace => Key::Backspace,
        KeyCode::Delete => Key::Delete,
        KeyCode::Insert => Key::Insert,
        KeyCode::Up => Key::Up,
        KeyCode::Down => Key::Down,
        KeyCode::Left => Key::Left,
        KeyCode::Right => Key::Right,
        KeyCode::Home => Key::Home,
        KeyCode::End => Key::End,
        KeyCode::PageUp => Key::PageUp,
        KeyCode::PageDown => Key::PageDown,
        KeyCode::F(n) => Key::F(n),
        _ => return None,
    };

    let mut mods = Modifiers::NONE;
    let has = |flag| event.modifiers.contains(flag);
    // Windows reports AltGr as Ctrl+Alt; a character typed that way is just a character.
    let alt_gr =
        has(KeyModifiers::CONTROL) && has(KeyModifiers::ALT) && matches!(key, Key::Char(_));
    if has(KeyModifiers::CONTROL) && !alt_gr {
        mods = mods | Modifiers::CTRL;
    }
    if has(KeyModifiers::ALT) && !alt_gr {
        mods = mods | Modifiers::ALT;
    }
    if has(KeyModifiers::SHIFT) {
        mods = mods | Modifiers::SHIFT;
    }
    Some(KeyChord::new(key, mods))
}

/// Converts a terminal event into a message, if it is one the app cares about.
pub fn translate_event(event: Event) -> Option<Msg> {
    match event {
        Event::Key(key) => translate_key(key).map(Msg::Key),
        Event::Resize(width, height) => Some(Msg::Resize(Size::new(width, height))),
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use crossterm::event::{KeyEventState, MouseEvent, MouseEventKind};

    use super::*;

    fn key(code: KeyCode, modifiers: KeyModifiers, kind: KeyEventKind) -> KeyEvent {
        KeyEvent {
            code,
            modifiers,
            kind,
            state: KeyEventState::NONE,
        }
    }

    #[test]
    fn only_press_and_repeat_are_actionable() {
        assert!(is_actionable(KeyEventKind::Press));
        assert!(is_actionable(KeyEventKind::Repeat));
        assert!(!is_actionable(KeyEventKind::Release));
    }

    #[test]
    fn releases_are_dropped_so_keys_do_not_fire_twice() {
        let press = key(KeyCode::Char('j'), KeyModifiers::NONE, KeyEventKind::Press);
        let release = key(
            KeyCode::Char('j'),
            KeyModifiers::NONE,
            KeyEventKind::Release,
        );
        assert_eq!(translate_key(press), Some(KeyChord::char('j')));
        assert_eq!(translate_key(release), None);
        assert_eq!(translate_event(Event::Key(release)), None);
    }

    #[test]
    fn modifiers_and_special_keys_map_to_chords() {
        let ctrl_p = key(
            KeyCode::Char('p'),
            KeyModifiers::CONTROL,
            KeyEventKind::Press,
        );
        assert_eq!(translate_key(ctrl_p), Some(KeyChord::ctrl('p')));
        // Shift on a character is folded into the character itself.
        let upper = key(KeyCode::Char('G'), KeyModifiers::SHIFT, KeyEventKind::Press);
        assert_eq!(translate_key(upper), Some(KeyChord::char('G')));
        let back_tab = key(KeyCode::BackTab, KeyModifiers::SHIFT, KeyEventKind::Press);
        assert_eq!(translate_key(back_tab), Some(KeyChord::plain(Key::BackTab)));
        let repeat = key(KeyCode::Down, KeyModifiers::NONE, KeyEventKind::Repeat);
        assert_eq!(translate_key(repeat), Some(KeyChord::plain(Key::Down)));
        let f5 = key(KeyCode::F(5), KeyModifiers::NONE, KeyEventKind::Press);
        assert_eq!(translate_key(f5), Some(KeyChord::plain(Key::F(5))));
    }

    #[test]
    fn alt_gr_characters_are_plain_characters() {
        let alt_gr = key(
            KeyCode::Char('ł'),
            KeyModifiers::CONTROL | KeyModifiers::ALT,
            KeyEventKind::Press,
        );
        assert_eq!(translate_key(alt_gr), Some(KeyChord::char('ł')));
        // Alt alone is still alt.
        let alt = key(KeyCode::Char('.'), KeyModifiers::ALT, KeyEventKind::Press);
        let chord = translate_key(alt).unwrap();
        assert!(chord.mods.contains(Modifiers::ALT));
    }

    #[test]
    fn unsupported_keys_are_ignored() {
        let caps = key(KeyCode::CapsLock, KeyModifiers::NONE, KeyEventKind::Press);
        assert_eq!(translate_key(caps), None);
    }

    #[test]
    fn resize_and_other_events() {
        assert_eq!(
            translate_event(Event::Resize(100, 30)),
            Some(Msg::Resize(Size::new(100, 30)))
        );
        assert_eq!(translate_event(Event::FocusGained), None);
        assert_eq!(translate_event(Event::Paste("x".into())), None);
        let mouse = Event::Mouse(MouseEvent {
            kind: MouseEventKind::Moved,
            column: 0,
            row: 0,
            modifiers: KeyModifiers::NONE,
        });
        assert_eq!(translate_event(mouse), None);
    }
}
