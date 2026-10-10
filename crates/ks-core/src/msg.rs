//! Everything that can happen to the app, as plain data.

use std::ffi::OsString;
use std::time::SystemTime;

use crate::config::Config;
use crate::drives::DriveInfo;
use crate::entry::Entry;
use crate::error::FsError;
use crate::generation::Gen;
use crate::git::GitSnapshot;
use crate::key::KeyChord;
use crate::keymap::Keymap;
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
    /// The watched directory changed (already debounced by the watcher).
    DirChanged {
        gen: Gen,
    },
    /// Result of `git status`; `None` when the directory is not in a repository.
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
}
