//! The effect runner: performs [`Effect`]s on background tasks and reports back as
//! [`Msg`]s.
//!
//! Every effect runs under a [`CancellationToken`] so it can be replaced, cancelled, or
//! torn down at shutdown without leaking tasks.

use std::collections::HashMap;

use ks_core::{Effect, Msg, TimerKind};
use tokio::sync::mpsc::Sender;
use tokio::time::sleep;
use tokio_util::sync::CancellationToken;

pub struct EffectRunner {
    tx: Sender<Msg>,
    /// Cancels every task at shutdown; per-effect tokens are children of it.
    shutdown: CancellationToken,
    timers: HashMap<TimerKind, CancellationToken>,
}

impl EffectRunner {
    pub fn new(tx: Sender<Msg>) -> Self {
        Self {
            tx,
            shutdown: CancellationToken::new(),
            timers: HashMap::new(),
        }
    }

    /// Starts the given effects. Must be called from within a tokio runtime.
    pub fn run(&mut self, effects: impl IntoIterator<Item = Effect>) {
        for effect in effects {
            match effect {
                Effect::StartTimer { kind, gen, delay } => {
                    let token = self.shutdown.child_token();
                    if let Some(previous) = self.timers.insert(kind, token.clone()) {
                        previous.cancel();
                    }
                    let tx = self.tx.clone();
                    tokio::spawn(async move {
                        tokio::select! {
                            () = token.cancelled() => {}
                            () = sleep(delay) => {
                                // A closed channel means the app is exiting.
                                let _ = tx.send(Msg::Timer { kind, gen }).await;
                            }
                        }
                    });
                }
                Effect::CancelTimer(kind) => {
                    if let Some(token) = self.timers.remove(&kind) {
                        token.cancel();
                    }
                }
            }
        }
    }

    /// Cancels everything still running.
    pub fn shutdown(&mut self) {
        self.shutdown.cancel();
        self.timers.clear();
    }
}

#[cfg(test)]
mod tests {
    use std::time::Duration;

    use ks_core::GenCounter;
    use tokio::sync::mpsc;

    use super::*;

    fn timer(kind: TimerKind, gens: &mut GenCounter, ms: u64) -> Effect {
        Effect::StartTimer {
            kind,
            gen: gens.bump(),
            delay: Duration::from_millis(ms),
        }
    }

    #[tokio::test(start_paused = true)]
    async fn timer_fires_after_its_delay() {
        let (tx, mut rx) = mpsc::channel(8);
        let mut runner = EffectRunner::new(tx);
        let mut gens = GenCounter::default();
        let effect = timer(TimerKind::WhichKey, &mut gens, 300);
        let Effect::StartTimer { gen, .. } = effect else {
            unreachable!()
        };
        runner.run([effect]);

        tokio::time::sleep(Duration::from_millis(299)).await;
        assert!(rx.try_recv().is_err(), "must not fire early");
        tokio::time::sleep(Duration::from_millis(2)).await;
        assert_eq!(
            rx.recv().await,
            Some(Msg::Timer {
                kind: TimerKind::WhichKey,
                gen
            })
        );
    }

    #[tokio::test(start_paused = true)]
    async fn cancelled_timer_never_fires() {
        let (tx, mut rx) = mpsc::channel(8);
        let mut runner = EffectRunner::new(tx);
        let mut gens = GenCounter::default();
        runner.run([
            timer(TimerKind::WhichKey, &mut gens, 300),
            Effect::CancelTimer(TimerKind::WhichKey),
        ]);
        tokio::time::sleep(Duration::from_secs(1)).await;
        assert!(rx.try_recv().is_err());
    }

    #[tokio::test(start_paused = true)]
    async fn restarting_a_timer_replaces_the_previous_one() {
        let (tx, mut rx) = mpsc::channel(8);
        let mut runner = EffectRunner::new(tx);
        let mut gens = GenCounter::default();
        runner.run([timer(TimerKind::StatusExpiry, &mut gens, 100)]);
        let second = timer(TimerKind::StatusExpiry, &mut gens, 100);
        let Effect::StartTimer { gen, .. } = second else {
            unreachable!()
        };
        runner.run([second]);

        tokio::time::sleep(Duration::from_millis(200)).await;
        assert_eq!(
            rx.recv().await,
            Some(Msg::Timer {
                kind: TimerKind::StatusExpiry,
                gen
            })
        );
        assert!(
            rx.try_recv().is_err(),
            "the replaced timer must stay silent"
        );
    }

    #[tokio::test(start_paused = true)]
    async fn different_kinds_run_independently_and_shutdown_cancels_all() {
        let (tx, mut rx) = mpsc::channel(8);
        let mut runner = EffectRunner::new(tx);
        let mut gens = GenCounter::default();
        runner.run([
            timer(TimerKind::WhichKey, &mut gens, 100),
            timer(TimerKind::StatusExpiry, &mut gens, 100),
        ]);
        runner.shutdown();
        tokio::time::sleep(Duration::from_secs(1)).await;
        assert!(rx.try_recv().is_err());
    }
}
