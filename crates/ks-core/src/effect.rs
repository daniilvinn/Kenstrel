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
    /// Start a find-file session (replaces the running one). The runner resolves `scope`
    /// against `cwd`, walks (or serves the walk cache), applies `query` and reports
    /// `Msg::FinderResults` for the window `offset..offset + limit`.
    FinderStart {
        gen: u64,
        scope: ks_search::Scope,
        cwd: PathBuf,
        show_hidden: bool,
        respect_gitignore: bool,
        query: String,
        offset: u32,
        limit: u32,
    },
    /// Change the query of the running find-file session `gen`.
    FinderQuery {
        gen: u64,
        query: String,
        offset: u32,
        limit: u32,
    },
    /// Ask the running find-file session `gen` for another window of results.
    FinderWindow { gen: u64, offset: u32, limit: u32 },
    /// Stop the find-file session (cancels its walk).
    FinderStop,
    /// Start a content search (cancels the running one); events return as `Msg::Grep`.
    GrepStart {
        gen: u64,
        root: PathBuf,
        pattern: String,
        regex: bool,
        whole_word: bool,
        glob: String,
        show_hidden: bool,
        respect_gitignore: bool,
    },
    /// Cancel the running content search.
    GrepCancel,
    /// Apply listing differences of the displayed directory to the walk cache (the
    /// incremental refresh of PLAN 6.2).
    WalkCacheChanges(Vec<ks_search::PathChange>),
    /// Open `path` in the editor, at `line` when given. Owned by P6; P3+P4 only emits it.
    OpenInEditor { path: PathBuf, line: Option<u32> },
    /// Open `path` with the default application. Owned by P6; P3+P4 only emits it.
    OpenDefault { path: PathBuf },
}
