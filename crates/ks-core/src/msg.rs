//! Everything that can happen to the app, as plain data.

use crate::generation::Gen;
use crate::key::KeyChord;
use crate::state::{JobId, Level, Size};

/// A delayed wake-up requested through [`Effect::StartTimer`](crate::effect::Effect).
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum TimerKind {
    /// Show the which-key popup for a pending key prefix.
    WhichKey,
    /// Remove the status-line message.
    StatusExpiry,
}

/// Input to [`update`](crate::update::update).
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Msg {
    /// A key press (already filtered to press/repeat by the backend).
    Key(KeyChord),
    /// The terminal was resized.
    Resize(Size),
    /// Spinner tick; only delivered while jobs are running.
    Tick,
    /// A timer started by an effect has fired. Stale generations are ignored.
    Timer {
        kind: TimerKind,
        gen: Gen,
    },
    /// Show a message in the status line.
    Notify {
        level: Level,
        text: String,
    },
    JobStarted(JobId),
    JobFinished(JobId),
    /// The process was asked to stop (ctrl-c outside raw mode, console close).
    Interrupt,
}
