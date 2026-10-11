//! Light markdown rendering: headings, lists, quotes, rules, inline emphasis/code/links
//! and highlighted fenced code blocks. Line based, no full CommonMark parser.

use tokio_util::sync::CancellationToken;

use crate::highlight::{build_theme, find_syntax, syntax_set, LineHighlighter};
use crate::model::{Encoding, Role, SpanStyle, StyledLine, TextPreview};
use crate::options::{Palette, PreviewOptions};
use crate::sanitize::Sanitizer;
use crate::text::{plain_line, split_lines};

fn rgb_style(c: crate::model::Rgb) -> SpanStyle {
    SpanStyle::rgb(c)
}

/// Render inline markdown of `text` into `line`, using `base` as the starting style.
fn inline(text: &str, base: SpanStyle, pal: &Palette, san: &mut Sanitizer, line: &mut StyledLine) {
    let chars: Vec<char> = text.chars().collect();
    // next_close[k] / next_paren[k]: index of the first `]` / `)` at or after k (len if none),
    // so link lookups are O(1) instead of rescanning the rest of the line for every `[`.
    let mut next_close = vec![chars.len(); chars.len() + 2];
    let mut next_paren = vec![chars.len(); chars.len() + 2];
    for k in (0..chars.len()).rev() {
        next_close[k] = if chars[k] == ']' {
            k
        } else {
            next_close[k + 1]
        };
        next_paren[k] = if chars[k] == ')' {
            k
        } else {
            next_paren[k + 1]
        };
    }
    // Backtick run length -> a start offset from which no closing run of that length exists.
    let mut no_tick_close: std::collections::HashMap<usize, usize> = Default::default();
    let mut buf = String::new();
    let (mut bold, mut italic) = (false, false);
    let cur = |bold: bool, italic: bool| {
        let mut s = base;
        s.bold |= bold;
        s.italic |= italic;
        s
    };
    let mut i = 0;
    macro_rules! flush {
        () => {
            if !buf.is_empty() {
                let mut out = String::with_capacity(buf.len());
                san.push_str(&buf, &mut out);
                line.push(out, cur(bold, italic));
                buf.clear();
            }
        };
    }
    while i < chars.len() {
        let c = chars[i];
        match c {
            '\\' if i + 1 < chars.len() && chars[i + 1].is_ascii_punctuation() => {
                buf.push(chars[i + 1]);
                i += 2;
            }
            '`' => {
                let ticks = chars[i..].iter().take_while(|&&c| c == '`').count();
                let start = i + ticks;
                let mut j = start;
                let mut end = None;
                // A failed search from an earlier start covers every later start too.
                let known_none = no_tick_close.get(&ticks).is_some_and(|&from| from <= start);
                while !known_none && j + ticks <= chars.len() {
                    if chars[j..j + ticks].iter().all(|&c| c == '`')
                        && chars.get(j + ticks) != Some(&'`')
                    {
                        end = Some(j);
                        break;
                    }
                    j += 1;
                }
                if end.is_none() && !known_none {
                    no_tick_close.insert(ticks, start);
                }
                match end {
                    Some(e) => {
                        flush!();
                        let code: String = chars[start..e].iter().collect();
                        let mut out = String::new();
                        san.push_str(&code, &mut out);
                        line.push(out, rgb_style(pal.string));
                        i = e + ticks;
                    }
                    None => {
                        buf.push_str(&"`".repeat(ticks));
                        i += ticks;
                    }
                }
            }
            '*' | '_' if chars.get(i + 1) == Some(&c) && (c == '*' || word_boundary(&chars, i)) => {
                flush!();
                bold = !bold;
                i += 2;
            }
            '*' => {
                flush!();
                italic = !italic;
                i += 1;
            }
            '[' => {
                // [text](url)
                let close = Some(next_close[i + 1]).filter(|&p| p < chars.len());
                let link = close.and_then(|cl| {
                    if chars.get(cl + 1) == Some(&'(') {
                        Some(next_paren[cl + 2])
                            .filter(|&p| p < chars.len())
                            .map(|p| (cl, p))
                    } else {
                        None
                    }
                });
                match link {
                    Some((cl, paren)) => {
                        flush!();
                        let label: String = chars[i + 1..cl].iter().collect();
                        let url: String = chars[cl + 2..paren].iter().collect();
                        let mut s = rgb_style(pal.function).underline();
                        s.bold = bold;
                        let mut out = String::new();
                        san.push_str(&label, &mut out);
                        line.push(out, s);
                        let mut out = String::new();
                        san.push_str(&format!(" ({url})"), &mut out);
                        line.push(out, SpanStyle::role(Role::FgDim));
                        i = paren + 1;
                    }
                    None => {
                        buf.push('[');
                        i += 1;
                    }
                }
            }
            _ => {
                buf.push(c);
                i += 1;
            }
        }
    }
    flush!();
}

fn word_boundary(chars: &[char], i: usize) -> bool {
    i == 0 || !chars[i - 1].is_alphanumeric()
}

