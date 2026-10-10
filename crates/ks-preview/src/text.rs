//! Text decoding and rendering (plain, highlighted and markdown).

use std::path::Path;

use tokio_util::sync::CancellationToken;

use crate::highlight::{build_theme, find_syntax, syntax_set, LineHighlighter};
use crate::model::{Encoding, SpanStyle, StyledLine, TextPreview};
use crate::options::PreviewOptions;
use crate::sanitize::Sanitizer;

/// Result of classifying the head of a file.
pub(crate) enum Decoded {
    Text { text: String, encoding: Encoding },
    Binary,
}

/// Decide whether `head` is text and decode it. `head` may end mid-character when the
/// file was truncated (`truncated`).
pub(crate) fn decode(head: &[u8], truncated: bool) -> Decoded {
    if let Some(rest) = head.strip_prefix(&[0xEF, 0xBB, 0xBF]) {
        return match decode_utf8(rest, truncated) {
            Some((text, _)) => Decoded::Text {
                text,
                encoding: Encoding::Utf8Bom,
            },
            None => Decoded::Binary,
        };
    }
    // UTF-32 LE BOM (FF FE 00 00) is not supported; do not mis-decode it as UTF-16.
    if head.starts_with(&[0xFF, 0xFE, 0x00, 0x00]) || head.starts_with(&[0x00, 0x00, 0xFE, 0xFF]) {
        return Decoded::Binary;
    }
    if let Some(rest) = head.strip_prefix(&[0xFF, 0xFE]) {
        return Decoded::Text {
            text: decode_utf16(rest, false, truncated),
            encoding: Encoding::Utf16Le,
        };
    }
    if let Some(rest) = head.strip_prefix(&[0xFE, 0xFF]) {
        return Decoded::Text {
            text: decode_utf16(rest, true, truncated),
            encoding: Encoding::Utf16Be,
        };
    }
    // NUL bytes in the first 8 KB: binary, unless it looks like BOM-less UTF-16.
    if head.iter().take(8192).any(|&b| b == 0) {
        return sniff_utf16_without_bom(head, truncated).unwrap_or(Decoded::Binary);
    }
    match decode_utf8(head, truncated) {
        Some((text, lossy)) => Decoded::Text {
            text,
            encoding: if lossy {
                Encoding::Lossy
            } else {
                Encoding::Utf8
            },
        },
        None => Decoded::Binary,
    }
}

/// Returns `(text, was_lossy)`, or `None` when the content is mostly not text.
fn decode_utf8(bytes: &[u8], truncated: bool) -> Option<(String, bool)> {
    let mut bytes = bytes;
    match std::str::from_utf8(bytes) {
        Ok(s) => return Some((s.to_owned(), false)),
        Err(e) if truncated && e.error_len().is_none() => {
            // Incomplete character at the cut: drop it and retry.
            bytes = &bytes[..e.valid_up_to()];
            if let Ok(s) = std::str::from_utf8(bytes) {
                return Some((s.to_owned(), false));
            }
        }
        Err(_) => {}
    }
    let s = String::from_utf8_lossy(bytes);
    let total = s.chars().count().max(1);
    let bad = s
        .chars()
        .filter(|&c| {
            c == '\u{fffd}'
                || (c.is_control() && !matches!(c, '\n' | '\r' | '\t' | '\x0c' | '\x1b'))
        })
        .count();
    // More than 10% garbage: call it binary.
    if bad * 10 > total {
        None
    } else {
        Some((s.into_owned(), true))
    }
}

