//! Claude Code integration state and logic, as plain data.
//!
//! The engine (`ks-claude`) lives outside this crate; the app's effect runner turns engine events
//! into the `Msg::Claude*` variants that carry the types defined here, and performs the
//! `Effect::Claude*` requests that `update` emits. Everything in this module is pure: the
//! launch flow (preflight, billing confirmation, spawn), focus handling, key forwarding, the
//! pane layout, IPC request handling and the files-touched-by-Claude log are testable by
//! feeding messages to [`update`](crate::update::update).
//!
//! # Layout
//!
//! The pane is carved out of the body of the main screen (between the header rule and the
//! prompt) by [`split_body`]. It is deliberately a separate function: the preview pane
//! splits the *list area* that this function returns, so the two compose without
//! knowing about each other.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use crate::action::Action;
use crate::config::{ClaudeConfig, PanePosition};
use crate::effect::Effect;
use crate::fileops::{ConfirmAction, ConfirmDialog, PromptInput};
use crate::key::KeyChord;
use crate::layout;
use crate::mode::{KeyContext, Overlay};
use crate::nav;
use crate::state::{AppState, Level, Size};
use crate::update::set_status;

/// The exact warning shown when `claude auth status` reports API-key billing.
pub const BILLING_WARNING: &str = "Claude would bill your API key; aborting";
/// Shown when the user is not logged in.
pub const NOT_LOGGED_IN_HELP: &str =
    "Run `claude` in a terminal once and use /login with your Pro/Max account,\nthen try again.";
/// Shown for [`ClaudeDialog::Billing`] under the warning.
pub const BILLING_DETAIL: &str = "Your Pro/Max subscription would not be used for this session.";

/// Smallest and largest share of the body the pane may take, and the resize step.
pub const MIN_PERCENT: u16 = 15;
pub const MAX_PERCENT: u16 = 85;
pub const PERCENT_STEP: u16 = 5;

/// Rows (bottom position) the pane needs at least, including its border row.
const MIN_PANE_ROWS: u16 = 5;
/// Rows the list keeps when the pane is at the bottom.
const MIN_LIST_ROWS: u16 = 4;
/// Columns (right position) the pane needs at least, including its border column.
const MIN_PANE_COLS: u16 = 40;
/// Columns the list keeps when the pane is on the right.
const MIN_LIST_COLS: u16 = 30;
/// Rows above the body: the header and its rule.
const BODY_TOP: u16 = 2;
/// Most files shown in the changes view's counter before it says "+N".
const LAST_TOOL_LABEL: usize = 16;

// ----- geometry -----

/// A rectangle in terminal cells.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Area {
    pub x: u16,
    pub y: u16,
    pub width: u16,
    pub height: u16,
}

impl Area {
    pub const fn new(x: u16, y: u16, width: u16, height: u16) -> Self {
        Self {
            x,
            y,
            width,
            height,
        }
    }
}

/// How the body is divided between the list and the Claude pane.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct BodySplit {
    /// What is left for the file list (and, later, its preview).
    pub list: Area,
    /// The pane including its border.
    pub pane: Area,
    /// The pane has a left border column (right position, side by side).
    pub border_left: bool,
    /// The pane covers the whole body because the terminal is too small to share it.
    pub covers: bool,
}

impl BodySplit {
    /// Cells available to the embedded terminal: the pane minus its border.
    pub const fn inner(self) -> Size {
        Size::new(
            self.pane.width.saturating_sub(self.border_left as u16),
            self.pane.height.saturating_sub(1),
        )
    }
}

/// Divides `body` between the list and the pane.
pub fn split_body(body: Area, position: PanePosition, percent: u16) -> BodySplit {
    let percent = percent.clamp(MIN_PERCENT, MAX_PERCENT);
    let cover = BodySplit {
        list: body,
        pane: body,
        border_left: false,
        covers: true,
    };
    match position {
        PanePosition::Bottom => {
            if body.height < MIN_PANE_ROWS + MIN_LIST_ROWS {
                return cover;
            }
            let wanted = u32::from(body.height) * u32::from(percent) / 100;
            let pane_h = u16::try_from(wanted)
                .unwrap_or(u16::MAX)
                .clamp(MIN_PANE_ROWS, body.height - MIN_LIST_ROWS);
            let list_h = body.height - pane_h;
            BodySplit {
                list: Area::new(body.x, body.y, body.width, list_h),
                pane: Area::new(body.x, body.y + list_h, body.width, pane_h),
                border_left: false,
                covers: false,
            }
        }
        PanePosition::Right => {
            if body.width < MIN_PANE_COLS + MIN_LIST_COLS {
                return cover;
            }
            let wanted = u32::from(body.width) * u32::from(percent) / 100;
            let pane_w = u16::try_from(wanted)
                .unwrap_or(u16::MAX)
                .clamp(MIN_PANE_COLS, body.width - MIN_LIST_COLS);
            let list_w = body.width - pane_w;
            BodySplit {
                list: Area::new(body.x, body.y, list_w, body.height),
                pane: Area::new(body.x + list_w, body.y, pane_w, body.height),
                border_left: true,
                covers: false,
            }
        }
    }
}

/// The body of the main screen for a terminal of `size` (what `ks-ui` renders into).
pub fn body_area(size: Size) -> Area {
    Area::new(0, BODY_TOP, size.width, layout::body_height(size))
}

/// The split for the current state, or `None` when no pane is shown.
pub fn current_split(state: &AppState) -> Option<BodySplit> {
    state.claude.is_running().then(|| {
        split_body(
            body_area(state.size),
            state.claude.position,
            state.claude.percent,
        )
    })
}

/// Rows available to the list when a pane may be at the bottom (used for scrolling).
pub(crate) fn list_height(state: &AppState, body_height: u16) -> u16 {
    current_split(state).map_or(body_height, |split| split.list.height)
}

/// Columns available to the list/preview split when a pane may be on the right.
pub(crate) fn list_width(state: &AppState, body_width: u16) -> u16 {
    current_split(state).map_or(body_width, |split| split.list.width)
}

/// Size of the terminal inside the pane, or `None` when there is no room for one.
pub fn pane_inner_size(state: &AppState) -> Option<Size> {
    let split = split_body(
        body_area(state.size),
        state.claude.position,
        state.claude.percent,
    );
    let inner = split.inner();
    (inner.width >= 2 && inner.height >= 1).then_some(inner)
}

// ----- screen snapshot types -----

/// A terminal colour of an embedded-terminal cell.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum ClaudeColor {
    #[default]
    Default,
    Indexed(u8),
    Rgb(u8, u8, u8),
}

/// One cell of the embedded terminal.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct ClaudeCell {
    /// Grapheme; empty for blanks and for the right half of a wide character.
    pub text: String,
    pub fg: ClaudeColor,
    pub bg: ClaudeColor,
    pub bold: bool,
    pub dim: bool,
    pub italic: bool,
    pub underline: bool,
    pub inverse: bool,
}

/// A snapshot of the embedded terminal, row-major.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct ClaudeScreen {
    pub cols: u16,
    pub rows: u16,
    pub cells: Vec<ClaudeCell>,
    /// Cursor `(row, col)`, `None` when hidden.
    pub cursor: Option<(u16, u16)>,
}

impl ClaudeScreen {
    pub fn cell(&self, row: u16, col: u16) -> Option<&ClaudeCell> {
        if row >= self.rows || col >= self.cols {
            return None;
        }
        self.cells
            .get(usize::from(row) * usize::from(self.cols) + usize::from(col))
    }
}

// ----- preflight, launch flow -----

/// Result of the `claude auth status` preflight (mirrors `ks_claude::preflight::PreflightResult`).
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum ClaudePreflight {
    /// Subscription login: safe to launch.
    Ok,
    /// Logged in but Claude would bill an API key / provider; needs explicit confirmation.
    BillingWarning { method: String, provider: String },
    /// Not logged in.
    NotLoggedIn,
    /// `claude` is not installed; the text holds the install instructions.
    NotInstalled(String),
    /// The check itself failed.
    Failed(String),
}

/// How to start the `claude` executable: carried by every effect that spawns or probes it.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct ClaudeSpawn {
    /// Explicit executable; `None` means look it up on `PATH`.
    pub binary: Option<PathBuf>,
    /// Environment variables to remove from the child.
    pub strip_env: Vec<String>,
}

impl ClaudeSpawn {
    pub fn from_config(config: &ClaudeConfig) -> Self {
        Self {
            binary: Some(config.binary.trim())
                .filter(|b| !b.is_empty())
                .map(PathBuf::from),
            strip_env: config.strip_env.clone(),
        }
    }
}

/// What the user asked for; carried through the preflight.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum LaunchKind {
    /// The embedded pane (10b).
    Embedded { resume: bool },
    /// A Windows Terminal split (10a).
    WtSplit,
}

/// Lifecycle of the embedded pane.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum ClaudePhase {
    #[default]
    Idle,
    /// `claude auth status` is running.
    Checking,
    /// Preflight passed (or was confirmed); the process is being started.
    Starting,
    Running,
}

/// A modal message about the Claude integration. It is presented by the generic confirm
/// overlay (`Overlay::Confirm` with a `fileops::ConfirmDialog` built by [`show_dialog`]); this
/// records which Claude question is open.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum ClaudeDialog {
    /// API billing warning; only `y` launches anyway.
    Billing { method: String, provider: String },
    /// Information only (install instructions, not logged in, failures).
    Info { title: String, body: String },
}

// ----- files touched by Claude -----

/// A file Claude edited in this Kestrel session.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Change {
    pub path: PathBuf,
    /// Tool that made the edit (`Edit`, `Write`, ...).
    pub tool: String,
    /// Increases with every recorded edit; the largest is the most recent.
    pub seq: u64,
}

/// Lexically normalised, case-folded (on Windows) key for comparing paths without touching the
/// file system.
pub fn path_key(path: &Path) -> String {
    let raw = path.to_string_lossy().replace('\\', "/");
    let raw = raw.strip_prefix("//?/").unwrap_or(&raw);
    let mut parts: Vec<&str> = Vec::new();
    for part in raw.split('/') {
        match part {
            "" | "." => {}
            ".." => {
                if parts.last().is_some_and(|p| *p != "..") {
                    parts.pop();
                }
            }
            other => parts.push(other),
        }
    }
    let joined = parts.join("/");
    let joined = if raw.starts_with('/') {
        format!("/{joined}")
    } else {
        joined
    };
    if cfg!(windows) {
        joined.to_lowercase()
    } else {
        joined
    }
}

