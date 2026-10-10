//! Rendering of the file-operation feature (P6): the inline prompt, the job summary in the
//! prompt line, and the overlays (jobs, conflict, confirm, copy-as / open-with menus, and the
//! `!` pager).

use ratatui::layout::Rect;
use ratatui::style::{Color, Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, BorderType, Clear, Paragraph};
use ratatui::Frame;

use ks_core::config::IconSet;
use ks_core::fileops::{
    pager_rows, pager_width, ConfirmAction, ConfirmDialog, ConflictDialog, ItemMeta, JobRow,
    JobStatus, Menu, MenuKind, Pager, PagerStatus, PromptInput,
};
use ks_core::{AppState, Mode, Overlay};

use crate::format::{human_size, relative_time};
use crate::render::spinner_glyph;
use crate::text::{sanitize, truncate_end, truncate_start, width};
use crate::theme::Theme;

// ----- shared pieces -----

fn ascii(state: &AppState) -> bool {
    state.config.icons == IconSet::Ascii
}

/// A centered rectangle of at most `w` x `h` inside `area`.
fn centered(area: Rect, w: u16, h: u16) -> Rect {
    let w = w.min(area.width);
    let h = h.min(area.height);
    Rect::new(
        area.x + (area.width - w) / 2,
        area.y + (area.height - h) / 2,
        w,
        h,
    )
}

/// Clears `rect`, draws a rounded box with `title` in the top border and returns the inner
/// area (one cell of padding at the sides).
fn dialog(frame: &mut Frame, rect: Rect, title: &str, border: Color, theme: &Theme) -> Rect {
    frame.render_widget(Clear, rect);
    let title = truncate_end(title, usize::from(rect.width).saturating_sub(6));
    let block = Block::bordered()
        .border_type(BorderType::Rounded)
        .border_style(Style::new().fg(border))
        .title(Line::from(vec![
            Span::styled("─ ", Style::new().fg(border)),
            Span::styled(
                title,
                Style::new().fg(theme.fg).add_modifier(Modifier::BOLD),
            ),
            Span::styled(" ", Style::new().fg(border)),
        ]))
        .style(theme.base_style());
    let inner = block.inner(rect);
    frame.render_widget(block, rect);
    Rect::new(
        inner.x + 1,
        inner.y,
        inner.width.saturating_sub(2),
        inner.height,
    )
}

/// `‹key› label` pairs on one line.
fn hint_line(pairs: &[(&str, &str)], theme: &Theme) -> Line<'static> {
    let mut spans = Vec::new();
    for (i, (key, label)) in pairs.iter().enumerate() {
        if i > 0 {
            spans.push(Span::raw("  "));
        }
        spans.push(Span::styled("‹", theme.muted()));
        spans.push(Span::styled((*key).to_owned(), theme.dim()));
        spans.push(Span::styled(format!("› {label}"), theme.muted()));
    }
    Line::from(spans)
}

fn put_line(frame: &mut Frame, inner: Rect, row: u16, line: Line<'static>) {
    if row < inner.height {
        frame.render_widget(
            Paragraph::new(line),
            Rect::new(inner.x, inner.y + row, inner.width, 1),
        );
    }
}

fn eta_text(secs: u64) -> String {
    let (h, m, s) = (secs / 3600, (secs / 60) % 60, secs % 60);
    if h > 0 {
        format!("{h}:{m:02}:{s:02}")
    } else {
        format!("{m}:{s:02}")
    }
}

/// `████░░░░░░`
fn bar(percent: u8, cells: usize, ascii: bool) -> (String, String) {
    let filled = (usize::from(percent) * cells / 100).min(cells);
    let (on, off) = if ascii { ('#', '-') } else { ('█', '░') };
    (
        on.to_string().repeat(filled),
        off.to_string().repeat(cells - filled),
    )
}

