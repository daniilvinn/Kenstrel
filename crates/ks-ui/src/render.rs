//! The main screen (PLAN §4.2): header, separator, body, separator, prompt, hint line.
//!
//! `render` is a pure function of the state and the theme.

use ratatui::layout::{Constraint, Layout, Rect};
use ratatui::style::{Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, Paragraph};
use ratatui::Frame;

use ks_core::config::IconSet;
use ks_core::keymap::{Continuation, ContinuationTarget, KeyPattern};
use ks_core::{Action, AppState, KeyContext, Level};

use crate::text::{breadcrumb, truncate_end, width};
use crate::theme::Theme;

/// The hint line is hidden on terminals shorter than this (PLAN §4.2).
pub const HINT_MIN_HEIGHT: u16 = 20;

const PROMPT_GLYPH: &str = "›";
const SEPARATOR_GLYPH: &str = "─";
const UNICODE_SPINNER: &[&str] = &["⠋", "⠙", "⠹", "⠸", "⠼", "⠴", "⠦", "⠧", "⠇", "⠏"];
const ASCII_SPINNER: &[&str] = &["|", "/", "-", "\\"];

/// Draws the whole screen.
pub fn render(frame: &mut Frame, state: &AppState, theme: &Theme) {
    let area = frame.area();
    frame.render_widget(Block::new().style(theme.base_style()), area);

    let show_hint = area.height >= HINT_MIN_HEIGHT;
    let [header, top_rule, _body, bottom_rule, prompt, hint] = Layout::vertical([
        Constraint::Length(1),
        Constraint::Length(1),
        Constraint::Min(0),
        Constraint::Length(1),
        Constraint::Length(1),
        Constraint::Length(u16::from(show_hint)),
    ])
    .areas(area);

    render_header(frame, header, state, theme);
    render_rule(frame, top_rule, theme);
    render_rule(frame, bottom_rule, theme);
    render_prompt(frame, prompt, state, theme);
    if show_hint {
        render_hints(frame, hint, state, theme);
    }
}

fn render_header(frame: &mut Frame, area: Rect, state: &AppState, theme: &Theme) {
    let budget = usize::from(area.width).saturating_sub(2);
    let text = breadcrumb(&state.cwd, state.home.as_deref(), budget);
    let line = match text.rsplit_once('/') {
        // Parent segments are dimmed, the current directory is emphasised.
        Some((parents, last)) if !last.is_empty() => Line::from(vec![
            Span::raw(" "),
            Span::styled(format!("{parents}/"), theme.dim()),
            Span::styled(
                last.to_owned(),
                Style::new().fg(theme.fg).add_modifier(Modifier::BOLD),
            ),
        ]),
        _ => Line::from(vec![Span::raw(" "), Span::styled(text, theme.dim())]),
    };
    frame.render_widget(Paragraph::new(line), area);
}

fn render_rule(frame: &mut Frame, area: Rect, theme: &Theme) {
    let rule = SEPARATOR_GLYPH.repeat(usize::from(area.width));
    frame.render_widget(
        Paragraph::new(Span::styled(rule, Style::new().fg(theme.border))),
        area,
    );
}

fn spinner_glyph(state: &AppState) -> &'static str {
    let frames = if state.config.icons == IconSet::Ascii {
        ASCII_SPINNER
    } else {
        UNICODE_SPINNER
    };
    frames[state.spinner_frame % frames.len()]
}