fn fence_marker(line: &str) -> Option<(char, usize, &str)> {
    let t = line.trim_start();
    if line.len() - t.len() > 3 {
        return None;
    }
    let c = t.chars().next()?;
    if c != '`' && c != '~' {
        return None;
    }
    let n = t.chars().take_while(|&x| x == c).count();
    (n >= 3).then(|| (c, n, t[n..].trim()))
}

fn is_rule(t: &str) -> bool {
    let t: String = t.chars().filter(|c| !c.is_whitespace()).collect();
    t.len() >= 3
        && (t.chars().all(|c| c == '-')
            || t.chars().all(|c| c == '*')
            || t.chars().all(|c| c == '_'))
}

/// Render markdown `text`.
pub(crate) fn render(
    text: &str,
    encoding: Encoding,
    bytes_read: u64,
    truncated_file: bool,
    opts: &PreviewOptions,
    cancel: &CancellationToken,
) -> Option<TextPreview> {
    let pal = &opts.palette;
    let (raw, more) = split_lines(text, opts.limits.max_text_lines);
    let dim = SpanStyle::role(Role::FgDim);
    let theme = build_theme(pal);
    let mut lines: Vec<StyledLine> = Vec::with_capacity(raw.len());
    // Open fence: marker char, length, highlighter.
    let mut fence: Option<(char, usize, Option<LineHighlighter<'_>>)> = None;
    let mut in_front_matter = false;

    for (idx, l) in raw.iter().enumerate() {
        if idx % 64 == 0 && cancel.is_cancelled() {
            return None;
        }
        let mut san = Sanitizer::new(opts.tab_width);

        // Front matter only when it is closed somewhere below; otherwise `---` is a rule.
        if idx == 0
            && l.trim_end() == "---"
            && raw[1..]
                .iter()
                .any(|x| matches!(x.trim_end(), "---" | "..."))
        {
            in_front_matter = true;
            lines.push(StyledLine::styled("---", dim));
            continue;
        }
        if in_front_matter {
            if matches!(l.trim_end(), "---" | "...") {
                in_front_matter = false;
            }
            lines.push(plain_line(l, opts.tab_width));
            if let Some(last) = lines.last_mut() {
                for s in &mut last.spans {
                    s.style = dim;
                }
            }
            continue;
        }

        if let Some((ch, n, hl)) = fence.as_mut() {
            if let Some((c2, n2, info)) = fence_marker(l) {
                if c2 == *ch && n2 >= *n && info.is_empty() {
                    lines.push(StyledLine::styled(l.trim().to_string(), dim));
                    fence = None;
                    continue;
                }
            }
            lines.push(match hl {
                Some(h) => h.line(l),
                None => plain_line(l, opts.tab_width),
            });
            continue;
        }
        if let Some((c, n, info)) = fence_marker(l) {
            let lang = info.split_whitespace().next().unwrap_or("");
            let syn = if opts.highlight && !lang.is_empty() {
                let ss = syntax_set();
                ss.find_syntax_by_token(lang)
                    .or_else(|| ss.find_syntax_by_extension(lang))
                    .or_else(|| find_syntax(std::path::Path::new(&format!("x.{lang}")), ""))
            } else {
                None
            };
            let hl = syn.map(|s| LineHighlighter::new(s, &theme, pal, opts.limits, opts.tab_width));
            let mut text = String::new();
            san.push_str(l.trim(), &mut text);
            lines.push(StyledLine::styled(text, dim));
            fence = Some((c, n, hl));
            continue;
        }

        let trimmed = l.trim_start();
        let mut line = StyledLine::default();

        // Heading
        let hashes = trimmed.chars().take_while(|&c| c == '#').count();
        if (1..=6).contains(&hashes) && trimmed[hashes..].starts_with([' ', '\t']) {
            let mut style = SpanStyle::role(Role::Accent).bold();
            if hashes == 1 {
                style = style.underline();
            }
            let mut prefix = String::new();
            san.push_str(&trimmed[..hashes], &mut prefix);
            line.push(format!("{prefix} "), SpanStyle::role(Role::FgMuted));
            inline(trimmed[hashes..].trim(), style, pal, &mut san, &mut line);
            lines.push(line);
            continue;
        }
        if is_rule(trimmed) && !trimmed.starts_with(|c: char| c.is_alphanumeric()) {
            lines.push(StyledLine::styled("─".repeat(40), dim));
            continue;
        }
        // Block quote
        if let Some(rest) = trimmed.strip_prefix('>') {
            line.push("\u{258e} ".into(), dim);
            let mut body = rest;
            let mut depth = 0;
            while let Some(r) = body.trim_start().strip_prefix('>') {
                body = r;
                depth += 1;
                line.push("\u{258e} ".into(), dim);
                if depth > 6 {
                    break;
                }
            }
            inline(
                body.trim_start(),
                SpanStyle::default().italic(),
                pal,
                &mut san,
                &mut line,
            );
            lines.push(line);
            continue;
        }
        // Lists
        let indent: usize = l[..l.len() - trimmed.len()]
            .chars()
            .map(|c| if c == '\t' { opts.tab_width } else { 1 })
            .sum();
        let bullet = trimmed
            .strip_prefix(['-', '*', '+'])
            .filter(|r| r.starts_with(' '));
        if let Some(rest) = bullet {
            line.push(" ".repeat(indent), SpanStyle::default());
            let (task, rest) = match rest.trim_start().strip_prefix("[ ]") {
                Some(r) => (Some("\u{2610} "), r),
                None => match rest
                    .trim_start()
                    .strip_prefix("[x]")
                    .or_else(|| rest.trim_start().strip_prefix("[X]"))
                {
                    Some(r) => (Some("\u{2611} "), r),
                    None => (None, rest),
                },
            };
            line.push(
                task.unwrap_or("\u{2022} ").to_string(),
                SpanStyle::role(Role::Accent),
            );
            inline(
                rest.trim_start(),
                SpanStyle::default(),
                pal,
                &mut san,
                &mut line,
            );
            lines.push(line);
            continue;
        }
        let digits = trimmed.chars().take_while(|c| c.is_ascii_digit()).count();
        if (1..=9).contains(&digits) && trimmed[digits..].starts_with(". ") {
            line.push(" ".repeat(indent), SpanStyle::default());
            line.push(
                format!("{}. ", &trimmed[..digits]),
                SpanStyle::role(Role::Accent),
            );
            inline(
                trimmed[digits + 2..].trim_start(),
                SpanStyle::default(),
                pal,
                &mut san,
                &mut line,
            );
            lines.push(line);
            continue;
        }
        inline(l, SpanStyle::default(), pal, &mut san, &mut line);
        lines.push(line);
    }

    Some(TextPreview {
        first_line: 0,
        line_count: lines.len(),
        total_lines: (!(more || truncated_file)).then_some(lines.len()),
        lines,
        truncated: more || truncated_file,
        encoding,
        syntax: Some("Markdown".into()),
        bytes_read,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::model::Color;

    #[test]
    fn pathological_single_lines_render_in_linear_time() {
        let n = 250_000;
        let cases = [
            "[".repeat(n),
            "[](".repeat(n / 3),
            format!("{}]", "[".repeat(n)),
            "[x](".repeat(n / 4),
            "` ``".repeat(n / 4),
            "`a``b```c````".repeat(n / 13),
        ];
        for text in &cases {
            let start = std::time::Instant::now();
            let p = render_str(text);
            let took = start.elapsed();
            assert!(!p.lines.is_empty());
            assert!(
                took < std::time::Duration::from_secs(5),
                "{} chars took {took:?}",
                text.chars().count()
            );
        }
    }

    #[test]
    fn links_and_code_still_render_after_the_linear_rewrite() {
        let p = render_str("see [docs](http://x.y/z) and `code` and [broken](nope");
        let text = p.lines[0].text();
        assert!(text.contains("docs (http://x.y/z)"), "{text}");
        assert!(text.contains("code"), "{text}");
        assert!(text.contains("[broken](nope"), "{text}");
    }

    fn render_str(s: &str) -> TextPreview {
        render(
            s,
            Encoding::Utf8,
            s.len() as u64,
            false,
            &PreviewOptions::default(),
            &CancellationToken::new(),
        )
        .expect("not cancelled")
    }

    #[test]
    fn headings_lists_quotes() {
        let p = render_str("# Title\n\n- one\n1. two\n> quoted\n---\n");
        assert_eq!(p.lines[0].text(), "# Title");
        let title = &p.lines[0].spans[1];
        assert!(title.style.bold);
        assert_eq!(title.style.fg, Some(Color::Role(Role::Accent)));
        assert_eq!(p.lines[2].text(), "\u{2022} one");
        assert_eq!(p.lines[3].text(), "1. two");
        assert!(p.lines[4].text().contains("quoted"));
        assert!(p.lines[5].text().starts_with('─'));
    }

    #[test]
    fn code_fence_is_highlighted() {
        let p = render_str("```rust\nfn main() {}\n```\n");
        assert_eq!(p.lines.len(), 3);
        assert_eq!(p.lines[1].text(), "fn main() {}");
        assert!(p.lines[1].spans.len() > 1, "expected highlighted spans");
        assert_eq!(p.lines[0].spans[0].style.fg, Some(Color::Role(Role::FgDim)));
    }

    #[test]
    fn inline_styles_and_sanitisation() {
        let p = render_str("a **bold** `code` [l](http://x) \x1b[31m");
        let l = &p.lines[0];
        assert!(l.spans.iter().any(|s| s.text == "bold" && s.style.bold));
        assert!(l.spans.iter().any(|s| s.text == "code"));
        assert!(l.spans.iter().any(|s| s.text == "l" && s.style.underline));
        assert!(!l.text().chars().any(|c| c.is_control()));
    }
}