/// The text cursor is drawn as a reversed cell so that it is part of the buffer.
fn input_spans(input: &PromptInput, max: usize, theme: &Theme) -> Vec<Span<'static>> {
    let chars: Vec<char> = input.text().chars().collect();
    let cursor = input.cursor().min(chars.len());
    // Scroll so that the cursor cell stays inside `max` cells.
    let start = (cursor + 1).saturating_sub(max);
    let mut used = 0;
    let mut spans = Vec::new();
    let text_style = Style::new().fg(theme.fg);
    let cursor_style = Style::new().add_modifier(Modifier::REVERSED);
    let mut before = String::new();
    let mut after = String::new();
    let mut at = String::from(" ");
    for (i, c) in chars.iter().enumerate().skip(start) {
        let w = unicode_width::UnicodeWidthChar::width(*c).unwrap_or(0);
        let c = if c.is_control() { '?' } else { *c };
        if i < cursor {
            before.push(c);
        } else if i == cursor {
            at = c.to_string();
        } else if used + w < max {
            after.push(c);
        }
        used += w;
    }
    spans.push(Span::styled(before, text_style));
    spans.push(Span::styled(at, cursor_style));
    spans.push(Span::styled(after, text_style));
    spans
}

// ----- prompt line -----

/// Draws the rename / create / shell prompt in place of the normal prompt line. Returns
/// whether it did.
pub fn render_input_prompt(frame: &mut Frame, area: Rect, state: &AppState, theme: &Theme) -> bool {
    let Mode::Prompt(_) = state.mode else {
        return false;
    };
    let Some(prompt) = state.fileops.prompt.as_ref() else {
        return false;
    };
    let hint = "enter ok · esc cancel";
    let label = format!("{} ", prompt.label);
    let fixed = 3 + width(&label);
    let hint_cells = if usize::from(area.width) > fixed + 30 + width(hint) {
        width(hint) + 2
    } else {
        0
    };
    let room = usize::from(area.width)
        .saturating_sub(fixed + hint_cells + 1)
        .max(1);

    let mut spans = vec![
        Span::raw(" "),
        Span::styled("›", theme.bold_accent()),
        Span::raw(" "),
        Span::styled(label, theme.dim()),
    ];
    spans.extend(input_spans(&prompt.input, room, theme));
    frame.render_widget(Paragraph::new(Line::from(spans)), area);
    if hint_cells > 0 {
        let w = u16::try_from(width(hint)).unwrap_or(0);
        let x = area.right().saturating_sub(w + 1);
        frame.render_widget(
            Paragraph::new(Span::styled(hint, theme.muted())),
            Rect::new(x, area.y, w, 1),
        );
    }
    true
}

/// `3 yanked` / `2 cut`, shown with the list position while the register is filled.
pub fn register_segment(state: &AppState) -> Option<String> {
    let register = state.fileops.register.as_ref()?;
    let word = if register.cut { "cut" } else { "yanked" };
    Some(format!("{} {word}", register.paths.len()))
}