fn render_prompt(frame: &mut Frame, area: Rect, state: &AppState, theme: &Theme) {
    let left = Line::from(vec![
        Span::raw(" "),
        Span::styled(PROMPT_GLYPH, theme.bold_accent()),
        Span::raw(" "),
    ]);
    let left_width = width(PROMPT_GLYPH) + 2;
    frame.render_widget(Paragraph::new(left), area);

    // Right side: pending keys, then a status message or the job indicator.
    let mut spans: Vec<Span> = Vec::new();
    if !state.pending.is_empty() {
        let pending = state
            .pending
            .iter()
            .map(ToString::to_string)
            .collect::<Vec<_>>()
            .join(" ");
        spans.push(Span::styled(pending, theme.bold_accent()));
    }
    if state.has_running_jobs() {
        if !spans.is_empty() {
            spans.push(Span::raw("  "));
        }
        let count = state.jobs.len();
        let label = if count == 1 { "job" } else { "jobs" };
        spans.push(Span::styled(
            format!("{} {count} {label}", spinner_glyph(state)),
            theme.accent_style(),
        ));
    }
    if let Some(status) = &state.status {
        if !spans.is_empty() {
            spans.push(Span::raw("  "));
        }
        let color = match status.level {
            Level::Info => theme.fg_dim,
            Level::Warning => theme.warning,
            Level::Error => theme.error,
        };
        let used: usize = spans.iter().map(|s| width(&s.content)).sum();
        let budget = usize::from(area.width).saturating_sub(left_width + used + 2);
        spans.push(Span::styled(
            truncate_end(&status.text, budget),
            Style::new().fg(color),
        ));
    }
    if spans.is_empty() {
        return;
    }
    let line = Line::from(spans);
    let line_width = u16::try_from(line.width()).unwrap_or(u16::MAX);
    let x = area
        .right()
        .saturating_sub(line_width.saturating_add(1))
        .max(area.x + u16::try_from(left_width).unwrap_or(0));
    let right = Rect::new(x, area.y, area.right().saturating_sub(x), 1);
    frame.render_widget(Paragraph::new(line), right);
}

/// Hint entries `(action, label)` for the current context.
fn hints_for(ctx: KeyContext) -> &'static [(Action, &'static str)] {
    match ctx {
        KeyContext::Normal => &[(Action::Quit, "quit")],
        _ => &[(Action::Cancel, "cancel")],
    }
}

fn render_hints(frame: &mut Frame, area: Rect, state: &AppState, theme: &Theme) {
    let entries: Vec<(String, String)> = if state.which_key_visible {
        state
            .keymap
            .continuations(state.key_context(), &state.pending)
            .iter()
            .map(which_key_entry)
            .collect()
    } else {
        let ctx = state.key_context();
        hints_for(ctx)
            .iter()
            .filter_map(|(action, label)| {
                let keys = state.keymap.keys_for(ctx, *action);
                keys.first().map(|k| (k.clone(), (*label).to_owned()))
            })
            .collect()
    };

    let budget = usize::from(area.width).saturating_sub(3);
    let mut spans = vec![Span::raw("   ")];
    let mut used = 0;
    for (index, (key, label)) in entries.iter().enumerate() {
        let gap = if index == 0 { 0 } else { 2 };
        let cost = gap + width(key) + width(label) + 3;
        if used + cost > budget {
            spans.push(Span::styled("…", theme.muted()));
            break;
        }
        used += cost;
        if gap > 0 {
            spans.push(Span::raw("  "));
        }
        spans.push(Span::styled("‹", theme.muted()));
        spans.push(Span::styled(key.clone(), theme.dim()));
        spans.push(Span::styled(format!("› {label}"), theme.muted()));
    }
    frame.render_widget(Paragraph::new(Line::from(spans)), area);
}

fn which_key_entry(c: &Continuation) -> (String, String) {
    let key = match c.key {
        KeyPattern::Chord(chord) => chord.to_string(),
        KeyPattern::AnyChar => "any".to_owned(),
    };
    let label = match c.target {
        ContinuationTarget::Action(action) => action.title().to_owned(),
        ContinuationTarget::Group => "…".to_owned(),
    };
    (key, label)
}

#[cfg(test)]
mod tests {
    use std::path::PathBuf;

    use ks_core::{update, Config, KeyChord, Keymap, Msg, Size, StartParams, TimerKind};
    use ratatui::backend::TestBackend;
    use ratatui::Terminal;

    use super::*;
    use crate::testutil::dump;
    use crate::theme::{ColorDepth, Theme};