/// Heuristic for UTF-16 without BOM, mostly-ASCII content: NULs on every other byte.
fn sniff_utf16_without_bom(head: &[u8], truncated: bool) -> Option<Decoded> {
    let sample = &head[..head.len().min(8192) & !1];
    if sample.len() < 32 {
        return None;
    }
    let half = sample.len() / 2;
    let (mut even_nul, mut odd_nul) = (0usize, 0usize);
    for (i, &b) in sample.iter().enumerate() {
        if b == 0 {
            if i % 2 == 0 {
                even_nul += 1;
            } else {
                odd_nul += 1;
            }
        }
    }
    let big_endian = if odd_nul * 10 >= half * 4 && even_nul * 50 <= half {
        false
    } else if even_nul * 10 >= half * 4 && odd_nul * 50 <= half {
        true
    } else {
        return None;
    };
    let text = decode_utf16(head, big_endian, truncated);
    let total = text.chars().count().max(1);
    let bad = text
        .chars()
        .filter(|&c| {
            c == '\u{fffd}' || (c.is_control() && !matches!(c, '\n' | '\r' | '\t' | '\x0c'))
        })
        .count();
    if bad * 10 > total {
        return None;
    }
    Some(Decoded::Text {
        text,
        encoding: if big_endian {
            Encoding::Utf16Be
        } else {
            Encoding::Utf16Le
        },
    })
}

fn decode_utf16(bytes: &[u8], big_endian: bool, truncated: bool) -> String {
    let units: Vec<u16> = bytes
        .as_chunks::<2>()
        .0
        .iter()
        .map(|c| {
            if big_endian {
                u16::from_be_bytes([c[0], c[1]])
            } else {
                u16::from_le_bytes([c[0], c[1]])
            }
        })
        .collect();
    let mut units = units;
    // A cut head can end between the two halves of a surrogate pair: drop the lone half.
    if truncated && units.last().is_some_and(|u| (0xD800..0xDC00).contains(u)) {
        units.pop();
    }
    String::from_utf16_lossy(&units)
}

/// Split into at most `max_lines` lines (`\n`, `\r\n` or a lone `\r`). Returns the
/// lines and whether more remained.
pub(crate) fn split_lines(text: &str, max_lines: usize) -> (Vec<&str>, bool) {
    let mut lines = Vec::new();
    let mut rest = text;
    while !rest.is_empty() {
        if lines.len() >= max_lines {
            return (lines, true);
        }
        let (line, next) = match rest.find(['\n', '\r']) {
            Some(i) => {
                let skip = if rest[i..].starts_with("\r\n") { 2 } else { 1 };
                (&rest[..i], &rest[i + skip..])
            }
            None => (rest, ""),
        };
        lines.push(line);
        rest = next;
    }
    (lines, false)
}

pub(crate) fn plain_line(text: &str, tab_width: usize) -> StyledLine {
    let mut s = String::with_capacity(text.len());
    Sanitizer::new(tab_width).push_str(text, &mut s);
    StyledLine::styled(s, SpanStyle::default())
}

/// Build the text preview. `cancel` is checked every 64 lines.
pub(crate) fn render_text(
    path: &Path,
    text: &str,
    encoding: Encoding,
    bytes_read: u64,
    truncated_file: bool,
    opts: &PreviewOptions,
    cancel: &CancellationToken,
) -> Option<TextPreview> {
    let (raw_lines, more) = split_lines(text, opts.limits.max_text_lines);
    let first = raw_lines.first().copied().unwrap_or("");
    let syntax = if opts.highlight {
        find_syntax(path, first)
    } else {
        None
    };
    let mut lines = Vec::with_capacity(raw_lines.len());
    if let Some(syn) = syntax {
        let theme = build_theme(&opts.palette);
        let mut hl = LineHighlighter::new(syn, &theme, &opts.palette, opts.limits, opts.tab_width);
        for (i, l) in raw_lines.iter().enumerate() {
            if i % 64 == 0 && cancel.is_cancelled() {
                return None;
            }
            lines.push(hl.line(l));
        }
    } else {
        for (i, l) in raw_lines.iter().enumerate() {
            if i % 64 == 0 && cancel.is_cancelled() {
                return None;
            }
            lines.push(plain_line(l, opts.tab_width));
        }
    }
    let truncated = more || truncated_file;
    Some(TextPreview {
        line_count: lines.len(),
        total_lines: (!truncated).then_some(lines.len()),
        lines,
        truncated,
        encoding,
        syntax: syntax.map(|s| s.name.clone()),
        bytes_read,
    })
}

