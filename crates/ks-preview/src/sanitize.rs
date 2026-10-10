//! Neutralises file content so it can never inject terminal escape sequences.

use unicode_width::UnicodeWidthChar;

/// Stateful sanitiser: tracks the display column so tabs expand to the right width
/// across several spans of the same line.
#[derive(Debug, Clone)]
pub struct Sanitizer {
    tab_width: usize,
    col: usize,
}

impl Sanitizer {
    pub fn new(tab_width: usize) -> Self {
        Self {
            tab_width: tab_width.max(1),
            col: 0,
        }
    }

    pub fn reset(&mut self) {
        self.col = 0;
    }

    /// Append the sanitised form of `s` to `out`.
    ///
    /// - `\t` expands to spaces up to the next tab stop.
    /// - C0 controls (including ESC, BEL, CR) become Unicode control pictures (U+2400..).
    /// - DEL becomes U+2421; C1 controls (U+0080..U+009F), bidi controls and
    ///   line/paragraph separators become U+FFFD.
    pub fn push_str(&mut self, s: &str, out: &mut String) {
        for c in s.chars() {
            match c {
                '\t' => {
                    let n = self.tab_width - (self.col % self.tab_width);
                    out.extend(std::iter::repeat_n(' ', n));
                    self.col += n;
                }
                '\u{0}'..='\u{1f}' => {
                    let pic = char::from_u32(0x2400 + c as u32).unwrap_or('\u{fffd}');
                    out.push(pic);
                    self.col += 1;
                }
                '\u{7f}' => {
                    out.push('\u{2421}');
                    self.col += 1;
                }
                '\u{80}'..='\u{9f}'
                | '\u{202a}'..='\u{202e}'
                | '\u{2066}'..='\u{2069}'
                | '\u{2028}'
                | '\u{2029}'
                | '\u{200e}'
                | '\u{200f}'
                | '\u{61c}' => {
                    out.push('\u{fffd}');
                    self.col += 1;
                }
                _ => {
                    out.push(c);
                    self.col += c.width().unwrap_or(0);
                }
            }
        }
    }
}

/// Sanitise a whole line (column starts at 0).
pub fn sanitize_line(s: &str, tab_width: usize) -> String {
    let mut out = String::with_capacity(s.len());
    Sanitizer::new(tab_width).push_str(s, &mut out);
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn escapes_are_neutralised() {
        let s = sanitize_line("a\x1b[31mred\x1b]0;title\x07\u{9b}x\u{7f}", 4);
        assert!(!s.chars().any(|c| c.is_control()), "{s:?}");
        assert!(s.contains('\u{241b}'));
    }

    #[test]
    fn tabs_expand_to_stops() {
        assert_eq!(sanitize_line("a\tb", 4), "a   b");
        assert_eq!(sanitize_line("\tb", 4), "    b");
        assert_eq!(sanitize_line("abcd\tb", 4), "abcd    b");
    }

    #[test]
    fn bidi_overrides_removed() {
        assert!(!sanitize_line("a\u{202e}b", 4).contains('\u{202e}'));
    }
}
