//! The which-key popup: the continuations of a pending key prefix, bottom-right, shown after
//! a short delay (`ui.which_key_delay_ms`).

use ratatui::layout::Rect;
use ratatui::style::{Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::Paragraph;
use ratatui::Frame;

use ks_core::keymap::{Continuation, ContinuationTarget, KeyPattern};
use ks_core::AppState;

use crate::popup::framed;
use crate::text::{truncate_end, width};
use crate::theme::Theme;

/// Smallest body area (rows) in which the popup is attempted: a border pair plus one row.
pub const MIN_AREA_HEIGHT: u16 = 3;
/// Gap between columns when the entries do not fit in one.
const COLUMN_GAP: usize = 3;

pub fn key_label(key: KeyPattern) -> String {
    match key {
        KeyPattern::Chord(chord) => chord.to_string(),
        KeyPattern::AnyChar => "any key".to_owned(),
    }
}

fn entries(state: &AppState) -> Vec<(String, String, bool)> {
    state
        .keymap
        .continuations(state.key_context(), &state.pending)
        .iter()
        .map(|c: &Continuation| match c.target {
            ContinuationTarget::Action(action) => {
                (key_label(c.key), action.title().to_owned(), false)
            }
            ContinuationTarget::Group => (key_label(c.key), "more…".to_owned(), true),
        })
        .collect()
}

/// Draws the popup inside `area` (the list body). Returns false when there is no room, so
/// the caller can fall back to the hint line.
pub fn render(frame: &mut Frame, area: Rect, state: &AppState, theme: &Theme) -> bool {
    if !state.which_key_visible || state.pending.is_empty() {
        return true;
    }
    if area.height < MIN_AREA_HEIGHT || area.width < 14 {
        return false;
    }
    let entries = entries(state);
    if entries.is_empty() {
        return true;
    }
    let max_rows = usize::from(area.height - 2);
    let columns = entries.len().div_ceil(max_rows).max(1);
    let per_column = entries.len().div_ceil(columns);

    let key_w = entries.iter().map(|(k, _, _)| width(k)).max().unwrap_or(1);
    let title_w = entries.iter().map(|(_, t, _)| width(t)).max().unwrap_or(1);
    let column_w = key_w + 2 + title_w;
    let inner_w = column_w * columns + COLUMN_GAP * (columns - 1) + 2;
    let max_inner = usize::from(area.width.saturating_sub(2));
    let inner_w = inner_w.min(max_inner);
    let rows = per_column.min(max_rows);

    let w = u16::try_from(inner_w + 2).unwrap_or(area.width);
    let h = u16::try_from(rows + 2).unwrap_or(area.height);
    let rect = Rect::new(
        area.right().saturating_sub(w + 1).max(area.x),
        area.bottom() - h,
        w,
        h,
    );
    let prefix = state
        .pending
        .iter()
        .map(ToString::to_string)
        .collect::<Vec<_>>()
        .join(" ");
    let inner = framed(frame, rect, &prefix, None, false, theme);

    let budget = usize::from(inner.width).saturating_sub(2);
    for (column, chunk) in entries.chunks(per_column).enumerate() {
        let x_off = 1 + column * (column_w + COLUMN_GAP);
        for (row, (key, title, group)) in chunk.iter().enumerate().take(rows) {
            let avail = budget.saturating_sub(x_off - 1).min(column_w);
            if avail <= key_w + 2 {
                continue;
            }
            let title = truncate_end(title, avail - key_w - 2);
            let title_style = if *group {
                theme.muted()
            } else {
                Style::new().fg(theme.fg_dim)
            };
            let line = Line::from(vec![
                Span::styled(
                    format!("{key:<key_w$}"),
                    theme.accent_style().add_modifier(Modifier::BOLD),
                ),
                Span::raw("  "),
                Span::styled(title, title_style),
            ]);
            let x = inner.x + u16::try_from(x_off).unwrap_or(0);
            let y = inner.y + u16::try_from(row).unwrap_or(0);
            if x < inner.right() && y < inner.bottom() {
                frame.render_widget(Paragraph::new(line), Rect::new(x, y, inner.right() - x, 1));
            }
        }
    }
    true
}

#[cfg(test)]
mod tests {
    use ks_core::{update, KeyChord, Msg, TimerKind};
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

    /// Presses `keys`, then fires the which-key timer like the event loop would.
    fn pending(w: u16, h: u16, keys: &[char]) -> AppState {
        let mut s = listing_state(w, h);
        let mut gen = None;
        for &c in keys {
            let effects = update(&mut s, Msg::Key(KeyChord::char(c)));
            for e in effects {
                if let ks_core::Effect::StartTimer {
                    kind: TimerKind::WhichKey,
                    gen: g,
                    ..
                } = e
                {
                    gen = Some(g);
                }
            }
        }
        update(
            &mut s,
            Msg::Timer {
                kind: TimerKind::WhichKey,
                gen: gen.unwrap(),
            },
        );
        assert!(s.which_key_visible);
        s
    }

    #[test]
    fn which_key_snapshots() {
        for name in ["ember", "paper"] {
            for (w, h) in [(80, 24), (160, 48)] {
                for (label, keys) in [("g", vec!['g']), ("sort", vec!['S']), ("mark", vec!['m'])] {
                    insta::assert_snapshot!(
                        format!("which_key_{label}_{name}_{w}x{h}"),
                        draw(&pending(w, h, &keys), &theme(name))
                    );
                }
            }
        }
    }

    #[test]
    fn short_terminals_still_show_the_popup() {
        for h in [8u16, 10, 12, 15, 19] {
            let s = pending(80, h, &['g']);
            let text = draw(&s, &theme("ember"));
            assert!(text.contains("Go to home directory"), "height {h}:\n{text}");
        }
        for name in ["ember", "paper"] {
            insta::assert_snapshot!(
                format!("which_key_g_{name}_80x12"),
                draw(&pending(80, 12, &['g']), &theme(name))
            );
        }
    }

    #[test]
    fn very_small_areas_fall_back_without_panicking() {
        for (w, h) in [(0, 0), (20, 3), (20, 5), (13, 10), (80, 6)] {
            let s = pending(w, h, &['g']);
            let _ = draw(&s, &theme("paper"));
        }
    }

    #[test]
    fn nothing_is_drawn_before_the_timer_fires() {
        let mut s = listing_state(80, 24);
        update(&mut s, Msg::Key(KeyChord::char('g')));
        assert!(!draw(&s, &theme("ember")).contains("Go to home directory"));
    }

    #[test]
    fn rebinding_changes_the_popup() {
        let mut s = listing_state(80, 24);
        s.keymap.apply_overrides(
            &[(
                "normal".to_owned(),
                [("g x".to_owned(), "go.drives".to_owned())].into(),
            )]
            .into(),
        );
        let mut s = {
            let effects = update(&mut s, Msg::Key(KeyChord::char('g')));
            let Some(ks_core::Effect::StartTimer { gen, .. }) = effects.into_iter().next() else {
                panic!("expected a timer");
            };
            update(
                &mut s,
                Msg::Timer {
                    kind: TimerKind::WhichKey,
                    gen,
                },
            );
            s
        };
        s.dirty = true;
        let text = draw(&s, &theme("ember"));
        assert!(text.contains("x  Go to This PC"), "{text}");
    }
}
