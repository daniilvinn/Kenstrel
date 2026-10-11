//! Everything that can happen to the app, as plain data.

use std::ffi::OsString;
use std::path::PathBuf;
use std::time::SystemTime;

use crate::aids::LoadedNavData;
use crate::claude::{ClaudePreflight, ClaudeRequest, ClaudeScreen};
use crate::config::Config;
use crate::drives::DriveInfo;
use crate::entry::Entry;
use crate::error::FsError;
use crate::fileops::{BulkPlanResult, ForegroundRequest, JobMsg, ShellMsg};
use crate::generation::Gen;
use crate::git::GitSnapshot;
use crate::key::KeyChord;
use crate::keymap::Keymap;
use crate::preview::PreviewMsg;
use crate::state::{JobId, Level, Size};

/// A delayed wake-up requested through [`Effect::StartTimer`](crate::effect::Effect).
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum TimerKind {
    /// Show the which-key popup for a pending key prefix.
    WhichKey,
    /// Remove the status-line message.
    StatusExpiry,
    /// The debounce for re-running `git status` after the directory changed.
    GitRefresh,
    /// The debounce before the frecency database is written.
    StateSave,
    /// The 120 ms quiet period of the content-search query.
    GrepDebounce,
    /// The grace period after which a freshly opened conflict dialog accepts keys.
    ConflictArm,
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
    /// Sent once before the first frame: list the start directory.
    Start,
    /// Periodic wall-clock update (relative times such as "3m ago" age with it).
    Clock {
        now: SystemTime,
        utc_offset_secs: i32,
    },
    /// A batch of entries of the listing started with generation `gen`.
    ListingChunk {
        gen: Gen,
        entries: Vec<Entry>,
    },
    /// The listing with generation `gen` ended, successfully or not.
    ListingFinished {
        gen: Gen,
        error: Option<FsError>,
    },
    /// The drive list requested with generation `gen`.
    DrivesLoaded {
        gen: Gen,
        drives: Vec<DriveInfo>,
    },
    /// Child counts for directories of the current listing.
    ChildCounts {
        gen: Gen,
        counts: Vec<(OsString, u32)>,
    },
    /// Size and modification time of entries of the displayed results list.
    EntryStats {
        gen: Gen,
        stats: Vec<(OsString, u64, Option<std::time::SystemTime>)>,
    },
    /// The watched directory changed (already debounced by the watcher).
    DirChanged {
        gen: Gen,
    },
    /// The drives changed while "This PC" is shown (see `Effect::WatchDrives`).
    DrivesChanged {
        gen: Gen,
    },
    /// A directory shown by a parked (background) tab changed (see `Effect::WatchParked`).
    ParkedDirChanged {
        path: std::path::PathBuf,
    },
    /// Result of `git status`; `None` when the directory is not in a repository.
    /// A snapshot of the find-file session `gen` (results window, counters).
    FinderResults {
        gen: u64,
        snapshot: Box<crate::search::FinderSnapshot>,
    },
    /// An event of the content search with generation `event.generation()`, 1:1 from the
    /// engine.
    Grep(ks_search::GrepEvent),
    GitStatus {
        gen: Gen,
        snapshot: Option<Box<GitSnapshot>>,
    },
    /// `config.toml` was re-read. `issues` are keymap problems (already logged).
    ConfigReloaded {
        config: Box<Config>,
        keymap: Box<Keymap>,
        issues: Vec<String>,
    },
    /// The themes that can be switched to.
    ThemesAvailable(Vec<String>),
    /// The named theme is now the active one.
    ThemeApplied(String),
    /// Bookmarks and frecency read from the state directory (`Effect::LoadNavData`).
    NavDataLoaded(Box<LoadedNavData>),
    // ----- file operations -----
    /// An event of the job engine (progress, conflict prompt, result).
    Job(JobMsg),
    /// An event of a `!` shell command.
    Shell(ShellMsg),
    /// The runner asks to suspend the terminal and run a program in the foreground.
    Foreground(ForegroundRequest),
    /// The foreground program ended (`error` is set when it could not run or failed).
    ForegroundDone {
        id: u64,
        error: Option<String>,
    },
    /// The programs `O` can open `paths` with.
    OpenHandlers {
        paths: Vec<PathBuf>,
        names: Vec<String>,
    },
    /// The edited bulk-rename file was validated.
    BulkPlanned(BulkPlanResult),
    /// The preview engine finished the request for the main pane. Stale generations are
    /// dropped.
    Preview(PreviewMsg),
    /// The preview engine listed the parent directory for the parent column.
    ParentPreview(PreviewMsg),
    /// A background resize and encode of the preview image finished; redraw.
    ImageReady,
    /// Text pasted into the terminal (bracketed paste, only enabled while a pane has focus).
    Paste(String),
    /// `claude auth status` finished.
    ClaudePreflight {
        generation: u64,
        result: ClaudePreflight,
    },
    /// The embedded `claude` process is running.
    ClaudeStarted {
        generation: u64,
    },
    /// The embedded `claude` process could not be started.
    ClaudeLaunchFailed {
        generation: u64,
        message: String,
    },
    /// The embedded terminal changed (coalesced and rate limited by the runner).
    ClaudeScreen {
        generation: u64,
        screen: Box<ClaudeScreen>,
    },
    /// The embedded `claude` process exited.
    ClaudePaneExited {
        generation: u64,
        code: Option<u32>,
    },
    /// A request from Claude's MCP / hook process; answer with `Effect::ClaudeIpcReply { id, .. }`.
    ClaudeIpc {
        id: u64,
        request: ClaudeRequest,
    },
    /// Show `path` in the explorer, cursor on it (an MCP `reveal` that was verified to exist).
    ClaudeReveal {
        path: PathBuf,
    },
    /// The git diff of `path` for the Claude changes view (answer to `Effect::ClaudeDiff`).
    ClaudeDiff {
        path: PathBuf,
        text: String,
    },
    /// Type `@` mentions for these paths into the pane (focusing it).
    ClaudeSendPaths(Vec<PathBuf>),
    /// Type an `@path#Lstart-end` mention (visual mode in the preview, a search hit).
    ClaudeSendRange {
        path: PathBuf,
        start: usize,
        end: usize,
    },
}