/// The running jobs for the right side of the prompt line: spinner, what, percentage,
/// throughput and ETA. `None` when nothing runs.
pub fn job_segment(state: &AppState) -> Option<String> {
    let active: Vec<&JobRow> = state
        .fileops
        .jobs
        .iter()
        .filter(|j| j.status.is_active())
        .collect();
    if active.is_empty() {
        return None;
    }
    let spin = spinner_glyph(state);
    let cap = (usize::from(state.size.width) / 2).clamp(18, 46);
    let (head, row) = if let [only] = active.as_slice() {
        (only.description.clone(), Some(*only))
    } else {
        // Several jobs: the combined fraction of the ones with a known total.
        (format!("{} jobs", active.len()), None)
    };
    let mut tail = String::new();
    let measured: Vec<&JobRow> = match row {
        Some(r) => vec![r],
        None => active.clone(),
    };
    let known: Vec<_> = measured
        .iter()
        .filter(|j| j.progress.bytes_total > 0)
        .collect();
    if !known.is_empty() {
        let done: u64 = known.iter().map(|j| j.progress.bytes_done).sum();
        let total: u64 = known.iter().map(|j| j.progress.bytes_total).sum();
        let pct = (u128::from(done.min(total)) * 100) / u128::from(total.max(1));
        tail.push_str(&format!(" {pct}%"));
        if let Some(only) = row {
            let base = state.config.files.size_base;
            if only.progress.throughput_bps > 0 {
                tail.push_str(&format!(
                    " · {}/s",
                    human_size(only.progress.throughput_bps, base)
                ));
            }
            if let Some(eta) = only.progress.eta_secs {
                tail.push_str(&format!(" · {}", eta_text(eta)));
            }
        }
    } else if row.is_some_and(|j| j.scanning) {
        tail.push_str(&format!(
            " · scanning {} files",
            row.map_or(0, |j| j.progress.files_total)
        ));
    }
    let room = cap.saturating_sub(width(&tail) + 2);
    Some(format!(
        "{spin} {}{tail}",
        truncate_end(&sanitize(&head), room)
    ))
}

// ----- overlays -----

/// Draws the file-operation overlays that are open, bottom to top.
pub fn render_overlays(frame: &mut Frame, state: &AppState, theme: &Theme) {
    let area = frame.area();
    for overlay in &state.overlays {
        match overlay {
            Overlay::Jobs => render_jobs(frame, area, state, theme),
            Overlay::Confirm => {
                if let Some(confirm) = &state.fileops.confirm {
                    render_confirm(frame, area, confirm, theme);
                }
            }
            Overlay::Conflict => {
                if let Some(dialog) = state.fileops.conflicts.front() {
                    let waiting = state.fileops.conflicts.len() - 1;
                    render_conflict(frame, area, state, dialog, waiting, theme);
                }
            }
            Overlay::Menu => {
                if let Some(menu) = &state.fileops.menu {
                    render_menu(frame, area, menu, theme);
                }
            }
            Overlay::Pager => {
                if let Some(pager) = &state.fileops.pager {
                    render_pager(frame, area, state, pager, theme);
                }
            }
            _ => {}
        }
    }
}

fn status_glyph(state: &AppState, status: JobStatus, theme: &Theme) -> Span<'static> {
    let a = ascii(state);
    match status {
        JobStatus::Queued => Span::styled(if a { "." } else { "○" }, theme.muted()),
        JobStatus::Running => Span::styled(spinner_glyph(state), theme.accent_style()),
        JobStatus::AwaitingConflict => Span::styled("?", theme.bold_accent()),
        JobStatus::Done => Span::styled(if a { "+" } else { "✓" }, Style::new().fg(theme.success)),
        JobStatus::Failed => Span::styled(if a { "x" } else { "✗" }, Style::new().fg(theme.error)),
        JobStatus::Cancelled => {
            Span::styled(if a { "-" } else { "⊘" }, Style::new().fg(theme.warning))
        }
    }
}

