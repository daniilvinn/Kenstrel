//! Screen geometry shared by `update` (scrolling needs the list height) and the renderer.

use crate::state::Size;

/// The hint line is hidden on terminals shorter than this (PLAN §4.2).
pub const HINT_MIN_HEIGHT: u16 = 20;
/// Terminals narrower than this hide the size column (PLAN §4.2).
pub const SIZES_MIN_WIDTH: u16 = 60;

/// Rows above the list: the header and its rule.
const TOP_ROWS: u16 = 2;
/// Rows below the list: the rule and the prompt line.
const BOTTOM_ROWS: u16 = 2;

/// Whether the hint line is drawn at this height.
pub const fn shows_hint_line(height: u16) -> bool {
    height >= HINT_MIN_HEIGHT
}

/// Number of rows available to the list for a terminal of `size`.
pub const fn body_height(size: Size) -> u16 {
    let chrome = TOP_ROWS + BOTTOM_ROWS + shows_hint_line(size.height) as u16;
    size.height.saturating_sub(chrome)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn body_height_accounts_for_chrome() {
        assert_eq!(body_height(Size::new(80, 24)), 19);
        assert_eq!(body_height(Size::new(80, 19)), 15);
        assert_eq!(body_height(Size::new(80, 20)), 15);
        assert_eq!(body_height(Size::new(80, 3)), 0);
        assert_eq!(body_height(Size::new(80, 0)), 0);
    }
}