/// Files Claude touched, with fast lookups for the list's `◆` glyph.
#[derive(Clone, Debug, Default)]
pub struct ChangeLog {
    entries: BTreeMap<String, Change>,
    seq: u64,
}

impl ChangeLog {
    /// Records an edit; a file edited again moves to the front.
    pub fn record(&mut self, path: PathBuf, tool: &str) {
        self.seq += 1;
        let key = path_key(&path);
        self.entries.insert(
            key,
            Change {
                path,
                tool: tool.to_owned(),
                seq: self.seq,
            },
        );
    }

    pub fn len(&self) -> usize {
        self.entries.len()
    }

    pub fn is_empty(&self) -> bool {
        self.entries.is_empty()
    }

    pub fn clear(&mut self) {
        self.entries.clear();
    }

    /// Claude edited exactly this file.
    pub fn contains(&self, path: &Path) -> bool {
        !self.entries.is_empty() && self.entries.contains_key(&path_key(path))
    }

    /// Claude edited something below this directory.
    pub fn contains_below(&self, dir: &Path) -> bool {
        if self.entries.is_empty() {
            return false;
        }
        let prefix = format!("{}/", path_key(dir).trim_end_matches('/'));
        self.entries
            .range(prefix.clone()..)
            .next()
            .is_some_and(|(key, _)| key.starts_with(&prefix))
    }

    /// Most recent first.
    pub fn recent(&self) -> Vec<&Change> {
        let mut all: Vec<&Change> = self.entries.values().collect();
        all.sort_by_key(|c| std::cmp::Reverse(c.seq));
        all
    }

    /// The most recent edit.
    pub fn latest(&self) -> Option<&Change> {
        self.entries.values().max_by_key(|c| c.seq)
    }
}

// ----- IPC (10d) -----

/// A request from Claude's MCP / hook processes (mirrors `ks_claude::ipc::Request`).
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum ClaudeRequest {
    GetSelection,
    SearchFiles {
        query: String,
        root: Option<String>,
        limit: usize,
    },
    SearchContents {
        pattern: String,
        glob: Option<String>,
        regex: bool,
        limit: usize,
    },
    ListDir {
        path: String,
    },
    Reveal {
        path: String,
    },
    HookEvent {
        session_id: String,
        tool_name: String,
        file_path: Option<String>,
    },
}

/// A content-search hit.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ClaudeHit {
    pub path: String,
    pub line: usize,
    pub text: String,
}

/// A directory entry.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ClaudeDirEntry {
    pub name: String,
    pub is_dir: bool,
    pub size: Option<u64>,
}

/// The answer to a [`ClaudeRequest`] (mirrors `ks_claude::ipc::Response`).
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum ClaudeResponse {
    Selection {
        marked: Vec<String>,
        cursor: Option<String>,
        cwd: String,
    },
    Files {
        paths: Vec<String>,
        truncated: bool,
    },
    Matches {
        hits: Vec<ClaudeHit>,
        truncated: bool,
    },
    Dir {
        entries: Vec<ClaudeDirEntry>,
    },
    Ok,
    Error {
        message: String,
    },
}

/// Facts the file-system workers need that live in the state.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct IpcContext {
    /// The project root Claude runs in (relative paths and default search root).
    pub root: PathBuf,
    /// The directory the explorer shows.
    pub cwd: PathBuf,
    pub show_hidden: bool,
    pub respect_gitignore: bool,
}

// ----- state -----

/// `git diff` of one changed file, as loaded for the changes view.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ChangeDiff {
    pub path: PathBuf,
    pub text: String,
}

/// Everything the app remembers about Claude.
#[derive(Clone, Debug)]
pub struct ClaudeState {
    pub phase: ClaudePhase,
    /// Generation of the current preflight / pane; results carrying another one are stale.
    pub generation: u64,
    /// What the running preflight is for.
    pub pending: Option<LaunchKind>,
    pub dialog: Option<ClaudeDialog>,
    /// Latest screen of the pane while running.
    pub screen: Option<ClaudeScreen>,
    pub position: PanePosition,
    pub percent: u16,
    /// Project root of the running (or last) pane.
    pub root: Option<PathBuf>,
    /// Size last sent to the pane's PTY.
    pub pty: Option<Size>,
    /// Files Claude edited (10e); drives the `◆` glyph and the changes view.
    pub changes: ChangeLog,
    /// Cursor of the changes view.
    pub changes_cursor: usize,
    /// The changes view shows the git diff of the selected file.
    pub diff_open: bool,
    /// Diff text of the selected file once loaded (`None` while loading).
    pub diff: Option<ChangeDiff>,
}

impl ClaudeState {
    pub fn new(config: &ClaudeConfig) -> Self {
        Self {
            phase: ClaudePhase::Idle,
            generation: 0,
            pending: None,
            dialog: None,
            screen: None,
            position: config.pane_position,
            percent: config.pane_percent.clamp(MIN_PERCENT, MAX_PERCENT),
            root: None,
            pty: None,
            changes: ChangeLog::default(),
            changes_cursor: 0,
            diff_open: false,
            diff: None,
        }
    }

    pub fn next_generation(&mut self) -> u64 {
        self.generation += 1;
        self.generation
    }

    pub fn is_running(&self) -> bool {
        self.phase == ClaudePhase::Running
    }
}

// ----- focus -----

fn focused(state: &AppState) -> bool {
    state.overlays.last() == Some(&Overlay::Claude)
}

fn focus(state: &mut AppState, effects: &mut Vec<Effect>) {
    if !state.overlays.contains(&Overlay::Claude) {
        state.overlays.push(Overlay::Claude);
        effects.push(Effect::SetBracketedPaste(true));
    }
    state.dirty = true;
}

fn unfocus(state: &mut AppState, effects: &mut Vec<Effect>) {
    if focused(state) {
        state.overlays.pop();
        effects.push(Effect::SetBracketedPaste(false));
        state.dirty = true;
    }
}

/// Back to "no pane": drops focus, screen and the pane's room in the layout.
fn reset(state: &mut AppState, effects: &mut Vec<Effect>) {
    if state.overlays.contains(&Overlay::Claude) {
        state.overlays.retain(|o| *o != Overlay::Claude);
        effects.push(Effect::SetBracketedPaste(false));
    }
    state.claude.phase = ClaudePhase::Idle;
    state.claude.screen = None;
    state.claude.pty = None;
    state.dirty = true;
    nav::fit_viewport(state);
}

fn is_current(state: &AppState, generation: u64) -> bool {
    state.claude.generation == generation
}

/// Re-fits the list viewport and tells the pane's PTY about a changed inner size.
pub(crate) fn sync_layout(state: &mut AppState, effects: &mut Vec<Effect>) {
    nav::fit_viewport(state);
    if !state.claude.is_running() {
        return;
    }
    let want = pane_inner_size(state);
    if want != state.claude.pty {
        state.claude.pty = want;
        if let Some(size) = want {
            effects.push(Effect::ClaudeResize { size });
        }
    }
    state.dirty = true;
}

// ----- launch flow -----

fn status(state: &mut AppState, level: Level, text: &str, effects: &mut Vec<Effect>) {
    set_status(state, level, text.to_owned(), effects);
}

fn start(state: &mut AppState, kind: LaunchKind, effects: &mut Vec<Effect>) {
    if matches!(kind, LaunchKind::Embedded { .. }) && state.claude.is_running() {
        focus(state, effects);
        if matches!(kind, LaunchKind::Embedded { resume: true }) {
            status(
                state,
                Level::Info,
                "Claude is already running; close it (g X) to resume another session",
                effects,
            );
        }
        return;
    }
    if matches!(
        state.claude.phase,
        ClaudePhase::Checking | ClaudePhase::Starting
    ) {
        status(state, Level::Info, "Claude is already starting", effects);
        return;
    }
    state.claude.pending = Some(kind);
    state.claude.phase = ClaudePhase::Checking;
    let generation = state.claude.next_generation();
    effects.push(Effect::ClaudePreflight {
        generation,
        spawn: ClaudeSpawn::from_config(&state.config.claude),
    });
    status(state, Level::Info, "Checking Claude login...", effects);
}

/// The preflight passed (or the user confirmed): do what was asked.
fn proceed(state: &mut AppState, effects: &mut Vec<Effect>) {
    let Some(kind) = state.claude.pending.take() else {
        state.claude.phase = ClaudePhase::Idle;
        return;
    };
    let spawn = ClaudeSpawn::from_config(&state.config.claude);
    match kind {
        LaunchKind::WtSplit => {
            state.claude.phase = ClaudePhase::Idle;
            effects.push(Effect::ClaudeWtSplit {
                cwd: state.cwd.clone(),
                spawn,
            });
        }
        LaunchKind::Embedded { resume } => {
            let Some(size) = pane_inner_size(state) else {
                state.claude.phase = ClaudePhase::Idle;
                status(
                    state,
                    Level::Warning,
                    "The terminal is too small for the Claude pane",
                    effects,
                );
                return;
            };
            state.claude.phase = ClaudePhase::Starting;
            // The PTY is created at this size; `on_started` compares it with the layout then.
            state.claude.pty = Some(size);
            state.claude.root = Some(state.cwd.clone());
            effects.push(Effect::ClaudeLaunch {
                generation: state.claude.generation,
                cwd: state.cwd.clone(),
                resume,
                size,
                spawn,
            });
        }
    }
}