fn job_line(
    state: &AppState,
    job: &JobRow,
    selected: bool,
    inner_width: usize,
    theme: &Theme,
) -> Line<'static> {
    let marker = if selected {
        Span::styled("▌", theme.accent_style())
    } else {
        Span::raw(" ")
    };
    let mut spans = vec![
        marker,
        Span::raw(" "),
        status_glyph(state, job.status, theme),
    ];
    spans.push(Span::raw(" "));
    let room = inner_width.saturating_sub(4);

    let label = match (&job.summary, job.status.is_active()) {
        (Some(summary), false) => summary.clone(),
        _ => job.description.clone(),
    };
    let label = sanitize(&label);
    let base = state.config.files.size_base;

    if job.status.is_active() {
        let mut right: Vec<Span<'static>> = Vec::new();
        let mut right_w = 0;
        if job.status == JobStatus::AwaitingConflict {
            let text = "waiting for your answer";
            right_w = width(text);
            right.push(Span::styled(text, theme.accent_style()));
        } else if job.scanning {
            let text = format!("scanning {} files", job.progress.files_total);
            right_w = width(&text);
            right.push(Span::styled(text, theme.dim()));
        } else if let Some(pct) = job.progress.percent() {
            let cells = if room >= 60 { 12 } else { 8 };
            let (on, off) = bar(pct, cells, ascii(state));
            right.push(Span::styled(on, theme.accent_style()));
            right.push(Span::styled(off, theme.muted()));
            right.push(Span::styled(format!(" {pct:>3}%"), theme.dim()));
            right_w = cells + 5;
            if room >= 64 {
                let rate = if job.progress.throughput_bps > 0 {
                    format!("{}/s", human_size(job.progress.throughput_bps, base))
                } else {
                    String::new()
                };
                let eta = job.progress.eta_secs.map(eta_text).unwrap_or_default();
                let text = format!("  {rate:>10}  {eta:>7}");
                right_w += width(&text);
                right.push(Span::styled(text, theme.dim()));
            }
        }
        let left_room = room.saturating_sub(right_w + 2);
        let label = truncate_end(&label, left_room);
        let pad = room.saturating_sub(width(&label) + right_w);
        spans.push(Span::styled(label, Style::new().fg(theme.fg)));
        spans.push(Span::raw(" ".repeat(pad)));
        spans.extend(right);
    } else {
        let style = match job.status {
            JobStatus::Failed => Style::new().fg(theme.error),
            JobStatus::Cancelled => Style::new().fg(theme.warning),
            _ => theme.dim(),
        };
        spans.push(Span::styled(truncate_end(&label, room), style));
    }
    Line::from(spans)
}

fn render_jobs(frame: &mut Frame, area: Rect, state: &AppState, theme: &Theme) {
    let jobs = &state.fileops.jobs;
    let w = area.width.saturating_sub(4).clamp(20, 100);
    let rows = jobs.len().max(1);
    let h = u16::try_from(rows + 4)
        .unwrap_or(u16::MAX)
        .min(area.height.saturating_sub(2).max(5));
    let rect = centered(area, w, h);
    let active = jobs.iter().filter(|j| j.status.is_active()).count();
    let title = if active > 0 {
        format!("Jobs · {active} running")
    } else {
        "Jobs".to_owned()
    };
    let inner = dialog(frame, rect, &title, theme.border_focus, theme);
    let list_rows = usize::from(inner.height.saturating_sub(2));
    if jobs.is_empty() {
        put_line(
            frame,
            inner,
            0,
            Line::from(Span::styled(
                "No jobs yet. Copy, move or delete something.",
                theme.muted(),
            )),
        );
    } else {
        let selected = state.fileops.jobs_selected.min(jobs.len() - 1);
        let offset = if selected >= list_rows {
            selected + 1 - list_rows
        } else {
            0
        };
        for (i, job) in jobs.iter().enumerate().skip(offset).take(list_rows) {
            let line = job_line(state, job, i == selected, usize::from(inner.width), theme);
            put_line(frame, inner, u16::try_from(i - offset).unwrap_or(0), line);
        }
    }
    put_line(
        frame,
        inner,
        inner.height.saturating_sub(1),
        hint_line(
            &[("j/k", "move"), ("x", "cancel job"), ("esc", "close")],
            theme,
        ),
    );
}

fn meta_text(state: &AppState, meta: &ItemMeta) -> String {
    let size = if meta.is_dir {
        "folder".to_owned()
    } else {
        human_size(meta.len, state.config.files.size_base)
    };
    let when = meta.modified.map_or_else(
        || "unknown time".to_owned(),
        |t| relative_time(t, state.now, state.utc_offset_secs),
    );
    format!("{size:>9}   {when}")
}

