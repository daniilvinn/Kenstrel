//! The command palette overlay (PLAN §4.3).

use ratatui::layout::Rect;
use ratatui::style::{Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, Paragraph};
use ratatui::Frame;

use ks_core::palette::{box_size, Item, PaletteState};
use ks_core::{AppState, Overlay};

use crate::popup::{centered, framed};
use crate::text::{truncate_end, width};
use crate::theme::Theme;

const PROMPT: &str = "›";

/// Draws the palette when it is the top overlay.
pub fn render(frame: &mut Frame, state: &AppState, theme: &Theme) {
    if state.overlays.last() != Some(&Overlay::Palette) {
        return;
    }
    let area = frame.area();
    let (w, h) = box_size(state.size);
    let rect = centered(area, w, h);
    let palette = &state.palette;
    if rect.width < 12 || rect.height < 5 {
        // Too small for the box: keep a one-line prompt so typed keys stay visible.
        let row = Rect::new(area.x, area.bottom().saturating_sub(1), area.width, 1);
        let line = format!("{PROMPT} {}", palette.input.text());
        let line = crate::text::truncate_start(&line, usize::from(row.width));
        frame.render_widget(Paragraph::new(Span::styled(line, theme.bold_accent())), row);
        return;
    }
    let hint = footer_hint(usize::from(rect.width));
    let inner = framed(frame, rect, palette.page.title(), hint, true, theme);
    if inner.height < 3 {
        return;
    }

    render_input(
        frame,
        Rect::new(inner.x, inner.y, inner.width, 1),
        palette,
        theme,
    );
    let rule = "─".repeat(usize::from(inner.width));
    frame.render_widget(
        Paragraph::new(Span::styled(rule, Style::new().fg(theme.border))),
        Rect::new(inner.x, inner.y + 1, inner.width, 1),
    );

    let list = Rect::new(inner.x, inner.y + 2, inner.width, inner.height - 2);
    if palette.hits.is_empty() {
        let text = if palette.items.is_empty() {
            "nothing to choose from"
        } else {
            "no matching commands"
        };
        frame.render_widget(
            Paragraph::new(Span::styled(format!("  {text}"), theme.muted())),
            Rect::new(list.x, list.y, list.width, 1.min(list.height)),
        );
        return;
    }
    for (row, hit) in palette
        .hits
        .iter()
        .enumerate()
        .skip(palette.scroll)
        .take(usize::from(list.height))
    {
        let y = list.y + u16::try_from(row - palette.scroll).unwrap_or(0);
        let selected = row == palette.selected;
        let line = item_line(
            &palette.items[hit.item],
            &hit.indices,
            selected,
            usize::from(list.width),
            theme,
        );
        let rect = Rect::new(list.x, y, list.width, 1);
        if selected {
            frame.render_widget(
                Block::new().style(Style::new().bg(theme.selection_bg)),
                rect,
            );
        }
        frame.render_widget(Paragraph::new(line), rect);
    }
}

/// The key hint in the bottom border, the longest variant that fits the box.
fn footer_hint(box_width: usize) -> Option<&'static str> {
    // Border corners, the dashes around the text and the title side need about 8 columns.
    let room = box_width.saturating_sub(8);
    [
        "ctrl-n/ctrl-p move · enter run · esc close",
        "ctrl-n/p move · esc close",
    ]
    .into_iter()
    .find(|text| width(text) <= room)
}

