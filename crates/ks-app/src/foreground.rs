//! Running programs attached to the terminal (terminal editors, bulk rename): the TUI is
//! suspended (alternate screen and raw mode left), the program runs with the real stdio, and
//! the TUI is restored; the event loop then forces a full redraw.
//!
//! The crossterm event stream has a reader thread that would compete with the child for
//! console input, so the stream is dropped while the program runs and recreated afterwards.

use std::io;
use std::pin::Pin;
use std::sync::{Arc, Mutex, MutexGuard, PoisonError};
use std::task::{Context, Poll};

use crossterm::event::{Event, EventStream};
use futures_util::Stream;
use ks_core::fileops::ForegroundSpec;
use ks_fs::launch::{run_foreground, CommandSpec};

use crate::terminal;

/// Something that can run a program in the foreground of the terminal.
pub trait Foreground {
    /// Suspends the TUI, runs `spec` to completion and restores the TUI. `Ok(code)` is the
    /// exit code (`None` when killed by a signal); `Err` means it could not run at all.
    fn run(&mut self, spec: &ForegroundSpec) -> Result<Option<i32>, String>;
}

type Slot = Arc<Mutex<Option<EventStream>>>;

fn lock(slot: &Slot) -> MutexGuard<'_, Option<EventStream>> {
    slot.lock().unwrap_or_else(PoisonError::into_inner)
}

/// The terminal's key events, with the ability to stop reading while a child owns the console.
pub struct PausableEvents {
    slot: Slot,
}

impl PausableEvents {
    /// The stream for the event loop and the [`TerminalForeground`] that pauses it.
    pub fn new() -> (Self, TerminalForeground) {
        let slot: Slot = Arc::new(Mutex::new(Some(EventStream::new())));
        (
            Self {
                slot: Arc::clone(&slot),
            },
            TerminalForeground { slot },
        )
    }
}

impl Stream for PausableEvents {
    type Item = io::Result<Event>;

    fn poll_next(self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Option<Self::Item>> {
        match lock(&self.slot).as_mut() {
            Some(stream) => Pin::new(stream).poll_next(cx),
            // Only while a foreground program runs, which is synchronous inside the loop.
            None => Poll::Pending,
        }
    }
}

/// The real thing: leaves and re-enters the TUI around the child process.
pub struct TerminalForeground {
    slot: Slot,
}

impl Foreground for TerminalForeground {
    fn run(&mut self, spec: &ForegroundSpec) -> Result<Option<i32>, String> {
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
        *lock(&self.slot) = Some(EventStream::new());
        resumed.map_err(|e| format!("cannot restore the terminal: {e}"))?;
        match result {
            Ok(status) => Ok(status.code()),
            Err(error) => Err(format!("cannot run {}: {error}", spec.program)),
        }
    }
}