fn render_conflict(
    frame: &mut Frame,
    area: Rect,
    state: &AppState,
    dialog_state: &ConflictDialog,
    waiting: usize,
    theme: &Theme,
) {
    let w = area.width.saturating_sub(4).clamp(30, 76);
    let rect = centered(area, w, 12);
    let inner = dialog(frame, rect, "Already exists", theme.warning, theme);
    let info = &dialog_state.info;
    let iw = usize::from(inner.width);
    let name = info.dst.file_name().map_or_else(
        || info.dst.display().to_string(),
        |n| n.to_string_lossy().into_owned(),
    );
    let dir = info
        .dst
        .parent()
        .map(|p| p.display().to_string())
        .unwrap_or_default();

    put_line(
        frame,
        inner,
        0,
        Line::from(vec![
            Span::styled(
                truncate_end(&sanitize(&name), iw.saturating_sub(15)),
                Style::new().fg(theme.fg).add_modifier(Modifier::BOLD),
            ),
            Span::styled(" already exists", theme.dim()),
        ]),
    );
    put_line(
        frame,
        inner,
        1,
        Line::from(Span::styled(
            truncate_start(&sanitize(&format!("in {dir}")), iw),
            theme.muted(),
        )),
    );
    for (row, label, meta) in [
        (3, "existing", &info.dst_meta),
        (4, "incoming", &info.src_meta),
    ] {
        put_line(
            frame,
            inner,
            row,
            Line::from(vec![
                Span::styled(format!("{label:<9}"), theme.muted()),
                Span::styled(
                    truncate_end(&meta_text(state, meta), iw.saturating_sub(9)),
                    theme.dim(),
                ),
            ]),
        );
    }

    if let Some(input) = &dialog_state.rename {
        let mut spans = vec![
            Span::styled("rename ", theme.dim()),
            Span::styled("› ", theme.bold_accent()),
        ];
        spans.extend(input_spans(input, iw.saturating_sub(9), theme));
        put_line(frame, inner, 6, Line::from(spans));
        put_line(
            frame,
            inner,
            inner.height.saturating_sub(1),
            hint_line(&[("enter", "use this name"), ("esc", "back")], theme),
        );
        return;
    }

    let key = |k: &str, label: &str| {
        vec![
            Span::styled(k.to_owned(), theme.bold_accent()),
            Span::styled(format!(" {label}"), Style::new().fg(theme.fg)),
        ]
    };
    let mut choices = Vec::new();
    for (i, (k, label)) in [
        ("o", "overwrite"),
        ("s", "skip"),
        ("r", "rename"),
        ("k", "keep both"),
    ]
    .iter()
    .enumerate()
    {
        if i > 0 {
            choices.push(Span::raw("   "));
        }
        choices.extend(key(k, label));
    }
    put_line(frame, inner, 6, Line::from(choices));

    let on = dialog_state.apply_to_all;
    let check = if on { "[x]" } else { "[ ]" };
    put_line(
        frame,
        inner,
        8,
        Line::from(vec![
            Span::styled("a", theme.bold_accent()),
            Span::styled(
                " apply to all remaining conflicts ",
                Style::new().fg(theme.fg),
            ),
            Span::styled(
                check,
                if on {
                    theme.bold_accent()
                } else {
                    theme.muted()
                },
            ),
            Span::styled(if on { " on" } else { " off" }, theme.dim()),
        ]),
    );
    if waiting > 0 {
        put_line(
            frame,
            inner,
            9,
            Line::from(Span::styled(
                format!("{waiting} more waiting"),
                theme.muted(),
            )),
        );
    }
    put_line(
        frame,
        inner,
        inner.height.saturating_sub(1),
        hint_line(
            &[("enter", "keep both"), ("c/esc", "cancel the job")],
            theme,
        ),
    );
}