pub(crate) fn on_preflight(
    state: &mut AppState,
    generation: u64,
    result: ClaudePreflight,
    effects: &mut Vec<Effect>,
) {
    if !is_current(state, generation) || state.claude.phase != ClaudePhase::Checking {
        return;
    }
    state.dirty = true;
    let info = |title: &str, body: String| ClaudeDialog::Info {
        title: title.to_owned(),
        body,
    };
    let dialog = match result {
        ClaudePreflight::Ok => {
            proceed(state, effects);
            return;
        }
        ClaudePreflight::BillingWarning { method, provider } => {
            ClaudeDialog::Billing { method, provider }
        }
        ClaudePreflight::NotLoggedIn => info("Claude is not logged in", NOT_LOGGED_IN_HELP.into()),
        ClaudePreflight::NotInstalled(text) => info("Claude Code is not installed", text),
        ClaudePreflight::Failed(text) => info("Could not check Claude", text),
    };
    let keeps_pending = matches!(dialog, ClaudeDialog::Billing { .. });
    state.claude.phase = ClaudePhase::Idle;
    if !keeps_pending {
        state.claude.pending = None;
    }
    state.status = None;
    state.status_gen.bump();
    effects.push(Effect::CancelTimer(crate::msg::TimerKind::StatusExpiry));
    crate::fileops::discard_reedit_if_any(state, effects);
    show_dialog(state, dialog);
}

/// Word-wraps `text` to `width` columns (the generic confirm overlay draws one line per row).
fn wrap_lines(text: &str, width: usize) -> Vec<String> {
    let mut out = Vec::new();
    for paragraph in text.lines() {
        let mut line = String::new();
        for word in paragraph.split_whitespace() {
            let used = line.chars().count();
            if used > 0 && used + 1 + word.chars().count() > width {
                out.push(std::mem::take(&mut line));
            }
            if !line.is_empty() {
                line.push(' ');
            }
            line.push_str(word);
        }
        out.push(line);
    }
    out
}

/// Opens `dialog` in the confirm overlay. The billing warning asks `y` (the overlay's
/// accept key for questions with an action); enter does not accept it, see [`handle_action`].
fn show_dialog(state: &mut AppState, dialog: ClaudeDialog) {
    const WRAP: usize = 64;
    let confirm = match &dialog {
        ClaudeDialog::Billing { method, provider } => {
            let mut lines = vec![BILLING_WARNING.to_owned(), String::new()];
            lines.extend(wrap_lines(
                &format!(
                    "`claude auth status` reports auth method {method} (provider {provider})."
                ),
                WRAP,
            ));
            lines.extend(wrap_lines(BILLING_DETAIL, WRAP));
            ConfirmDialog {
                title: "Claude billing check".to_owned(),
                lines,
                more: 0,
                danger: true,
                token: None,
                typed: PromptInput::new(),
                mismatch: false,
                action: Some(ConfirmAction::ClaudeProceed),
                accept_label: "proceed anyway (billed to the API)".to_owned(),
            }
        }
        ClaudeDialog::Info { title, body } => ConfirmDialog {
            title: title.clone(),
            lines: wrap_lines(body, WRAP),
            more: 0,
            danger: false,
            token: None,
            typed: PromptInput::new(),
            mismatch: false,
            action: None,
            accept_label: String::new(),
        },
    };
    state.claude.dialog = Some(dialog);
    state.fileops.confirm = Some(confirm);
    if state.overlays.last() != Some(&Overlay::Confirm) {
        state.overlays.push(Overlay::Confirm);
    }
    state.dirty = true;
}

/// The confirm overlay was closed: forget the Claude question it showed.
pub(crate) fn on_confirm_closed(state: &mut AppState) {
    state.claude.dialog = None;
}

/// The user accepted the billing warning: a fresh attempt under a new generation.
pub(crate) fn confirm_proceed(state: &mut AppState, effects: &mut Vec<Effect>) {
    state.claude.next_generation();
    state.claude.phase = ClaudePhase::Checking;
    proceed(state, effects);
    state.dirty = true;
}

fn close_dialog(state: &mut AppState) {
    state.claude.dialog = None;
    state.fileops.confirm = None;
    state.overlays.retain(|o| *o != Overlay::Confirm);
    state.dirty = true;
}

pub(crate) fn on_started(state: &mut AppState, generation: u64, effects: &mut Vec<Effect>) {
    if is_current(state, generation) && state.claude.phase == ClaudePhase::Starting {
        state.claude.phase = ClaudePhase::Running;
        // Screens that arrived while starting were discarded: ask for a fresh one.
        effects.push(Effect::ClaudeScroll { lines: 0 });
        // The layout may have changed while the process was starting.
        let want = pane_inner_size(state);
        if want != state.claude.pty {
            state.claude.pty = want;
            if let Some(size) = want {
                effects.push(Effect::ClaudeResize { size });
            }
        }
        focus(state, effects);
        nav::fit_viewport(state);
    }
}

pub(crate) fn on_launch_failed(
    state: &mut AppState,
    generation: u64,
    message: &str,
    effects: &mut Vec<Effect>,
) {
    if is_current(state, generation) {
        reset(state, effects);
        set_status(
            state,
            Level::Error,
            format!("Could not start Claude: {message}"),
            effects,
        );
    }
}

pub(crate) fn on_screen(state: &mut AppState, generation: u64, screen: ClaudeScreen) {
    if is_current(state, generation) && state.claude.is_running() {
        state.claude.screen = Some(screen);
        state.dirty = true;
    }
}

pub(crate) fn on_exited(
    state: &mut AppState,
    generation: u64,
    code: Option<u32>,
    effects: &mut Vec<Effect>,
) {
    if is_current(state, generation)
        && matches!(
            state.claude.phase,
            ClaudePhase::Running | ClaudePhase::Starting
        )
    {
        // What the process printed last is the reason for a failure (a rejected `--resume`,
        // a bad flag): keep it, the screen goes away with the pane.
        let tail = state.claude.screen.as_ref().and_then(screen_tail);
        reset(state, effects);
        let (level, text) = match (code, tail) {
            (Some(0) | None, _) => (Level::Info, "Claude exited".to_owned()),
            (Some(code), Some(tail)) => {
                tracing::warn!(code, %tail, "Claude exited with an error");
                (
                    Level::Warning,
                    format!("Claude exited with code {code}: {tail}"),
                )
            }
            (Some(code), None) => (Level::Warning, format!("Claude exited with code {code}")),
        };
        set_status(state, level, text, effects);
    }
}

/// The non-blank rows of the pane, trimmed.
fn screen_rows(screen: &ClaudeScreen) -> Vec<String> {
    let cols = usize::from(screen.cols).max(1);
    screen
        .cells
        .chunks(cols)
        .map(|row| {
            row.iter()
                .map(|c| {
                    if c.text.is_empty() {
                        " "
                    } else {
                        c.text.as_str()
                    }
                })
                .collect::<String>()
                .trim()
                .to_owned()
        })
        .filter(|row| !row.is_empty())
        .collect()
}

/// All the text on the pane, one non-blank row per line.
pub fn screen_text(screen: &ClaudeScreen) -> String {
    screen_rows(screen).join(
        "
",
    )
}

/// The last two non-blank rows of the pane, trimmed and joined (at most 160 characters).
fn screen_tail(screen: &ClaudeScreen) -> Option<String> {
    let rows = screen_rows(screen);
    let from = rows.len().saturating_sub(2);
    let tail = rows[from..].join(" / ");
    (!tail.is_empty()).then(|| tail.chars().take(160).collect())
}

fn close(state: &mut AppState, effects: &mut Vec<Effect>) {
    if !state.claude.is_running() && state.claude.phase != ClaudePhase::Starting {
        status(state, Level::Info, "Claude is not running", effects);
        return;
    }
    // A new generation makes the late exit event of the killed process stale.
    let generation = state.claude.next_generation();
    reset(state, effects);
    effects.push(Effect::ClaudeClose { generation });
    status(state, Level::Info, "Closed Claude", effects);
}

fn toggle_focus(state: &mut AppState, effects: &mut Vec<Effect>) {
    if focused(state) {
        unfocus(state, effects);
    } else if state.claude.is_running() && state.overlays.is_empty() {
        focus(state, effects);
    } else if !state.claude.is_running() {
        status(
            state,
            Level::Info,
            "Claude is not running (press c to open it)",
            effects,
        );
    }
}

fn resize_pane(state: &mut AppState, delta: i32, effects: &mut Vec<Effect>) {
    if !state.claude.is_running() {
        status(
            state,
            Level::Info,
            "Claude is not running (press c to open it)",
            effects,
        );
        return;
    }
    let percent = i32::from(state.claude.percent) + delta;
    state.claude.percent = u16::try_from(percent)
        .unwrap_or(MIN_PERCENT)
        .clamp(MIN_PERCENT, MAX_PERCENT);
    sync_layout(state, effects);
}

fn toggle_position(state: &mut AppState, effects: &mut Vec<Effect>) {
    state.claude.position = state.claude.position.toggled();
    sync_layout(state, effects);
    state.dirty = true;
}

fn scroll(state: &mut AppState, up: bool, effects: &mut Vec<Effect>) {
    if !state.claude.is_running() {
        return;
    }
    let half = state
        .claude
        .pty
        .map_or(10, |s| isize::try_from(s.height.max(2) / 2).unwrap_or(10));
    effects.push(Effect::ClaudeScroll {
        lines: if up { half } else { -half },
    });
}

// ----- sending context (10c) -----

/// Marked entries (or the one under the cursor) as absolute paths, in list order.
fn selection_paths(state: &AppState) -> Vec<PathBuf> {
    if state.show_drives {
        return Vec::new();
    }
    let view = &state.view;
    let marked: Vec<PathBuf> = view
        .entries()
        .iter()
        .filter(|e| view.is_marked(e))
        .map(|e| state.cwd.join(&e.name))
        .collect();
    if !marked.is_empty() {
        return marked;
    }
    view.current()
        .map(|e| state.cwd.join(&e.name))
        .into_iter()
        .collect()
}

/// The toast for a send while no pane runs.
pub(crate) fn not_running(state: &mut AppState, effects: &mut Vec<Effect>) {
    status(
        state,
        Level::Info,
        "Claude is not running (press c to open it)",
        effects,
    );
}

fn send_marked(state: &mut AppState, effects: &mut Vec<Effect>) {
    if !state.claude.is_running() {
        not_running(state, effects);
        return;
    }
    let paths = selection_paths(state);
    if paths.is_empty() {
        status(state, Level::Info, "Nothing to send", effects);
        return;
    }
    let count = paths.len();
    effects.push(Effect::ClaudeSendPaths { paths });
    focus(state, effects);
    let noun = if count == 1 { "path" } else { "paths" };
    status(
        state,
        Level::Info,
        &format!("Sent {count} {noun} to Claude"),
        effects,
    );
}

