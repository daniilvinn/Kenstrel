//! The find-file and content-search overlays and the helpers that draw matched
//! characters.
//!
//! Layout comes from [`ks_core::search::geometry`], the same numbers `update` uses to size the
//! result windows. On terminals of 100 columns or more both overlays show the preview pane
//! next to the results: [`preview_rect`] says where it goes and [`ks_core::preview_target`]
//! what it shows (`ks_core::preview` keeps the pane's request in step with the highlight).

use ratatui::layout::Rect;
use ratatui::style::{Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, BorderType, Clear, Paragraph};
use ratatui::Frame;

use ks_core::config::IconSet;
use ks_core::search::geometry::{self, Cells};
use ks_core::search::Scope;
use ks_core::{preview_target, AppState, GrepField, GrepState, LineInput, Overlay};

use crate::text::{breadcrumb, sanitize, truncate_end, width};
use crate::theme::Theme;

const PROMPT_GLYPH: &str = "›";
const CURSOR_GLYPH: &str = "▌";
const MARK_GLYPH: &str = "●";
const RULE_GLYPH: &str = "─";
const DIVIDER_GLYPH: &str = "│";
const UNICODE_SPINNER: &[&str] = &["⠋", "⠙", "⠹", "⠸", "⠼", "⠴", "⠦", "⠧", "⠇", "⠏"];
const ASCII_SPINNER: &[&str] = &["|", "/", "-", "\\"];
/// Width of the line-number gutter of a content hit: `  42│ `.
const GUTTER: usize = 7;

fn spinner(state: &AppState) -> &'static str {
    let frames = if state.config.icons == IconSet::Ascii {
        ASCII_SPINNER
    } else {
        UNICODE_SPINNER
    };
    frames[state.spinner_frame % frames.len()]
}

/// Draws the topmost search overlay, if there is one.
pub fn render_overlays(frame: &mut Frame, state: &AppState, theme: &Theme) {
    match state.overlays.last() {
        Some(Overlay::Finder) => render_finder(frame, state, theme),
        Some(Overlay::Grep) => render_grep(frame, state, theme),
        _ => {}
    }
}

/// Where the preview pane of the open search overlay goes (inner area right of the results),
/// or `None` when the terminal is too narrow for it or no search overlay is open.
pub fn preview_rect(state: &AppState) -> Option<Rect> {
    if !matches!(state.overlays.last(), Some(Overlay::Finder | Overlay::Grep))
        || !geometry::shows_preview(state.size)
    {
        return None;
    }
    let rect = geometry::overlay_rect(state.size);
    let inner = inner_of(rect);
    let results = geometry::results_width(state.size);
    // One column for the divider.
    let top = if matches!(state.overlays.last(), Some(Overlay::Grep)) {
        inner.y + 3
    } else {
        inner.y + 2
    };
    let bottom = inner.bottom().saturating_sub(1);
    Some(Rect::new(
        inner.x + results + 1,
        top,
        inner.width.saturating_sub(results + 1),
        bottom.saturating_sub(top),
    ))
}

fn inner_of(rect: Cells) -> Rect {
    Rect::new(
        rect.x + 1,
        rect.y + 1,
        rect.width.saturating_sub(2),
        rect.height.saturating_sub(2),
    )
}

/// Clears the overlay area and draws its border with `title` in the top border.
fn frame_overlay(frame: &mut Frame, rect: Cells, title: Vec<Span<'static>>, theme: &Theme) -> Rect {
    let area = Rect::new(rect.x, rect.y, rect.width, rect.height);
    frame.render_widget(Clear, area);
    let mut spans = vec![Span::raw(" ")];
    spans.extend(title);
    spans.push(Span::raw(" "));
    let block = Block::bordered()
        .border_type(BorderType::Rounded)
        .border_style(Style::new().fg(theme.border_focus))
        .style(theme.base_style())
        .title(Line::from(spans));
    frame.render_widget(block, area);
    inner_of(rect)
}

fn rule(frame: &mut Frame, area: Rect, theme: &Theme) {
    let line = RULE_GLYPH.repeat(usize::from(area.width));
    frame.render_widget(
        Paragraph::new(Span::styled(line, Style::new().fg(theme.border))),
        area,
    );
}