fn render_confirm(frame: &mut Frame, area: Rect, confirm: &ConfirmDialog, theme: &Theme) {
    let w = area.width.saturating_sub(4).clamp(30, 70);
    let extra = u16::from(confirm.more > 0) + if confirm.token.is_some() { 3 } else { 0 };
    let h = u16::try_from(confirm.lines.len())
        .unwrap_or(u16::MAX)
        .saturating_add(extra + 4)
        .min(area.height.saturating_sub(2).max(6));
    let rect = centered(area, w, h);
    let border = if confirm.danger {
        theme.error
    } else {
        theme.border_focus
    };
    let inner = dialog(frame, rect, &confirm.title, border, theme);
    let iw = usize::from(inner.width);
    let token_rows = if confirm.token.is_some() { 3 } else { 0 };
    let avail = usize::from(inner.height).saturating_sub(2 + token_rows);
    // Lines that do not fit on a short terminal count as "more", so the number stays true.
    let (shown, hidden) = if confirm.lines.len() + usize::from(confirm.more > 0) <= avail {
        (confirm.lines.len(), confirm.more)
    } else {
        let shown = avail.saturating_sub(1);
        (shown, confirm.more + confirm.lines.len() - shown)
    };
    let mut row = 0u16;
    for line in confirm.lines.iter().take(shown) {
        put_line(
            frame,
            inner,
            row,
            Line::from(Span::styled(
                truncate_end(&sanitize(line), iw),
                Style::new().fg(theme.fg),
            )),
        );
        row += 1;
    }
    if hidden > 0 {
        put_line(
            frame,
            inner,
            row,
            Line::from(Span::styled(format!("… and {hidden} more"), theme.muted())),
        );
        row += 1;
    }
    if let Some(token) = &confirm.token {
        row += 1;
        let mut spans = vec![
            Span::styled(
                format!("Type {token} to confirm "),
                Style::new().fg(theme.error),
            ),
            Span::styled("› ", theme.bold_accent()),
        ];
        let used = width(&format!("Type {token} to confirm › "));
        spans.extend(input_spans(
            &confirm.typed,
            iw.saturating_sub(used + 1),
            theme,
        ));
        put_line(frame, inner, row, Line::from(spans));
        if confirm.mismatch {
            put_line(
                frame,
                inner,
                row + 1,
                Line::from(Span::styled(
                    format!("That is not {token}; nothing was deleted"),
                    Style::new().fg(theme.error),
                )),
            );
        }
    }
    let hints: Vec<(&str, &str)> = if confirm.action.is_some() {
        // Launching Claude on API billing is accepted with `y` only, never enter.
        let key = if confirm.action == Some(ConfirmAction::ClaudeProceed) {
            "y"
        } else {
            "enter"
        };
        vec![(key, confirm.accept_label.as_str()), ("esc", "cancel")]
    } else {
        vec![("enter/esc", "close")]
    };
    put_line(
        frame,
        inner,
        inner.height.saturating_sub(1),
        hint_line(&hints, theme),
    );
}

fn render_menu(frame: &mut Frame, area: Rect, menu: &Menu, theme: &Theme) {
    let widest = menu
        .items
        .iter()
        .map(|i| width(&i.label))
        .max()
        .unwrap_or(0)
        .max(width(&menu.title));
    let w = u16::try_from(widest + 12).unwrap_or(u16::MAX).max(46);
    let h = u16::try_from(menu.items.len() + 4).unwrap_or(u16::MAX);
    let rect = centered(area, w, h);
    let inner = dialog(frame, rect, &menu.title, theme.border_focus, theme);
    for (i, item) in menu.items.iter().enumerate() {
        let selected = i == menu.selected;
        let marker = if selected {
            Span::styled("▌", theme.accent_style())
        } else {
            Span::raw(" ")
        };
        let key = item
            .hotkey
            .map_or_else(|| " ".to_owned(), |c| c.to_string());
        let style = if selected {
            Style::new().fg(theme.fg).bg(theme.selection_bg)
        } else {
            Style::new().fg(theme.fg)
        };
        let label = truncate_end(
            &sanitize(&item.label),
            usize::from(inner.width).saturating_sub(6),
        );
        put_line(
            frame,
            inner,
            u16::try_from(i).unwrap_or(0),
            Line::from(vec![
                marker,
                Span::raw(" "),
                Span::styled(key, theme.bold_accent()),
                Span::raw("  "),
                Span::styled(label, style),
            ]),
        );
    }
    let hints: &[(&str, &str)] = match menu.kind {
        MenuKind::CopyPath => &[("a/r/n", "pick"), ("enter", "copy"), ("esc", "close")],
        MenuKind::OpenWith => &[("1-9", "pick"), ("enter", "open"), ("esc", "close")],
    };
    put_line(
        frame,
        inner,
        inner.height.saturating_sub(1),
        hint_line(hints, theme),
    );
}

