//! Work requested by `update` and performed by the application's effect runner.
//!
//! Effects never touch state. They run off the UI thread and report back as
//! [`Msg`](crate::msg::Msg)s tagged with the generation they were started under.

use std::ffi::OsString;
use std::path::PathBuf;
use std::time::Duration;

use crate::generation::Gen;
use crate::msg::TimerKind;

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Effect {
    /// Deliver `Msg::Timer { kind, gen }` after `delay`. Starting a timer of a kind that is
    /// already running replaces it.
    StartTimer {
        kind: TimerKind,
        gen: Gen,
        delay: Duration,
    },
    /// Cancel the running timer of this kind, if any.
    CancelTimer(TimerKind),
    /// List `path`, reporting `Msg::ListingChunk`/`ListingFinished` tagged with `gen`.
    /// Replaces (cancels) any listing or drive query still running.
    ListDir { gen: Gen, path: PathBuf },
    /// Query the drives, reporting `Msg::DrivesLoaded`. Shares the listing slot with
    /// [`Effect::ListDir`].
    ListDrives { gen: Gen },
    /// Watch `path` (not recursively) and report `Msg::DirChanged { gen }`. Replaces the
    /// previous watch.
    WatchDir { gen: Gen, path: PathBuf },
    /// Stop watching.
    StopWatching,
    /// Run `git status` in `dir`, reporting `Msg::GitStatus`. Replaces a running one.
    GitStatus { gen: Gen, dir: PathBuf },
    /// Count the children of the directories `names` below `dir`, reporting
    /// `Msg::ChildCounts` in batches. Replaces a running count.
    CountChildren {
        gen: Gen,
        dir: PathBuf,
        names: Vec<OsString>,
    },
}
