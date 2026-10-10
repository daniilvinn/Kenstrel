//! A cursor with a scroll offset over a list of known length.

/// Rows kept between the cursor and the edge of the viewport while scrolling.
pub const SCROLL_MARGIN: usize = 3;

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Cursor {
    /// Index of the highlighted row.
    pub index: usize,
    /// Index of the first row drawn.
    pub offset: usize,
}

impl Cursor {
    /// Moves by `delta` rows, clamped to `0..len`.
    pub fn move_by(&mut self, delta: isize, len: usize) {
        let target = self.index.saturating_add_signed(delta);
        self.set(target, len);
    }

    /// Jumps to `index`, clamped to the last row.
    pub fn set(&mut self, index: usize, len: usize) {
        self.index = index.min(len.saturating_sub(1));
    }

    /// Scrolls the minimum needed to keep the cursor inside the viewport with a margin.
    pub fn ensure_visible(&mut self, len: usize, height: usize) {
        if height == 0 {
            return;
        }
        let margin = SCROLL_MARGIN.min((height - 1) / 2);
        if self.index < self.offset + margin {
            self.offset = self.index.saturating_sub(margin);
        } else if self.index + margin >= self.offset + height {
            self.offset = self.index + margin + 1 - height;
        }
        self.offset = self.offset.min(len.saturating_sub(height));
    }

    /// Like [`ensure_visible`](Self::ensure_visible), but a cursor outside the viewport is
    /// centred instead (used after a jump to a distant row).
    pub fn reveal(&mut self, len: usize, height: usize) {
        if height == 0 {
            return;
        }
        if self.index < self.offset || self.index >= self.offset + height {
            self.offset = self.index.saturating_sub(height / 2);
        }
        self.ensure_visible(len, height);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn movement_is_clamped() {
        let mut c = Cursor::default();
        c.move_by(-1, 10);
        assert_eq!(c.index, 0);
        c.move_by(25, 10);
        assert_eq!(c.index, 9);
        c.move_by(-100, 10);
        assert_eq!(c.index, 0);
        c.move_by(3, 0);
        assert_eq!(c.index, 0);
    }

    #[test]
    fn scrolls_when_the_cursor_nears_the_bottom_edge() {
        let mut c = Cursor::default();
        let (len, height) = (100, 10);
        for _ in 0..6 {
            c.move_by(1, len);
            c.ensure_visible(len, height);
        }
        // Index 6 with a margin of 3 in a 10-row window: still no scrolling needed.
        assert_eq!((c.index, c.offset), (6, 0));
        c.move_by(1, len);
        c.ensure_visible(len, height);
        assert_eq!((c.index, c.offset), (7, 1));
    }

    #[test]
    fn scrolls_back_up_with_the_margin() {
        let mut c = Cursor {
            index: 50,
            offset: 45,
        };
        c.move_by(-3, 100);
        c.ensure_visible(100, 10);
        assert_eq!((c.index, c.offset), (47, 44));
    }

    #[test]
    fn never_scrolls_past_the_end() {
        let mut c = Cursor::default();
        c.set(99, 100);
        c.ensure_visible(100, 10);
        assert_eq!(c.offset, 90);
        // A short list never scrolls.
        let mut s = Cursor::default();
        s.set(4, 5);
        s.ensure_visible(5, 10);
        assert_eq!(s.offset, 0);
    }

    #[test]
    fn margin_shrinks_in_tiny_viewports() {
        let mut c = Cursor::default();
        c.set(5, 50);
        c.ensure_visible(50, 1);
        assert_eq!(c.offset, 5);
        c.ensure_visible(50, 0);
        assert_eq!(c.offset, 5);
    }

    #[test]
    fn reveal_centres_distant_rows_only() {
        let mut c = Cursor::default();
        c.set(500, 1000);
        c.reveal(1000, 20);
        assert_eq!(c.offset, 490);
        // Already inside the window: unchanged.
        c.set(495, 1000);
        c.reveal(1000, 20);
        assert_eq!(c.offset, 490);
    }
}