    fn state(width: u16, height: u16) -> AppState {
        AppState::new(StartParams {
            cwd: PathBuf::from("/home/me/projects/omniforce/src/renderer"),
            home: Some(PathBuf::from("/home/me")),
            config: Config::default(),
            keymap: Keymap::defaults(),
            size: Size::new(width, height),
        })
    }

    fn draw(state: &AppState, theme: &Theme) -> String {
        let backend = TestBackend::new(state.size.width, state.size.height);
        let mut terminal = Terminal::new(backend).unwrap();
        terminal.draw(|f| render(f, state, theme)).unwrap();
        dump(terminal.backend().buffer())
    }

    fn theme(name: &str) -> Theme {
        Theme::builtin(name, ColorDepth::TrueColor).unwrap()
    }

    #[test]
    fn main_screen_snapshots() {
        for name in ["ember", "paper"] {
            for (w, h) in [(80, 24), (160, 48)] {
                let rendered = draw(&state(w, h), &theme(name));
                insta::assert_snapshot!(format!("main_{name}_{w}x{h}"), rendered);
            }
        }
    }

    fn pending_with_status(w: u16, h: u16) -> AppState {
        let mut s = state(w, h);
        update(&mut s, Msg::Key(KeyChord::char('g')));
        update(&mut s, Msg::JobStarted(ks_core::JobId(1)));
        update(
            &mut s,
            Msg::Notify {
                level: Level::Error,
                text: "config.toml: expected a string".into(),
            },
        );
        s
    }

    #[test]
    fn prompt_status_snapshots() {
        for name in ["ember", "paper"] {
            for (w, h) in [(80, 24), (160, 48)] {
                let rendered = draw(&pending_with_status(w, h), &theme(name));
                insta::assert_snapshot!(format!("prompt_status_{name}_{w}x{h}"), rendered);
            }
        }
    }

    #[test]
    fn which_key_snapshots() {
        for name in ["ember", "paper"] {
            for (w, h) in [(80, 24), (160, 48)] {
                let mut s = state(w, h);
                let effects = update(&mut s, Msg::Key(KeyChord::char('g')));
                let ks_core::Effect::StartTimer { gen, .. } = effects[0] else {
                    panic!("expected which-key timer");
                };
                update(
                    &mut s,
                    Msg::Timer {
                        kind: TimerKind::WhichKey,
                        gen,
                    },
                );
                insta::assert_snapshot!(
                    format!("which_key_{name}_{w}x{h}"),
                    draw(&s, &theme(name))
                );
            }
        }
    }

    #[test]
    fn hint_line_is_hidden_on_short_terminals() {
        let short = draw(&state(80, HINT_MIN_HEIGHT - 1), &theme("ember"));
        assert!(!short.contains("quit"), "{short}");
        let tall = draw(&state(80, HINT_MIN_HEIGHT), &theme("ember"));
        assert!(tall.contains("‹q› quit"), "{tall}");
    }

    #[test]
    fn which_key_replaces_the_hint_line_when_visible() {
        let mut s = state(200, 24);
        let effects = update(&mut s, Msg::Key(KeyChord::char('g')));
        let ks_core::Effect::StartTimer { gen, .. } = effects[0] else {
            panic!("expected which-key timer");
        };
        update(
            &mut s,
            Msg::Timer {
                kind: TimerKind::WhichKey,
                gen,
            },
        );
        let out = draw(&s, &theme("ember"));
        assert!(out.contains("‹g› Jump to top"), "{out}");
        assert!(out.contains("‹j› Jobs overlay"), "{out}");
    }

    #[test]
    fn tiny_terminals_do_not_panic() {
        for (w, h) in [(0, 0), (1, 1), (10, 3), (20, 5), (200, 2)] {
            draw(&state(w, h), &theme("paper"));
        }
    }

    #[test]
    fn long_status_is_truncated_not_wrapped() {
        let mut s = state(40, 24);
        update(
            &mut s,
            Msg::Notify {
                level: Level::Warning,
                text: "x".repeat(200),
            },
        );
        let out = draw(&s, &theme("ember"));
        assert!(out.contains('…'));
    }
}