/// `1234567` becomes `1,234,567`.
pub fn grouped(n: u64) -> String {
    let digits = n.to_string();
    let mut out = String::with_capacity(digits.len() + digits.len() / 3);
    for (i, c) in digits.chars().enumerate() {
        if i > 0 && (digits.len() - i).is_multiple_of(3) {
            out.push(',');
        }
        out.push(c);
    }
    out
}

// ----- text helpers -----

/// Spans for `text` where the characters at `indices` (character positions) are drawn with
/// `hl` and the rest with `base`. Neighbouring characters of equal style share a span.
pub fn highlighted(text: &str, indices: &[u32], base: Style, hl: Style) -> Vec<Span<'static>> {
    let mut spans: Vec<Span<'static>> = Vec::new();
    let mut run = String::new();
    let mut run_hl = false;
    let mut next = indices.iter().peekable();
    for (i, c) in text.chars().enumerate() {
        while next.next_if(|&&n| (n as usize) < i).is_some() {}
        let matched = next.next_if(|&&n| n as usize == i).is_some();
        if matched != run_hl && !run.is_empty() {
            spans.push(Span::styled(
                std::mem::take(&mut run),
                if run_hl { hl } else { base },
            ));
        }
        run_hl = matched;
        run.push(c);
    }
    if !run.is_empty() {
        spans.push(Span::styled(run, if run_hl { hl } else { base }));
    }
    spans
}

/// A relative path in `max` cells, keeping the end (the file name) when it must be cut. The
/// directory part is drawn dim, the file name in the normal colour, matches accent + bold.
fn path_spans(
    rel: &str,
    indices: &[u32],
    max: usize,
    theme: &Theme,
    row_bg: Style,
) -> Vec<Span<'static>> {
    let clean = sanitize(rel);
    let cl = clusters(&clean);
    let total: usize = cl.iter().map(|c| c.w).sum();
    // Drop leading clusters until the rest plus the ellipsis fits.
    let mut skip = 0;
    if total > max {
        let mut used = 1;
        skip = cl.len();
        for (i, c) in cl.iter().enumerate().rev() {
            if used + c.w > max {
                break;
            }
            used += c.w;
            skip = i;
        }
    }
    let name_start = cl
        .iter()
        .rposition(|c| c.text == "/" || c.text == "\\")
        .map_or(0, |i| i + 1);
    let mut spans = Vec::new();
    if skip > 0 {
        spans.push(Span::styled("…", theme.muted().patch(row_bg)));
    }
    let mut run = String::new();
    let mut run_style = Style::new();
    for (i, c) in cl.iter().enumerate().skip(skip) {
        // Highlight indices are char positions: a cluster is matched when any of its chars is.
        let first = indices.partition_point(|&n| (n as usize) < c.start);
        let matched = indices
            .get(first)
            .is_some_and(|&n| (n as usize) < c.start + c.nchars);
        let style = if matched {
            theme.bold_accent()
        } else if i < name_start {
            theme.dim()
        } else {
            Style::new().fg(theme.fg)
        }
        .patch(row_bg);
        if style != run_style && !run.is_empty() {
            spans.push(Span::styled(std::mem::take(&mut run), run_style));
        }
        run_style = style;
        run.push_str(c.text);
    }
    if !run.is_empty() {
        spans.push(Span::styled(run, run_style));
    }
    spans
}

/// One grapheme cluster of a string: where it starts (in chars and bytes), its text and its
/// width in cells (measured on the whole cluster, so VS16 emoji and ZWJ sequences are right).
struct Cluster<'a> {
    start: usize,
    byte: usize,
    nchars: usize,
    text: &'a str,
    w: usize,
}

fn clusters(s: &str) -> Vec<Cluster<'_>> {
    use unicode_segmentation::UnicodeSegmentation;
    let mut start = 0;
    s.grapheme_indices(true)
        .map(|(byte, text)| {
            let nchars = text.chars().count();
            let c = Cluster {
                start,
                byte,
                nchars,
                text,
                w: unicode_width::UnicodeWidthStr::width(text),
            };
            start += nchars;
            c
        })
        .collect()
}

