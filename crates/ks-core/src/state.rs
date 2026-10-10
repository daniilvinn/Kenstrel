//! The single source of truth for the UI.

use std::collections::BTreeSet;
use std::path::PathBuf;

use crate::config::Config;
use crate::generation::{Gen, GenCounter};
use crate::key::KeyChord;
use crate::keymap::Keymap;
use crate::mode::{KeyContext, Mode, Overlay};

/// Terminal size in cells.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Size {
    pub width: u16,
    pub height: u16,
}

impl Size {
    pub const fn new(width: u16, height: u16) -> Self {
        Self { width, height }
    }
}

/// Severity of a status-line message.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub enum Level {
    Info,
    Warning,
    Error,
}

/// The message currently shown on the status line.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Status {
    pub level: Level,
    pub text: String,
    pub(crate) gen: Gen,
}

/// Identifies a background job (copy, move, delete, ...).
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct JobId(pub u64);

/// How the application should terminate.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum QuitKind {
    /// Plain quit.
    Exit,
    /// Quit and ask the shell wrapper to `cd` to the current directory (`--cwd-file`).
    ChangeDir,
}

/// Everything needed to start a session.
pub struct StartParams {
    pub cwd: PathBuf,
    pub home: Option<PathBuf>,
    pub config: Config,
    pub keymap: Keymap,
    pub size: Size,
}

pub struct AppState {
    pub cwd: PathBuf,
    /// Home directory, used to abbreviate paths as `~`.
    pub home: Option<PathBuf>,
    pub config: Config,
    pub keymap: Keymap,
    pub size: Size,
    pub mode: Mode,
    /// Overlay stack; the last element is on top.
    pub overlays: Vec<Overlay>,
    /// Keys of a partially entered multi-key binding (`g` waiting for the next key).
    pub pending: Vec<KeyChord>,
    /// True once the which-key popup should be shown for `pending`.
    pub which_key_visible: bool,
    pub status: Option<Status>,
    /// Running background jobs.
    pub jobs: BTreeSet<JobId>,
    /// Animation frame of the job spinner.
    pub spinner_frame: usize,
    /// Set when the state changed in a way that requires a redraw.
    pub dirty: bool,
    pub quit: Option<QuitKind>,
    pub(crate) which_key_gen: GenCounter,
    pub(crate) status_gen: GenCounter,
}

impl AppState {
    pub fn new(params: StartParams) -> Self {
        Self {
            cwd: params.cwd,
            home: params.home,
            config: params.config,
            keymap: params.keymap,
            size: params.size,
            mode: Mode::Normal,
            overlays: Vec::new(),
            pending: Vec::new(),
            which_key_visible: false,
            status: None,
            jobs: BTreeSet::new(),
            spinner_frame: 0,
            dirty: true,
            quit: None,
            which_key_gen: GenCounter::default(),
            status_gen: GenCounter::default(),
        }
    }

    /// The keymap context that receives the next key: the topmost overlay, else the mode.
    pub fn key_context(&self) -> KeyContext {
        self.overlays
            .last()
            .map_or_else(|| self.mode.into(), |&overlay| overlay.into())
    }

    pub fn has_running_jobs(&self) -> bool {
        !self.jobs.is_empty()
    }
}
