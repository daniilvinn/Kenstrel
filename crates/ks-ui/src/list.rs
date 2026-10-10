//! The file list and the drives view. Only the rows inside the viewport are built, so a
//! directory with 100k entries costs the same to draw as one with 20.

use ratatui::layout::Rect;
use ratatui::style::{Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::Paragraph;
use ratatui::Frame;

use ks_core::config::IconSet;
use ks_core::layout::SIZES_MIN_WIDTH;
use ks_core::{AppState, DriveInfo, DriveKind, Entry, GitState};

use crate::format::{human_size, relative_time};
use crate::icons::icon_for;
use crate::text::{truncate_end, truncate_middle, width};
use crate::theme::Theme;

const CURSOR_GLYPH: &str = "▌";
const MARK_GLYPH: &str = "●";
/// Cells before the icon: left margin, cursor bar, mark.
const LEAD_WIDTH: usize = 3;
/// Icon and the space after it.
const ICON_WIDTH: usize = 2;
/// Space and the git glyph, then a right margin.
const GIT_WIDTH: usize = 2;
const MARGIN_RIGHT: usize = 1;
const SIZE_WIDTH: usize = 9;
const TIME_WIDTH: usize = 10;
/// The modified-time column needs a roomy list.
const TIME_MIN_WIDTH: u16 = 90;
/// Names are never squeezed below this; other columns give way first.
const MIN_NAME_WIDTH: usize = 12;

/// Which optional columns fit.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct Columns {
    size: bool,
    time: bool,
}

impl Columns {
    fn for_width(width: u16) -> Self {
        Self {
            size: width >= SIZES_MIN_WIDTH,
            time: width >= TIME_MIN_WIDTH,
        }
    }

    fn fixed_width(self) -> usize {
        LEAD_WIDTH
            + ICON_WIDTH
            + GIT_WIDTH
            + MARGIN_RIGHT
            + if self.size { SIZE_WIDTH + 1 } else { 0 }
            + if self.time { TIME_WIDTH + 1 } else { 0 }
    }
}

pub fn render_list(frame: &mut Frame, area: Rect, state: &AppState, theme: &Theme) {
    if area.is_empty() {
        return;
    }
    if state.show_drives {
        render_drives(frame, area, state, theme);
        return;
    }
    let view = &state.view;
    if view.is_empty() {
        render_placeholder(frame, area, state, theme);
        return;
    }

    let columns = Columns::for_width(area.width);
    let name_width = usize::from(area.width)
        .saturating_sub(columns.fixed_width())
        .max(MIN_NAME_WIDTH.min(usize::from(area.width)));
    let first = view.cursor.offset;
    let rows = usize::from(area.height);
    let lines: Vec<Line> = (first..first.saturating_add(rows).min(view.len()))
        .filter_map(|index| {
            let entry = view.row(index)?;
            Some(entry_line(
                entry,
                RowContext {
                    state,
                    theme,
                    columns,
                    name_width,
                    cursor: index == view.cursor.index,
                    marked: view.is_marked(entry),
                },
            ))
        })
        .collect();
    frame.render_widget(Paragraph::new(lines), area);
}

struct RowContext<'a> {
    state: &'a AppState,
    theme: &'a Theme,
    columns: Columns,
    name_width: usize,
    cursor: bool,
    marked: bool,
}

