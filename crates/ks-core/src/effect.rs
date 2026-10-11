//! Work requested by `update` and performed by the application's effect runner.
//!
//! Effects never touch state. They run off the UI thread and report back as
//! [`Msg`](crate::msg::Msg)s tagged with the generation they were started under.

use std::ffi::OsString;
use std::path::PathBuf;
use std::time::Duration;

use crate::aids::StateFile;
use crate::claude::{ClaudeRequest, ClaudeResponse, ClaudeSpawn, IpcContext};
use crate::fileops::{ConflictAnswer, ForegroundSpec, JobRequest, PathFormatKind};
use crate::generation::Gen;
use crate::key::KeyChord;
use crate::msg::TimerKind;
use crate::preview::PreviewRequest;
use crate::state::JobId;
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
    /// Report `Msg::DrivesChanged { gen }` when a drive letter appears or goes away, or a
    /// removable drive gets or loses its medium. Replaces the previous watch (it shares the
    /// slot of [`Effect::WatchDir`]).
    WatchDrives { gen: Gen },
    /// Stop watching.
    StopWatching,
    /// Read size and modification time of `dir/<name>` for each name (names may be relative
    /// paths), reporting `Msg::EntryStats` in batches. Replaces a running count of children
    /// or an earlier stat run. Used by the search results list, whose entries start without
    /// metadata.
    StatEntries {
        gen: Gen,
        dir: PathBuf,
        names: Vec<OsString>,
    },
    /// Watch exactly these directories (the dirs of the parked tabs, at most one per tab) in
    /// addition to the one of [`Effect::WatchDir`], reporting `Msg::ParkedDirChanged { path }`.
    /// Replaces the previous set; paths that stay in the set keep their watch.
    WatchParked { paths: Vec<PathBuf> },
    /// List the directory of a parked tab in the background, reporting the same messages as
    /// [`Effect::ListDir`] but without touching the running listing of the active tab.
    ListParked { gen: Gen, path: PathBuf },
    /// Run `git status` in `dir`, reporting `Msg::GitStatus`. Replaces a running one.
    GitStatus { gen: Gen, dir: PathBuf },
    /// Count the children of the directories `names` below `dir`, reporting
    /// `Msg::ChildCounts` in batches. Replaces a running count.
    CountChildren {
        gen: Gen,
        dir: PathBuf,
        names: Vec<OsString>,
    },
    /// Load the theme `name` (built-in or user file) and make it the active one. The result
    /// comes back as `Msg::ThemeApplied` (or an error `Msg::Notify`); `announce` adds an
    /// info message on success.
    SetTheme { name: String, announce: bool },
    /// Re-read `config.toml` and rebuild the keymap, reporting `Msg::ConfigReloaded` (or an
    /// error `Msg::Notify` that leaves the running settings untouched).
    ReloadConfig,
    /// Read the bookmarks and the frecency database from the state directory, reporting
    /// `Msg::NavDataLoaded`.
    LoadNavData,
    /// Atomically replace a state file with `contents`. Writes are applied in order.
    WriteState { file: StateFile, contents: String },
    // ----- file operations -----
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
    /// Look up the programs the (first of the) `paths` can be opened with, answered by
    /// `Msg::OpenHandlers`. `paths` are the marked entries, or the cursor entry.
    ListOpenHandlers { paths: Vec<PathBuf> },
    /// Open every one of `paths` with the handler of that name.
    OpenWith {
        paths: Vec<PathBuf>,
        handler: String,
    },
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
    /// Write the names to a temp file and open the editor on it (bulk rename). With `reuse` the
    /// kept edit file of an invalid attempt is opened again instead of writing a new one.
    BulkRenameEdit {
        dir: PathBuf,
        names: Vec<String>,
        reuse: Option<PathBuf>,
    },
    /// Delete the kept edit file of an abandoned bulk rename.
    BulkRenameDiscard { file: PathBuf },
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
    /// Preview `request.path`, reporting `Msg::Preview` tagged with `gen`. Cancels the
    /// request still running for the main pane.
    RequestPreview { gen: Gen, request: PreviewRequest },
    /// The main pane no longer needs a preview: cancel the running request.
    CancelPreview,
    /// List the parent directory for the parent column, reporting `Msg::ParentPreview`.
    RequestParent { gen: Gen, request: PreviewRequest },
    /// The parent column is gone: cancel its request.
    CancelParent,
    /// Files were created, replaced or removed by a finished job: drop their cached previews.
    InvalidatePreview { paths: Vec<PathBuf> },
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
    /// incremental refresh).
    WalkCacheChanges(Vec<ks_search::PathChange>),
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
    /// Load the git diff of a file Claude changed (answered by `Msg::ClaudeDiff`).
    ClaudeDiff { path: PathBuf },
    /// Answer the IPC request `id` with file-system work done off the UI thread.
    ClaudeIpcWork {
        id: u64,
        request: ClaudeRequest,
        ctx: IpcContext,
    },
}