/// The text of an input field with a block cursor, scrolled so the cursor stays visible.
pub(crate) fn input_spans(
    input: &LineInput,
    avail: usize,
    active: bool,
    placeholder: &str,
    theme: &Theme,
) -> Vec<Span<'static>> {
    if input.is_empty() {
        let mut spans = Vec::new();
        if active {
            spans.push(Span::styled(
                " ",
                Style::new().add_modifier(Modifier::REVERSED),
            ));
        }
        spans.push(Span::styled(
            truncate_end(placeholder, avail.saturating_sub(1)),
            theme.muted(),
        ));
        return spans;
    }
    let clean = sanitize(input.text());
    let cl = clusters(&clean);
    // The cluster holding the cursor (a cursor inside one counts as on it).
    let cursor = cl
        .iter()
        .position(|c| c.start + c.nchars > input.cursor())
        .unwrap_or(cl.len());
    // Keep the cursor inside the window: drop leading clusters while it would not fit.
    let span_width = |from: usize, to: usize| -> usize { cl[from..to].iter().map(|c| c.w).sum() };
    let mut start = 0;
    while start < cursor && span_width(start, cursor) + 1 > avail {
        start += 1;
    }
    let mut end = cl.len();
    while end > cursor && span_width(start, end) + usize::from(end == cl.len()) > avail {
        end -= 1;
    }
    let join = |from: usize, to: usize| -> String { cl[from..to].iter().map(|c| c.text).collect() };
    let mut spans = vec![Span::styled(join(start, cursor), Style::new().fg(theme.fg))];
    if active {
        let under = cl.get(cursor).filter(|_| cursor < end);
        spans.push(Span::styled(
            under.map_or_else(|| " ".to_owned(), |c| c.text.to_owned()),
            Style::new().add_modifier(Modifier::REVERSED),
        ));
        let after_from = if under.is_some() { cursor + 1 } else { cursor };
        spans.push(Span::styled(
            join(after_from.min(end), end),
            Style::new().fg(theme.fg),
        ));
    } else {
        spans.push(Span::styled(join(cursor, end), Style::new().fg(theme.fg)));
    }
    spans
}

/// A toggle chip: ` regex `, highlighted when on, muted when off.
fn chip(label: &str, on: bool, theme: &Theme) -> Span<'static> {
    if on {
        Span::styled(
            format!(" {label} "),
            Style::new()
                .fg(theme.accent_fg)
                .bg(theme.accent)
                .add_modifier(Modifier::BOLD),
        )
    } else {
        Span::styled(format!(" {label} "), theme.muted())
    }
}

fn right_aligned(frame: &mut Frame, area: Rect, spans: Vec<Span<'static>>) {
    let line = Line::from(spans);
    let w = u16::try_from(line.width())
        .unwrap_or(u16::MAX)
        .min(area.width);
    let x = area.right() - w;
    frame.render_widget(Paragraph::new(line), Rect::new(x, area.y, w, 1));
}

fn row_style(selected: bool, theme: &Theme) -> Style {
    if selected {
        Style::new().bg(theme.selection_bg)
    } else {
        Style::new()
    }
}

/// Fills `area` with the selection background so the bar spans the whole row.
fn fill_row(frame: &mut Frame, area: Rect, theme: &Theme) {
    frame.render_widget(
        Block::new().style(Style::new().bg(theme.selection_bg)),
        area,
    );
}

// ----- find file -----

fn scope_label(scope: Scope) -> &'static str {
    match scope {
        Scope::Cwd => "cwd",
        Scope::GitRoot => "git root",
        Scope::Home => "home",
        Scope::Drive => "drive",
    }
}

fn on_off(on: bool) -> &'static str {
    if on {
        "on"
    } else {
        "off"
    }
}