fn render_input(frame: &mut Frame, area: Rect, palette: &PaletteState, theme: &Theme) {
    let text = palette.input.text();
    let cursor = palette.input.cursor();
    let (before, after) = text.split_at(cursor);
    // Scroll horizontally so the cursor stays visible: room for the prompt (3 columns),
    // the cursor cell and the counter.
    let room = usize::from(area.width).saturating_sub(3 + 1 + 10).max(1);
    let after = truncate_end(after, room.saturating_sub(1).max(1));
    let after = after.as_str();
    let before = &crate::text::truncate_start(before, room.saturating_sub(width(after)).max(1));
    let mut spans = vec![
        Span::raw(" "),
        Span::styled(PROMPT, theme.bold_accent()),
        Span::raw(" "),
        Span::styled(before.to_owned(), Style::new().fg(theme.fg)),
    ];
    // A reversed cell is the cursor; the terminal's own cursor stays hidden.
    let mut rest = after.chars();
    let under = rest.next();
    spans.push(Span::styled(
        under.map_or_else(|| " ".to_owned(), String::from),
        Style::new().fg(theme.fg).add_modifier(Modifier::REVERSED),
    ));
    spans.push(Span::styled(
        rest.as_str().to_owned(),
        Style::new().fg(theme.fg),
    ));
    frame.render_widget(Paragraph::new(Line::from(spans)), area);

    let count = if palette.hits.is_empty() {
        "0".to_owned()
    } else {
        format!("{}/{}", palette.selected + 1, palette.hits.len())
    };
    let w = u16::try_from(width(&count)).unwrap_or(0);
    if area.width > w + 12 {
        frame.render_widget(
            Paragraph::new(Span::styled(count, theme.muted())),
            Rect::new(area.right() - w - 1, area.y, w, 1),
        );
    }
}

/// One result row: marker, label with highlighted matches, bindings right-aligned.
fn item_line(
    item: &Item,
    matched: &[u32],
    selected: bool,
    total: usize,
    theme: &Theme,
) -> Line<'static> {
    let keys = if item.current {
        "current".to_owned()
    } else {
        item.keys.join(", ")
    };
    let keys_budget = total / 2;
    let keys = truncate_end(&keys, keys_budget);
    let keys_w = width(&keys);
    // Marker and padding: `▌ ` before, one space after the bindings.
    let label_budget = total.saturating_sub(2 + keys_w + 3);
    let label = truncate_end(&item.label, label_budget);

    let mut spans = vec![if selected {
        Span::styled("▌ ", theme.accent_style())
    } else {
        Span::raw("  ")
    }];
    let base = Style::new().fg(theme.fg);
    let hit = Style::new().fg(theme.accent).add_modifier(Modifier::BOLD);
    let mut buf = String::new();
    let mut buf_hit = false;
    let flush = |buf: &mut String, is_hit: bool, spans: &mut Vec<Span<'static>>| {
        if !buf.is_empty() {
            spans.push(Span::styled(
                std::mem::take(buf),
                if is_hit { hit } else { base },
            ));
        }
    };
    for (index, c) in label.chars().enumerate() {
        let is_hit = u32::try_from(index).is_ok_and(|i| matched.contains(&i));
        if is_hit != buf_hit {
            flush(&mut buf, buf_hit, &mut spans);
            buf_hit = is_hit;
        }
        buf.push(c);
    }
    flush(&mut buf, buf_hit, &mut spans);

    let used = 2 + width(&label);
    let gap = total.saturating_sub(used + keys_w + 1);
    spans.push(Span::raw(" ".repeat(gap)));
    spans.push(Span::styled(keys, theme.muted()));
    Line::from(spans)
}

#[cfg(test)]
mod tests {
    use ks_core::{update, KeyChord, Msg};
    use ratatui::backend::TestBackend;
    use ratatui::buffer::Buffer;
    use ratatui::style::Color;
    use ratatui::Terminal;

    use super::*;
    use crate::testutil::{dump, listing_state};
    use crate::theme::{ColorDepth, Theme};

    fn theme(name: &str) -> Theme {
        Theme::builtin(name, ColorDepth::TrueColor).unwrap()
    }

    fn buffer(state: &AppState, theme: &Theme) -> Buffer {
        let backend = TestBackend::new(state.size.width, state.size.height);
        let mut terminal = Terminal::new(backend).unwrap();
        terminal.draw(|f| crate::render(f, state, theme)).unwrap();
        terminal.backend().buffer().clone()
    }

    fn key(s: &mut AppState, text: &str) {
        update(s, Msg::Key(text.parse::<KeyChord>().unwrap()));
    }

    fn type_text(s: &mut AppState, text: &str) {
        for c in text.chars() {
            update(s, Msg::Key(KeyChord::char(c)));
        }
    }

