//! Running programs attached to the terminal (terminal editors, bulk rename): the TUI is
//! suspended (alternate screen and raw mode left), the program runs with the real stdio, and
//! the TUI is restored; the event loop then forces a full redraw.
//!
//! The crossterm event stream has a reader thread that would compete with the child for
//! console input, so the stream is dropped while the program runs and recreated afterwards.

use std::io;
use std::pin::Pin;
use std::sync::{Arc, Mutex, MutexGuard, PoisonError};
use std::task::{Context, Poll, Waker};

use crossterm::event::{Event, EventStream};
use futures_util::Stream;
use ks_core::config::ImageProtocol;
use ks_core::fileops::ForegroundSpec;
use ks_fs::launch::{run_foreground, CommandSpec};

use crate::terminal;

/// Something that can run a program in the foreground of the terminal.
pub trait Foreground {
    /// Suspends the TUI, runs `spec` to completion and restores the TUI. `Ok(code)` is the
    /// exit code (`None` when killed by a signal); `Err` means it could not run at all.
    fn run(&mut self, spec: &ForegroundSpec) -> Result<Option<i32>, String>;

    /// Called once, right after the first frame was drawn. The real terminal uses it to do the
    /// work that must not delay that frame (graphics query) and to start reading input.
    fn first_frame_drawn(&mut self) {}
}

type Slot = Arc<Mutex<Option<EventStream>>>;
/// The waker of a `poll_next` that found the slot empty (the deferred start-up).
type WakerSlot = Arc<Mutex<Option<Waker>>>;

fn lock_waker(slot: &WakerSlot) -> MutexGuard<'_, Option<Waker>> {
    slot.lock().unwrap_or_else(PoisonError::into_inner)
}

fn lock(slot: &Slot) -> MutexGuard<'_, Option<EventStream>> {
    slot.lock().unwrap_or_else(PoisonError::into_inner)
}

/// The terminal's key events, with the ability to stop reading while a child owns the console.
pub struct PausableEvents {
    slot: Slot,
    waker: WakerSlot,
}

impl PausableEvents {
    /// The stream for the event loop and the [`TerminalForeground`] that pauses it.
    pub fn new() -> (Self, TerminalForeground) {
        let slot: Slot = Arc::new(Mutex::new(Some(EventStream::new())));
        let waker = WakerSlot::default();
        (
            Self {
                slot: Arc::clone(&slot),
                waker: Arc::clone(&waker),
            },
            TerminalForeground {
                slot,
                waker,
                images: None,
                query: None,
            },
        )
    }

    /// Like [`new`](Self::new), but nothing reads the terminal until the loop reports its first
    /// frame ([`Foreground::first_frame_drawn`]). Then the graphics-protocol query runs (when
    /// `images` is set) on its own thread, which needs stdin to itself, and only after it the
    /// event stream starts and wakes the loop. The loop itself is never blocked by the query: it
    /// keeps drawing, ticking and handling messages, only terminal input waits (in the console
    /// buffer, so no key is lost; the query reader may consume keys typed during its few ms).
    pub fn deferred(images: Option<ImageProtocol>) -> (Self, TerminalForeground) {
        let slot: Slot = Arc::new(Mutex::new(None));
        let waker = WakerSlot::default();
        (
            Self {
                slot: Arc::clone(&slot),
                waker: Arc::clone(&waker),
            },
            TerminalForeground {
                slot,
                waker,
                images,
                query: None,
            },
        )
    }
}

impl Stream for PausableEvents {
    type Item = io::Result<Event>;