fn render_finder(frame: &mut Frame, state: &AppState, theme: &Theme) {
    let Some(f) = state.search.finder.as_ref() else {
        return;
    };
    let rect = geometry::overlay_rect(state.size);
    let mut title = vec![Span::styled(
        "Find file",
        Style::new().fg(theme.fg).add_modifier(Modifier::BOLD),
    )];
    title.push(Span::styled(
        format!(" · {}", scope_label(f.scope)),
        theme.accent_style(),
    ));
    if let Some(root) = &f.root {
        let budget = usize::from(rect.width).saturating_sub(24);
        if budget > 8 {
            title.push(Span::styled(
                format!(" · {}", breadcrumb(root, state.home.as_deref(), budget)),
                theme.dim(),
            ));
        }
    }
    let inner = frame_overlay(frame, rect, title, theme);
    if inner.height < 4 || inner.width < 10 {
        return;
    }

    // Input line with the counter on the right.
    let input_area = Rect::new(inner.x, inner.y, inner.width, 1);
    let counter_text = format!(
        "{} / {}",
        grouped(u64::from(f.matched)),
        grouped(u64::from(f.total))
    );
    let mut counter = vec![Span::styled(counter_text, theme.dim())];
    if f.walking {
        counter.push(Span::styled(
            format!(" {}", spinner(state)),
            theme.accent_style(),
        ));
    } else {
        counter.push(Span::raw("  "));
    }
    let counter_width: usize = counter.iter().map(|s| width(&s.content)).sum();
    let avail = usize::from(inner.width).saturating_sub(counter_width + 5);
    let mut spans = vec![
        Span::raw(" "),
        Span::styled(PROMPT_GLYPH, theme.bold_accent()),
        Span::raw(" "),
    ];
    spans.extend(input_spans(
        &f.input,
        avail,
        true,
        "type to find a file",
        theme,
    ));
    frame.render_widget(Paragraph::new(Line::from(spans)), input_area);
    right_aligned(frame, input_area, counter);
    rule(
        frame,
        Rect::new(inner.x, inner.y + 1, inner.width, 1),
        theme,
    );

    // Results.
    let rows = geometry::finder_rows(state.size);
    let results_width = geometry::results_width(state.size).min(inner.width);
    let list = Rect::new(inner.x, inner.y + 2, results_width, rows);
    if f.results.is_empty() {
        let text = if f.walking {
            "walking…"
        } else if f.input.is_empty() {
            "no files"
        } else {
            "no matches"
        };
        frame.render_widget(
            Paragraph::new(Line::from(vec![
                Span::raw("   "),
                Span::styled(text, theme.muted()),
            ])),
            list,
        );
    }
    for i in 0..rows {
        let rank = f.scroll + u32::from(i);
        let Some(result) = f.result_at(rank) else {
            continue;
        };
        let selected = rank == f.selected;
        let row = Rect::new(list.x, list.y + i, list.width, 1);
        if selected {
            fill_row(frame, row, theme);
        }
        let bg = row_style(selected, theme);
        let marked = f.is_marked(result);
        let mut spans = vec![
            Span::raw(" "),
            Span::styled(
                if selected { CURSOR_GLYPH } else { " " },
                theme.accent_style().patch(bg),
            ),
            Span::styled(
                if marked { MARK_GLYPH } else { " " },
                theme.accent_style().patch(bg),
            ),
        ];
        let max = usize::from(row.width).saturating_sub(4);
        spans.extend(path_spans(&result.rel, &result.indices, max, theme, bg));
        frame.render_widget(Paragraph::new(Line::from(spans)), row);
    }

    if let Some(area) = preview_rect(state) {
        draw_divider(frame, inner, results_width, theme);
        render_preview_pane(frame, area, state, theme);
    }

    // Footer.
    let footer = Rect::new(inner.x, inner.bottom() - 1, inner.width, 1);
    let mut spans = vec![Span::raw(" ")];
    spans.push(Span::styled("‹ctrl-r›", theme.muted()));
    spans.push(Span::styled(" scope ", theme.muted()));
    spans.push(Span::styled(scope_label(f.scope), theme.accent_style()));
    spans.push(Span::styled("  ‹alt-.›", theme.muted()));
    spans.push(Span::styled(" hidden ", theme.muted()));
    spans.push(toggle_value(f.show_hidden, theme));
    spans.push(Span::styled("  ‹alt-g›", theme.muted()));
    spans.push(Span::styled(" ignored ", theme.muted()));
    spans.push(toggle_value(!f.respect_gitignore, theme));
    if !f.marks.is_empty() {
        spans.push(Span::styled(
            format!("  {} marked", f.marks.len()),
            theme.bold_accent(),
        ));
    }
    if f.from_cache && !f.walking {
        spans.push(Span::styled("  cached", theme.muted()));
    }
    if f.denied > 0 {
        spans.push(Span::styled(
            format!("  {} denied", grouped(f.denied)),
            theme.muted(),
        ));
    }
    frame.render_widget(Paragraph::new(Line::from(spans)), footer);
}

