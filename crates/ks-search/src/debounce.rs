//! A tiny, pure debouncer (no timers or threads): the UI loop feeds it timestamps.

use std::time::{Duration, Instant};

/// Debounce helper. Call [`Debouncer::trigger`] on every input; ask [`Debouncer::poll`] from
/// the event loop (and use [`Debouncer::deadline`] as the loop's sleep deadline).
#[derive(Debug, Clone)]
pub struct Debouncer {
    delay: Duration,
    deadline: Option<Instant>,
}

impl Debouncer {
    /// The 120 ms delay from PLAN.md section 6.3.
    pub const DEFAULT_DELAY: Duration = Duration::from_millis(120);

    /// New debouncer with the given quiet period.
    pub fn new(delay: Duration) -> Self {
        Self {
            delay,
            deadline: None,
        }
    }

    /// Records an input at `now`, pushing the deadline out.
    pub fn trigger(&mut self, now: Instant) {
        self.deadline = Some(now + self.delay);
    }

    /// When the pending action becomes due, if any.
    pub fn deadline(&self) -> Option<Instant> {
        self.deadline
    }

    /// Returns `true` exactly once when the quiet period has elapsed.
    pub fn poll(&mut self, now: Instant) -> bool {
        match self.deadline {
            Some(d) if now >= d => {
                self.deadline = None;
                true
            }
            _ => false,
        }
    }

    /// Drops any pending action.
    pub fn cancel(&mut self) {
        self.deadline = None;
    }
}

impl Default for Debouncer {
    fn default() -> Self {
        Self::new(Self::DEFAULT_DELAY)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn fires_once_after_quiet_period() {
        let t0 = Instant::now();
        let mut d = Debouncer::new(Duration::from_millis(100));
        assert!(!d.poll(t0));
        d.trigger(t0);
        assert!(!d.poll(t0 + Duration::from_millis(50)));
        d.trigger(t0 + Duration::from_millis(50));
        assert!(!d.poll(t0 + Duration::from_millis(120)));
        assert!(d.poll(t0 + Duration::from_millis(151)));
        assert!(!d.poll(t0 + Duration::from_millis(300)));
    }

    #[test]
    fn cancel_clears() {
        let t0 = Instant::now();
        let mut d = Debouncer::default();
        d.trigger(t0);
        d.cancel();
        assert!(d.deadline().is_none());
    }
}