/// `Msg::ClaudeSendRange`: a line range from visual mode in the preview or a search hit.
pub(crate) fn send_range(
    state: &mut AppState,
    path: PathBuf,
    start: usize,
    end: usize,
    effects: &mut Vec<Effect>,
) {
    if !state.claude.is_running() {
        status(
            state,
            Level::Info,
            "Claude is not running (press c to open it)",
            effects,
        );
        return;
    }
    effects.push(Effect::ClaudeSendRange { path, start, end });
    focus(state, effects);
}

pub(crate) fn send_paths(state: &mut AppState, paths: Vec<PathBuf>, effects: &mut Vec<Effect>) {
    if state.claude.is_running() && !paths.is_empty() {
        effects.push(Effect::ClaudeSendPaths { paths });
        focus(state, effects);
    }
}

pub(crate) fn on_paste(state: &mut AppState, text: String, effects: &mut Vec<Effect>) {
    if focused(state) && state.claude.is_running() {
        effects.push(Effect::ClaudePaste(text));
    }
}

/// A key pressed while the pane has focus and no binding claims it: Claude gets it verbatim.
pub(crate) fn forward_key(state: &AppState, key: KeyChord, effects: &mut Vec<Effect>) {
    if state.claude.is_running() {
        effects.push(Effect::ClaudeKey(key));
    }
}

// ----- IPC requests -----

fn ipc_context(state: &AppState) -> IpcContext {
    IpcContext {
        root: state
            .claude
            .root
            .clone()
            .unwrap_or_else(|| state.cwd.clone()),
        cwd: state.cwd.clone(),
        show_hidden: state.view.show_hidden,
        respect_gitignore: !state.config.files.show_ignored,
    }
}

fn resolve(root: &Path, path: &str) -> PathBuf {
    let p = Path::new(path);
    if p.is_absolute() {
        p.to_path_buf()
    } else {
        root.join(p)
    }
}

pub(crate) fn on_ipc(
    state: &mut AppState,
    id: u64,
    request: ClaudeRequest,
    effects: &mut Vec<Effect>,
) {
    let reply = |response| Effect::ClaudeIpcReply { id, response };
    match request {
        ClaudeRequest::GetSelection => {
            let to_string = |p: &PathBuf| p.to_string_lossy().into_owned();
            let marked: Vec<String> = if state.show_drives {
                Vec::new()
            } else {
                let view = &state.view;
                view.entries()
                    .iter()
                    .filter(|e| view.is_marked(e))
                    .map(|e| to_string(&state.cwd.join(&e.name)))
                    .collect()
            };
            let cursor = (!state.show_drives)
                .then(|| state.view.current())
                .flatten()
                .map(|e| to_string(&state.cwd.join(&e.name)));
            effects.push(reply(ClaudeResponse::Selection {
                marked,
                cursor,
                cwd: to_string(&state.cwd),
            }));
        }
        ClaudeRequest::HookEvent {
            tool_name,
            file_path,
            ..
        } => {
            if let Some(file) = file_path {
                let root = ipc_context(state).root;
                let path = resolve(&root, &file);
                // The finder's cache must see the edit (a created file shows up in find file).
                effects.push(Effect::WalkCacheChanges(vec![
                    ks_search::PathChange::Touched(path.clone()),
                ]));
                // With the changes view open the selection must stay on the same file (the
                // list is ordered by recency, so a new edit shifts every row) and the diff of
                // a re-edited file is stale.
                let view_open = state.overlays.last() == Some(&Overlay::ClaudeChanges);
                let selected = view_open.then(|| selected_change(state)).flatten();
                state.claude.changes.record(path.clone(), &tool_name);
                if view_open {
                    if let Some(selected) = &selected {
                        if let Some(index) = state
                            .claude
                            .changes
                            .recent()
                            .iter()
                            .position(|c| c.path == *selected)
                        {
                            state.claude.changes_cursor = index;
                        }
                        if *selected == path {
                            state.claude.diff = None;
                        }
                    }
                    request_diff(state, effects);
                }
                state.dirty = true;
            }
            effects.push(reply(ClaudeResponse::Ok));
        }
        request @ (ClaudeRequest::SearchFiles { .. }
        | ClaudeRequest::SearchContents { .. }
        | ClaudeRequest::ListDir { .. }
        | ClaudeRequest::Reveal { .. }) => effects.push(Effect::ClaudeIpcWork {
            id,
            request,
            ctx: ipc_context(state),
        }),
    }
}

/// `Msg::ClaudeReveal`: Claude (via MCP `reveal`) asked to show a path that exists.
pub(crate) fn on_reveal(state: &mut AppState, path: &Path, effects: &mut Vec<Effect>) {
    nav::reveal(state, effects, path);
    let name = path
        .file_name()
        .map_or_else(String::new, |n| n.to_string_lossy().into_owned());
    set_status(
        state,
        Level::Info,
        format!("Claude revealed {name}"),
        effects,
    );
}

// ----- changes view (10e) -----

fn open_changes(state: &mut AppState, effects: &mut Vec<Effect>) {
    if state.claude.changes.is_empty() {
        status(
            state,
            Level::Info,
            "Claude has not changed any files yet",
            effects,
        );
        return;
    }
    state.claude.changes_cursor = 0;
    if state.overlays.last() != Some(&Overlay::ClaudeChanges) {
        state.overlays.push(Overlay::ClaudeChanges);
    }
    state.dirty = true;
    state.claude.diff = None;
    request_diff(state, effects);
}

/// Path of the selected entry of the changes view.
pub fn selected_change(state: &AppState) -> Option<PathBuf> {
    state
        .claude
        .changes
        .recent()
        .get(state.claude.changes_cursor)
        .map(|c| c.path.clone())
}

/// Asks for the diff of the selected file when the diff is shown and not already loaded.
fn request_diff(state: &mut AppState, effects: &mut Vec<Effect>) {
    if !state.claude.diff_open {
        return;
    }
    let Some(path) = selected_change(state) else {
        return;
    };
    if state.claude.diff.as_ref().is_some_and(|d| d.path == path) {
        return;
    }
    state.claude.diff = None;
    effects.push(Effect::ClaudeDiff { path });
}

fn toggle_diff(state: &mut AppState, effects: &mut Vec<Effect>) {
    state.claude.diff_open = !state.claude.diff_open;
    state.dirty = true;
    request_diff(state, effects);
}

/// `Msg::ClaudeDiff`: ignored unless it is for the file currently selected.
pub(crate) fn on_diff(state: &mut AppState, path: &Path, text: String) {
    if !state.claude.diff_open || selected_change(state).as_deref() != Some(path) {
        return;
    }
    state.claude.diff = Some(ChangeDiff {
        path: path.to_path_buf(),
        text,
    });
    state.dirty = true;
}

fn move_changes_cursor(
    state: &mut AppState,
    delta: isize,
    to_edge: Option<bool>,
    effects: &mut Vec<Effect>,
) {
    let len = state.claude.changes.len();
    let cursor = &mut state.claude.changes_cursor;
    match to_edge {
        Some(false) => *cursor = 0,
        Some(true) => *cursor = len.saturating_sub(1),
        None => {
            *cursor = cursor
                .saturating_add_signed(delta)
                .min(len.saturating_sub(1))
        }
    }
    state.dirty = true;
    request_diff(state, effects);
}

fn reveal_selected_change(state: &mut AppState, effects: &mut Vec<Effect>) {
    let path = state
        .claude
        .changes
        .recent()
        .get(state.claude.changes_cursor)
        .map(|c| c.path.clone());
    state.overlays.retain(|o| *o != Overlay::ClaudeChanges);
    if let Some(path) = path {
        nav::reveal(state, effects, &path);
    }
    state.dirty = true;
}

fn clear_changes(state: &mut AppState, effects: &mut Vec<Effect>) {
    state.claude.changes.clear();
    state.claude.changes_cursor = 0;
    state.claude.diff = None;
    state.overlays.retain(|o| *o != Overlay::ClaudeChanges);
    status(
        state,
        Level::Info,
        "Cleared the Claude changes list",
        effects,
    );
}

/// Whether the command palette should list `action` right now (hook: filter its entries with
/// this). `claude.proceed` only means something inside the billing dialog, and
/// `claude.changes_clear` only when there is something to clear; every other action is listed.
pub fn palette_visible(state: &AppState, action: Action) -> bool {
    match action {
        Action::ClaudeProceed => matches!(state.claude.dialog, Some(ClaudeDialog::Billing { .. })),
        Action::ClaudeChangesClear => !state.claude.changes.is_empty(),
        _ => true,
    }
}

// ----- action dispatch -----

