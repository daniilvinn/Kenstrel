//! The embedded Claude Code pane, its dialogs, the "Claude changes" view and the `◆` marker
//! (PLAN section 10).
//!
//! The layout lives in [`split_body`], deliberately separate from the main screen so that the
//! preview layout of phase P2 only has to split the list area this function returns.

use std::path::Path;

use ratatui::layout::{Alignment, Position, Rect};
use ratatui::style::{Color, Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, BorderType, Borders, Clear, Padding, Paragraph};
use ratatui::Frame;

use ks_core::claude::{self, activity_line, Area, BodySplit, ClaudeColor, ClaudeScreen};
use ks_core::config::IconSet;
use ks_core::{Action, AppState, Entry, KeyContext, Overlay};

use crate::text::{truncate_end, truncate_start, width};
use crate::theme::Theme;

/// The marker for a file Claude edited in this session.
pub const TOUCHED_GLYPH: &str = "◆";
/// The marker for a directory with an edited file somewhere below it.
pub const TOUCHED_DIR_GLYPH: &str = "◇";
const TOUCHED_GLYPH_ASCII: &str = "*";
const TOUCHED_DIR_GLYPH_ASCII: &str = "+";
/// Cells the marker takes in a list row: a space and the glyph.
pub const TOUCHED_WIDTH: usize = 2;

fn rect(area: Area) -> Rect {
    Rect::new(area.x, area.y, area.width, area.height)
}

fn area(rect: Rect) -> Area {
    Area::new(rect.x, rect.y, rect.width, rect.height)
}

/// Divides the body of the main screen: the area left for the list, and the pane if Claude
/// is running. P2's preview layout divides the returned list area further.
pub fn split_body(body: Rect, state: &AppState) -> (Rect, Option<(Rect, BodySplit)>) {
    if !state.claude.is_running() {
        return (body, None);
    }
    let split = claude::split_body(area(body), state.claude.position, state.claude.percent);
    let list = if split.covers {
        Rect::new(body.x, body.y, body.width, 0)
    } else {
        rect(split.list)
    };
    (list, Some((rect(split.pane), split)))
}

/// The glyph to draw after the name of `entry` when Claude touched it (or something below it).
pub fn touched_glyph(entry: &Entry, state: &AppState) -> Option<&'static str> {
    let changes = &state.claude.changes;
    if changes.is_empty() {
        return None;
    }
    let path = state.cwd.join(&entry.name);
    let ascii = state.config.icons == IconSet::Ascii;
    if changes.contains(&path) {
        Some(if ascii {
            TOUCHED_GLYPH_ASCII
        } else {
            TOUCHED_GLYPH
        })
    } else if entry.is_dir && changes.contains_below(&path) {
        Some(if ascii {
            TOUCHED_DIR_GLYPH_ASCII
        } else {
            TOUCHED_DIR_GLYPH
        })
    } else {
        None
    }
}

fn color(c: ClaudeColor, default: Color) -> Color {
    match c {
        ClaudeColor::Default => default,
        ClaudeColor::Indexed(i) => Color::Indexed(i),
        ClaudeColor::Rgb(r, g, b) => Color::Rgb(r, g, b),
    }
}

fn focus_key_label(state: &AppState) -> String {
    state
        .keymap
        .keys_for(KeyContext::Normal, Action::ClaudeFocus)
        .into_iter()
        .next()
        .unwrap_or_else(|| "ctrl-\\".to_owned())
}

