//! Key event to terminal byte encoder for the embedded pane (10b).
//!
//! The engine has its own [`KeyEvent`] type so it does not depend on crossterm; the app converts.
//! Encoding follows xterm conventions (what Claude Code expects on a legacy-keyboard terminal):
//! `Esc` = `ESC`, `Enter` = `CR`, `Backspace` = `DEL`, `Shift-Tab` = `CSI Z`, `Alt-x` = `ESC x`,
//! modified cursor/function keys use `CSI 1 ; <mod> X` with `mod = 1 + shift + 2*alt + 4*ctrl`.

/// Key identity.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum KeyCode {
    /// A character (already shifted, as the terminal would deliver it).
    Char(char),
    /// Enter / Return.
    Enter,
    /// Tab.
    Tab,
    /// Shift-Tab.
    BackTab,
    /// Backspace.
    Backspace,
    /// Escape.
    Esc,
    /// Arrow up.
    Up,
    /// Arrow down.
    Down,
    /// Arrow left.
    Left,
    /// Arrow right.
    Right,
    /// Home.
    Home,
    /// End.
    End,
    /// Page up.
    PageUp,
    /// Page down.
    PageDown,
    /// Delete.
    Delete,
    /// Insert.
    Insert,
    /// Function key F1..=F12.
    F(u8),
}

/// Modifier flags.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct Modifiers {
    /// Shift held.
    pub shift: bool,
    /// Alt held.
    pub alt: bool,
    /// Ctrl held.
    pub ctrl: bool,
}

impl Modifiers {
    /// No modifiers.
    pub const NONE: Modifiers = Modifiers {
        shift: false,
        alt: false,
        ctrl: false,
    };
    /// Ctrl only.
    pub const CTRL: Modifiers = Modifiers {
        shift: false,
        alt: false,
        ctrl: true,
    };
    /// Alt only.
    pub const ALT: Modifiers = Modifiers {
        shift: false,
        alt: true,
        ctrl: false,
    };
    /// Shift only.
    pub const SHIFT: Modifiers = Modifiers {
        shift: true,
        alt: false,
        ctrl: false,
    };

    fn is_none(self) -> bool {
        !(self.shift || self.alt || self.ctrl)
    }

    /// xterm modifier parameter (`1 + shift + 2 alt + 4 ctrl`).
    fn param(self) -> u8 {
        1 + u8::from(self.shift) + 2 * u8::from(self.alt) + 4 * u8::from(self.ctrl)
    }
}

/// A key press.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct KeyEvent {
    /// Which key.
    pub code: KeyCode,
    /// Modifiers.
    pub mods: Modifiers,
}

impl KeyEvent {
    /// Key without modifiers.
    pub fn new(code: KeyCode) -> Self {
        Self {
            code,
            mods: Modifiers::NONE,
        }
    }
    /// Key with modifiers.
    pub fn with(code: KeyCode, mods: Modifiers) -> Self {
        Self { code, mods }
    }
}

fn csi_final(final_byte: char, mods: Modifiers, app_cursor: bool) -> Vec<u8> {
    if mods.is_none() {
        let intro = if app_cursor { "\x1bO" } else { "\x1b[" };
        format!("{intro}{final_byte}").into_bytes()
    } else {
        format!("\x1b[1;{}{final_byte}", mods.param()).into_bytes()
    }
}

fn csi_tilde(n: u8, mods: Modifiers) -> Vec<u8> {
    if mods.is_none() {
        format!("\x1b[{n}~").into_bytes()
    } else {
        format!("\x1b[{n};{}~", mods.param()).into_bytes()
    }
}

fn ctrl_char(c: char) -> Option<u8> {
    match c {
        'a'..='z' => Some(c as u8 - b'a' + 1),
        'A'..='Z' => Some(c as u8 - b'A' + 1),
        '@' | ' ' | '2' => Some(0),
        '[' | '3' => Some(0x1b),
        '\\' | '4' => Some(0x1c),
        ']' | '5' => Some(0x1d),
        '^' | '6' => Some(0x1e),
        '_' | '7' | '/' => Some(0x1f),
        '?' | '8' => Some(0x7f),
        _ => None,
    }
}