    pub(crate) fn opened(w: u16, h: u16) -> AppState {
        let mut s = listing_state(w, h);
        s.themes = vec!["ember".into(), "paper".into()];
        key(&mut s, ":");
        s
    }

    #[test]
    fn palette_snapshots() {
        for name in ["ember", "paper"] {
            for (w, h) in [(80, 24), (160, 48)] {
                let rendered = dump(&buffer(&opened(w, h), &theme(name)));
                insta::assert_snapshot!(format!("palette_{name}_{w}x{h}"), rendered);
            }
        }
    }

    #[test]
    fn palette_with_query_snapshots() {
        for name in ["ember", "paper"] {
            for (w, h) in [(80, 24), (160, 48)] {
                let mut s = opened(w, h);
                type_text(&mut s, "srt");
                key(&mut s, "down");
                let rendered = dump(&buffer(&s, &theme(name)));
                insta::assert_snapshot!(format!("palette_query_{name}_{w}x{h}"), rendered);
            }
        }
    }

    #[test]
    fn theme_page_and_empty_results_snapshots() {
        for name in ["ember", "paper"] {
            for (w, h) in [(80, 24), (160, 48)] {
                let mut s = opened(w, h);
                type_text(&mut s, "switch theme");
                key(&mut s, "enter");
                insta::assert_snapshot!(
                    format!("palette_themes_{name}_{w}x{h}"),
                    dump(&buffer(&s, &theme(name)))
                );
                let mut s = opened(w, h);
                type_text(&mut s, "zzzzqq");
                insta::assert_snapshot!(
                    format!("palette_empty_{name}_{w}x{h}"),
                    dump(&buffer(&s, &theme(name)))
                );
            }
        }
    }

    #[test]
    fn a_long_query_keeps_the_cursor_visible() {
        let mut s = opened(80, 24);
        type_text(&mut s, &"x".repeat(80));
        type_text(&mut s, "END");
        let text = dump(&buffer(&s, &theme("ember")));
        assert!(text.contains("ENDx") || text.contains("END "), "{text}");
    }

    #[test]
    fn a_terminal_too_small_for_the_box_still_shows_the_prompt() {
        let mut s = opened(10, 4);
        type_text(&mut s, "ab");
        let text = dump(&buffer(&s, &theme("ember")));
        assert!(text.contains("› ab"), "{text}");
    }

    #[test]
    fn matches_are_accent_and_bold_and_bindings_are_muted() {
        let t = theme("ember");
        let mut s = opened(80, 24);
        type_text(&mut s, "recycle");
        let buf = buffer(&s, &t);
        let mut found_hit = false;
        let mut found_key = false;
        for y in 0..buf.area.height {
            for x in 0..buf.area.width {
                let cell = &buf[(x, y)];
                if cell.symbol() == "R"
                    && cell.fg == t.accent
                    && cell.modifier.contains(Modifier::BOLD)
                {
                    found_hit = true;
                }
                if cell.symbol() == "d" && cell.fg == t.fg_muted {
                    found_key = true;
                }
            }
        }
        assert!(found_hit, "matched characters use accent + bold");
        assert!(found_key, "bindings are drawn in fg_muted");
    }

    #[test]
    fn small_terminals_do_not_panic() {
        for (w, h) in [(0, 0), (10, 4), (30, 8), (44, 12), (80, 12), (200, 6)] {
            let mut s = opened(w, h);
            type_text(&mut s, "sort");
            let _ = buffer(&s, &theme("ember"));
        }
    }

    #[test]
    fn indexed_256_colours_never_use_rgb() {
        let t = Theme::builtin("ember", ColorDepth::Indexed256).unwrap();
        let buf = buffer(&opened(80, 24), &t);
        for y in 0..buf.area.height {
            for x in 0..buf.area.width {
                let cell = &buf[(x, y)];
                assert!(!matches!(cell.fg, Color::Rgb(..)), "rgb fg at {x},{y}");
                assert!(!matches!(cell.bg, Color::Rgb(..)), "rgb bg at {x},{y}");
            }
        }
    }
}