/// Draws the pane (border with title and activity feed, then the terminal contents).
pub fn render_pane(
    frame: &mut Frame,
    outer: Rect,
    split: BodySplit,
    state: &AppState,
    theme: &Theme,
) {
    let outer = outer.intersection(frame.area());
    if outer.width < 2 || outer.height < 2 {
        return;
    }
    let focused = state.overlays.last() == Some(&Overlay::Claude);
    let border_color = if focused {
        theme.border_focus
    } else {
        theme.border
    };
    let borders = if split.border_left {
        Borders::TOP | Borders::LEFT
    } else {
        Borders::TOP
    };

    // Left of the border: the name and how to get focus; right: the latest edit.
    let title_style = if focused {
        theme.bold_accent()
    } else {
        theme.dim().add_modifier(Modifier::BOLD)
    };
    let mut left = vec![Span::styled(" Claude ", title_style)];
    if !focused {
        left.push(Span::styled(
            format!("‹{}› focus ", focus_key_label(state)),
            theme.muted(),
        ));
    }
    let left_width: usize = left.iter().map(|s| width(&s.content)).sum();

    let mut block = Block::new()
        .borders(borders)
        .border_type(BorderType::Rounded)
        .border_style(Style::new().fg(border_color))
        .style(theme.base_style())
        .title_top(Line::from(left));
    if let Some(activity) = activity_line(state) {
        let budget = usize::from(outer.width).saturating_sub(left_width + 6);
        if budget >= 6 {
            let text = truncate_start(&activity, budget);
            block = block.title_top(
                Line::from(vec![
                    Span::raw(" "),
                    Span::styled(TOUCHED_GLYPH, theme.accent_style()),
                    Span::styled(format!(" {text} "), theme.dim()),
                ])
                .alignment(Alignment::Right),
            );
        }
    }
    let inner = block.inner(outer);
    frame.render_widget(block, outer);

    match &state.claude.screen {
        Some(screen) => draw_screen(frame, inner, screen, theme),
        None => {
            frame.render_widget(
                Paragraph::new(Line::styled("Starting Claude…", theme.muted())),
                inner,
            );
        }
    }

    if focused {
        if let Some((row, col)) = state.claude.screen.as_ref().and_then(|s| s.cursor) {
            if row < inner.height && col < inner.width {
                frame.set_cursor_position(Position::new(inner.x + col, inner.y + row));
            }
        }
    }
}

fn draw_screen(frame: &mut Frame, inner: Rect, screen: &ClaudeScreen, theme: &Theme) {
    let buf = frame.buffer_mut();
    for row in 0..inner.height.min(screen.rows) {
        for col in 0..inner.width.min(screen.cols) {
            let Some(cell) = screen.cell(row, col) else {
                continue;
            };
            let (mut fg, mut bg) = (color(cell.fg, theme.fg), color(cell.bg, theme.bg));
            if cell.inverse {
                std::mem::swap(&mut fg, &mut bg);
                // A reversed default colour must still be visible on the real background.
                if fg == Color::Reset {
                    fg = Color::Black;
                }
                if bg == Color::Reset {
                    bg = Color::White;
                }
            }
            let mut style = Style::new().fg(fg).bg(bg);
            for (on, modifier) in [
                (cell.bold, Modifier::BOLD),
                (cell.dim, Modifier::DIM),
                (cell.italic, Modifier::ITALIC),
                (cell.underline, Modifier::UNDERLINED),
            ] {
                if on {
                    style = style.add_modifier(modifier);
                }
            }
            if let Some(target) = buf.cell_mut(Position::new(inner.x + col, inner.y + row)) {
                target.reset();
                target.set_style(style);
                // Blank and wide-continuation cells keep a space so stale glyphs are overwritten.
                target.set_symbol(if cell.text.is_empty() {
                    " "
                } else {
                    &cell.text
                });
            }
        }
    }
}

fn centered(area: Rect, wanted_width: u16, wanted_height: u16) -> Rect {
    let width = wanted_width.min(area.width);
    let height = wanted_height.min(area.height);
    Rect::new(
        area.x + (area.width - width) / 2,
        area.y + (area.height - height) / 2,
        width,
        height,
    )
}

/// Draws the Claude changes view when it is the overlay on top (the billing / install / login
/// dialogs are P6's generic confirm overlay, drawn by `fileops::render_overlays`).
pub fn render_overlays(frame: &mut Frame, state: &AppState, theme: &Theme) {
    if state.overlays.last() == Some(&Overlay::ClaudeChanges) {
        render_changes(frame, state, theme);
    }
}

