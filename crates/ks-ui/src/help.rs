//! The help overlay: every command by category with its live bindings.

use ratatui::layout::Rect;
use ratatui::style::{Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::Paragraph;
use ratatui::Frame;

use ks_core::help::{box_size, visible_rows, HelpLine};
use ks_core::{Action, AppState, KeyContext, Overlay};

use crate::popup::{centered, framed};
use crate::text::{truncate_end, width};
use crate::theme::Theme;

/// Draws the help overlay when it is the top overlay.
pub fn render(frame: &mut Frame, state: &AppState, theme: &Theme) {
    if state.overlays.last() != Some(&Overlay::Help) {
        return;
    }
    let (w, h) = box_size(state.size);
    let rect = centered(frame.area(), w, h);
    let help = &state.help;
    if rect.width < 20 || rect.height < 5 {
        // Too small for the box: say so on the last row so keys are not swallowed silently.
        let area = frame.area();
        let row = Rect::new(area.x, area.bottom().saturating_sub(1), area.width, 1);
        let text = crate::text::truncate_end("Help - esc to close", usize::from(row.width));
        frame.render_widget(Paragraph::new(Span::styled(text, theme.bold_accent())), row);
        return;
    }
    let rows = visible_rows(state.size).min(usize::from(rect.height.saturating_sub(3)));
    let total = help.lines.len();
    let first = help.scroll.min(total.saturating_sub(1));
    let last = (first + rows).min(total);
    let position = if total == 0 {
        "0 / 0".to_owned()
    } else {
        format!("{}-{} / {total}", first + 1, last)
    };
    let inner = framed(frame, rect, "Help", Some(&position), true, theme);
    if inner.height < 2 {
        return;
    }

    render_filter(
        frame,
        Rect::new(inner.x, inner.y, inner.width, 1),
        state,
        theme,
    );

    let body = Rect::new(inner.x, inner.y + 1, inner.width, inner.height - 1);
    if help.lines.is_empty() {
        frame.render_widget(
            Paragraph::new(Span::styled("  nothing matches", theme.muted())),
            Rect::new(body.x, body.y, body.width, 1.min(body.height)),
        );
        return;
    }
    let key_col = help
        .lines
        .iter()
        .filter_map(|l| match l {
            HelpLine::Entry { keys, .. } => Some(width(keys)),
            _ => None,
        })
        .max()
        .unwrap_or(0)
        .clamp(4, 22);
    let title_col = help
        .lines
        .iter()
        .filter_map(|l| match l {
            HelpLine::Entry { title, .. } => Some(width(title)),
            _ => None,
        })
        .max()
        .unwrap_or(0)
        .min(34);
    let total_width = usize::from(body.width);
    for (offset, line) in help.lines[first..last]
        .iter()
        .take(usize::from(body.height))
        .enumerate()
    {
        let y = body.y + u16::try_from(offset).unwrap_or(0);
        let rendered = match line {
            HelpLine::Blank => Line::default(),
            HelpLine::Heading(category) => Line::from(vec![
                Span::raw(" "),
                Span::styled(category.title().to_owned(), theme.bold_accent()),
            ]),
            HelpLine::Entry {
                keys,
                title,
                description,
            } => entry_line(
                keys,
                title,
                description,
                (key_col, title_col, total_width),
                theme,
            ),
        };
        frame.render_widget(
            Paragraph::new(rendered),
            Rect::new(body.x, y, body.width, 1),
        );
    }
}

fn entry_line(
    keys: &str,
    title: &str,
    description: &str,
    (key_col, title_col, total): (usize, usize, usize),
    theme: &Theme,
) -> Line<'static> {
    let keys_text = if keys.is_empty() {
        "—".to_owned()
    } else {
        truncate_end(keys, key_col)
    };
    let key_style = if keys.is_empty() {
        theme.muted()
    } else {
        Style::new().fg(theme.fg).add_modifier(Modifier::BOLD)
    };
    let mut spans = vec![
        Span::raw("   "),
        Span::styled(pad(&keys_text, key_col), key_style),
        Span::raw("  "),
    ];
    // What is left after indent, keys and gaps goes to the title, then the description.
    let rest = total.saturating_sub(3 + key_col + 2);
    let title_w = title_col.min(rest);
    spans.push(Span::styled(
        pad(&truncate_end(title, title_w), title_w),
        Style::new().fg(theme.fg_dim),
    ));
    let desc_budget = rest.saturating_sub(title_w + 2 + 1);
    if desc_budget >= 16 {
        spans.push(Span::raw("  "));
        spans.push(Span::styled(
            truncate_end(description, desc_budget),
            theme.muted(),
        ));
    }
    Line::from(spans)
}

fn pad(text: &str, to: usize) -> String {
    let w = width(text);
    format!("{text}{}", " ".repeat(to.saturating_sub(w)))
}