/// Make sure the syntax set is loaded (used by tests / warm-up).
#[allow(dead_code)]
pub(crate) fn ensure_loaded() {
    let _ = syntax_set();
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn utf16_le_and_be() {
        let le: Vec<u8> = [0xFF, 0xFE]
            .into_iter()
            .chain("hi\nthere".encode_utf16().flat_map(|u| u.to_le_bytes()))
            .collect();
        match decode(&le, false) {
            Decoded::Text { text, encoding } => {
                assert_eq!(text, "hi\nthere");
                assert_eq!(encoding, Encoding::Utf16Le);
            }
            Decoded::Binary => panic!("binary"),
        }
        let be: Vec<u8> = [0xFE, 0xFF]
            .into_iter()
            .chain("é".encode_utf16().flat_map(|u| u.to_be_bytes()))
            .collect();
        match decode(&be, false) {
            Decoded::Text { text, encoding } => {
                assert_eq!(text, "é");
                assert_eq!(encoding, Encoding::Utf16Be);
            }
            Decoded::Binary => panic!("binary"),
        }
    }

    #[test]
    fn truncated_multibyte_is_not_lossy() {
        let s = "aé".as_bytes();
        match decode(&s[..2], true) {
            Decoded::Text { text, encoding } => {
                assert_eq!(text, "a");
                assert_eq!(encoding, Encoding::Utf8);
            }
            Decoded::Binary => panic!("binary"),
        }
    }

    #[test]
    fn latin1_is_lossy_text_but_noise_is_binary() {
        let latin = b"caf\xe9 au lait, this is mostly text\n";
        assert!(matches!(
            decode(latin, false),
            Decoded::Text {
                encoding: Encoding::Lossy,
                ..
            }
        ));
        let noise: Vec<u8> = (0..2000u32).map(|i| (128 + (i * 7) % 100) as u8).collect();
        assert!(matches!(decode(&noise, false), Decoded::Binary));
        assert!(matches!(decode(b"ab\0cd", false), Decoded::Binary));
    }

    #[test]
    fn split_lines_handles_crlf_and_cap() {
        let (l, more) = split_lines("a\r\nb\nc", 10);
        assert_eq!(l, vec!["a", "b", "c"]);
        assert!(!more);
        let (l, more) = split_lines("a\nb\nc\n", 2);
        assert_eq!(l, vec!["a", "b"]);
        assert!(more);
        let (l, more) = split_lines("a\nb\n", 2);
        assert_eq!(l.len(), 2);
        assert!(!more);
        let (l, _) = split_lines("a\rb\r\nc\n\nd", 10);
        assert_eq!(l, vec!["a", "b", "c", "", "d"]);
    }

    #[test]
    fn utf32_bom_is_not_utf16() {
        assert!(matches!(
            decode(&[0xFF, 0xFE, 0, 0, b'a', 0, 0, 0], false),
            Decoded::Binary
        ));
    }

    #[test]
    fn bomless_utf16_is_detected() {
        let le: Vec<u8> = "hello world, plain text\nsecond line"
            .encode_utf16()
            .flat_map(|u| u.to_le_bytes())
            .collect();
        match decode(&le, false) {
            Decoded::Text { text, encoding } => {
                assert!(text.starts_with("hello world"));
                assert_eq!(encoding, Encoding::Utf16Le);
            }
            Decoded::Binary => panic!("binary"),
        }
        let be: Vec<u8> = "hello world, plain text"
            .encode_utf16()
            .flat_map(|u| u.to_be_bytes())
            .collect();
        assert!(matches!(
            decode(&be, false),
            Decoded::Text {
                encoding: Encoding::Utf16Be,
                ..
            }
        ));
        assert!(matches!(decode(&[0u8; 64], false), Decoded::Binary));
    }

    #[test]
    fn truncated_utf16_drops_lone_surrogate() {
        // U+1F600 is a surrogate pair; cut after the high half.
        let mut b = vec![0xFF, 0xFE];
        b.extend("a".encode_utf16().flat_map(|u| u.to_le_bytes()));
        let pair: Vec<u16> = "\u{1F600}".encode_utf16().collect();
        b.extend(pair[0].to_le_bytes());
        match decode(&b, true) {
            Decoded::Text { text, .. } => assert_eq!(text, "a"),
            Decoded::Binary => panic!("binary"),
        }
    }
}