fn toggle_value(on: bool, theme: &Theme) -> Span<'static> {
    if on {
        Span::styled(on_off(true), theme.bold_accent())
    } else {
        Span::styled(on_off(false), theme.muted())
    }
}

fn draw_divider(frame: &mut Frame, inner: Rect, results_width: u16, theme: &Theme) {
    let x = inner.x + results_width;
    if x >= inner.right() {
        return;
    }
    let height = inner.height.saturating_sub(3);
    let lines: Vec<Line> = (0..height)
        .map(|_| Line::from(Span::styled(DIVIDER_GLYPH, Style::new().fg(theme.border))))
        .collect();
    frame.render_widget(Paragraph::new(lines), Rect::new(x, inner.y + 2, 1, height));
}

/// The preview pane of the open overlay: the highlighted result, drawn by the main preview
/// renderer (the preview state follows `ks_core::preview_target`).
fn render_preview_pane(frame: &mut Frame, area: Rect, state: &AppState, theme: &Theme) {
    if area.is_empty() {
        return;
    }
    let Some(target) = preview_target(state) else {
        return;
    };
    crate::preview::render_overlay_pane(frame, area, state, theme, &target);
}

// ----- content search -----

fn render_grep(frame: &mut Frame, state: &AppState, theme: &Theme) {
    let Some(g) = state.search.grep.as_ref() else {
        return;
    };
    let rect = geometry::overlay_rect(state.size);
    let mut title = vec![Span::styled(
        "Search contents",
        Style::new().fg(theme.fg).add_modifier(Modifier::BOLD),
    )];
    let budget = usize::from(rect.width).saturating_sub(30);
    if budget > 8 {
        title.push(Span::styled(
            format!(" · {}", breadcrumb(&g.root, state.home.as_deref(), budget)),
            theme.dim(),
        ));
    }
    let inner = frame_overlay(frame, rect, title, theme);
    if inner.height < 5 || inner.width < 10 {
        return;
    }

    // Query line with the toggles as chips on the right.
    let query_area = Rect::new(inner.x, inner.y, inner.width, 1);
    let chips = vec![
        chip("regex", g.regex, theme),
        Span::raw(" "),
        chip("word", g.whole_word, theme),
        Span::raw(" "),
        Span::styled("smart-case ", theme.muted()),
    ];
    let chips_width: usize = chips.iter().map(|s| width(&s.content)).sum();
    let avail = usize::from(inner.width).saturating_sub(chips_width + 5);
    let query_active = g.field == GrepField::Query;
    let mut spans = vec![
        Span::raw(" "),
        Span::styled(
            PROMPT_GLYPH,
            if query_active {
                theme.bold_accent()
            } else {
                theme.muted()
            },
        ),
        Span::raw(" "),
    ];
    spans.extend(input_spans(
        &g.query,
        avail,
        query_active,
        "search text",
        theme,
    ));
    frame.render_widget(Paragraph::new(Line::from(spans)), query_area);
    right_aligned(frame, query_area, chips);

    // Glob line.
    let glob_area = Rect::new(inner.x, inner.y + 1, inner.width, 1);
    let glob_active = g.field == GrepField::Glob;
    let mut spans = vec![
        Span::raw(" "),
        Span::styled(
            PROMPT_GLYPH,
            if glob_active {
                theme.bold_accent()
            } else {
                theme.muted()
            },
        ),
        Span::styled(" glob ", theme.dim()),
    ];
    spans.extend(input_spans(
        &g.glob,
        usize::from(inner.width).saturating_sub(10),
        glob_active,
        "*.rs !target/**",
        theme,
    ));
    frame.render_widget(Paragraph::new(Line::from(spans)), glob_area);
    rule(
        frame,
        Rect::new(inner.x, inner.y + 2, inner.width, 1),
        theme,
    );

    // Results.
    let rows = geometry::grep_rows(state.size);
    let results_width = geometry::results_width(state.size).min(inner.width);
    let list = Rect::new(inner.x, inner.y + 3, results_width, rows);
    if g.rows.is_empty() {
        let text = if g.waiting || g.running {
            "searching…"
        } else if g.error.is_some() {
            ""
        } else if g.query.is_empty() && !g.glob.text().is_empty() {
            // The glob only narrows the files that are searched; listing files is find file's job.
            "type text to search the files the glob matches"
        } else if g.query.is_empty() {
            "type to search file contents"
        } else {
            "no matches"
        };
        frame.render_widget(
            Paragraph::new(Line::from(vec![
                Span::raw("   "),
                Span::styled(text, theme.muted()),
            ])),
            list,
        );
    }
    render_grep_rows(frame, list, g, theme);

    if let Some(area) = preview_rect(state) {
        let height = inner.height.saturating_sub(4);
        let x = inner.x + results_width;
        let lines: Vec<Line> = (0..height)
            .map(|_| Line::from(Span::styled(DIVIDER_GLYPH, Style::new().fg(theme.border))))
            .collect();
        frame.render_widget(Paragraph::new(lines), Rect::new(x, inner.y + 3, 1, height));
        render_preview_pane(frame, area, state, theme);
    }

    render_grep_footer(
        frame,
        Rect::new(inner.x, inner.bottom() - 1, inner.width, 1),
        state,
        g,
        theme,
    );
}

