//! Shared drawing helpers for floating boxes (palette, help, which-key).

use ratatui::layout::Rect;
use ratatui::style::Style;
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, BorderType, Borders, Clear};
use ratatui::Frame;

use crate::text::width;
use crate::theme::Theme;

/// A `w` by `h` rectangle centered in `area` (clamped to it).
pub fn centered(area: Rect, w: u16, h: u16) -> Rect {
    let w = w.min(area.width);
    let h = h.min(area.height);
    Rect::new(
        area.x + (area.width - w) / 2,
        area.y + (area.height - h) / 2,
        w,
        h,
    )
}

/// A double-width character just left of `rect` would spill into the box; blank it so the
/// border is not swallowed by its second cell.
fn clear_wide_edge(frame: &mut Frame, rect: Rect) {
    if rect.x == 0 {
        return;
    }
    let buf = frame.buffer_mut();
    let x = rect.x - 1;
    for y in rect.y..rect.bottom() {
        if buf.area.contains((x, y).into()) && width(buf[(x, y)].symbol()) > 1 {
            buf[(x, y)].set_symbol(" ");
        }
    }
}

/// Clears `rect`, draws a rounded border with `─ title ` in the top edge and returns the
/// inner area. `bottom` is drawn right-aligned in the bottom edge.
pub fn framed(
    frame: &mut Frame,
    rect: Rect,
    title: &str,
    bottom: Option<&str>,
    focus: bool,
    theme: &Theme,
) -> Rect {
    clear_wide_edge(frame, rect);
    frame.render_widget(Clear, rect);
    let border = Style::new().fg(if focus {
        theme.border_focus
    } else {
        theme.border
    });
    let mut block = Block::new()
        .borders(Borders::ALL)
        .border_type(BorderType::Rounded)
        .border_style(border)
        .style(theme.base_style())
        .title(Line::from(vec![
            Span::styled("─ ", border),
            Span::styled(title.to_owned(), theme.bold_accent()),
            Span::raw(" "),
        ]));
    if let Some(bottom) = bottom {
        block = block.title_bottom(
            Line::from(vec![
                Span::raw(" "),
                Span::styled(bottom.to_owned(), theme.muted()),
                Span::raw(" "),
                Span::styled("─", border),
            ])
            .right_aligned(),
        );
    }
    let inner = block.inner(rect);
    frame.render_widget(block, rect);
    inner
}
