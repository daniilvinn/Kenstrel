//! Work requested by `update` and performed by the application's effect runner.
//!
//! Effects never touch state. They run off the UI thread and report back as
//! [`Msg`](crate::msg::Msg)s tagged with the generation they were started under.

use std::ffi::OsString;
use std::path::PathBuf;
use std::time::Duration;

use crate::claude::{ClaudeRequest, ClaudeResponse, ClaudeSpawn, IpcContext};
use crate::generation::Gen;
use crate::key::KeyChord;
use crate::msg::TimerKind;
use crate::state::Size;

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
    /// Enable or disable bracketed paste in the terminal (on while a pane has focus).
    SetBracketedPaste(bool),
    /// Run `claude auth status`, reporting `Msg::ClaudePreflight`.
    ClaudePreflight { generation: u64, spawn: ClaudeSpawn },
    /// Start the embedded pane, reporting `Msg::ClaudeStarted` or `Msg::ClaudeLaunchFailed`.
    /// Later output arrives as `Msg::ClaudeScreen` / `Msg::ClaudePaneExited`.
    ClaudeLaunch {
        generation: u64,
        cwd: PathBuf,
        resume: bool,
        size: Size,
        spawn: ClaudeSpawn,
    },
    /// Open Claude in a Windows Terminal split (10a); failures come back as `Msg::Notify`.
    ClaudeWtSplit { cwd: PathBuf, spawn: ClaudeSpawn },
    /// Send a key press to the pane verbatim.
    ClaudeKey(KeyChord),
    /// Send pasted text to the pane.
    ClaudePaste(String),
    /// Type `@` mentions for these paths into the pane.
    ClaudeSendPaths { paths: Vec<PathBuf> },
    /// Type an `@path#Lstart-end` mention into the pane.
    ClaudeSendRange {
        path: PathBuf,
        start: usize,
        end: usize,
    },
    /// Resize the pane's pseudo console.
    ClaudeResize { size: Size },
    /// Scroll the pane's history (positive = towards older output).
    ClaudeScroll { lines: isize },
    /// Kill the pane's process, and any launch still in progress under an older generation than
    /// `generation`.
    ClaudeClose { generation: u64 },
    /// Answer the IPC request `id` right away.
    ClaudeIpcReply { id: u64, response: ClaudeResponse },
    /// Answer the IPC request `id` with file-system work done off the UI thread.
    /// Load the git diff of a file Claude changed (answered by `Msg::ClaudeDiff`).
    ClaudeDiff { path: PathBuf },
    ClaudeIpcWork {
        id: u64,
        request: ClaudeRequest,
        ctx: IpcContext,
    },
}