fn render_grep_rows(frame: &mut Frame, list: Rect, g: &GrepState, theme: &Theme) {
    for i in 0..list.height {
        let index = g.scroll + usize::from(i);
        let Some(row) = g.rows.get(index) else {
            break;
        };
        let Some(file) = g.files.get(row.file as usize) else {
            continue;
        };
        let area = Rect::new(list.x, list.y + i, list.width, 1);
        let line = match row.line {
            None => {
                let count = file.lines.len();
                let rel = file.rel.to_string_lossy();
                let budget = usize::from(area.width).saturating_sub(count.to_string().len() + 6);
                let mut spans = vec![Span::raw(" ")];
                spans.push(Span::styled(
                    crate::text::truncate_start(&sanitize(&rel), budget),
                    Style::new().fg(theme.dir).add_modifier(Modifier::BOLD),
                ));
                spans.push(Span::styled(format!("  {count}"), theme.muted()));
                Line::from(spans)
            }
            Some(line_index) => {
                let Some(hit) = file.lines.get(line_index as usize) else {
                    continue;
                };
                let selected = index == g.selected;
                if selected {
                    fill_row(frame, area, theme);
                }
                let bg = row_style(selected, theme);
                let marked = g.is_marked(&file.path, hit.line_number);
                let mut spans = vec![
                    Span::styled(
                        if marked { MARK_GLYPH } else { " " },
                        theme.accent_style().patch(bg),
                    ),
                    Span::styled(
                        if selected { CURSOR_GLYPH } else { " " },
                        theme.accent_style().patch(bg),
                    ),
                    Span::styled(format!("{:>4}", hit.line_number), theme.dim().patch(bg)),
                    Span::styled("│ ", Style::new().fg(theme.border).patch(bg)),
                ];
                let avail = usize::from(area.width).saturating_sub(GUTTER + 1);
                spans.extend(hit_spans(&hit.text, &hit.ranges, avail, theme, bg));
                Line::from(spans)
            }
        };
        frame.render_widget(Paragraph::new(line), area);
    }
}