fn render_pager(frame: &mut Frame, area: Rect, state: &AppState, pager: &Pager, theme: &Theme) {
    let rect = Rect::new(
        area.x + 2,
        area.y + 1,
        area.width.saturating_sub(4),
        area.height.saturating_sub(3),
    );
    let border = match &pager.status {
        PagerStatus::Exited(Some(0)) | PagerStatus::Running => theme.border_focus,
        PagerStatus::Exited(_) | PagerStatus::Failed(_) => theme.error,
        PagerStatus::Cancelled => theme.warning,
    };
    let title = format!("$ {}", sanitize(&pager.command));
    let inner = dialog(frame, rect, &title, border, theme);
    let rows = pager_rows(state.size).min(usize::from(inner.height).saturating_sub(1));
    let cols = pager_width(state.size).min(usize::from(inner.width));
    let max_scroll = pager.lines.len().saturating_sub(rows);
    let scroll = pager.scroll.min(max_scroll);
    for (i, line) in pager.lines.iter().skip(scroll).take(rows).enumerate() {
        let style = if line.stderr {
            Style::new().fg(theme.warning)
        } else {
            Style::new().fg(theme.fg)
        };
        put_line(
            frame,
            inner,
            u16::try_from(i).unwrap_or(0),
            Line::from(Span::styled(truncate_end(&line.text, cols), style)),
        );
    }
    if pager.lines.is_empty() && pager.is_running() {
        put_line(
            frame,
            inner,
            0,
            Line::from(Span::styled("(no output yet)", theme.muted())),
        );
    }

    let (status_text, status_style) = match &pager.status {
        PagerStatus::Running => ("running".to_owned(), theme.accent_style()),
        PagerStatus::Exited(Some(0)) => ("exit 0".to_owned(), Style::new().fg(theme.success)),
        PagerStatus::Exited(Some(code)) => (format!("exit {code}"), Style::new().fg(theme.error)),
        PagerStatus::Exited(None) => ("terminated".to_owned(), Style::new().fg(theme.error)),
        PagerStatus::Cancelled => ("cancelled".to_owned(), Style::new().fg(theme.warning)),
        PagerStatus::Failed(error) => (
            format!("cannot start: {}", truncate_end(error, 40)),
            Style::new().fg(theme.error),
        ),
    };
    let total = pager.lines.len();
    let last = (scroll + rows).min(total);
    let first = if total == 0 { 0 } else { scroll + 1 };
    let dropped = if pager.dropped > 0 {
        format!(" (+{} dropped)", pager.dropped)
    } else {
        String::new()
    };
    let mut footer = vec![
        Span::styled(status_text, status_style),
        Span::styled(format!("  {first}-{last}/{total}{dropped}  "), theme.dim()),
    ];
    let hints: &[(&str, &str)] = if pager.is_running() {
        &[("ctrl-c", "stop"), ("j/k", "scroll"), ("esc", "close")]
    } else {
        &[("j/k", "scroll"), ("g/G", "top/end"), ("esc", "close")]
    };
    footer.extend(hint_line(hints, theme).spans);
    put_line(
        frame,
        inner,
        inner.height.saturating_sub(1),
        Line::from(footer),
    );
}

#[cfg(test)]
mod tests;
