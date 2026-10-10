//! Generation ids: monotonically increasing tickets used to drop stale results.
//!
//! Every long-running or restartable job (timer, search, preview, ...) is tagged with the
//! [`Gen`] that was current when it was started. Results carry that tag back in a `Msg`;
//! `update` compares it with the counter and ignores anything that is no longer current.

/// An opaque generation ticket.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct Gen(u64);

/// Issues [`Gen`] tickets and answers "is this ticket still the latest one?".
#[derive(Clone, Debug, Default)]
pub struct GenCounter {
    current: Gen,
}

impl GenCounter {
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
}