fn entry_line(entry: &Entry, ctx: RowContext<'_>) -> Line<'static> {
    let RowContext {
        state,
        theme,
        columns,
        name_width,
        cursor,
        marked,
    } = ctx;
    let row_bg = if cursor {
        Style::new().bg(theme.selection_bg)
    } else {
        Style::new()
    };
    let span = |text: String, style: Style| Span::styled(text, style.patch(row_bg));

    let mut spans: Vec<Span> = Vec::with_capacity(9);
    spans.push(span(" ".into(), Style::new()));
    spans.push(span(
        if cursor { CURSOR_GLYPH } else { " " }.into(),
        theme.accent_style(),
    ));
    spans.push(span(
        if marked { MARK_GLYPH } else { " " }.into(),
        theme.accent_style(),
    ));

    let name_style = name_style(entry, marked, theme);
    spans.push(span(
        format!("{} ", icon_for(entry, state.config.icons)),
        icon_style(entry, theme),
    ));

    let mut name = crate::text::sanitize(&entry.display_name());
    if entry.is_dir {
        name.push('/');
    }
    // A file Claude edited gets a glyph at the end of the name column (PLAN 10e).
    let touched = crate::claude::touched_glyph(entry, state);
    let name_width = if touched.is_some() {
        name_width.saturating_sub(crate::claude::TOUCHED_WIDTH)
    } else {
        name_width
    };
    let name = truncate_middle(&name, name_width, !entry.is_dir);
    let padding = name_width.saturating_sub(width(&name));
    spans.push(span(name, name_style));
    spans.push(span(" ".repeat(padding), Style::new()));
    if let Some(glyph) = touched {
        spans.push(span(format!(" {glyph}"), theme.accent_style()));
    }

    if columns.size {
        spans.push(span(" ".into(), Style::new()));
        spans.push(span(
            format!("{:>SIZE_WIDTH$}", size_text(entry, state)),
            theme.dim(),
        ));
    }
    if columns.time {
        let text = entry.mtime.map_or_else(String::new, |t| {
            relative_time(t, state.now, state.utc_offset_secs)
        });
        spans.push(span(" ".into(), Style::new()));
        spans.push(span(format!("{text:>TIME_WIDTH$}"), theme.muted()));
    }

    let git = state
        .git
        .as_ref()
        .and_then(|snapshot| snapshot.state_of(&entry.display_name()));
    spans.push(span(" ".into(), Style::new()));
    spans.push(match git {
        Some(git) => span(git.glyph().to_string(), git_style(git, theme)),
        None => span(" ".into(), Style::new()),
    });
    spans.push(span(" ".repeat(MARGIN_RIGHT), Style::new()));
    Line::from(spans)
}

/// Directories show their number of children (once known), files their size.
fn size_text(entry: &Entry, state: &AppState) -> String {
    if entry.is_dir {
        entry.children.map_or_else(String::new, |n| n.to_string())
    } else {
        human_size(entry.size, state.config.files.size_base)
    }
}

fn name_style(entry: &Entry, marked: bool, theme: &Theme) -> Style {
    if marked {
        return theme.bold_accent();
    }
    if entry.is_cloud_placeholder() {
        return theme.dim().add_modifier(Modifier::ITALIC);
    }
    if entry.is_hidden() {
        return Style::new().fg(theme.hidden).add_modifier(Modifier::DIM);
    }
    if entry.is_link() {
        return Style::new().fg(theme.symlink);
    }
    if entry.is_dir {
        return Style::new().fg(theme.dir);
    }
    if entry.is_exec() {
        return Style::new().fg(theme.exec);
    }
    Style::new().fg(theme.fg)
}

fn icon_style(entry: &Entry, theme: &Theme) -> Style {
    if entry.is_dir {
        Style::new().fg(theme.dir)
    } else if entry.is_link() {
        Style::new().fg(theme.symlink)
    } else if entry.is_exec() {
        Style::new().fg(theme.exec)
    } else {
        theme.dim()
    }
}

pub fn git_style(state: GitState, theme: &Theme) -> Style {
    Style::new().fg(match state {
        GitState::Ignored => theme.git_ignored,
        GitState::Untracked => theme.git_untracked,
        GitState::Added | GitState::Renamed => theme.git_added,
        GitState::Deleted | GitState::Modified => theme.git_modified,
        GitState::Conflict => theme.git_conflict,
    })
}

fn render_placeholder(frame: &mut Frame, area: Rect, state: &AppState, theme: &Theme) {
    let view = &state.view;
    let text = if view.loading || state.is_navigating() {
        "loading…".to_owned()
    } else if view.hidden_count() > 0 {
        let n = view.hidden_count();
        let noun = if n == 1 { "entry" } else { "entries" };
        format!("{n} hidden {noun} (press . to show)")
    } else {
        "empty directory".to_owned()
    };
    let line = Line::from(vec![
        Span::raw(" ".repeat(LEAD_WIDTH)),
        Span::styled(
            truncate_end(&text, usize::from(area.width).saturating_sub(LEAD_WIDTH)),
            theme.muted(),
        ),
    ]);
    frame.render_widget(Paragraph::new(line), area);
}

// ----- drives -----

const DRIVE_ROOT_WIDTH: usize = 4;
const DRIVE_FS_WIDTH: usize = 6;
const DRIVE_BAR_WIDTH: usize = 10;
/// `1024.0 GB free of 1024.0 GB`
const DRIVE_SPACE_WIDTH: usize = 26;

fn render_drives(frame: &mut Frame, area: Rect, state: &AppState, theme: &Theme) {
    let view = &state.drives;
    let ascii = state.config.icons == IconSet::Ascii;
    let first = view.cursor.offset;
    let rows = usize::from(area.height);
    let total_width = usize::from(area.width);
    // Drop details from the right as the terminal narrows.
    let show_space = total_width >= 70;
    let show_bar = total_width >= 54;
    let show_fs = total_width >= 40;
    let fixed = LEAD_WIDTH
        + DRIVE_ROOT_WIDTH
        + 1
        + if show_fs { DRIVE_FS_WIDTH + 1 } else { 0 }
        + if show_bar { DRIVE_BAR_WIDTH + 1 } else { 0 }
        + if show_space { DRIVE_SPACE_WIDTH + 1 } else { 0 }
        + MARGIN_RIGHT;
    let label_width = total_width.saturating_sub(fixed).max(1);

    let lines: Vec<Line> = (first..first.saturating_add(rows).min(view.drives.len()))
        .map(|index| {
            let drive = &view.drives[index];
            let cursor = index == view.cursor.index;
            let row_bg = if cursor {
                Style::new().bg(theme.selection_bg)
            } else {
                Style::new()
            };
            let span = |text: String, style: Style| Span::styled(text, style.patch(row_bg));
            let mut spans = vec![
                span(" ".into(), Style::new()),
                span(
                    if cursor { CURSOR_GLYPH } else { " " }.into(),
                    theme.accent_style(),
                ),
                span(" ".into(), Style::new()),
                span(
                    format!("{:<DRIVE_ROOT_WIDTH$}", root_label(drive)),
                    Style::new().fg(theme.dir).add_modifier(Modifier::BOLD),
                ),
                span(" ".into(), Style::new()),
                span(
                    format!(
                        "{:<label_width$}",
                        truncate_end(&drive_title(drive), label_width)
                    ),
                    Style::new().fg(theme.fg),
                ),
            ];
            if show_fs {
                spans.push(span(
                    format!(
                        " {:<DRIVE_FS_WIDTH$}",
                        truncate_end(&drive.fs, DRIVE_FS_WIDTH)
                    ),
                    theme.muted(),
                ));
            }
            if show_bar {
                spans.push(span(" ".into(), Style::new()));
                spans.push(span(
                    usage_bar(drive, DRIVE_BAR_WIDTH, ascii),
                    bar_style(drive, theme),
                ));
            }
            if show_space {
                spans.push(span(" ".into(), Style::new()));
                spans.push(span(
                    format!(
                        "{:>DRIVE_SPACE_WIDTH$}",
                        space_text(drive, state.config.files.size_base)
                    ),
                    theme.dim(),
                ));
            }
            spans.push(span(" ".repeat(MARGIN_RIGHT), Style::new()));
            Line::from(spans)
        })
        .collect();
    frame.render_widget(Paragraph::new(lines), area);
}

/// `C:` for `C:\`; other roots as they are.
fn root_label(drive: &DriveInfo) -> String {
    let text = dunce::simplified(&drive.root).display().to_string();
    match text.strip_suffix(['\\', '/']) {
        Some(stripped) if !stripped.is_empty() => stripped.to_owned(),
        _ => text,
    }
}

/// The volume label, or a description of the drive when it has none.
fn drive_title(drive: &DriveInfo) -> String {
    if !drive.label.is_empty() {
        return drive.label.clone();
    }
    match drive.kind {
        DriveKind::Fixed => "Local Disk",
        DriveKind::Removable => "Removable Disk",
        DriveKind::Network => "Network Drive",
        DriveKind::Optical => "CD/DVD Drive",
        DriveKind::RamDisk => "RAM Disk",
        DriveKind::Unknown => "Drive",
    }
    .to_owned()
}

/// A bar of `cells` cells showing how full the drive is. Empty (all shaded) when unknown.
pub fn usage_bar(drive: &DriveInfo, cells: usize, ascii: bool) -> String {
    let (full, empty) = if ascii { ('#', '-') } else { ('█', '░') };
    let filled = drive
        .used_fraction()
        .map_or(0, |f| (f * cells as f64).round() as usize)
        .min(cells);
    let mut bar = String::new();
    bar.extend(std::iter::repeat_n(full, filled));
    bar.extend(std::iter::repeat_n(empty, cells - filled));
    bar
}

fn bar_style(drive: &DriveInfo, theme: &Theme) -> Style {
    match drive.used_fraction() {
        Some(f) if f >= 0.95 => Style::new().fg(theme.error),
        Some(f) if f >= 0.85 => Style::new().fg(theme.warning),
        Some(_) => theme.accent_style(),
        None => theme.muted(),
    }
}

fn space_text(drive: &DriveInfo, base: ks_core::config::SizeBase) -> String {
    match (drive.free, drive.total) {
        (Some(free), Some(total)) => format!(
            "{} free of {}",
            human_size(free, base),
            human_size(total, base)
        ),
        _ => String::new(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn drive(total: Option<u64>, free: Option<u64>) -> DriveInfo {
        DriveInfo {
            total,
            free,
            ..DriveInfo::new("C:/")
        }
    }

    #[test]
    fn usage_bar_fills_proportionally() {
        assert_eq!(
            usage_bar(&drive(Some(100), Some(100)), 10, false),
            "░░░░░░░░░░"
        );
        assert_eq!(
            usage_bar(&drive(Some(100), Some(0)), 10, false),
            "██████████"
        );
        assert_eq!(
            usage_bar(&drive(Some(100), Some(50)), 10, false),
            "█████░░░░░"
        );
        assert_eq!(usage_bar(&drive(Some(100), Some(25)), 8, true), "######--");
        assert_eq!(usage_bar(&drive(None, None), 4, true), "----");
    }

    #[test]
    fn usage_bar_is_always_the_requested_width() {
        for used in 0..=100 {
            let d = drive(Some(100), Some(100 - used));
            assert_eq!(usage_bar(&d, 10, false).chars().count(), 10);
        }
    }

    #[test]
    fn drive_titles_and_roots() {
        assert_eq!(drive_title(&DriveInfo::new("C:/")), "Drive");
        let mut labelled = DriveInfo::new("D:/");
        labelled.label = "Data".into();
        assert_eq!(drive_title(&labelled), "Data");
        labelled.label.clear();
        labelled.kind = DriveKind::Removable;
        assert_eq!(drive_title(&labelled), "Removable Disk");
        assert_eq!(root_label(&DriveInfo::new("C:/")), "C:");
        assert_eq!(root_label(&DriveInfo::new("/")), "/");
    }

    #[test]
    fn optional_columns_follow_the_terminal_width() {
        assert_eq!(
            Columns::for_width(59),
            Columns {
                size: false,
                time: false
            }
        );
        assert_eq!(
            Columns::for_width(80),
            Columns {
                size: true,
                time: false
            }
        );
        assert_eq!(
            Columns::for_width(120),
            Columns {
                size: true,
                time: true
            }
        );
    }
}