    fn poll_next(self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Option<Self::Item>> {
        let mut slot = lock(&self.slot);
        match slot.as_mut() {
            Some(stream) => Pin::new(stream).poll_next(cx),
            // While the start-up query runs, or a foreground program (synchronous inside the
            // loop). The starter wakes this task after it installs the stream; the waker is
            // stored while the slot is locked, so the wake-up cannot be missed.
            None => {
                *lock_waker(&self.waker) = Some(cx.waker().clone());
                Poll::Pending
            }
        }
    }
}

/// The real thing: leaves and re-enters the TUI around the child process.
pub struct TerminalForeground {
    slot: Slot,
    waker: WakerSlot,
    /// Protocol preference still to be queried after the first frame (`None`: no query).
    images: Option<ImageProtocol>,
    /// The start-up thread (graphics query, then the event stream).
    query: Option<std::thread::JoinHandle<()>>,
}

/// Installs the event stream (if none) and wakes the task waiting for it.
fn start_events(slot: &Slot, waker: &WakerSlot) {
    {
        let mut slot = lock(slot);
        if slot.is_none() {
            *slot = Some(EventStream::new());
        }
    }
    if let Some(waker) = lock_waker(waker).take() {
        waker.wake();
    }
}

impl Foreground for TerminalForeground {
    fn first_frame_drawn(&mut self) {
        let Some(preference) = self.images.take() else {
            start_events(&self.slot, &self.waker);
            return;
        };
        let (slot, waker) = (Arc::clone(&self.slot), Arc::clone(&self.waker));
        let spawned = std::thread::Builder::new()
            .name("ks-terminal-query".into())
            .spawn(move || {
                // Nothing else reads stdin until the stream exists, so the query owns it.
                crate::init_images(preference);
                start_events(&slot, &waker);
            });
        match spawned {
            Ok(handle) => self.query = Some(handle),
            Err(error) => {
                tracing::warn!(%error, "cannot start the terminal query thread");
                start_events(&self.slot, &self.waker);
            }
        }
    }

    fn run(&mut self, spec: &ForegroundSpec) -> Result<Option<i32>, String> {
        // A start-up query still in flight finishes first (it owns stdin), then the stream goes.
        if let Some(handle) = self.query.take() {
            let _ = handle.join();
        }
        // Dropping the stream stops its reader thread.
        drop(lock(&self.slot).take());
        terminal::suspend();
        let result = run_foreground(&CommandSpec {
            program: spec.program.clone(),
            args: spec.args.clone(),
        });
        let resumed = terminal::resume();
        // Keys typed while the program was exiting belong to it, not to the list.
        if resumed.is_ok() {
            while crossterm::event::poll(std::time::Duration::ZERO).unwrap_or(false) {
                if crossterm::event::read().is_err() {
                    break;
                }
            }
        }
        start_events(&self.slot, &self.waker);
        resumed.map_err(|e| format!("cannot restore the terminal: {e}"))?;
        match result {
            Ok(status) => Ok(status.code()),
            Err(error) => Err(format!("cannot run {}: {error}", spec.program)),
        }
    }
}

#[cfg(test)]
mod tests {
    use std::sync::atomic::{AtomicBool, Ordering};
    use std::task::Wake;

    use super::*;

    struct Flag(AtomicBool);

    impl Wake for Flag {
        fn wake(self: Arc<Self>) {
            self.0.store(true, Ordering::SeqCst);
        }
    }

    #[test]
    fn deferred_events_wait_for_the_first_frame_and_then_wake_the_loop() {
        let (mut events, mut foreground) =
            PausableEvents::deferred(Some(ImageProtocol::Halfblocks));
        let flag = Arc::new(Flag(AtomicBool::new(false)));
        let waker = Waker::from(Arc::clone(&flag));
        let mut cx = Context::from_waker(&waker);
        assert!(
            Pin::new(&mut events).poll_next(&mut cx).is_pending(),
            "no input is read before the first frame"
        );
        assert!(lock(&events.slot).is_none(), "no event stream exists yet");

        // The hook returns at once (the work happens on a thread) ...
        foreground.first_frame_drawn();
        // ... and when that thread is done the stream exists and the loop was woken.
        foreground
            .query
            .take()
            .expect("a start-up thread")
            .join()
            .unwrap();
        assert!(lock(&events.slot).is_some());
        assert!(flag.0.load(Ordering::SeqCst), "the waiting task was woken");
    }
}
