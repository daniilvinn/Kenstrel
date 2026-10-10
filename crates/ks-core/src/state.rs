//! The single source of truth for the UI.

use std::collections::{BTreeSet, HashMap};
use std::ffi::OsString;
use std::path::PathBuf;
use std::time::SystemTime;

use crate::aids::NavAids;
use crate::config::Config;
use crate::drives::DrivesView;
use crate::fileops::FileOps;
use crate::generation::{Gen, GenCounter};
use crate::git::GitSnapshot;
use crate::help::HelpState;
use crate::key::KeyChord;
use crate::keymap::Keymap;
use crate::layout;
use crate::mode::{KeyContext, Mode, Overlay};
use crate::nav::{History, PendingNav};
use crate::palette::PaletteState;
use crate::preview::PreviewState;
use crate::tabs::Tabs;
use crate::view::DirView;

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

/// Well-known directories the `g` jump keys lead to. Any of them may be unavailable.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Places {
    pub home: Option<PathBuf>,
    pub downloads: Option<PathBuf>,
    pub desktop: Option<PathBuf>,
    /// Kestrel's own config directory.
    pub config: Option<PathBuf>,
}

/// Everything needed to start a session.
pub struct StartParams {
    pub cwd: PathBuf,
    pub home: Option<PathBuf>,
    pub places: Places,
    pub config: Config,
    pub keymap: Keymap,
    pub size: Size,
    /// The current time, injected so relative times are testable.
    pub now: SystemTime,
    /// Offset of local time from UTC in seconds.
    pub utc_offset_secs: i32,
}

pub struct AppState {
    /// The directory being browsed. While the drives view is shown it is the last real
    /// directory (used for `Q` and to place the drives cursor).
    pub cwd: PathBuf,
    /// Home directory, used to abbreviate paths as `~`.
    pub home: Option<PathBuf>,
    pub places: Places,
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
    pub now: SystemTime,
    pub utc_offset_secs: i32,
    /// Entries of `cwd`.
    pub view: DirView,
    /// The other tabs (the active tab's data is in the fields of this struct).
    pub tabs: Tabs,
    /// Bookmarks, frecency database and the overlays built on them.
    pub aids: NavAids,
    /// The "This PC" view is shown instead of `view`.
    pub show_drives: bool,
    pub drives: DrivesView,
    /// Git status of `cwd`, when it is inside a repository.
    pub git: Option<GitSnapshot>,
    /// Command palette state (valid while `Overlay::Palette` is open; recents persist).
    pub palette: PaletteState,
    /// Help overlay state.
    pub help: HelpState,
    /// Names of the themes that can be switched to (built-ins and user files).
    pub themes: Vec<String>,
    /// File operations: yank register, jobs table, dialogs, pager (P6).
    pub fileops: FileOps,
    /// The preview pane and parent column.
    pub preview: PreviewState,
    pub(crate) history: History,
    /// Where the cursor was in directories left earlier (by entry name).
    pub(crate) cursor_memory: HashMap<PathBuf, OsString>,
    /// A navigation or refresh waiting for its first results.
    pub(crate) nav: Option<PendingNav>,
    pub(crate) which_key_gen: GenCounter,
    pub(crate) status_gen: GenCounter,
    pub(crate) list_gen: GenCounter,
    pub(crate) watch_gen: GenCounter,
    pub(crate) git_gen: GenCounter,
    pub(crate) git_timer_gen: GenCounter,
    pub(crate) children_gen: GenCounter,
    /// In-directory filter input, find file and content search (P3 + P4).
    pub search: crate::search::SearchState,
}

impl AppState {
    pub fn new(params: StartParams) -> Self {
        let view = DirView::new(params.config.sort.clone(), params.config.files.show_hidden);
        let preview = PreviewState::new(&params.config);
        Self {
            cwd: params.cwd,
            home: params.home,
            places: params.places,
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
            now: params.now,
            utc_offset_secs: params.utc_offset_secs,
            view,
            tabs: Tabs::default(),
            aids: NavAids::default(),
            show_drives: false,
            drives: DrivesView::default(),
            git: None,
            palette: PaletteState::default(),
            help: HelpState::default(),
            themes: Vec::new(),
            fileops: FileOps::default(),
            preview,
            history: History::default(),
            cursor_memory: HashMap::new(),
            nav: None,
            which_key_gen: GenCounter::default(),
            status_gen: GenCounter::default(),
            list_gen: GenCounter::default(),
            watch_gen: GenCounter::default(),
            git_gen: GenCounter::default(),
            git_timer_gen: GenCounter::default(),
            children_gen: GenCounter::default(),
            search: crate::search::SearchState::default(),
        }
    }

    /// Rows available to the list at the current terminal size.
    pub fn viewport_height(&self) -> usize {
        usize::from(layout::body_height(self.size))
    }

    /// The keymap context that receives the next key: the topmost overlay, else the mode.
    pub fn key_context(&self) -> KeyContext {
        self.overlays
            .last()
            .map_or_else(|| self.mode.into(), |&overlay| overlay.into())
    }

    /// A navigation or refresh is waiting for its first results.
    pub fn is_navigating(&self) -> bool {
        self.nav.is_some()
    }

    pub fn has_running_jobs(&self) -> bool {
        !self.jobs.is_empty()
    }
}
