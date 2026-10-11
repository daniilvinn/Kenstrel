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
    // Some consoles deliver ctrl-\ as the raw FS control character.
    if event.code == KeyCode::Char('\u{1c}') {
        return Some(KeyChord::ctrl('\\'));
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
    let key = match key {
        Key::Char(c)
            if !c.is_ascii()
                && (mods.contains(Modifiers::CTRL) || mods.contains(Modifiers::ALT)) =>
        {
            Key::Char(by_position(c, ks_fs::latin_key))
        }
        other => other,
    };
    Some(KeyChord::new(key, mods))
}

/// A shortcut typed with a non-Latin layout active (`ctrl-з` on a Russian one) means the key
/// at that position: `ctrl-p`. The case is kept (shift or caps lock).
fn by_position(c: char, latin_key: impl Fn(char) -> Option<char>) -> char {
    match latin_key(c) {
        Some(latin) if c.is_uppercase() => latin.to_ascii_uppercase(),
        Some(latin) => latin,
        None => c,
    }
}

/// Converts a terminal event into a message, if it is one the app cares about.
pub fn translate_event(event: Event) -> Option<Msg> {
    // `KESTREL_LOG=ks_app::input=trace` shows what the terminal really sends.
    tracing::trace!(?event, "terminal event");
    match event {
        Event::Key(key) => translate_key(key).map(Msg::Key),
        Event::Resize(width, height) => Some(Msg::Resize(Size::new(width, height))),
        Event::Paste(text) => Some(Msg::Paste(text)),
        _ => None,
    }
}

/// A run of at least this many text keys that arrive together is a paste.
pub const PASTE_BURST_MIN: usize = 4;

/// True for keys a paste can consist of: plain characters, enter and tab.
pub fn is_paste_key(chord: KeyChord) -> bool {
    let plain = !chord.mods.contains(Modifiers::CTRL) && !chord.mods.contains(Modifiers::ALT);
    plain && matches!(chord.key, Key::Char(_) | Key::Enter | Key::Tab)
}

/// The text of a burst of paste keys (enter becomes a newline).
pub fn burst_text(keys: &[KeyChord]) -> String {
    keys.iter()
        .map(|k| match k.key {
            Key::Char(c) => c,
            Key::Tab => '\t',
            _ => '\n',
        })
        .collect()
}

/// crossterm's Windows reader never produces `Event::Paste`: a paste arrives as one key event
/// per character, with every newline as an Enter that would submit a prompt. Given the keys
/// of a run that were all ready at once, returns the messages to apply: one `Msg::Paste` for
/// a run of [`PASTE_BURST_MIN`] or more, else the keys unchanged.
pub fn coalesce_burst(keys: Vec<KeyChord>) -> Vec<Msg> {
    if keys.len() >= PASTE_BURST_MIN {
        vec![Msg::Paste(burst_text(&keys))]
    } else {
        keys.into_iter().map(Msg::Key).collect()
    }
}

#[cfg(test)]
mod tests {
    use crossterm::event::{KeyEventState, MouseEvent, MouseEventKind};

    use super::*;

    fn russian(c: char) -> Option<char> {
        match c {
            'з' | 'З' => Some('p'),
            'ю' => Some('.'),
            _ => None,
        }
    }

    #[test]
    fn shortcuts_on_a_non_latin_layout_mean_the_key_position() {
        assert_eq!(by_position('з', russian), 'p');
        assert_eq!(by_position('З', russian), 'P', "shift is kept");
        assert_eq!(by_position('ю', russian), '.');
        assert_eq!(by_position('ж', russian), 'ж', "no Latin key: unchanged");
    }

    #[test]
    fn plain_non_latin_text_is_never_remapped() {
        let typed = translate_key(key(
            KeyCode::Char('з'),
            KeyModifiers::NONE,
            KeyEventKind::Press,
        ))
        .unwrap();
        assert_eq!(typed.key, Key::Char('з'));
        // AltGr (ctrl+alt) characters are text too.
        let alt_gr = translate_key(key(
            KeyCode::Char('ё'),
            KeyModifiers::CONTROL | KeyModifiers::ALT,
            KeyEventKind::Press,
        ))
        .unwrap();
        assert_eq!(alt_gr.key, Key::Char('ё'));
    }

    fn key(code: KeyCode, modifiers: KeyModifiers, kind: KeyEventKind) -> KeyEvent {
        KeyEvent {
            code,
            modifiers,
            kind,
            state: KeyEventState::NONE,
        }
    }

    #[test]
    fn a_burst_of_text_keys_becomes_one_paste() {
        let keys: Vec<KeyChord> = "ab\ncd"
            .chars()
            .map(|c| {
                if c == '\n' {
                    KeyChord::plain(Key::Enter)
                } else {
                    KeyChord::char(c)
                }
            })
            .collect();
        assert_eq!(coalesce_burst(keys), vec![Msg::Paste("ab\ncd".into())]);
        let short = vec![KeyChord::char('l'), KeyChord::plain(Key::Enter)];
        assert_eq!(
            coalesce_burst(short),
            vec![
                Msg::Key(KeyChord::char('l')),
                Msg::Key(KeyChord::plain(Key::Enter))
            ]
        );
        assert!(!is_paste_key(KeyChord::ctrl('c')));
        assert!(!is_paste_key(KeyChord::plain(Key::Esc)));
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
    fn ctrl_backslash_in_both_spellings() {
        let plain = key(
            KeyCode::Char('\\'),
            KeyModifiers::CONTROL,
            KeyEventKind::Press,
        );
        let raw = key(
            KeyCode::Char('\u{1c}'),
            KeyModifiers::NONE,
            KeyEventKind::Press,
        );
        assert_eq!(translate_key(plain), Some(KeyChord::ctrl('\\')));
        assert_eq!(translate_key(raw), Some(KeyChord::ctrl('\\')));
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
        assert_eq!(
            translate_event(Event::Paste("x".into())),
            Some(Msg::Paste("x".into()))
        );
        let mouse = Event::Mouse(MouseEvent {
            kind: MouseEventKind::Moved,
            column: 0,
            row: 0,
            modifiers: KeyModifiers::NONE,
        });
        assert_eq!(translate_event(mouse), None);
    }
}