fn display_path(path: &Path, state: &AppState) -> String {
    let root = state.claude.root.as_deref().unwrap_or(&state.cwd);
    let rel = path.strip_prefix(root).unwrap_or(path);
    rel.to_string_lossy().replace('\\', "/")
}

fn render_changes(frame: &mut Frame, state: &AppState, theme: &Theme) {
    let area = frame.area();
    let changes = state.claude.changes.recent();
    let rows = u16::try_from(changes.len()).unwrap_or(u16::MAX);
    let diff_open = state.claude.diff_open;
    let rect = centered(
        area,
        area.width
            .saturating_sub(4)
            .clamp(1, if diff_open { 140 } else { 100 }),
        if diff_open {
            area.height.saturating_sub(2).max(3)
        } else {
            rows.saturating_add(4)
                .min(area.height.saturating_sub(2))
                .max(3)
        },
    );
    frame.render_widget(Clear, rect);
    let block = Block::bordered()
        .border_type(BorderType::Rounded)
        .border_style(Style::new().fg(theme.border_focus))
        .title(Span::styled(" Claude changes ", theme.bold_accent()))
        .padding(Padding::horizontal(1))
        .style(theme.base_style());
    let inner = block.inner(rect);
    frame.render_widget(block, rect);
    if inner.height < 2 || inner.width < 4 {
        return;
    }
    // With the diff shown the list keeps a third of the height (at least 3 rows).
    let list_height = if diff_open {
        (usize::from(inner.height - 1) / 3)
            .max(3)
            .min(changes.len().max(1))
    } else {
        usize::from(inner.height - 1)
    };
    let cursor = state
        .claude
        .changes_cursor
        .min(changes.len().saturating_sub(1));
    // Keep the cursor row visible.
    let first = cursor.saturating_sub(list_height.saturating_sub(1));
    let tool_width = changes.iter().map(|c| width(&c.tool)).max().unwrap_or(0);
    let path_budget = usize::from(inner.width).saturating_sub(6 + tool_width);

    let mut lines: Vec<Line> = changes
        .iter()
        .enumerate()
        .skip(first)
        .take(list_height)
        .map(|(index, change)| {
            let selected = index == cursor;
            let row_bg = if selected {
                Style::new().bg(theme.selection_bg)
            } else {
                Style::new()
            };
            let pad = tool_width.saturating_sub(width(&change.tool));
            Line::from(vec![
                Span::styled(
                    if selected { "▌" } else { " " },
                    theme.accent_style().patch(row_bg),
                ),
                Span::styled(
                    format!("{TOUCHED_GLYPH} "),
                    theme.accent_style().patch(row_bg),
                ),
                Span::styled(
                    format!("{}{} ", change.tool, " ".repeat(pad)),
                    theme.dim().patch(row_bg),
                ),
                Span::styled(
                    truncate_start(&display_path(&change.path, state), path_budget),
                    Style::new().fg(theme.fg).patch(row_bg),
                ),
            ])
        })
        .collect();
    lines.truncate(list_height);
    let count = changes.len();
    let noun = if count == 1 { "file" } else { "files" };
    let footer = format!(
        " {count} {noun} · enter reveal · d {} diff · X clear · esc close",
        if diff_open { "hide" } else { "show" }
    );
    let mut all = lines;
    if diff_open {
        let total = usize::from(inner.height - 1);
        all.push(Line::styled(
            "─".repeat(usize::from(inner.width)),
            Style::new().fg(theme.border),
        ));
        let room = total.saturating_sub(all.len());
        all.extend(diff_lines(state, theme, room, usize::from(inner.width)));
    }
    // Pad so the footer sits on the last row.
    while all.len() < usize::from(inner.height - 1) {
        all.push(Line::default());
    }
    all.push(Line::styled(
        truncate_end(&footer, usize::from(inner.width)),
        theme.muted(),
    ));
    frame.render_widget(Paragraph::new(all), inner);
}

