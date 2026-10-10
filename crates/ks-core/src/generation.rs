//! Generation ids: monotonically increasing tickets used to drop stale results.
//!
//! Every long-running or restartable job (timer, search, preview, ...) is tagged with the
//! [`Gen`] that was current when it was started. Results carry that tag back in a `Msg`;
//! `update` compares it with the counter and ignores anything that is no longer current.

/// An opaque generation ticket.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct Gen(u64);

impl Gen {
    /// The ticket as a plain number, for engines that carry generations as `u64`.
    pub const fn raw(self) -> u64 {
        self.0
    }

    /// Rebuilds a ticket from [`raw`](Self::raw).
    pub const fn from_raw(raw: u64) -> Self {
        Self(raw)
    }
}

/// Issues [`Gen`] tickets and answers "is this ticket still the latest one?".
#[derive(Clone, Debug, Default)]
pub struct GenCounter {
    current: Gen,
}

impl GenCounter {
    /// A counter whose tickets can never equal those of a counter in another `space`
    /// (each tab uses its own space, so a stale result from one tab is not mistaken for a
    /// current one of another tab).
    pub fn in_space(space: u32) -> Self {
        Self {
            current: Gen(u64::from(space) << 32),
        }
    }

    /// Invalidates all previously issued tickets and returns the new current one.
    pub fn bump(&mut self) -> Gen {
        self.current = Gen(self.current.0 + 1);
        self.current
    }

    /// The most recently issued ticket.
    pub fn current(&self) -> Gen {
        self.current
    }

    /// True when `gen` has not been superseded by a later [`bump`](Self::bump).
    pub fn is_current(&self, gen: Gen) -> bool {
        self.current == gen
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn bump_invalidates_older_tickets() {
        let mut counter = GenCounter::default();
        let first = counter.bump();
        assert!(counter.is_current(first));
        let second = counter.bump();
        assert!(!counter.is_current(first));
        assert!(counter.is_current(second));
        assert_eq!(counter.current(), second);
        assert!(first < second);
    }

    #[test]
    fn spaces_never_collide() {
        let mut a = GenCounter::in_space(1);
        let mut b = GenCounter::in_space(2);
        let (ta, tb) = (a.bump(), b.bump());
        assert_ne!(ta, tb);
        assert!(!b.is_current(ta));
        assert!(!a.is_current(tb));
    }
}