/// The text of a matching line in `avail` cells with the matches highlighted. A match that
/// would be cut off by the right edge scrolls the line so it stays visible.
fn hit_spans(
    text: &str,
    ranges: &[std::ops::Range<usize>],
    avail: usize,
    theme: &Theme,
    bg: Style,
) -> Vec<Span<'static>> {
    let chars = clusters(text);
    let matched = |c: &Cluster| {
        ranges
            .iter()
            .any(|r| r.start < c.byte + c.text.len() && c.byte < r.end)
    };
    // Skip leading characters so the first match starts near the left edge when it would not
    // fit otherwise.
    let mut skip = 0;
    if let Some(first) = ranges.first() {
        let first_char = chars
            .iter()
            .position(|c| c.byte + c.text.len() > first.start)
            .unwrap_or(0);
        let before: usize = chars[..first_char].iter().map(|c| c.w).sum();
        if before + 8 > avail {
            skip = first_char.saturating_sub(6);
        }
    }
    let mut spans = Vec::new();
    let mut used = 0;
    if skip > 0 {
        spans.push(Span::styled("…", theme.muted().patch(bg)));
        used += 1;
    }
    let mut run = String::new();
    let mut run_hl = false;
    let style_of = |hl: bool| {
        if hl {
            theme.bold_accent().patch(bg)
        } else {
            Style::new().fg(theme.fg).patch(bg)
        }
    };
    let mut truncated = false;
    for c in chars.iter().skip(skip) {
        let w = c.w;
        if used + w + 1 > avail && used + w > avail {
            truncated = true;
            break;
        }
        let hl = matched(c);
        if hl != run_hl && !run.is_empty() {
            spans.push(Span::styled(std::mem::take(&mut run), style_of(run_hl)));
        }
        run_hl = hl;
        run.push_str(c.text);
        used += w;
    }
    if !run.is_empty() {
        spans.push(Span::styled(run, style_of(run_hl)));
    }
    if truncated {
        // Replace the last cell with an ellipsis.
        if let Some(last) = spans.last_mut() {
            let mut content = last.content.to_string();
            content.pop();
            content.push('…');
            *last = Span::styled(content, last.style);
        }
    }
    spans
}

fn render_grep_footer(
    frame: &mut Frame,
    area: Rect,
    state: &AppState,
    g: &GrepState,
    theme: &Theme,
) {
    let mut spans = vec![Span::raw(" ")];
    let budget = usize::from(area.width).saturating_sub(2);
    if let Some(error) = &g.error {
        spans.push(Span::styled(
            truncate_end(error, budget),
            Style::new().fg(theme.error),
        ));
        frame.render_widget(Paragraph::new(Line::from(spans)), area);
        return;
    }
    let matches = g.match_count();
    let mut counts = format!(
        "{} {} in {} {}",
        grouped(matches as u64),
        if matches == 1 { "match" } else { "matches" },
        grouped(g.files.len() as u64),
        if g.files.len() == 1 { "file" } else { "files" },
    );
    if g.stats.files_searched > 0 {
        counts.push_str(&format!(" · searched {}", grouped(g.stats.files_searched)));
    }
    let mut skipped = Vec::new();
    for (n, what) in [
        (g.stats.skipped_cloud, "cloud"),
        (g.stats.denied, "denied"),
        (g.stats.binary, "binary"),
        (g.stats.skipped_large, "large"),
    ] {
        if n > 0 {
            skipped.push(format!("{} {what}", grouped(n)));
        }
    }
    if !skipped.is_empty() {
        counts.push_str(&format!(" · skipped {}", skipped.join(", ")));
    }
    if g.running || g.waiting {
        spans.push(Span::styled(
            format!("{} ", spinner(state)),
            theme.accent_style(),
        ));
    }
    if g.capped {
        let hint = "10,000+ matches: refine your search";
        let keep = budget.saturating_sub(width(hint) + 3);
        spans.push(Span::styled(truncate_end(&counts, keep), theme.dim()));
        spans.push(Span::styled(" · ", theme.muted()));
        spans.push(Span::styled(hint, Style::new().fg(theme.warning)));
    } else {
        let used: usize = spans.iter().map(|s| width(&s.content)).sum();
        spans.push(Span::styled(
            truncate_end(&counts, budget.saturating_sub(used)),
            theme.dim(),
        ));
    }
    frame.render_widget(Paragraph::new(Line::from(spans)), area);
}

#[cfg(test)]
mod tests;
