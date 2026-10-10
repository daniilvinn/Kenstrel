//! Overlay geometry shared by `update` (how many result rows exist) and the renderer.
//!
//! All numbers are cells. The overlay is centred, leaves two columns and two rows of the main
//! screen visible around it, and shows a preview pane to the right of the results when the
//! terminal is at least [`PREVIEW_MIN_WIDTH`] wide (PLAN 4.3).

use crate::state::Size;

/// Terminal width from which the overlays show a preview pane.
pub const PREVIEW_MIN_WIDTH: u16 = 100;
/// The overlay never grows beyond this many columns / rows.
const MAX_WIDTH: u16 = 160;
const MAX_HEIGHT: u16 = 44;

/// A rectangle in terminal cells.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Cells {
    pub x: u16,
    pub y: u16,
    pub width: u16,
    pub height: u16,
}

/// The overlay's outer rectangle (border included).
pub fn overlay_rect(size: Size) -> Cells {
    let width = size
        .width
        .saturating_sub(4)
        .clamp(size.width.min(20), MAX_WIDTH);
    let height = size
        .height
        .saturating_sub(4)
        .clamp(size.height.min(8), MAX_HEIGHT);
    Cells {
        x: (size.width - width) / 2,
        y: (size.height - height) / 2,
        width,
        height,
    }
}

/// Whether the preview pane is shown next to the results.
pub const fn shows_preview(size: Size) -> bool {
    size.width >= PREVIEW_MIN_WIDTH
}

/// Width of the results pane inside the border (the rest, minus a divider, is the preview).
pub fn results_width(size: Size) -> u16 {
    let inner = overlay_rect(size).width.saturating_sub(2);
    if shows_preview(size) {
        inner * 11 / 20
    } else {
        inner
    }
}

/// Result rows of the find-file overlay: the border, the input line, a rule and the footer
/// take the rest.
pub fn finder_rows(size: Size) -> u16 {
    overlay_rect(size).height.saturating_sub(2 + 3)
}

/// Display lines of the content-search overlay: two input lines instead of one.
pub fn grep_rows(size: Size) -> u16 {
    overlay_rect(size).height.saturating_sub(2 + 4)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn eighty_by_twenty_four() {
        let rect = overlay_rect(Size::new(80, 24));
        assert_eq!(
            rect,
            Cells {
                x: 2,
                y: 2,
                width: 76,
                height: 20
            }
        );
        assert_eq!(finder_rows(Size::new(80, 24)), 15);
        assert_eq!(grep_rows(Size::new(80, 24)), 14);
        assert!(!shows_preview(Size::new(80, 24)));
    }

    #[test]
    fn wide_terminals_get_a_preview_and_a_capped_overlay() {
        let size = Size::new(200, 60);
        let rect = overlay_rect(size);
        assert_eq!((rect.width, rect.height), (160, 44));
        assert!(shows_preview(size));
        assert!(results_width(size) < rect.width - 2);
    }

    #[test]
    fn tiny_terminals_do_not_underflow() {
        for (w, h) in [(0, 0), (1, 1), (10, 5), (30, 6)] {
            let size = Size::new(w, h);
            let rect = overlay_rect(size);
            assert!(rect.x + rect.width <= w.max(rect.width));
            let _ = (finder_rows(size), grep_rows(size), results_width(size));
        }
    }
}