/// The diff of the selected change as styled lines (at most `room`): additions and deletions
/// carry `+` / `-` themselves, so the colour is a second cue, not the only one.
fn diff_lines(
    state: &AppState,
    theme: &Theme,
    room: usize,
    width_cells: usize,
) -> Vec<Line<'static>> {
    // Only ever the diff of the highlighted file; anything else is stale.
    let selected = ks_core::claude::selected_change(state);
    let Some(diff) = state
        .claude
        .diff
        .as_ref()
        .filter(|d| selected.as_deref() == Some(d.path.as_path()))
    else {
        return vec![Line::styled("Loading diff…", theme.muted())];
    };
    diff.text
        .lines()
        .take(room)
        .map(|line| {
            let style = if line.starts_with("+++") || line.starts_with("---") {
                theme.dim()
            } else if line.starts_with('+') {
                Style::new().fg(theme.git_added)
            } else if line.starts_with('-') {
                Style::new().fg(theme.error)
            } else if line.starts_with("@@") {
                theme.accent_style()
            } else {
                Style::new().fg(theme.fg)
            };
            Line::styled(
                truncate_end(&line.replace('\t', "    "), width_cells),
                style,
            )
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use ks_core::claude::{ClaudeCell, ClaudePhase, ClaudePreflight, BILLING_WARNING};
    use ks_core::config::PanePosition;
    use ks_core::{update, Effect, KeyChord, Msg, Size};
    use ratatui::backend::TestBackend;
    use ratatui::Terminal;

    use super::*;
    use crate::testutil::{dump, listing_state};
    use crate::theme::ColorDepth;

    const THEMES: [&str; 2] = ["ember", "paper"];
    const SIZES: [(u16, u16); 2] = [(80, 24), (160, 48)];

    fn theme(name: &str) -> Theme {
        Theme::builtin(name, ColorDepth::TrueColor).unwrap()
    }

    fn buffer(state: &AppState, theme: &Theme) -> ratatui::buffer::Buffer {
        let mut terminal =
            Terminal::new(TestBackend::new(state.size.width, state.size.height)).unwrap();
        terminal.draw(|f| crate::render(f, state, theme)).unwrap();
        terminal.backend().buffer().clone()
    }

    fn draw(state: &AppState, theme: &Theme) -> String {
        dump(&buffer(state, theme))
    }

    fn row_text(buf: &ratatui::buffer::Buffer, y: u16) -> String {
        (0..buf.area.width).map(|x| buf[(x, y)].symbol()).collect()
    }

    /// A fake Claude screen: a welcome box, a prompt line with a coloured word, an inverse cell.
    fn fake_screen(rows: u16, cols: u16) -> ClaudeScreen {
        let mut cells = vec![ClaudeCell::default(); usize::from(rows) * usize::from(cols)];
        let mut put = |row: u16, col: u16, text: &str, f: &dyn Fn(&mut ClaudeCell)| {
            for (i, ch) in text.chars().enumerate() {
                let idx = usize::from(row) * usize::from(cols) + usize::from(col) + i;
                if idx < cells.len() {
                    cells[idx].text = ch.to_string();
                    f(&mut cells[idx]);
                }
            }
        };
        put(0, 1, "* Welcome to Claude Code", &|c| {
            c.fg = ClaudeColor::Rgb(217, 119, 87);
            c.bold = true;
        });
        put(1, 1, "cwd: /home/me/projects/omniforce", &|c| c.dim = true);
        put(2, 1, "> refactor the renderer", &|_| {});
        put(2, 3, "refactor", &|c| c.fg = ClaudeColor::Indexed(6));
        put(3, 1, "[inverse]", &|c| c.inverse = true);
        ClaudeScreen {
            cols,
            rows,
            cells,
            cursor: Some((2, 25)),
        }
    }

    fn with_pane(w: u16, h: u16, position: PanePosition) -> AppState {
        let mut s = listing_state(w, h);
        s.claude.position = position;
        // Drive the real flow so the layout, focus and sizes are the ones the app uses.
        update(&mut s, Msg::Key("c".parse::<KeyChord>().unwrap()));
        let generation = s.claude.generation;
        update(
            &mut s,
            Msg::ClaudePreflight {
                generation,
                result: ClaudePreflight::Ok,
            },
        );
        update(&mut s, Msg::ClaudeStarted { generation });
        let inner = claude::pane_inner_size(&s).unwrap();
        update(
            &mut s,
            Msg::ClaudeScreen {
                generation,
                screen: Box::new(fake_screen(inner.height, inner.width)),
            },
        );
        s
    }

    fn edit(s: &mut AppState, tool: &str, rel: &str) {
        let path = s.cwd.join(rel);
        s.claude.changes.record(path, tool);
    }

    #[test]
    fn pane_snapshots_bottom_and_right() {
        for name in THEMES {
            for (w, h) in SIZES {
                for (position, label) in [
                    (PanePosition::Bottom, "bottom"),
                    (PanePosition::Right, "right"),
                ] {
                    let mut s = with_pane(w, h, position);
                    edit(&mut s, "Edit", "vulkan_device.rs");
                    let rendered = draw(&s, &theme(name));
                    insta::assert_snapshot!(format!("pane_{label}_{name}_{w}x{h}"), rendered);
                }
            }
        }
    }

    #[test]
    fn pane_focused_with_a_long_activity_path_snapshots() {
        for name in THEMES {
            for (w, h) in SIZES {
                let mut s = with_pane(w, h, PanePosition::Bottom);
                edit(
                    &mut s,
                    "MultiEdit",
                    "renderer/passes/very_long_directory_name/deferred_lighting_pass_v2.rs",
                );
                let rendered = draw(&s, &theme(name));
                insta::assert_snapshot!(
                    format!("pane_focused_with_activity_{name}_{w}x{h}"),
                    rendered
                );
            }
        }
    }

    #[test]
    fn dialog_key_hint_survives_a_short_terminal() {
        let mut s = listing_state(80, 10);
        update(&mut s, Msg::Key("c".parse::<KeyChord>().unwrap()));
        let generation = s.claude.generation;
        update(
            &mut s,
            Msg::ClaudePreflight {
                generation,
                result: ClaudePreflight::NotInstalled(
                    "Claude Code was not found on PATH. Install it with:
  irm https://claude.ai/install.ps1 | iex
Then run claude once and log in with /login. More text follows to make the body long enough to be clipped on a tiny terminal."
                        .into(),
                ),
            },
        );
        let text = draw(&s, &theme("ember"));
        assert!(
            text.contains("enter/esc") && text.contains("close"),
            "{text}"
        );
        let s = billing_state(80, 8);
        let text = draw(&s, &theme("ember"));
        assert!(text.contains("proceed anyway"), "{text}");
    }

    #[test]
    fn unfocused_pane_shows_the_focus_hint_and_focused_does_not() {
        let mut s = with_pane(120, 40, PanePosition::Bottom);
        let focused = buffer(&s, &theme("ember"));
        let split = claude::current_split(&s).unwrap();
        let border_row = row_text(&focused, split.pane.y);
        assert!(!border_row.contains("focus"), "{border_row}");
        update(&mut s, Msg::Key("ctrl-\\".parse::<KeyChord>().unwrap()));
        let unfocused = buffer(&s, &theme("ember"));
        let border_row = row_text(&unfocused, split.pane.y);
        assert!(border_row.contains("‹ctrl-\\› focus"), "{border_row}");
    }

    #[test]
    fn focused_pane_uses_the_focus_border_colour_and_a_hardware_cursor() {
        let s = with_pane(100, 30, PanePosition::Bottom);
        let t = theme("ember");
        let buf = buffer(&s, &t);
        let split = claude::current_split(&s).unwrap();
        assert_eq!(buf[(split.pane.x + 20, split.pane.y)].fg, t.border_focus);
        // Colours from the screen survive: the Rgb word, the indexed word, the inverse cell.
        let y = split.pane.y + 1;
        assert_eq!(buf[(split.pane.x + 2, y)].fg, Color::Rgb(217, 119, 87));
        assert_eq!(buf[(split.pane.x + 4, y + 2)].fg, Color::Indexed(6));
        assert_eq!(buf[(split.pane.x + 2, y + 3)].bg, t.fg);
        assert!(row_text(&buf, y).contains("Welcome to Claude Code"));
    }

    #[test]
    fn list_keeps_its_rows_above_the_bottom_pane() {
        let s = with_pane(80, 24, PanePosition::Bottom);
        let text = draw(&s, &theme("ember"));
        // The list's first entries are still visible above the pane.
        assert!(text.contains("shaders/"), "{text}");
        assert!(text.contains("Claude"), "{text}");
    }

    #[test]
    fn nothing_is_drawn_without_a_running_pane() {
        let mut s = listing_state(100, 30);
        s.claude.screen = Some(fake_screen(5, 5));
        let text = draw(&s, &theme("ember"));
        assert!(!text.contains("Welcome"));
        assert!(!text.contains("Claude"));
    }

    #[test]
    fn a_starting_pane_shows_a_placeholder() {
        let mut s = listing_state(100, 30);
        s.claude.phase = ClaudePhase::Running;
        let text = draw(&s, &theme("ember"));
        assert!(text.contains("Starting Claude…"), "{text}");
    }

    // ----- dialogs -----

    fn billing_state(w: u16, h: u16) -> AppState {
        let mut s = listing_state(w, h);
        update(&mut s, Msg::Key("c".parse::<KeyChord>().unwrap()));
        let generation = s.claude.generation;
        update(
            &mut s,
            Msg::ClaudePreflight {
                generation,
                result: ClaudePreflight::BillingWarning {
                    method: "api_key".into(),
                    provider: "firstParty".into(),
                },
            },
        );
        s
    }

    #[test]
    fn billing_warning_snapshots() {
        for name in THEMES {
            for (w, h) in SIZES {
                let s = billing_state(w, h);
                let rendered = draw(&s, &theme(name));
                assert!(rendered.contains(BILLING_WARNING), "{rendered}");
                insta::assert_snapshot!(format!("billing_{name}_{w}x{h}"), rendered);
            }
        }
    }

    #[test]
    fn install_and_login_dialog_snapshots() {
        for (label, result) in [
            (
                "install",
                ClaudePreflight::NotInstalled(
                    "Claude Code was not found on PATH. Install it with:\n  irm https://claude.ai/install.ps1 | iex".into(),
                ),
            ),
            ("login", ClaudePreflight::NotLoggedIn),
        ] {
            for name in THEMES {
                for (w, h) in SIZES {
                    let mut s = listing_state(w, h);
                    update(&mut s, Msg::Key("c".parse::<KeyChord>().unwrap()));
                    let generation = s.claude.generation;
                    update(
                        &mut s,
                        Msg::ClaudePreflight {
                            generation,
                            result: result.clone(),
                        },
                    );
                    let rendered = draw(&s, &theme(name));
                    insta::assert_snapshot!(format!("dialog_{label}_{name}_{w}x{h}"), rendered);
                }
            }
        }
    }

    // ----- the ◆ marker -----

    #[test]
    fn touched_files_and_their_directories_get_a_glyph_beyond_colour() {
        let mut s = listing_state(100, 30);
        s.config.icons = IconSet::Unicode;
        edit(&mut s, "Write", "new_pass.rs");
        edit(&mut s, "Edit", "passes/pass_a.rs");
        let buf = buffer(&s, &theme("ember"));
        let all: Vec<String> = (0..30).map(|y| row_text(&buf, y)).collect();
        let line_of = |needle: &str| all.iter().find(|l| l.contains(needle)).unwrap().clone();
        assert!(line_of("new_pass.rs").contains(TOUCHED_GLYPH));
        assert!(line_of("passes/").contains(TOUCHED_DIR_GLYPH));
        assert!(!line_of("mod.rs").contains(TOUCHED_GLYPH));
        assert!(!line_of("shaders/").contains(TOUCHED_DIR_GLYPH));
        // The glyph is accent coloured, never the only cue: the row still carries the name.
        let y = all.iter().position(|l| l.contains("new_pass.rs")).unwrap() as u16;
        let x = (0..100u16)
            .find(|&x| buf[(x, y)].symbol() == TOUCHED_GLYPH)
            .unwrap();
        assert_eq!(buf[(x, y)].fg, theme("ember").accent);
    }

    #[test]
    fn ascii_icon_sets_use_ascii_markers() {
        let mut s = listing_state(100, 30);
        s.config.icons = IconSet::Ascii;
        edit(&mut s, "Write", "new_pass.rs");
        edit(&mut s, "Edit", "passes/pass_a.rs");
        let buf = buffer(&s, &theme("ember"));
        let all: Vec<String> = (0..30).map(|y| row_text(&buf, y)).collect();
        assert!(all.iter().all(|l| !l.contains(TOUCHED_GLYPH)));
        assert!(all
            .iter()
            .any(|l| l.contains("new_pass.rs") && l.contains('*')));
    }

    #[test]
    fn touched_list_snapshots() {
        for name in THEMES {
            for (w, h) in SIZES {
                let mut s = listing_state(w, h);
                s.config.icons = IconSet::Unicode;
                edit(&mut s, "Write", "new_pass.rs");
                edit(&mut s, "Edit", "vulkan_device.rs");
                edit(&mut s, "MultiEdit", "passes/pass_a.rs");
                let rendered = draw(&s, &theme(name));
                insta::assert_snapshot!(format!("touched_{name}_{w}x{h}"), rendered);
            }
        }
    }

    // ----- the changes view -----

    #[test]
    fn changes_view_snapshots() {
        for name in THEMES {
            for (w, h) in SIZES {
                let mut s = listing_state(w, h);
                edit(&mut s, "Edit", "vulkan_device.rs");
                edit(&mut s, "Write", "new_pass.rs");
                edit(&mut s, "MultiEdit", "passes/pass_a.rs");
                update(&mut s, Msg::Key("g".parse::<KeyChord>().unwrap()));
                update(&mut s, Msg::Key("a".parse::<KeyChord>().unwrap()));
                update(&mut s, Msg::Key("j".parse::<KeyChord>().unwrap()));
                let rendered = draw(&s, &theme(name));
                assert!(rendered.contains("Claude changes"), "{rendered}");
                insta::assert_snapshot!(format!("changes_{name}_{w}x{h}"), rendered);
            }
        }
    }

    #[test]
    fn changes_view_with_diff_snapshots() {
        let text = "diff --git a/vulkan_device.rs b/vulkan_device.rs\n--- a/vulkan_device.rs\n+++ b/vulkan_device.rs\n@@ -1,3 +1,3 @@\n fn main() {\n-    old();\n+    new();\n }";
        for name in THEMES {
            for (w, h) in SIZES {
                let mut s = listing_state(w, h);
                edit(&mut s, "Edit", "vulkan_device.rs");
                edit(&mut s, "Write", "new_pass.rs");
                update(&mut s, Msg::Key("g".parse::<KeyChord>().unwrap()));
                update(&mut s, Msg::Key("a".parse::<KeyChord>().unwrap()));
                let effects = update(&mut s, Msg::Key("d".parse::<KeyChord>().unwrap()));
                let Some(Effect::ClaudeDiff { path }) = effects
                    .iter()
                    .find(|e| matches!(e, Effect::ClaudeDiff { .. }))
                else {
                    panic!("no diff request: {effects:?}");
                };
                // Still loading.
                assert!(draw(&s, &theme(name)).contains("Loading diff"));
                let path = path.clone();
                update(
                    &mut s,
                    Msg::ClaudeDiff {
                        path,
                        text: text.to_owned(),
                    },
                );
                let rendered = draw(&s, &theme(name));
                assert!(rendered.contains("+    new();"), "{rendered}");
                insta::assert_snapshot!(format!("changes_diff_{name}_{w}x{h}"), rendered);
            }
        }
    }

    #[test]
    fn a_diff_for_another_file_is_never_drawn() {
        for (name, (w, h)) in THEMES
            .into_iter()
            .flat_map(|t| [(t, (160, 48)), (t, (80, 24))])
        {
            let mut s = listing_state(w, h);
            edit(&mut s, "Edit", "vulkan_device.rs");
            edit(&mut s, "Write", "new_pass.rs");
            update(&mut s, Msg::Key("g".parse::<KeyChord>().unwrap()));
            update(&mut s, Msg::Key("a".parse::<KeyChord>().unwrap()));
            let effects = update(&mut s, Msg::Key("d".parse::<KeyChord>().unwrap()));
            let Some(Effect::ClaudeDiff { path }) = effects
                .iter()
                .find(|e| matches!(e, Effect::ClaudeDiff { .. }))
            else {
                panic!("no diff request: {effects:?}");
            };
            let path = path.clone();
            update(
                &mut s,
                Msg::ClaudeDiff {
                    path,
                    text: "+only_for_the_first_file();".to_owned(),
                },
            );
            if w >= 160 {
                assert!(draw(&s, &theme(name)).contains("only_for_the_first_file"));
            }
            // Highlight the other file: its diff is not loaded, so the first one must not show.
            update(&mut s, Msg::Key("j".parse::<KeyChord>().unwrap()));
            let rendered = draw(&s, &theme(name));
            assert!(!rendered.contains("only_for_the_first_file"), "{rendered}");
            insta::assert_snapshot!(format!("diff_guard_{name}_{w}x{h}"), rendered);
        }
    }

    #[test]
    fn tiny_terminals_do_not_panic() {
        for (w, h) in [(0, 0), (3, 3), (30, 8), (200, 5), (45, 12), (70, 9)] {
            for position in [PanePosition::Bottom, PanePosition::Right] {
                let mut s = listing_state(w.max(1), h.max(1));
                s.size = Size::new(w, h);
                s.claude.phase = ClaudePhase::Running;
                s.claude.position = position;
                s.claude.screen = Some(fake_screen(10, 10));
                edit(&mut s, "Edit", "a.rs");
                if w > 0 && h > 0 {
                    buffer(&s, &theme("ember"));
                }
                s.claude.phase = ClaudePhase::Checking;
                let generation = s.claude.generation;
                update(
                    &mut s,
                    Msg::ClaudePreflight {
                        generation,
                        result: ClaudePreflight::BillingWarning {
                            method: "x".repeat(300),
                            provider: "y".into(),
                        },
                    },
                );
                assert_eq!(s.overlays.last(), Some(&Overlay::Confirm));
                if w > 0 && h > 0 {
                    buffer(&s, &theme("paper"));
                }
                s.overlays.pop();
                s.fileops.confirm = None;
                s.overlays.push(Overlay::ClaudeChanges);
                if w > 0 && h > 0 {
                    buffer(&s, &theme("paper"));
                }
            }
        }
    }

    #[test]
    fn effects_do_not_leak_into_rendering() {
        // Rendering never needs the effects; this guards the test helper above.
        let mut s = listing_state(80, 24);
        let effects = update(&mut s, Msg::Key("c".parse::<KeyChord>().unwrap()));
        assert!(effects
            .iter()
            .any(|e| matches!(e, Effect::ClaudePreflight { .. })));
    }
}