/// Handles the actions of this feature (and the generic ones while a Claude overlay is on top).
/// Returns false when `action` is not ours, so the caller falls through to its own handling.
pub(crate) fn handle_action(
    state: &mut AppState,
    action: Action,
    effects: &mut Vec<Effect>,
) -> bool {
    let top = state.overlays.last().copied();
    let dialog_open = top == Some(Overlay::Confirm) && state.claude.dialog.is_some();
    match action {
        Action::ClaudeOpen => start(state, LaunchKind::Embedded { resume: false }, effects),
        Action::ClaudeResume => start(state, LaunchKind::Embedded { resume: true }, effects),
        Action::ClaudeSplit => start(state, LaunchKind::WtSplit, effects),
        Action::ClaudeFocus => toggle_focus(state, effects),
        Action::ClaudeClose => close(state, effects),
        Action::ClaudeGrow => resize_pane(state, i32::from(PERCENT_STEP), effects),
        Action::ClaudeShrink => resize_pane(state, -i32::from(PERCENT_STEP), effects),
        Action::ClaudePosition => toggle_position(state, effects),
        Action::ClaudeScrollUp => scroll(state, true, effects),
        Action::ClaudeScrollDown => scroll(state, false, effects),
        Action::ClaudeSendMarked => send_marked(state, effects),
        Action::ClaudeChanges => open_changes(state, effects),
        Action::ClaudeChangesClear => clear_changes(state, effects),
        Action::ClaudeChangesDiff if top == Some(Overlay::ClaudeChanges) => {
            toggle_diff(state, effects);
        }
        Action::ClaudeChangesDiff => {
            // From the palette: open the view with the diff shown.
            state.claude.diff_open = true;
            open_changes(state, effects);
        }
        Action::ClaudeProceed => {
            if dialog_open && matches!(state.claude.dialog, Some(ClaudeDialog::Billing { .. })) {
                close_dialog(state);
                confirm_proceed(state, effects);
            }
        }
        Action::Accept if dialog_open => {
            // Enter closes information dialogs; the billing warning needs an explicit `y`.
            if matches!(state.claude.dialog, Some(ClaudeDialog::Info { .. })) {
                close_dialog(state);
            }
        }
        Action::Cancel if dialog_open => {
            close_dialog(state);
            state.claude.pending = None;
        }
        Action::Accept if top == Some(Overlay::ClaudeChanges) => {
            reveal_selected_change(state, effects);
        }
        Action::CursorDown if top == Some(Overlay::ClaudeChanges) => {
            move_changes_cursor(state, 1, None, effects);
        }
        Action::CursorUp if top == Some(Overlay::ClaudeChanges) => {
            move_changes_cursor(state, -1, None, effects);
        }
        Action::CursorTop if top == Some(Overlay::ClaudeChanges) => {
            move_changes_cursor(state, 0, Some(false), effects);
        }
        Action::CursorBottom if top == Some(Overlay::ClaudeChanges) => {
            move_changes_cursor(state, 0, Some(true), effects);
        }
        _ => return false,
    }
    true
}

/// True while keys are forwarded to the pane instead of the keymap.
pub fn has_focus(state: &AppState) -> bool {
    focused(state) && state.key_context() == KeyContext::Claude
}

/// Short activity text for the pane border: `Edit src/main.rs` (the most recent edit).
pub fn activity_line(state: &AppState) -> Option<String> {
    let change = state.claude.changes.latest()?;
    let root = state.claude.root.as_deref().unwrap_or(&state.cwd);
    let rel = change.path.strip_prefix(root).unwrap_or(&change.path);
    let rel = rel.to_string_lossy().replace('\\', "/");
    let rel = if rel.chars().count() > LAST_TOOL_LABEL * 3 {
        let tail: String = rel
            .chars()
            .rev()
            .take(LAST_TOOL_LABEL * 3)
            .collect::<Vec<_>>()
            .into_iter()
            .rev()
            .collect();
        format!("…{tail}")
    } else {
        rel
    };
    Some(format!("{} {}", change.tool, rel))
}

#[cfg(test)]
mod tests {
    use std::path::PathBuf;

    use super::*;
    use crate::config::Config;
    use crate::entry::Entry;
    use crate::key::KeyChord;
    use crate::keymap::Keymap;
    use crate::msg::Msg;
    use crate::state::{Places, StartParams};
    use crate::update::update;

    fn cwd() -> PathBuf {
        if cfg!(windows) {
            PathBuf::from("C:/work/proj")
        } else {
            PathBuf::from("/work/proj")
        }
    }

    fn state_sized(size: Size) -> AppState {
        let mut s = AppState::new(StartParams {
            cwd: cwd(),
            home: None,
            places: Places::default(),
            config: Config::default(),
            keymap: Keymap::defaults(),
            size,
            now: std::time::SystemTime::UNIX_EPOCH,
            utc_offset_secs: 0,
        });
        s.dirty = false;
        s
    }

    fn state() -> AppState {
        state_sized(Size::new(120, 32))
    }

    fn key(state: &mut AppState, text: &str) -> Vec<Effect> {
        update(state, Msg::Key(text.parse::<KeyChord>().unwrap()))
    }

    fn preflight(s: &mut AppState, result: ClaudePreflight) -> Vec<Effect> {
        let generation = s.claude.generation;
        update(s, Msg::ClaudePreflight { generation, result })
    }

    #[test]
    fn a_failed_preflight_dialog_discards_a_kept_bulk_rename_file() {
        let mut s = state();
        key(&mut s, "c");
        let file = PathBuf::from("/tmp/kestrel-bulk-rename-7.txt");
        update(
            &mut s,
            Msg::BulkPlanned(crate::fileops::BulkPlanResult::Invalid {
                problems: vec!["line 1: empty name".into()],
                retry: Some(crate::fileops::BulkRetry {
                    dir: cwd(),
                    names: vec!["a.txt".into()],
                    file: file.clone(),
                }),
            }),
        );
        let effects = preflight(&mut s, ClaudePreflight::NotLoggedIn);
        assert!(effects.contains(&Effect::BulkRenameDiscard { file }));
    }

    fn launch(effects: &[Effect]) -> Option<&Effect> {
        effects
            .iter()
            .find(|e| matches!(e, Effect::ClaudeLaunch { .. }))
    }

    /// `c`, passing preflight, started: the pane runs and has focus.
    fn running(s: &mut AppState) -> u64 {
        key(s, "c");
        preflight(s, ClaudePreflight::Ok);
        let generation = s.claude.generation;
        update(s, Msg::ClaudeStarted { generation });
        generation
    }

    fn billing(s: &mut AppState) {
        key(s, "c");
        preflight(
            s,
            ClaudePreflight::BillingWarning {
                method: "api_key".into(),
                provider: "firstParty".into(),
            },
        );
    }

    #[test]
    fn c_runs_preflight_then_launches_and_focuses() {
        let mut s = state();
        let effects = key(&mut s, "c");
        let generation = match effects.first() {
            Some(Effect::ClaudePreflight { generation, spawn }) => {
                assert_eq!(spawn.binary, None);
                assert_eq!(spawn.strip_env.len(), 6);
                *generation
            }
            other => panic!("unexpected {other:?}"),
        };
        assert_eq!(s.claude.phase, ClaudePhase::Checking);

        let effects = preflight(&mut s, ClaudePreflight::Ok);
        match launch(&effects) {
            Some(Effect::ClaudeLaunch {
                cwd: dir,
                resume,
                size,
                ..
            }) => {
                assert_eq!(dir, &cwd());
                assert!(!resume);
                let split = split_body(body_area(s.size), PanePosition::Bottom, 40);
                assert_eq!(*size, split.inner());
            }
            other => panic!("unexpected {other:?}"),
        }
        assert_eq!(s.claude.phase, ClaudePhase::Starting);
        let effects = update(&mut s, Msg::ClaudeStarted { generation });
        assert!(s.claude.is_running());
        assert_eq!(s.key_context(), KeyContext::Claude);
        assert!(effects.contains(&Effect::SetBracketedPaste(true)));
        assert!(has_focus(&s));
    }

    #[test]
    fn focus_key_toggles_and_other_keys_are_forwarded_verbatim() {
        let mut s = state();
        running(&mut s);
        // Everything but the toggle reaches Claude: esc, ctrl-c, shift-tab, plain text.
        for text in [
            "esc",
            "ctrl-c",
            "shift-tab",
            "q",
            "enter",
            "ctrl-d",
            "alt-p",
        ] {
            let chord: KeyChord = text.parse().unwrap();
            let effects = update(&mut s, Msg::Key(chord));
            assert_eq!(effects, vec![Effect::ClaudeKey(chord)], "{text}");
            assert_eq!(s.quit, None, "{text} must not quit Kestrel");
        }
        assert_eq!(s.key_context(), KeyContext::Claude);

        let effects = key(&mut s, "ctrl-\\");
        assert!(effects.contains(&Effect::SetBracketedPaste(false)));
        assert_eq!(s.key_context(), KeyContext::Normal);
        // Unfocused, the explorer's keys work again (q asks before ending the session).
        let effects = key(&mut s, "ctrl-\\");
        assert!(effects.contains(&Effect::SetBracketedPaste(true)));
        assert_eq!(s.key_context(), KeyContext::Claude);
        key(&mut s, "ctrl-\\");
        key(&mut s, "q");
        assert_eq!(s.overlays.last(), Some(&Overlay::Confirm));
        key(&mut s, "y");
        assert_eq!(s.quit, Some(crate::state::QuitKind::Exit));
    }

    #[test]
    fn started_refreshes_the_screen_and_catches_up_with_a_changed_layout() {
        let mut s = state();
        key(&mut s, "c");
        preflight(&mut s, ClaudePreflight::Ok);
        let generation = s.claude.generation;
        let launched = s.claude.pty.expect("launch size is remembered");
        // The window changes while the process is still starting: no resize can be sent yet.
        let during = update(&mut s, Msg::Resize(Size::new(100, 30)));
        assert!(!during
            .iter()
            .any(|e| matches!(e, Effect::ClaudeResize { .. })));
        let effects = update(&mut s, Msg::ClaudeStarted { generation });
        let want = pane_inner_size(&s).unwrap();
        assert_ne!(want, launched);
        assert!(effects.contains(&Effect::ClaudeResize { size: want }));
        // Output that arrived before the pane was registered was dropped: ask for a snapshot.
        assert!(effects.contains(&Effect::ClaudeScroll { lines: 0 }));
        assert_eq!(s.claude.pty, Some(want));
    }

    #[test]
    fn started_without_a_layout_change_sends_no_resize() {
        let mut s = state();
        key(&mut s, "c");
        preflight(&mut s, ClaudePreflight::Ok);
        let generation = s.claude.generation;
        let effects = update(&mut s, Msg::ClaudeStarted { generation });
        assert!(!effects
            .iter()
            .any(|e| matches!(e, Effect::ClaudeResize { .. })));
    }

    #[test]
    fn resuming_while_running_only_focuses_and_says_so() {
        let mut s = state();
        running(&mut s);
        key(&mut s, "ctrl-\\");
        key(&mut s, "g");
        let effects = key(&mut s, "r");
        assert!(launch(&effects).is_none());
        assert_eq!(s.key_context(), KeyContext::Claude);
        let text = s
            .status
            .as_ref()
            .map(|st| st.text.clone())
            .unwrap_or_default();
        assert!(text.contains("already running"), "{text}");
    }