/// Encode a key. `app_cursor` is the pane's DECCKM state (`ESC O A` instead of `ESC [ A` for
/// unmodified arrows/Home/End); get it from `ClaudePane::application_cursor`.
pub fn encode_key(key: KeyEvent, app_cursor: bool) -> Vec<u8> {
    let m = key.mods;
    let alt_prefix = |mut v: Vec<u8>| {
        if m.alt {
            v.insert(0, 0x1b);
        }
        v
    };
    match key.code {
        KeyCode::Char(c) => {
            if m.ctrl {
                if let Some(b) = ctrl_char(c) {
                    return alt_prefix(vec![b]);
                }
            }
            let mut buf = [0u8; 4];
            alt_prefix(c.encode_utf8(&mut buf).as_bytes().to_vec())
        }
        // Claude Code inserts a newline on Alt/Shift-Enter (ESC CR); Ctrl-Enter sends LF.
        KeyCode::Enter => {
            if m.alt || m.shift {
                b"\x1b\r".to_vec()
            } else if m.ctrl {
                b"\n".to_vec()
            } else {
                b"\r".to_vec()
            }
        }
        KeyCode::Tab => alt_prefix(b"\t".to_vec()),
        KeyCode::BackTab => b"\x1b[Z".to_vec(),
        KeyCode::Backspace => alt_prefix(if m.ctrl { vec![0x08] } else { vec![0x7f] }),
        KeyCode::Esc => alt_prefix(vec![0x1b]),
        KeyCode::Up => csi_final('A', m, app_cursor),
        KeyCode::Down => csi_final('B', m, app_cursor),
        KeyCode::Right => csi_final('C', m, app_cursor),
        KeyCode::Left => csi_final('D', m, app_cursor),
        KeyCode::Home => csi_final('H', m, app_cursor),
        KeyCode::End => csi_final('F', m, app_cursor),
        KeyCode::PageUp => csi_tilde(5, m),
        KeyCode::PageDown => csi_tilde(6, m),
        KeyCode::Delete => csi_tilde(3, m),
        KeyCode::Insert => csi_tilde(2, m),
        KeyCode::F(n) => match n {
            1..=4 => {
                let f = ['P', 'Q', 'R', 'S'][usize::from(n) - 1];
                if m.is_none() {
                    format!("\x1bO{f}").into_bytes()
                } else {
                    format!("\x1b[1;{}{f}", m.param()).into_bytes()
                }
            }
            5 => csi_tilde(15, m),
            6 => csi_tilde(17, m),
            7 => csi_tilde(18, m),
            8 => csi_tilde(19, m),
            9 => csi_tilde(20, m),
            10 => csi_tilde(21, m),
            11 => csi_tilde(23, m),
            12 => csi_tilde(24, m),
            _ => Vec::new(),
        },
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn enc(code: KeyCode, mods: Modifiers) -> Vec<u8> {
        encode_key(KeyEvent::with(code, mods), false)
    }

    #[test]
    fn table() {
        use KeyCode::*;
        let n = Modifiers::NONE;
        let cases: Vec<(KeyCode, Modifiers, &[u8])> = vec![
            (Char('a'), n, b"a"),
            (Char('A'), Modifiers::SHIFT, b"A"),
            (Char('é'), n, "é".as_bytes()),
            (Char('c'), Modifiers::CTRL, &[3]),
            (Char('C'), Modifiers::CTRL, &[3]),
            (Char('d'), Modifiers::CTRL, &[4]),
            (Char('z'), Modifiers::CTRL, &[26]),
            (Char('['), Modifiers::CTRL, &[0x1b]),
            (Char('\\'), Modifiers::CTRL, &[0x1c]),
            (Char(' '), Modifiers::CTRL, &[0]),
            (Char('x'), Modifiers::ALT, b"\x1bx"),
            (
                Char('b'),
                Modifiers {
                    shift: false,
                    alt: true,
                    ctrl: true,
                },
                &[0x1b, 2],
            ),
            (Enter, n, b"\r"),
            (Enter, Modifiers::ALT, b"\x1b\r"),
            (Enter, Modifiers::SHIFT, b"\x1b\r"),
            (Enter, Modifiers::CTRL, b"\n"),
            (Tab, n, b"\t"),
            (BackTab, Modifiers::SHIFT, b"\x1b[Z"),
            (BackTab, n, b"\x1b[Z"),
            (Backspace, n, &[0x7f]),
            (Backspace, Modifiers::ALT, &[0x1b, 0x7f]),
            (Esc, n, &[0x1b]),
            (Up, n, b"\x1b[A"),
            (Down, n, b"\x1b[B"),
            (Right, n, b"\x1b[C"),
            (Left, n, b"\x1b[D"),
            (Left, Modifiers::CTRL, b"\x1b[1;5D"),
            (Right, Modifiers::ALT, b"\x1b[1;3C"),
            (Up, Modifiers::SHIFT, b"\x1b[1;2A"),
            (Home, n, b"\x1b[H"),
            (End, n, b"\x1b[F"),
            (PageUp, n, b"\x1b[5~"),
            (PageDown, n, b"\x1b[6~"),
            (Delete, n, b"\x1b[3~"),
            (Delete, Modifiers::CTRL, b"\x1b[3;5~"),
            (Insert, n, b"\x1b[2~"),
            (F(1), n, b"\x1bOP"),
            (F(4), n, b"\x1bOS"),
            (F(1), Modifiers::SHIFT, b"\x1b[1;2P"),
            (F(5), n, b"\x1b[15~"),
            (F(10), n, b"\x1b[21~"),
            (F(12), n, b"\x1b[24~"),
            (F(12), Modifiers::CTRL, b"\x1b[24;5~"),
        ];
        for (code, mods, want) in cases {
            assert_eq!(enc(code, mods), want, "{code:?} {mods:?}");
        }
    }

    #[test]
    fn application_cursor_mode() {
        let up = encode_key(KeyEvent::new(KeyCode::Up), true);
        assert_eq!(up, b"\x1bOA");
        let home = encode_key(KeyEvent::new(KeyCode::Home), true);
        assert_eq!(home, b"\x1bOH");
        // modified keys keep CSI form
        assert_eq!(
            encode_key(KeyEvent::with(KeyCode::Up, Modifiers::CTRL), true),
            b"\x1b[1;5A"
        );
    }

    #[test]
    fn out_of_range_f_key_is_empty() {
        assert!(enc(KeyCode::F(13), Modifiers::NONE).is_empty());
        assert!(enc(KeyCode::F(0), Modifiers::NONE).is_empty());
    }
}
