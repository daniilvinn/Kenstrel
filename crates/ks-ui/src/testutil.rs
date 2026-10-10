//! Snapshot helper: renders a ratatui buffer as text plus a compact style legend, so that
//! snapshots catch colour regressions (and theme differences) as well as layout ones.

use ratatui::buffer::Buffer;
use ratatui::style::{Color, Modifier, Style};

/// Text rows followed by one line per run of identically styled, non-default cells.
pub fn dump(buffer: &Buffer) -> String {
    let area = buffer.area;
    let mut out = String::new();
    let mut runs = String::new();

    for y in 0..area.height {
        let mut row = String::new();
        let mut run_start = 0u16;
        let mut run_style: Option<Style> = None;
        let flush = |runs: &mut String, y: u16, from: u16, to: u16, style: Option<Style>| {
            if let Some(style) = style {
                if style != Style::default() && to > from {
                    runs.push_str(&format!("{y:>3} {from:>3}..{to:<3} {}\n", describe(style)));
                }
            }
        };
        for x in 0..area.width {
            let cell = &buffer[(x, y)];
            row.push_str(cell.symbol());
            let style = Style::new()
                .fg(cell.fg)
                .bg(cell.bg)
                .add_modifier(cell.modifier);
            let normalised = if cell.symbol() == " " && cell.bg == Color::Reset {
                // Spaces have no visible foreground; do not let them split runs.
                run_style.unwrap_or_default()
            } else {
                style
            };
            if run_style != Some(normalised) {
                flush(&mut runs, y, run_start, x, run_style);
                run_start = x;
                run_style = Some(normalised);
            }
        }
        flush(&mut runs, y, run_start, area.width, run_style);
        out.push_str(row.trim_end());
        out.push('\n');
    }
    format!("{out}--- styles (row cols fg bg mods) ---\n{runs}")
}

fn describe(style: Style) -> String {
    let fg = style.fg.map_or_else(|| "-".to_owned(), color_name);
    let bg = style.bg.map_or_else(|| "-".to_owned(), color_name);
    let mods = if style.add_modifier.is_empty() {
        String::new()
    } else {
        format!(" {}", modifier_name(style.add_modifier))
    };
    format!("fg={fg} bg={bg}{mods}")
}

fn color_name(color: Color) -> String {
    match color {
        Color::Rgb(r, g, b) => format!("#{r:02x}{g:02x}{b:02x}"),
        Color::Indexed(i) => format!("idx{i}"),
        Color::Reset => "reset".to_owned(),
        other => format!("{other:?}").to_lowercase(),
    }
}

fn modifier_name(m: Modifier) -> String {
    let mut names = Vec::new();
    if m.contains(Modifier::BOLD) {
        names.push("bold");
    }
    if m.contains(Modifier::DIM) {
        names.push("dim");
    }
    if m.contains(Modifier::ITALIC) {
        names.push("italic");
    }
    if m.contains(Modifier::UNDERLINED) {
        names.push("underline");
    }
    if m.contains(Modifier::REVERSED) {
        names.push("reverse");
    }
    names.join("+")
}