    #[test]
    fn confirm_y_has_one_meaning_and_other_confirms_ignore_it() {
        let matches = crate::keymap::DEFAULT_BINDINGS
            .iter()
            .filter(|(ctx, keys, _)| *ctx == KeyContext::Confirm && *keys == "y")
            .count();
        // `y` is the generic confirm overlay's own accept key (`confirm_key`), not a binding.
        assert_eq!(matches, 0, "a keymap binding for y in Confirm would clash");
        // A confirm that is not Claude's dialog (another feature's) does nothing on y.
        let mut s = state();
        s.overlays.push(Overlay::Confirm);
        assert!(s.claude.dialog.is_none());
        let effects = key(&mut s, "y");
        assert!(effects.is_empty(), "{effects:?}");
        assert_eq!(s.overlays.last(), Some(&Overlay::Confirm));
        assert!(s.claude.pending.is_none());
    }

    #[test]
    fn a_right_pane_narrows_the_list_and_preview_split() {
        let mut s = state_sized(Size::new(160, 40));
        let before = s.pane_widths();
        s.claude.position = PanePosition::Right;
        running(&mut s);
        let split = current_split(&s).unwrap();
        let after = s.pane_widths();
        assert!(split.pane.width > 0);
        assert!(
            after.parent + after.list + after.preview <= split.list.width,
            "{after:?} must fit the {} columns left of the pane",
            split.list.width
        );
        assert!(after.list < before.list || after.preview < before.preview);
        // A bottom pane keeps the columns and takes rows instead.
        s.claude.position = PanePosition::Bottom;
        assert_eq!(s.pane_widths(), before);
        assert!(s.preview_rows() < usize::from(layout::body_height(s.size)));
    }

    #[test]
    fn quitting_with_a_running_session_asks_first() {
        let mut s = state();
        running(&mut s);
        key(&mut s, "ctrl-\\");
        key(&mut s, "q");
        assert_eq!(s.quit, None);
        assert_eq!(s.overlays.last(), Some(&Overlay::Confirm));
        assert!(s
            .fileops
            .confirm
            .as_ref()
            .is_some_and(|c| c.title.contains("close Claude")));
        key(&mut s, "esc");
        assert_eq!(s.quit, None);
        key(&mut s, "q");
        key(&mut s, "y");
        assert!(s.quit.is_some());
    }

    #[test]
    fn pressing_c_again_focuses_the_running_pane() {
        let mut s = state();
        running(&mut s);
        key(&mut s, "ctrl-\\");
        assert_eq!(s.key_context(), KeyContext::Normal);
        let effects = key(&mut s, "c");
        assert_eq!(s.key_context(), KeyContext::Claude);
        assert!(launch(&effects).is_none());
        assert!(!effects
            .iter()
            .any(|e| matches!(e, Effect::ClaudePreflight { .. })));
    }

    #[test]
    fn g_r_resumes() {
        let mut s = state();
        key(&mut s, "g");
        key(&mut s, "r");
        assert_eq!(
            s.claude.pending,
            Some(LaunchKind::Embedded { resume: true })
        );
        let effects = preflight(&mut s, ClaudePreflight::Ok);
        assert!(matches!(
            launch(&effects),
            Some(Effect::ClaudeLaunch { resume: true, .. })
        ));
    }

    #[test]
    fn stale_preflight_is_ignored() {
        let mut s = state();
        key(&mut s, "c");
        let effects = update(
            &mut s,
            Msg::ClaudePreflight {
                generation: 99,
                result: ClaudePreflight::Ok,
            },
        );
        assert!(effects.is_empty());
        assert_eq!(s.claude.phase, ClaudePhase::Checking);
    }

    #[test]
    fn a_second_open_while_checking_does_not_start_another_preflight() {
        let mut s = state();
        key(&mut s, "c");
        let effects = key(&mut s, "c");
        assert!(!effects
            .iter()
            .any(|e| matches!(e, Effect::ClaudePreflight { .. })));
        assert!(s.status.as_ref().unwrap().text.contains("already starting"));
    }

    // ----- preflight decision flow (fixtures for every outcome) -----

    #[test]
    fn palette_hides_dialog_and_changes_only_actions_when_not_applicable() {
        let mut s = state();
        assert!(!palette_visible(&s, Action::ClaudeProceed));
        assert!(!palette_visible(&s, Action::ClaudeChangesClear));
        assert!(palette_visible(&s, Action::ClaudeOpen));
        billing(&mut s);
        assert!(palette_visible(&s, Action::ClaudeProceed));
        s.claude.dialog = None;
        s.claude.changes.record(PathBuf::from("/x/a.rs"), "Edit");
        assert!(palette_visible(&s, Action::ClaudeChangesClear));
    }

    #[test]
    fn billing_warning_blocks_until_explicitly_confirmed_with_y() {
        let mut s = state();
        billing(&mut s);
        assert_eq!(s.overlays, vec![Overlay::Confirm]);
        assert!(matches!(
            s.claude.dialog,
            Some(ClaudeDialog::Billing { .. })
        ));
        assert_eq!(s.claude.phase, ClaudePhase::Idle);

        // Enter (the usual "accept") must NOT launch: billing needs a deliberate y.
        let effects = key(&mut s, "enter");
        assert!(launch(&effects).is_none());
        assert_eq!(s.overlays, vec![Overlay::Confirm]);

        let effects = key(&mut s, "y");
        assert!(launch(&effects).is_some());
        assert!(s.overlays.is_empty());
        assert!(s.claude.dialog.is_none());
        assert_eq!(s.claude.phase, ClaudePhase::Starting);
    }

    #[test]
    fn declining_the_billing_warning_launches_nothing() {
        let mut s = state();
        billing(&mut s);
        let effects = key(&mut s, "esc");
        assert!(launch(&effects).is_none());
        assert!(s.overlays.is_empty());
        assert!(s.claude.dialog.is_none());
        assert_eq!(s.claude.phase, ClaudePhase::Idle);
        assert_eq!(s.claude.pending, None);
        // `y` outside a dialog does nothing harmful.
        let effects = key(&mut s, "y");
        assert!(launch(&effects).is_none());
    }

    #[test]
    fn billing_confirmation_also_applies_to_the_wt_split() {
        let mut s = state();
        key(&mut s, "g");
        key(&mut s, "w");
        preflight(
            &mut s,
            ClaudePreflight::BillingWarning {
                method: "api_key".into(),
                provider: "firstParty".into(),
            },
        );
        let effects = key(&mut s, "y");
        assert!(matches!(
            effects.iter().find(|e| matches!(e, Effect::ClaudeWtSplit { .. })),
            Some(Effect::ClaudeWtSplit { cwd: dir, .. }) if dir == &cwd()
        ));
        assert_eq!(s.claude.phase, ClaudePhase::Idle);
    }

    #[test]
    fn wt_split_runs_the_preflight_first() {
        let mut s = state();
        let effects = key(&mut s, "g");
        assert!(effects
            .iter()
            .all(|e| !matches!(e, Effect::ClaudePreflight { .. })));
        let effects = key(&mut s, "w");
        assert!(effects
            .iter()
            .any(|e| matches!(e, Effect::ClaudePreflight { .. })));
        assert!(!effects
            .iter()
            .any(|e| matches!(e, Effect::ClaudeWtSplit { .. })));
        let effects = preflight(&mut s, ClaudePreflight::Ok);
        assert!(effects
            .iter()
            .any(|e| matches!(e, Effect::ClaudeWtSplit { .. })));
        assert!(launch(&effects).is_none());
    }

    #[test]
    fn install_and_login_instructions_are_informational() {
        for result in [
            ClaudePreflight::NotInstalled("install it".into()),
            ClaudePreflight::NotLoggedIn,
            ClaudePreflight::Failed("timed out".into()),
        ] {
            let mut s = state();
            key(&mut s, "c");
            preflight(&mut s, result);
            assert!(matches!(s.claude.dialog, Some(ClaudeDialog::Info { .. })));
            assert_eq!(s.claude.phase, ClaudePhase::Idle);
            let effects = key(&mut s, "enter");
            assert!(launch(&effects).is_none());
            assert!(s.claude.dialog.is_none());
            assert!(s.overlays.is_empty());
            // Nothing is pending, so a stray y cannot launch.
            let effects = key(&mut s, "y");
            assert!(launch(&effects).is_none());
        }
    }

    #[test]
    fn not_logged_in_tells_the_user_to_run_claude() {
        let mut s = state();
        key(&mut s, "c");
        preflight(&mut s, ClaudePreflight::NotLoggedIn);
        match &s.claude.dialog {
            Some(ClaudeDialog::Info { body, .. }) => {
                assert!(body.contains("/login") && body.contains("`claude`"));
            }
            other => panic!("{other:?}"),
        }
    }

    #[test]
    fn exit_failure_and_close_reset_the_pane() {
        let mut s = state();
        let generation = running(&mut s);
        update(
            &mut s,
            Msg::ClaudeScreen {
                generation,
                screen: Box::new(ClaudeScreen::default()),
            },
        );
        assert!(s.claude.screen.is_some());
        update(
            &mut s,
            Msg::ClaudePaneExited {
                generation,
                code: Some(2),
            },
        );
        assert_eq!(s.claude.phase, ClaudePhase::Idle);
        assert!(s.claude.screen.is_none());
        assert!(s.overlays.is_empty());
        assert!(s.status.as_ref().unwrap().text.contains("code 2"));

        key(&mut s, "c");
        let generation = s.claude.generation;
        update(
            &mut s,
            Msg::ClaudeLaunchFailed {
                generation,
                message: "boom".into(),
            },
        );
        assert_eq!(s.claude.phase, ClaudePhase::Idle);
        assert_eq!(s.status.as_ref().unwrap().level, Level::Error);

        // Closing from the keyboard kills the process and ignores its late exit event.
        let generation = running(&mut s);
        key(&mut s, "ctrl-\\");
        let effects = key(&mut s, "g");
        assert!(effects
            .iter()
            .all(|e| !matches!(e, Effect::ClaudeClose { .. })));
        let effects = key(&mut s, "X");
        assert!(effects
            .iter()
            .any(|e| matches!(e, Effect::ClaudeClose { generation } if *generation == s.claude.generation)));
        assert_eq!(s.claude.phase, ClaudePhase::Idle);
        update(
            &mut s,
            Msg::ClaudePaneExited {
                generation,
                code: Some(1),
            },
        );
        assert!(!s.status.as_ref().unwrap().text.contains("code 1"));
    }