fn render_filter(frame: &mut Frame, area: Rect, state: &AppState, theme: &Theme) {
    let help = &state.help;
    let line = if help.filtering || !help.filter.is_empty() {
        let mut spans = vec![
            Span::raw(" "),
            Span::styled("/", theme.bold_accent()),
            Span::raw(" "),
            Span::styled(help.filter.text().to_owned(), Style::new().fg(theme.fg)),
        ];
        if help.filtering {
            spans.push(Span::styled(
                " ",
                Style::new().fg(theme.fg).add_modifier(Modifier::REVERSED),
            ));
        }
        let count = format!("{} shown", help.entry_count());
        let used: usize = spans.iter().map(|s| width(&s.content)).sum();
        let gap = usize::from(area.width).saturating_sub(used + width(&count) + 1);
        spans.push(Span::raw(" ".repeat(gap)));
        spans.push(Span::styled(count, theme.muted()));
        Line::from(spans)
    } else {
        let first = |action: Action| {
            state
                .keymap
                .keys_for(KeyContext::Help, action)
                .into_iter()
                .next()
        };
        let mut spans = vec![Span::raw(" ")];
        let hints = [
            (Action::FilterOpen, "filter"),
            (Action::CursorDown, "scroll"),
            (Action::PageHalfDown, "page"),
            (Action::Cancel, "close"),
        ];
        for (action, label) in hints {
            if let Some(keys) = first(action) {
                spans.push(Span::styled("‹", theme.muted()));
                spans.push(Span::styled(keys, theme.dim()));
                spans.push(Span::styled(format!("› {label}  "), theme.muted()));
            }
        }
        Line::from(spans)
    };
    frame.render_widget(Paragraph::new(line), area);
}

#[cfg(test)]
mod tests {
    use ks_core::{update, KeyChord, Msg};
    use ratatui::backend::TestBackend;
    use ratatui::Terminal;

    use super::*;
    use crate::testutil::{dump, listing_state};
    use crate::theme::{ColorDepth, Theme};

    fn theme(name: &str) -> Theme {
        Theme::builtin(name, ColorDepth::TrueColor).unwrap()
    }

    fn draw(state: &AppState, theme: &Theme) -> String {
        let backend = TestBackend::new(state.size.width, state.size.height);
        let mut terminal = Terminal::new(backend).unwrap();
        terminal.draw(|f| crate::render(f, state, theme)).unwrap();
        dump(terminal.backend().buffer())
    }

    fn key(s: &mut AppState, text: &str) {
        update(s, Msg::Key(text.parse::<KeyChord>().unwrap()));
    }

    fn opened(w: u16, h: u16) -> AppState {
        let mut s = listing_state(w, h);
        key(&mut s, "?");
        s
    }

    #[test]
    fn help_snapshots() {
        for name in ["ember", "paper"] {
            for (w, h) in [(80, 24), (160, 48)] {
                insta::assert_snapshot!(
                    format!("help_{name}_{w}x{h}"),
                    draw(&opened(w, h), &theme(name))
                );
            }
        }
    }

    #[test]
    fn scrolled_and_filtered_help_snapshots() {
        for name in ["ember", "paper"] {
            for (w, h) in [(80, 24), (160, 48)] {
                let mut s = opened(w, h);
                key(&mut s, "ctrl-d");
                key(&mut s, "ctrl-d");
                insta::assert_snapshot!(
                    format!("help_scrolled_{name}_{w}x{h}"),
                    draw(&s, &theme(name))
                );

                let mut s = opened(w, h);
                key(&mut s, "/");
                for c in "sort".chars() {
                    update(&mut s, Msg::Key(KeyChord::char(c)));
                }
                insta::assert_snapshot!(
                    format!("help_filtered_{name}_{w}x{h}"),
                    draw(&s, &theme(name))
                );
            }
        }
    }

    #[test]
    fn a_terminal_too_small_for_the_box_says_so() {
        let text = draw(&opened(10, 4), &theme("ember"));
        assert!(
            text.contains("esc to close") || text.contains("Help"),
            "{text}"
        );
    }

    #[test]
    fn help_shows_rebound_keys() {
        let mut s = listing_state(100, 40);
        s.keymap.apply_overrides(
            &[(
                "normal".to_owned(),
                [("T".to_owned(), "file.trash".to_owned())].into(),
            )]
            .into(),
        );
        key(&mut s, "?");
        // Jump to the file operations section.
        key(&mut s, "ctrl-d");
        let text = draw(&s, &theme("ember"));
        assert!(text.contains("d, T"), "{text}");
    }

    #[test]
    fn small_terminals_do_not_panic() {
        for (w, h) in [(0, 0), (10, 4), (30, 8), (44, 12), (200, 6)] {
            let mut s = opened(w, h);
            key(&mut s, "G");
            let _ = draw(&s, &theme("paper"));
        }
    }
}
