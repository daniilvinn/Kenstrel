//! Work requested by `update` and performed by the application's effect runner.
//!
//! Effects never touch state. They run off the UI thread and report back as
//! [`Msg`](crate::msg::Msg)s tagged with the generation they were started under.

use std::ffi::OsString;
use std::path::PathBuf;
use std::time::Duration;

use crate::fileops::{ConflictAnswer, ForegroundSpec, JobRequest, PathFormatKind};
use crate::generation::Gen;
use crate::msg::TimerKind;
use crate::state::JobId;

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
    // ----- file operations (P6) -----
    /// Start a job (copy, move, trash, delete, bulk rename). `keep_both` answers conflicts
    /// with `name (1).ext` instead of asking.
    SubmitJob {
        request: JobRequest,
        keep_both: bool,
    },
    /// Rename `from` to `new_name` (a bare name, validated by the runner).
    Rename { from: PathBuf, new_name: String },
    /// Create what `input` describes below `base` (`a/b.txt`; a trailing `/` makes a folder).
    Create { base: PathBuf, input: String },
    /// Undo the last file operation.
    Undo,
    /// Cancel a job.
    CancelJob(JobId),
    /// Answer the conflict prompt of `job`.
    ResolveConflict {
        job: JobId,
        answer: ConflictAnswer,
        apply_to_all: bool,
    },
    /// Open with the default application (`o`, `enter` on a file).
    OpenDefault { path: PathBuf },
    /// Open in the editor, at `line` when given. Terminal editors suspend the TUI.
    OpenInEditor { path: PathBuf, line: Option<u32> },
    /// Look up the programs `path` can be opened with, answered by `Msg::OpenHandlers`.
    ListOpenHandlers { path: PathBuf },
    /// Open `path` with the handler of that name.
    OpenWith { path: PathBuf, handler: String },
    /// Put the paths on the clipboard.
    CopyPaths {
        paths: Vec<PathBuf>,
        format: PathFormatKind,
        cwd: PathBuf,
    },
    /// Run a shell command, streaming `Msg::Shell` events tagged with `id`.
    RunShell {
        id: u64,
        command: String,
        cwd: PathBuf,
    },
    /// Kill the shell command with this id.
    CancelShell(u64),
    /// Write the names to a temp file and open the editor on it (bulk rename).
    BulkRenameEdit { dir: PathBuf, names: Vec<String> },
    /// Validate the edited bulk-rename file, answering `Msg::BulkPlanned`; the file is
    /// deleted. `error` is set when the editor could not run.
    BulkRenamePlan {
        dir: PathBuf,
        names: Vec<String>,
        file: PathBuf,
        error: Option<String>,
    },
    /// Suspend the TUI, run the program attached to the terminal, restore the TUI and force a
    /// full redraw, then deliver `Msg::ForegroundDone { id }`. Handled by the event loop
    /// (it owns the terminal), not by the effect runner.
    RunForeground { id: u64, spec: ForegroundSpec },
}