    #[test]
    fn a_failing_pane_reports_what_it_printed() {
        let mut s = state();
        let generation = running(&mut s);
        let text = "No conversation found with session ID: abc";
        let cols = 60u16;
        let mut cells = vec![ClaudeCell::default(); usize::from(cols) * 3];
        for (i, ch) in text.chars().enumerate() {
            cells[usize::from(cols) + i].text = ch.to_string();
        }
        update(
            &mut s,
            Msg::ClaudeScreen {
                generation,
                screen: Box::new(ClaudeScreen {
                    cols,
                    rows: 3,
                    cells,
                    cursor: None,
                }),
            },
        );
        update(
            &mut s,
            Msg::ClaudePaneExited {
                generation,
                code: Some(1),
            },
        );
        let status = s.status.as_ref().unwrap();
        assert_eq!(status.level, Level::Warning);
        assert_eq!(status.text, format!("Claude exited with code 1: {text}"));
        assert_eq!(s.claude.phase, ClaudePhase::Idle);
    }

    #[test]
    fn focus_toggle_without_a_pane_explains() {
        let mut s = state();
        key(&mut s, "ctrl-\\");
        assert!(s.status.as_ref().unwrap().text.contains("not running"));
        assert!(s.overlays.is_empty());
    }

    // ----- layout and resizing -----

    #[test]
    fn bottom_split_shrinks_the_list_and_the_viewport() {
        let mut s = state_sized(Size::new(80, 24));
        let before = s.viewport_height();
        assert_eq!(before, 19);
        running(&mut s);
        let split = current_split(&s).unwrap();
        assert_eq!(split.pane.height + split.list.height, 19);
        assert_eq!(s.viewport_height(), usize::from(split.list.height));
        assert!(s.viewport_height() < before);
        // The pane gives its room back when it goes away.
        key(&mut s, "ctrl-\\");
        key(&mut s, "g");
        key(&mut s, "X");
        assert_eq!(s.viewport_height(), before);
    }

    #[test]
    fn right_split_keeps_the_list_height() {
        let mut s = state_sized(Size::new(120, 32));
        s.claude.position = PanePosition::Right;
        running(&mut s);
        let split = current_split(&s).unwrap();
        assert!(split.border_left && !split.covers);
        assert_eq!(split.list.width + split.pane.width, 120);
        assert_eq!(
            s.viewport_height(),
            usize::from(layout::body_height(s.size))
        );
    }

    #[test]
    fn small_terminals_get_a_covering_pane() {
        let body = Area::new(0, 2, 60, 8);
        assert!(split_body(body, PanePosition::Bottom, 40).covers);
        assert!(split_body(body, PanePosition::Right, 40).covers);
        let big = Area::new(0, 2, 200, 50);
        let split = split_body(big, PanePosition::Bottom, 40);
        assert_eq!((split.pane.height, split.list.height), (20, 30));
        assert_eq!(split.inner(), Size::new(200, 19));
    }

    #[test]
    fn grow_shrink_and_position_resize_the_pty() {
        let mut s = state();
        running(&mut s);
        let before = s.claude.pty.unwrap();
        let effects = key(&mut s, "alt-=");
        let grown = s.claude.pty.unwrap();
        assert!(grown.height > before.height);
        assert_eq!(effects, vec![Effect::ClaudeResize { size: grown }]);
        key(&mut s, "alt--");
        key(&mut s, "alt--");
        assert!(s.claude.pty.unwrap().height < before.height);

        // Resizing keeps working while the pane has focus (the keys are bound there too).
        assert_eq!(s.key_context(), KeyContext::Claude);

        key(&mut s, "ctrl-\\");
        key(&mut s, "g");
        let effects = key(&mut s, "p");
        assert_eq!(s.claude.position, PanePosition::Right);
        assert!(effects
            .iter()
            .any(|e| matches!(e, Effect::ClaudeResize { .. })));
        let effects = update(&mut s, Msg::Resize(Size::new(100, 30)));
        assert!(effects
            .iter()
            .any(|e| matches!(e, Effect::ClaudeResize { .. })));
    }

    #[test]
    fn percent_is_clamped() {
        let mut s = state();
        running(&mut s);
        for _ in 0..40 {
            key(&mut s, "alt-=");
        }
        assert_eq!(s.claude.percent, MAX_PERCENT);
        for _ in 0..40 {
            key(&mut s, "alt--");
        }
        assert_eq!(s.claude.percent, MIN_PERCENT);
    }

    #[test]
    fn scrolling_is_only_for_a_running_pane() {
        let mut s = state();
        assert!(key(&mut s, "ctrl-\\")
            .iter()
            .all(|e| !matches!(e, Effect::ClaudeScroll { .. })));
        running(&mut s);
        let effects = key(&mut s, "shift-pageup");
        assert!(matches!(
            effects.as_slice(),
            [Effect::ClaudeScroll { lines }] if *lines > 0
        ));
        let effects = key(&mut s, "shift-pagedown");
        assert!(matches!(
            effects.as_slice(),
            [Effect::ClaudeScroll { lines }] if *lines < 0
        ));
    }

    #[test]
    fn paste_is_forwarded_only_while_focused() {
        let mut s = state();
        assert!(update(&mut s, Msg::Paste("x".into())).is_empty());
        running(&mut s);
        let effects = update(&mut s, Msg::Paste("hello\nworld".into()));
        assert_eq!(effects, vec![Effect::ClaudePaste("hello\nworld".into())]);
        key(&mut s, "ctrl-\\");
        assert!(update(&mut s, Msg::Paste("x".into())).is_empty());
    }

    // ----- mentions -----

    fn with_files(s: &mut AppState) {
        let height = s.viewport_height();
        s.view.push_chunk(
            vec![
                Entry::file("a.rs", 1),
                Entry::file("b.rs", 2),
                Entry::file("c.rs", 3),
            ],
            height,
        );
        s.view.finish_loading();
    }

    #[test]
    fn capital_c_sends_the_marked_files_then_the_cursor_file() {
        let mut s = state();
        with_files(&mut s);
        // Not running: explains.
        key(&mut s, "C");
        assert!(s.status.as_ref().unwrap().text.contains("not running"));

        running(&mut s);
        key(&mut s, "ctrl-\\");
        // Cursor file only.
        let effects = key(&mut s, "C");
        assert!(effects.contains(&Effect::ClaudeSendPaths {
            paths: vec![cwd().join("a.rs")]
        }));
        assert_eq!(
            s.key_context(),
            KeyContext::Claude,
            "sending focuses the pane"
        );

        // Marked files, in list order.
        key(&mut s, "ctrl-\\");
        key(&mut s, "j");
        key(&mut s, "space"); // marks b.rs, cursor moves to c.rs
        key(&mut s, "space"); // marks c.rs
        let effects = key(&mut s, "C");
        assert!(effects.contains(&Effect::ClaudeSendPaths {
            paths: vec![cwd().join("b.rs"), cwd().join("c.rs")]
        }));
    }

    #[test]
    fn line_ranges_have_their_own_message() {
        let mut s = state();
        let msg = || Msg::ClaudeSendRange {
            path: cwd().join("a.rs"),
            start: 3,
            end: 9,
        };
        assert!(update(&mut s, msg())
            .iter()
            .all(|e| !matches!(e, Effect::ClaudeSendRange { .. })));
        running(&mut s);
        key(&mut s, "ctrl-\\");
        let effects = update(&mut s, msg());
        assert!(effects.contains(&Effect::ClaudeSendRange {
            path: cwd().join("a.rs"),
            start: 3,
            end: 9
        }));
        assert_eq!(s.key_context(), KeyContext::Claude);
    }

    // ----- IPC -----

    fn ipc(s: &mut AppState, id: u64, request: ClaudeRequest) -> Vec<Effect> {
        update(s, Msg::ClaudeIpc { id, request })
    }

    #[test]
    fn get_selection_reports_marked_cursor_and_cwd() {
        let mut s = state();
        with_files(&mut s);
        key(&mut s, "j");
        key(&mut s, "space"); // marks b.rs
        let effects = ipc(&mut s, 7, ClaudeRequest::GetSelection);
        let to = |name: &str| cwd().join(name).to_string_lossy().into_owned();
        assert_eq!(
            effects,
            vec![Effect::ClaudeIpcReply {
                id: 7,
                response: ClaudeResponse::Selection {
                    marked: vec![to("b.rs")],
                    cursor: Some(to("c.rs")),
                    cwd: cwd().to_string_lossy().into_owned(),
                }
            }]
        );
    }

    #[test]
    fn file_system_requests_become_work_with_the_project_context() {
        let mut s = state();
        let effects = ipc(
            &mut s,
            1,
            ClaudeRequest::SearchFiles {
                query: "main".into(),
                root: None,
                limit: 10,
            },
        );
        match effects.as_slice() {
            [Effect::ClaudeIpcWork { id: 1, ctx, .. }] => {
                assert_eq!(ctx.root, cwd());
                assert!(ctx.respect_gitignore && !ctx.show_hidden);
            }
            other => panic!("{other:?}"),
        }
        for request in [
            ClaudeRequest::ListDir { path: ".".into() },
            ClaudeRequest::Reveal {
                path: "a.rs".into(),
            },
            ClaudeRequest::SearchContents {
                pattern: "x".into(),
                glob: None,
                regex: false,
                limit: 5,
            },
        ] {
            let effects = ipc(&mut s, 2, request);
            assert!(matches!(
                effects.as_slice(),
                [Effect::ClaudeIpcWork { id: 2, .. }]
            ));
        }
    }

    #[test]
    fn reveal_in_the_current_directory_moves_the_cursor() {
        let mut s = state();
        with_files(&mut s);
        let effects = update(
            &mut s,
            Msg::ClaudeReveal {
                path: cwd().join("c.rs"),
            },
        );
        assert_eq!(s.view.current().unwrap().name, "c.rs");
        assert!(effects.iter().all(|e| !matches!(e, Effect::ListDir { .. })));
    }

    #[test]
    fn reveal_elsewhere_navigates_with_the_cursor_on_the_file() {
        let mut s = state();
        with_files(&mut s);
        let target = cwd().join("sub").join("deep.rs");
        let effects = update(&mut s, Msg::ClaudeReveal { path: target });
        match effects.iter().find(|e| matches!(e, Effect::ListDir { .. })) {
            Some(Effect::ListDir { path, .. }) => assert_eq!(path, &cwd().join("sub")),
            other => panic!("{other:?}"),
        }
        assert!(s.is_navigating());
    }

    // ----- hook attribution (10e) -----

    fn hook(s: &mut AppState, tool: &str, file: Option<&str>) -> Vec<Effect> {
        ipc(
            s,
            3,
            ClaudeRequest::HookEvent {
                session_id: "sess".into(),
                tool_name: tool.into(),
                file_path: file.map(str::to_owned),
            },
        )
    }

    #[test]
    fn hook_events_mark_files_and_reply_ok() {
        let mut s = state();
        let file = cwd().join("src").join("lib.rs");
        let effects = hook(&mut s, "Edit", Some(&file.to_string_lossy()));
        assert_eq!(
            effects,
            vec![
                Effect::WalkCacheChanges(vec![ks_search::PathChange::Touched(file.clone())]),
                Effect::ClaudeIpcReply {
                    id: 3,
                    response: ClaudeResponse::Ok
                }
            ]
        );
        assert!(s.claude.changes.contains(&file));
        assert!(s.claude.changes.contains_below(&cwd().join("src")));
        assert!(s.claude.changes.contains_below(&cwd()));
        assert!(!s.claude.changes.contains(&cwd().join("src")));
        assert!(!s.claude.changes.contains_below(&cwd().join("other")));
        assert!(s.dirty);
    }

    #[test]
    fn hook_paths_are_normalised_and_relative_ones_use_the_project_root() {
        let mut s = state();
        hook(&mut s, "Write", Some("src/./x/../new.rs"));
        assert!(s.claude.changes.contains(&cwd().join("src").join("new.rs")));
        // Backslashes and a verbatim prefix still match on every platform.
        assert_eq!(
            path_key(Path::new("a\\b\\..\\c")),
            path_key(Path::new("a/c"))
        );
        assert_eq!(
            path_key(Path::new("\\\\?\\C:\\x\\y")),
            path_key(Path::new("C:/x/y"))
        );
        // A payload without a file only acknowledges.
        let before = s.claude.changes.len();
        let effects = hook(&mut s, "Edit", None);
        assert_eq!(effects.len(), 1);
        assert_eq!(s.claude.changes.len(), before);
    }

    #[cfg(windows)]
    #[test]
    fn windows_paths_compare_case_insensitively() {
        let mut log = ChangeLog::default();
        log.record(PathBuf::from("C:\\Work\\Proj\\A.rs"), "Edit");
        assert!(log.contains(Path::new("c:/work/proj/a.RS")));
    }

    #[test]
    fn changes_are_listed_most_recent_first_and_re_edits_move_up() {
        let mut log = ChangeLog::default();
        log.record(PathBuf::from("/p/a"), "Edit");
        log.record(PathBuf::from("/p/b"), "Write");
        log.record(PathBuf::from("/p/a"), "MultiEdit");
        let recent: Vec<_> = log.recent().iter().map(|c| c.path.clone()).collect();
        assert_eq!(recent, [PathBuf::from("/p/a"), PathBuf::from("/p/b")]);
        assert_eq!(log.latest().unwrap().tool, "MultiEdit");
        assert_eq!(log.len(), 2);
    }

    #[test]
    fn a_new_edit_keeps_the_changes_selection_and_never_shows_another_files_diff() {
        let mut s = state();
        let a = cwd().join("a.rs");
        let b = cwd().join("b.rs");
        hook(&mut s, "Edit", Some(&a.to_string_lossy()));
        key(&mut s, "g");
        key(&mut s, "a");
        key(&mut s, "d");
        update(
            &mut s,
            Msg::ClaudeDiff {
                path: a.clone(),
                text: "+a".into(),
            },
        );
        assert_eq!(s.claude.diff.as_ref().unwrap().path, a);
        // Claude edits b while the view is open: b becomes row 0, the selection stays on a.
        let effects = hook(&mut s, "Edit", Some(&b.to_string_lossy()));
        assert_eq!(selected_change(&s), Some(a.clone()));
        assert_eq!(s.claude.changes_cursor, 1);
        assert_eq!(s.claude.diff.as_ref().unwrap().path, a);
        assert!(!effects
            .iter()
            .any(|e| matches!(e, Effect::ClaudeDiff { .. })));
        // Re-editing the selected file invalidates its diff and asks for a fresh one.
        let effects = hook(&mut s, "Edit", Some(&a.to_string_lossy()));
        assert_eq!(selected_change(&s), Some(a.clone()));
        assert!(effects
            .iter()
            .any(|e| matches!(e, Effect::ClaudeDiff { path } if *path == a)));
        assert!(s.claude.diff.is_none());
    }

    #[test]
    fn changes_diff_is_requested_on_toggle_and_on_cursor_move_and_stale_answers_are_dropped() {
        let mut s = state();
        let one = cwd().join("one.rs");
        let two = cwd().join("two.rs");
        hook(&mut s, "Edit", Some(&one.to_string_lossy()));
        hook(&mut s, "Write", Some(&two.to_string_lossy()));
        key(&mut s, "g");
        let effects = key(&mut s, "a");
        assert!(!effects
            .iter()
            .any(|e| matches!(e, Effect::ClaudeDiff { .. })));
        // Most recent first: two.rs is selected.
        let effects = key(&mut s, "d");
        assert!(effects
            .iter()
            .any(|e| matches!(e, Effect::ClaudeDiff { path } if *path == two)));
        update(
            &mut s,
            Msg::ClaudeDiff {
                path: one.clone(),
                text: "stale".into(),
            },
        );
        assert!(s.claude.diff.is_none(), "answer for another file ignored");
        update(
            &mut s,
            Msg::ClaudeDiff {
                path: two.clone(),
                text: "+x".into(),
            },
        );
        assert_eq!(s.claude.diff.as_ref().unwrap().text, "+x");
        let effects = key(&mut s, "j");
        assert!(effects
            .iter()
            .any(|e| matches!(e, Effect::ClaudeDiff { path } if *path == one)));
        assert!(s.claude.diff.is_none());
        // Hiding stops requests.
        key(&mut s, "d");
        let effects = key(&mut s, "k");
        assert!(!effects
            .iter()
            .any(|e| matches!(e, Effect::ClaudeDiff { .. })));
    }

    #[test]
    fn changes_view_opens_navigates_reveals_and_clears() {
        let mut s = state();
        key(&mut s, "g");
        key(&mut s, "a");
        assert!(s.status.as_ref().unwrap().text.contains("not changed"));
        assert!(s.overlays.is_empty());

        hook(
            &mut s,
            "Edit",
            Some(&cwd().join("one.rs").to_string_lossy()),
        );
        hook(
            &mut s,
            "Write",
            Some(&cwd().join("two.rs").to_string_lossy()),
        );
        key(&mut s, "g");
        key(&mut s, "a");
        assert_eq!(s.overlays, vec![Overlay::ClaudeChanges]);
        assert_eq!(s.claude.changes_cursor, 0);
        key(&mut s, "j");
        assert_eq!(s.claude.changes_cursor, 1);
        key(&mut s, "j");
        assert_eq!(s.claude.changes_cursor, 1, "clamped");
        key(&mut s, "k");
        assert_eq!(s.claude.changes_cursor, 0);

        // enter reveals the selected (most recent) file and closes the view.
        with_files(&mut s);
        let effects = key(&mut s, "enter");
        assert!(s.overlays.is_empty());
        let _ = effects;

        key(&mut s, "g");
        key(&mut s, "a");
        key(&mut s, "X");
        assert!(s.claude.changes.is_empty());
        assert!(s.overlays.is_empty());

        key(&mut s, "g");
        key(&mut s, "a");
        hook(
            &mut s,
            "Edit",
            Some(&cwd().join("three.rs").to_string_lossy()),
        );
        key(&mut s, "g");
        key(&mut s, "a");
        key(&mut s, "esc");
        assert!(s.overlays.is_empty());
    }

    #[test]
    fn activity_line_shows_the_latest_edit_relative_to_the_root() {
        let mut s = state();
        assert_eq!(activity_line(&s), None);
        hook(
            &mut s,
            "Edit",
            Some(&cwd().join("src").join("a.rs").to_string_lossy()),
        );
        assert_eq!(activity_line(&s).as_deref(), Some("Edit src/a.rs"));
    }

    #[test]
    fn spawn_config_comes_from_the_config() {
        let mut config = Config::default();
        config.claude.binary = "  D:/tools/claude.exe ".into();
        config.claude.strip_env = vec!["X".into()];
        let spawn = ClaudeSpawn::from_config(&config.claude);
        assert_eq!(spawn.binary, Some(PathBuf::from("D:/tools/claude.exe")));
        assert_eq!(spawn.strip_env, ["X"]);
        assert_eq!(
            ClaudeSpawn::from_config(&Config::default().claude).binary,
            None
        );
    }

    #[test]
    fn screen_cell_lookup_is_bounds_checked() {
        let screen = ClaudeScreen {
            cols: 2,
            rows: 1,
            cells: vec![ClaudeCell::default(); 2],
            cursor: None,
        };
        assert!(screen.cell(0, 1).is_some());
        assert!(screen.cell(1, 0).is_none());
        assert!(screen.cell(0, 2).is_none());
    }

    #[test]
    fn tiny_terminals_refuse_the_pane_with_a_message() {
        let mut s = state_sized(Size::new(3, 3));
        key(&mut s, "c");
        let effects = preflight(&mut s, ClaudePreflight::Ok);
        assert!(launch(&effects).is_none());
        assert!(s.status.as_ref().unwrap().text.contains("too small"));
        assert_eq!(s.claude.phase, ClaudePhase::Idle);
    }
}
