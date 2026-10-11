//! Navigation: moving between directories, history, and applying listing results.
//!
//! A navigation is a *transaction*. Asking to open a directory starts a listing under a
//! fresh generation and records a [`PendingNav`]; the screen keeps showing the old
//! directory until the first results arrive. Only then is the new location committed
//! (history updated, cursor remembered, watcher and git restarted). A directory that cannot
//! be read therefore never replaces a working view: the user stays where they were and sees
//! the reason in the status line.

use std::ffi::OsString;
use std::path::PathBuf;
use std::time::Duration;

use crate::action::Action;
use crate::config::SortKey;
use crate::drives::DriveInfo;
use crate::effect::Effect;
use crate::entry::Entry;
use crate::error::{FsError, FsErrorKind};
use crate::generation::Gen;
use crate::git::GitSnapshot;
use crate::mode::Mode;
use crate::msg::TimerKind;
use crate::state::{AppState, Level};
use crate::update::set_status;
use crate::view::DirView;

/// How many directories get a lazily computed child count after a listing.
pub const CHILD_COUNT_LIMIT: usize = 256;
/// Debounce between a file system change and the `git status` it triggers.
pub const GIT_DEBOUNCE: Duration = Duration::from_millis(300);

const HISTORY_LIMIT: usize = 200;
const CURSOR_MEMORY_LIMIT: usize = 4096;

/// Somewhere the browser can be.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) enum Location {
    Dir(PathBuf),
    /// The virtual "This PC" directory.
    Drives,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct HistoryEntry {
    location: Location,
    /// Name of the entry the cursor was on.
    cursor: Option<OsString>,
}

#[derive(Debug, Default)]
pub(crate) struct History {
    back: Vec<HistoryEntry>,
    forward: Vec<HistoryEntry>,
}

impl History {
    fn push_back(&mut self, entry: HistoryEntry) {
        if self.back.len() >= HISTORY_LIMIT {
            self.back.remove(0);
        }
        self.back.push(entry);
    }
}

#[derive(Debug)]
pub(crate) enum NavKind {
    /// A normal move: the old location goes onto the back stack.
    Visit,
    /// History back; carries the popped entry so a failure can put it back.
    Back(HistoryEntry),
    Forward(HistoryEntry),
    /// Re-listing the current directory after a file system change.
    Refresh,
    /// Moving up out of a directory that vanished; leaves no history.
    Climb,
}

#[derive(Debug)]
pub(crate) struct PendingNav {
    gen: Gen,
    target: Location,
    kind: NavKind,
    reveal: Option<OsString>,
    /// Entries of a refresh, held back until the listing is complete.
    buffer: Vec<Entry>,
}

// ----- requesting navigation -----

/// `Msg::Start`: list the start directory.
pub(crate) fn start(state: &mut AppState, effects: &mut Vec<Effect>) {
    let cwd = state.cwd.clone();
    request(state, effects, Location::Dir(cwd), NavKind::Visit, None);
}

pub(crate) fn request(
    state: &mut AppState,
    effects: &mut Vec<Effect>,
    target: Location,
    kind: NavKind,
    reveal: Option<OsString>,
) {
    // A refresh (watcher event, after a file operation) lists the same directory; it must not
    // swallow marks that a find-file jump asked for while that directory was still loading.
    if !matches!(kind, NavKind::Refresh) {
        state.search.pending_marks = None;
        state.search.pending_unhide = None;
    }
    let reveal = match (&target, &kind) {
        (Location::Dir(path), NavKind::Visit) => {
            reveal.or_else(|| state.cursor_memory.get(path).cloned())
        }
        _ => reveal,
    };
    abandon_history_nav(state);
    let gen = state.list_gen.bump();
    effects.push(match &target {
        Location::Dir(path) => Effect::ListDir {
            gen,
            path: path.clone(),
        },
        Location::Drives => Effect::ListDrives { gen },
    });
    state.nav = Some(PendingNav {
        gen,
        target,
        kind,
        reveal,
        buffer: Vec::new(),
    });
    state.dirty = true;
}

/// Cancels a pending history move, putting the entry it popped back where it came from so
/// a superseding navigation does not lose it.
pub(crate) fn abandon_history_nav(state: &mut AppState) {
    let Some(pending) = state
        .nav
        .take_if(|p| matches!(p.kind, NavKind::Back(_) | NavKind::Forward(_)))
    else {
        return;
    };
    match pending.kind {
        NavKind::Back(entry) => state.history.back.push(entry),
        NavKind::Forward(entry) => state.history.forward.push(entry),
        _ => {}
    }
}

fn visit(
    state: &mut AppState,
    effects: &mut Vec<Effect>,
    target: Location,
    reveal: Option<OsString>,
) {
    request(state, effects, target, NavKind::Visit, reveal);
}

/// `l` / `enter` / `right`.
pub(crate) fn enter(state: &mut AppState, effects: &mut Vec<Effect>) {
    if state.show_drives {
        if let Some(drive) = state.drives.current() {
            let root = drive.root.clone();
            visit(state, effects, Location::Dir(root), None);
        }
        return;
    }
    let Some(entry) = state.view.current() else {
        return;
    };
    if entry.is_dir {
        let target = state.cwd.join(&entry.name);
        visit(state, effects, Location::Dir(target), None);
    } else {
        effects.push(Effect::OpenDefault {
            path: state.cwd.join(&entry.name),
        });
    }
}

/// Shows `path` with the cursor on it (a path Claude mentioned): in place when its parent is
/// the displayed directory, otherwise by navigating to the parent.
pub(crate) fn reveal(state: &mut AppState, effects: &mut Vec<Effect>, path: &std::path::Path) {
    let (Some(parent), Some(name)) = (path.parent(), path.file_name()) else {
        set_status(
            state,
            Level::Warning,
            format!("Cannot reveal {}", path.display()),
            effects,
        );
        return;
    };
    if !state.show_drives
        && crate::claude::path_key(parent) == crate::claude::path_key(&state.cwd)
        && state.nav.is_none()
    {
        let height = state.viewport_height();
        if state.view.reveal_now(name, height) {
            state.dirty = true;
            return;
        }
    }
    visit(
        state,
        effects,
        Location::Dir(parent.to_owned()),
        Some(name.to_owned()),
    );
}

/// `h` / `left` / `backspace`: up one level; above a drive root lies "This PC".
pub(crate) fn parent(state: &mut AppState, effects: &mut Vec<Effect>) {
    if state.show_drives {
        return;
    }
    let came_from = state.cwd.file_name().map(ToOwned::to_owned);
    match state.cwd.parent() {
        Some(parent) if !parent.as_os_str().is_empty() => {
            let target = Location::Dir(parent.to_owned());
            visit(state, effects, target, came_from);
        }
        _ => visit(state, effects, Location::Drives, None),
    }
}

pub(crate) fn history_back(state: &mut AppState, effects: &mut Vec<Effect>) {
    abandon_history_nav(state);
    let Some(entry) = state.history.back.pop() else {
        set_status(
            state,
            Level::Info,
            "Already at the oldest location".into(),
            effects,
        );
        return;
    };
    let (target, reveal) = (entry.location.clone(), entry.cursor.clone());
    request(state, effects, target, NavKind::Back(entry), reveal);
}

pub(crate) fn history_forward(state: &mut AppState, effects: &mut Vec<Effect>) {
    abandon_history_nav(state);
    let Some(entry) = state.history.forward.pop() else {
        set_status(
            state,
            Level::Info,
            "Already at the newest location".into(),
            effects,
        );
        return;
    };
    let (target, reveal) = (entry.location.clone(), entry.cursor.clone());
    request(state, effects, target, NavKind::Forward(entry), reveal);
}

/// The `~`, `g h`, `g d`, `g D`, `g c` and `g /` jumps.
pub(crate) fn go_place(state: &mut AppState, effects: &mut Vec<Effect>, action: Action) {
    let (path, name) = match action {
        Action::GoHome => (state.places.home.clone(), "Home"),
        Action::GoDownloads => (state.places.downloads.clone(), "Downloads"),
        Action::GoDesktop => (state.places.desktop.clone(), "Desktop"),
        Action::GoConfig => (state.places.config.clone(), "The config directory"),
        Action::GoDrives => {
            visit(state, effects, Location::Drives, None);
            return;
        }
        _ => return,
    };
    match path {
        Some(path) => visit(state, effects, Location::Dir(path), None),
        None => set_status(
            state,
            Level::Warning,
            format!("{name} is not available on this system"),
            effects,
        ),
    }
}

/// Re-lists what is shown: the drives on "This PC", otherwise the current directory.
pub(crate) fn refresh(state: &mut AppState, effects: &mut Vec<Effect>) {
    let target = if state.show_drives {
        Location::Drives
    } else {
        Location::Dir(state.cwd.clone())
    };
    request(state, effects, target, NavKind::Refresh, None);
}

/// Re-lists the displayed directory, unless a real navigation is in flight: that one lists
/// afresh anyway and a refresh would replace it. Used after file operations.
pub(crate) fn refresh_if_idle(state: &mut AppState, effects: &mut Vec<Effect>) {
    if !can_refresh(state) {
        return;
    }
    refresh(state, effects);
}

/// Whether [`refresh_if_idle`] would issue a listing.
pub(crate) fn can_refresh(state: &AppState) -> bool {
    // A results list (find/grep, `view.label`) is not the directory listing: re-listing the
    // search root would silently replace it.
    state.view.label.is_none()
        && state
            .nav
            .as_ref()
            .is_none_or(|p| matches!(p.kind, NavKind::Refresh))
}

// ----- search integration -----

/// Shows the directory of `path` with the cursor on `path`'s entry, and marks `marks` (names
/// in that directory) once it has loaded. Used by find file.
pub(crate) fn go_to_file(
    state: &mut AppState,
    effects: &mut Vec<Effect>,
    path: &std::path::Path,
    marks: Vec<OsString>,
) {
    let (Some(dir), Some(name)) = (path.parent(), path.file_name()) else {
        return;
    };
    let dir = dir.to_owned();
    visit(
        state,
        effects,
        Location::Dir(dir.clone()),
        Some(name.to_owned()),
    );
    // The finder has its own hidden toggle: a hidden target must be listed, or the cursor
    // could not land on it. The listing knows the attributes, so the decision is made when
    // it completes ([`apply_pending_unhide`]); `update` itself stays free of file I/O.
    state.search.pending_unhide = Some(name.to_owned());
    if !marks.is_empty() {
        state.search.pending_marks = Some((dir, marks));
    }
}

/// End of the listing started by [`go_to_file`]: when the wanted entry exists but is hidden
/// from the view, shows hidden entries so the cursor can land on it.
fn apply_pending_unhide(state: &mut AppState, effects: &mut Vec<Effect>) {
    let Some(name) = state.search.pending_unhide.take() else {
        return;
    };
    if state.show_drives {
        return;
    }
    let height = state.viewport_height();
    if state.view.unhide_for(&name, height) {
        set_status(
            state,
            Level::Info,
            "Showing hidden files to reveal the match".into(),
            effects,
        );
        state.dirty = true;
    }
}

/// Shows `entries` (names relative to `root`) as a virtual listing in a NEW TAB (the single
/// place that builds a results list). The header shows `label` instead of the path; `esc`
/// turns the tab into the real listing of `root`. When no more tabs can be opened the list
/// replaces the current pane instead.
pub(crate) fn show_results_list(
    state: &mut AppState,
    effects: &mut Vec<Effect>,
    root: PathBuf,
    label: String,
    entries: Vec<Entry>,
) {
    let height = state.viewport_height();
    let mut view = DirView::new(state.view.sort.clone(), true);
    view.label = Some(label);
    view.push_chunk(entries, height);
    view.finish_loading();
    if crate::tabs::has_room(state) {
        crate::tabs::open_in_new_tab(state, effects, root, view);
        stat_results(state, effects);
        return;
    }
    // Tab limit reached: take over the current pane.
    abandon_history_nav(state);
    state.nav = None;
    let gen = state.list_gen.bump();
    leave_current(state, &NavKind::Visit, &Location::Dir(root.clone()));
    state.cwd = root;
    state.show_drives = false;
    view.gen = gen;
    state.view = view;
    state.git = None;
    state.children_gen.bump();
    state.git_gen.bump();
    state.git_timer_gen.bump();
    state.watch_gen.bump();
    effects.push(Effect::CancelTimer(TimerKind::GitRefresh));
    effects.push(Effect::StopWatching);
    stat_results(state, effects);
    state.dirty = true;
}

/// Asks for size and modification time of the entries of the displayed results list (they
/// are read lazily, off the UI thread, so the columns fill in after the list appears).
pub(crate) fn stat_results(state: &mut AppState, effects: &mut Vec<Effect>) {
    let names: Vec<OsString> = state
        .view
        .entries()
        .iter()
        .map(|e| e.name.clone())
        .collect();
    if names.is_empty() {
        return;
    }
    effects.push(Effect::StatEntries {
        gen: state.children_gen.bump(),
        dir: state.cwd.clone(),
        names,
    });
}

/// Leaves a virtual results list: lists the real directory again (a fresh view, so the list
/// and its label are dropped).
pub(crate) fn leave_results_list(state: &mut AppState, effects: &mut Vec<Effect>) {
    let cwd = state.cwd.clone();
    visit(state, effects, Location::Dir(cwd), None);
}

// ----- results -----

fn here(state: &AppState) -> HistoryEntry {
    if state.show_drives {
        HistoryEntry {
            location: Location::Drives,
            cursor: None,
        }
    } else {
        HistoryEntry {
            location: Location::Dir(state.cwd.clone()),
            cursor: state.view.current().map(|e| e.name.clone()),
        }
    }
}

/// Bookkeeping common to every committed navigation: remember where the cursor was and
/// update the history.
fn leave_current(state: &mut AppState, kind: &NavKind, destination: &Location) {
    let origin = here(state);
    if let (Location::Dir(path), Some(name)) = (&origin.location, &origin.cursor) {
        if state.cursor_memory.len() >= CURSOR_MEMORY_LIMIT {
            state.cursor_memory.clear();
        }
        state.cursor_memory.insert(path.clone(), name.clone());
    }
    if origin.location != *destination {
        match kind {
            NavKind::Visit => {
                state.history.push_back(origin);
                state.history.forward.clear();
            }
            NavKind::Back(_) => state.history.forward.push(origin),
            NavKind::Forward(_) => state.history.push_back(origin),
            NavKind::Refresh | NavKind::Climb => {}
        }
    }
    if matches!(state.mode, Mode::Visual | Mode::Filter) {
        state.mode = Mode::Normal;
    }
}

/// Makes `pending` (a directory) the displayed location. Its entries follow as chunks.
fn commit_dir(state: &mut AppState, effects: &mut Vec<Effect>, pending: PendingNav) {
    let Location::Dir(path) = pending.target else {
        return;
    };
    leave_current(state, &pending.kind, &Location::Dir(path.clone()));
    crate::aids::record_visit(state, effects, &path);
    state.cwd = path.clone();
    state.show_drives = false;
    let mut view = DirView::new(state.view.sort.clone(), state.view.show_hidden);
    view.gen = pending.gen;
    view.loading = true;
    view.set_reveal(pending.reveal);
    state.view = view;
    state.git = None;
    state.children_gen.bump();
    state.git_timer_gen.bump();
    effects.push(Effect::CancelTimer(TimerKind::GitRefresh));
    effects.push(Effect::WatchDir {
        gen: state.watch_gen.bump(),
        path: path.clone(),
    });
    effects.push(Effect::GitStatus {
        gen: state.git_gen.bump(),
        dir: path,
    });
    state.dirty = true;
}

pub(crate) fn on_chunk(
    state: &mut AppState,
    effects: &mut Vec<Effect>,
    gen: Gen,
    entries: Vec<Entry>,
) {
    if let Some(pending) = state.nav.as_mut().filter(|p| p.gen == gen) {
        if matches!(pending.kind, NavKind::Refresh) {
            pending.buffer.extend(entries);
            return;
        }
        if let Some(pending) = state.nav.take() {
            commit_dir(state, effects, pending);
        }
    }
    if state.view.gen == gen && !state.show_drives {
        let height = state.viewport_height();
        state.view.push_chunk(entries, height);
        state.dirty = true;
    }
}

pub(crate) fn on_finished(
    state: &mut AppState,
    effects: &mut Vec<Effect>,
    gen: Gen,
    error: Option<FsError>,
) {
    if state.nav.as_ref().is_some_and(|p| p.gen == gen) {
        let Some(pending) = state.nav.take() else {
            return;
        };
        match (&pending.kind, error) {
            (NavKind::Refresh, None) => {
                let height = state.viewport_height();
                let changes =
                    crate::search::diff_listing(&state.cwd, state.view.entries(), &pending.buffer);
                if !changes.is_empty() {
                    effects.push(Effect::WalkCacheChanges(changes));
                }
                let was_loading = state.view.loading;
                let reveal = if was_loading {
                    state.view.take_reveal()
                } else {
                    None
                };
                let stale = state.view.replace_entries(pending.buffer, height);
                state.view.gen = gen;
                if was_loading {
                    apply_pending_unhide(state, effects);
                    // The refresh superseded the streamed listing, so it finishes the view:
                    // the visit's reveal (remembered cursor) is honoured on the full listing.
                    if let Some(name) = reveal {
                        state.view.reveal_now(&name, height);
                    }
                    state.view.finish_loading();
                    crate::search::apply_pending_marks(state);
                }
                state.dirty = true;
                recount_children(state, effects, stale);
            }
            (NavKind::Refresh, Some(error)) => refresh_failed(state, effects, &error),
            (_, None) => {
                // An empty directory: no chunk ever arrived to trigger the commit.
                commit_dir(state, effects, pending);
                finish_view(state, effects);
            }
            (_, Some(error)) => navigation_failed(state, effects, pending, &error),
        }
        return;
    }
    if state.view.gen == gen && !state.show_drives {
        if let Some(error) = error {
            let text = format!("Listing stopped: {error}");
            set_status(state, Level::Error, text, effects);
        }
        finish_view(state, effects);
    }
}

/// The listing of the displayed directory is complete.
fn finish_view(state: &mut AppState, effects: &mut Vec<Effect>) {
    apply_pending_unhide(state, effects);
    state.view.finish_loading();
    crate::search::apply_pending_marks(state);
    state.dirty = true;
    count_children(state, effects);
}

fn count_children(state: &mut AppState, effects: &mut Vec<Effect>) {
    let names = state.view.dirs_to_count(CHILD_COUNT_LIMIT);
    if names.is_empty() {
        return;
    }
    effects.push(Effect::CountChildren {
        gen: state.children_gen.bump(),
        dir: state.cwd.clone(),
        names,
    });
}

/// Counts the directories whose carried-over count may be stale after a refresh.
fn recount_children(state: &mut AppState, effects: &mut Vec<Effect>, mut names: Vec<OsString>) {
    names.truncate(CHILD_COUNT_LIMIT);
    if names.is_empty() {
        return;
    }
    effects.push(Effect::CountChildren {
        gen: state.children_gen.bump(),
        dir: state.cwd.clone(),
        names,
    });
}

fn vanished(error: &FsError) -> bool {
    matches!(
        error.kind,
        FsErrorKind::NotFound | FsErrorKind::NotADirectory
    )
}

fn navigation_failed(
    state: &mut AppState,
    effects: &mut Vec<Effect>,
    pending: PendingNav,
    error: &FsError,
) {
    let PendingNav { target, kind, .. } = pending;
    // The visit that wanted a hidden match revealed is over.
    state.search.pending_unhide = None;
    let climbing = matches!(kind, NavKind::Climb);
    match kind {
        NavKind::Back(entry) => state.history.back.push(entry),
        NavKind::Forward(entry) => state.history.forward.push(entry),
        _ => {}
    }
    match &target {
        // A directory in the way vanished while we climbed out of another one: keep climbing.
        Location::Dir(path) if climbing && vanished(error) => climb(state, effects, path.clone()),
        _ => {
            if let (Location::Dir(path), true) = (&target, vanished(error)) {
                crate::aids::forget_dir(state, effects, path);
            }
            report_open_failure(state, effects, &target, error);
        }
    }
}

fn report_open_failure(
    state: &mut AppState,
    effects: &mut Vec<Effect>,
    target: &Location,
    error: &FsError,
) {
    let what = match target {
        Location::Dir(path) => path.display().to_string(),
        Location::Drives => "This PC".to_owned(),
    };
    set_status(
        state,
        Level::Error,
        format!("Cannot open {what}: {error}"),
        effects,
    );
    // The abandoned listing may have been cutting a streaming view short, and change events
    // that arrived while this navigation was pending were swallowed: list afresh.
    if !state.show_drives {
        refresh(state, effects);
    }
}

fn refresh_failed(state: &mut AppState, effects: &mut Vec<Effect>, error: &FsError) {
    if vanished(error) && !state.show_drives {
        let gone = state.cwd.clone();
        climb(state, effects, gone);
        return;
    }
    state.view.finish_loading();
    state.search.pending_marks = None;
    let text = format!("Cannot refresh {}: {error}", state.cwd.display());
    set_status(state, Level::Error, text, effects);
}

/// Moves to the nearest parent of `gone`, which no longer exists.
fn climb(state: &mut AppState, effects: &mut Vec<Effect>, gone: PathBuf) {
    let name = gone.file_name().map_or_else(
        || gone.display().to_string(),
        |n| n.to_string_lossy().into_owned(),
    );
    let text = format!("{name} no longer exists");
    let reveal = gone.file_name().map(ToOwned::to_owned);
    match gone.parent() {
        Some(parent) if !parent.as_os_str().is_empty() => request(
            state,
            effects,
            Location::Dir(parent.to_owned()),
            NavKind::Climb,
            reveal,
        ),
        _ => request(state, effects, Location::Drives, NavKind::Climb, None),
    }
    set_status(state, Level::Warning, text, effects);
}

pub(crate) fn on_drives(
    state: &mut AppState,
    effects: &mut Vec<Effect>,
    gen: Gen,
    drives: Vec<DriveInfo>,
) {
    let Some(pending) = state.nav.take_if(|p| p.gen == gen) else {
        return;
    };
    // A refresh keeps the cursor on the drive it was on; arriving puts it on the drive of
    // the directory we came from.
    let near = match state.drives.current() {
        Some(drive) if state.show_drives && matches!(pending.kind, NavKind::Refresh) => {
            drive.root.clone()
        }
        _ => state.cwd.clone(),
    };
    leave_current(state, &pending.kind, &Location::Drives);
    state.show_drives = true;
    let height = state.viewport_height();
    state.drives.set(drives, &near, height);
    state.git = None;
    state.git_gen.bump();
    state.git_timer_gen.bump();
    state.children_gen.bump();
    effects.push(Effect::CancelTimer(TimerKind::GitRefresh));
    effects.push(Effect::WatchDrives {
        gen: state.watch_gen.bump(),
    });
    state.dirty = true;
}

/// A drive was plugged in or removed while "This PC" is shown: list the drives again.
pub(crate) fn on_drives_changed(state: &mut AppState, effects: &mut Vec<Effect>, gen: Gen) {
    if !state.watch_gen.is_current(gen) || !state.show_drives || state.view.label.is_some() {
        return;
    }
    // A navigation in flight lists afresh anyway, or leaves "This PC".
    if state.nav.is_some() {
        return;
    }
    refresh(state, effects);
}

/// The watcher reported a change in the displayed directory.
pub(crate) fn on_dir_changed(state: &mut AppState, effects: &mut Vec<Effect>, gen: Gen) {
    if !state.watch_gen.is_current(gen) || state.show_drives || state.view.label.is_some() {
        return;
    }
    // A real navigation in flight will list the directory afresh anyway.
    if state
        .nav
        .as_ref()
        .is_some_and(|p| !matches!(p.kind, NavKind::Refresh))
    {
        return;
    }
    refresh(state, effects);
    if state.git.is_some() {
        effects.push(Effect::StartTimer {
            kind: TimerKind::GitRefresh,
            gen: state.git_timer_gen.bump(),
            delay: GIT_DEBOUNCE,
        });
    }
}

/// The git debounce timer fired.
pub(crate) fn on_git_timer(state: &mut AppState, effects: &mut Vec<Effect>, gen: Gen) {
    if state.git_timer_gen.is_current(gen) && !state.show_drives {
        effects.push(Effect::GitStatus {
            gen: state.git_gen.bump(),
            dir: state.cwd.clone(),
        });
    }
}

pub(crate) fn on_git_status(state: &mut AppState, gen: Gen, snapshot: Option<Box<GitSnapshot>>) {
    if state.git_gen.is_current(gen) {
        state.git = snapshot.map(|boxed| *boxed);
        state.dirty = true;
    }
}

pub(crate) fn on_entry_stats(
    state: &mut AppState,
    gen: Gen,
    stats: Vec<(OsString, u64, Option<std::time::SystemTime>)>,
) {
    if state.children_gen.is_current(gen) && !state.show_drives && state.view.label.is_some() {
        let height = state.viewport_height();
        state.view.apply_stats(stats, height);
        state.dirty = true;
    }
}

pub(crate) fn on_child_counts(state: &mut AppState, gen: Gen, counts: Vec<(OsString, u32)>) {
    if state.children_gen.is_current(gen) && !state.show_drives {
        state.view.apply_child_counts(counts);
        state.dirty = true;
    }
}

// ----- cursor, marks, view options -----

pub(crate) fn move_cursor(state: &mut AppState, delta: isize) {
    let height = state.viewport_height();
    if state.show_drives {
        let len = state.drives.drives.len();
        state.drives.cursor.move_by(delta, len);
        state.drives.cursor.ensure_visible(len, height);
    } else {
        state.view.move_cursor(delta, height);
    }
}

pub(crate) fn cursor_edge(state: &mut AppState, bottom: bool) {
    let height = state.viewport_height();
    if state.show_drives {
        let len = state.drives.drives.len();
        state
            .drives
            .cursor
            .set(if bottom { usize::MAX } else { 0 }, len);
        state.drives.cursor.ensure_visible(len, height);
    } else if bottom {
        state.view.cursor_bottom(height);
    } else {
        state.view.cursor_top(height);
    }
}

pub(crate) fn half_page(state: &mut AppState, down: bool) {
    let height = state.viewport_height();
    if state.show_drives {
        let step = isize::try_from((height / 2).max(1)).unwrap_or(1);
        move_cursor(state, if down { step } else { -step });
    } else {
        state.view.half_page(down, height);
    }
}

/// Re-clamps scrolling after a resize.
pub(crate) fn fit_viewport(state: &mut AppState) {
    let height = state.viewport_height();
    state.view.fit_viewport(height);
    let len = state.drives.drives.len();
    state.drives.cursor.ensure_visible(len, height);
}

pub(crate) fn toggle_mark(state: &mut AppState) {
    if state.show_drives {
        return;
    }
    let height = state.viewport_height();
    state.view.toggle_mark(height);
    leave_visual_mode(state);
}

pub(crate) fn mark_all(state: &mut AppState) {
    if state.show_drives {
        return;
    }
    state.view.toggle_mark_all();
    leave_visual_mode(state);
}

pub(crate) fn toggle_visual(state: &mut AppState) {
    if state.show_drives {
        return;
    }
    if state.mode == Mode::Visual {
        leave_visual_mode(state);
    } else if state.view.current().is_some() {
        state.view.enter_visual();
        state.mode = Mode::Visual;
    }
}

/// Back to normal mode, keeping the marks the range produced.
pub(crate) fn leave_visual_mode(state: &mut AppState) {
    state.view.exit_visual();
    if state.mode == Mode::Visual {
        state.mode = Mode::Normal;
    }
}

pub(crate) fn toggle_hidden(state: &mut AppState, effects: &mut Vec<Effect>) {
    let height = state.viewport_height();
    state.view.toggle_hidden(height);
    let text = if state.view.show_hidden {
        "Showing hidden files"
    } else {
        "Hiding hidden files"
    };
    set_status(state, Level::Info, text.into(), effects);
}

/// Sorts by `key`; choosing the active key again reverses the order.
pub(crate) fn sort_by(state: &mut AppState, effects: &mut Vec<Effect>, key: SortKey) {
    let mut sort = state.view.sort.clone();
    if sort.key == key {
        sort.reverse = !sort.reverse;
    } else {
        sort.key = key;
        sort.reverse = false;
    }
    let label = match key {
        SortKey::Name => "name",
        SortKey::Size => "size",
        SortKey::Mtime => "modified time",
        SortKey::Ext => "extension",
    };
    let direction = if sort.reverse { " (reversed)" } else { "" };
    let text = format!("Sorted by {label}{direction}");
    let height = state.viewport_height();
    state.view.set_sort(sort, height);
    set_status(state, Level::Info, text, effects);
}

#[cfg(test)]
mod tests {
    use std::collections::HashMap;
    use std::path::Path;
    use std::time::SystemTime;

    use super::*;
    use crate::config::Config;
    use crate::entry::EntryKind;
    use crate::key::KeyChord;
    use crate::keymap::Keymap;
    use crate::msg::Msg;
    use crate::state::{Places, Size, StartParams};
    use crate::update::update;

    /// A fake file system plus a minimal effect runner, so navigation can be tested end to
    /// end without threads or a disk.
    struct World {
        state: AppState,
        dirs: HashMap<PathBuf, Vec<Entry>>,
        denied: Vec<PathBuf>,
        drives: Vec<DriveInfo>,
        /// Every effect `update` ever returned, in order.
        log: Vec<Effect>,
        /// How many entries go into one chunk.
        chunk: usize,
    }

    fn p(s: &str) -> PathBuf {
        PathBuf::from(s)
    }

    impl World {
        fn new(cwd: &str) -> Self {
            let state = AppState::new(StartParams {
                cwd: p(cwd),
                home: Some(p("/home/me")),
                places: Places {
                    home: Some(p("/home/me")),
                    downloads: Some(p("/home/me/Downloads")),
                    desktop: None,
                    config: Some(p("/home/me/.config/kestrel")),
                },
                config: Config::default(),
                keymap: Keymap::defaults(),
                size: Size::new(80, 24),
                now: SystemTime::UNIX_EPOCH,
                utc_offset_secs: 0,
            });
            let mut world = Self {
                state,
                dirs: HashMap::new(),
                denied: Vec::new(),
                drives: vec![DriveInfo::new("/")],
                log: Vec::new(),
                chunk: usize::MAX,
            };
            world.dir("/", vec![Entry::dir("home"), Entry::dir("etc")]);
            world
        }

        fn dir(&mut self, path: &str, entries: Vec<Entry>) -> &mut Self {
            self.dirs.insert(p(path), entries);
            self
        }

        fn send(&mut self, msg: Msg) {
            let mut queue = vec![msg];
            while let Some(msg) = queue.pop() {
                let effects = update(&mut self.state, msg);
                for effect in &effects {
                    queue.extend(self.run(effect));
                }
                self.log.extend(effects);
            }
        }

        /// Plays the part of the effect runner for the effects that matter here.
        fn run(&self, effect: &Effect) -> Vec<Msg> {
            match effect {
                Effect::ListDir { gen, path } => {
                    let gen = *gen;
                    if self.denied.contains(path) {
                        return vec![Msg::ListingFinished {
                            gen,
                            error: Some(FsError::new(
                                FsErrorKind::PermissionDenied,
                                "Access is denied.",
                            )),
                        }];
                    }
                    let Some(entries) = self.dirs.get(path) else {
                        return vec![Msg::ListingFinished {
                            gen,
                            error: Some(FsError::new(FsErrorKind::NotFound, "not found")),
                        }];
                    };
                    let mut msgs: Vec<Msg> = entries
                        .chunks(self.chunk.max(1))
                        .map(|c| Msg::ListingChunk {
                            gen,
                            entries: c.to_vec(),
                        })
                        .collect();
                    msgs.push(Msg::ListingFinished { gen, error: None });
                    // `send` pops from the back, so reverse to deliver in order.
                    msgs.reverse();
                    msgs
                }
                Effect::ListDrives { gen } => vec![Msg::DrivesLoaded {
                    gen: *gen,
                    drives: self.drives.clone(),
                }],
                _ => Vec::new(),
            }
        }

        fn key(&mut self, text: &str) {
            for part in text.split(' ') {
                self.send(Msg::Key(part.parse::<KeyChord>().unwrap()));
            }
        }

        fn start(&mut self) {
            self.send(Msg::Start);
        }

        fn names(&self) -> Vec<String> {
            (0..self.state.view.len())
                .map(|i| self.state.view.row(i).unwrap().display_name().into_owned())
                .collect()
        }

        fn cursor(&self) -> String {
            self.state
                .view
                .current()
                .map(|e| e.display_name().into_owned())
                .unwrap_or_default()
        }

        fn status(&self) -> String {
            self.state
                .status
                .as_ref()
                .map(|s| s.text.clone())
                .unwrap_or_default()
        }

        fn cwd(&self) -> &Path {
            &self.state.cwd
        }

        fn effects_of<'a>(&'a self, pred: impl Fn(&Effect) -> bool + 'a) -> usize {
            self.log.iter().filter(|e| pred(e)).count()
        }
    }

    fn project_world() -> World {
        let mut w = World::new("/home/me");
        w.dir(
            "/home/me",
            vec![
                Entry::dir("projects"),
                Entry::dir("Downloads"),
                Entry::file("notes.txt", 10),
            ],
        );
        w.dir(
            "/home/me/projects",
            vec![
                Entry::dir("alpha"),
                Entry::dir("beta"),
                Entry::file("todo.md", 4),
            ],
        );
        w.dir("/home/me/projects/alpha", vec![Entry::file("main.rs", 100)]);
        w.dir("/home/me/projects/beta", vec![]);
        w.dir("/home/me/Downloads", vec![Entry::file("setup.exe", 5000)]);
        w.dir(
            "/home/me/.config/kestrel",
            vec![Entry::file("config.toml", 1)],
        );
        w.start();
        w
    }

    #[test]
    fn start_lists_the_start_directory_and_starts_the_watcher_and_git() {
        let w = project_world();
        assert_eq!(w.names(), ["Downloads", "projects", "notes.txt"]);
        assert!(!w.state.view.loading);
        assert_eq!(w.effects_of(|e| matches!(e, Effect::WatchDir { .. })), 1);
        assert_eq!(w.effects_of(|e| matches!(e, Effect::GitStatus { .. })), 1);
        // Directories get their child counts afterwards.
        assert_eq!(
            w.effects_of(|e| matches!(e, Effect::CountChildren { .. })),
            1
        );
    }

    #[test]
    fn enter_and_parent_move_between_directories() {
        let mut w = project_world();
        w.key("j l");
        assert_eq!(w.cwd(), Path::new("/home/me/projects"));
        assert_eq!(w.names(), ["alpha", "beta", "todo.md"]);
        w.key("j l");
        assert_eq!(w.cwd(), Path::new("/home/me/projects/beta"));
        assert!(w.names().is_empty());
        w.key("h");
        assert_eq!(w.cwd(), Path::new("/home/me/projects"));
        assert_eq!(
            w.cursor(),
            "beta",
            "the cursor returns to where we came from"
        );
        w.key("h");
        assert_eq!(w.cwd(), Path::new("/home/me"));
        assert_eq!(w.cursor(), "projects");
    }

    #[test]
    fn arrows_enter_and_backspace_also_navigate() {
        let mut w = project_world();
        w.key("j enter");
        assert_eq!(w.cwd(), Path::new("/home/me/projects"));
        w.key("backspace");
        assert_eq!(w.cwd(), Path::new("/home/me"));
        w.key("right");
        assert_eq!(w.cwd(), Path::new("/home/me/projects"));
        w.key("left");
        assert_eq!(w.cwd(), Path::new("/home/me"));
    }

    #[test]
    fn entering_a_file_opens_it_instead_of_navigating() {
        let mut w = project_world();
        w.key("G l");
        assert_eq!(w.cwd(), Path::new("/home/me"));
        assert_eq!(
            w.effects_of(
                |e| matches!(e, Effect::OpenDefault { path } if path == Path::new("/home/me/notes.txt"))
            ),
            1
        );
    }

    #[test]
    fn per_directory_cursor_memory() {
        let mut w = project_world();
        // Into projects, down to todo.md, out, and in again: the cursor is where it was.
        w.key("j l j j");
        assert_eq!(w.cursor(), "todo.md");
        w.key("h");
        assert_eq!(w.cursor(), "projects");
        w.key("l");
        assert_eq!(w.cursor(), "todo.md");
    }

    #[test]
    fn history_back_and_forward_restore_location_and_cursor() {
        let mut w = project_world();
        w.key("j l j l");
        assert_eq!(w.cwd(), Path::new("/home/me/projects/beta"));
        w.key("ctrl-o");
        assert_eq!(w.cwd(), Path::new("/home/me/projects"));
        assert_eq!(w.cursor(), "beta");
        w.key("ctrl-o");
        assert_eq!(w.cwd(), Path::new("/home/me"));
        assert_eq!(w.cursor(), "projects");
        w.key("ctrl-o");
        assert!(w.status().contains("oldest"), "{}", w.status());
        w.key("tab");
        assert_eq!(w.cwd(), Path::new("/home/me/projects"));
        w.key("tab");
        assert_eq!(w.cwd(), Path::new("/home/me/projects/beta"));
        w.key("tab");
        assert!(w.status().contains("newest"), "{}", w.status());
    }

    #[test]
    fn a_new_visit_clears_the_forward_history() {
        let mut w = project_world();
        w.key("j l");
        w.key("ctrl-o");
        w.key("g d");
        assert_eq!(w.cwd(), Path::new("/home/me/Downloads"));
        w.key("tab");
        assert!(w.status().contains("newest"), "{}", w.status());
        assert_eq!(w.cwd(), Path::new("/home/me/Downloads"));
    }

    #[test]
    fn jump_keys_go_to_known_places_and_report_missing_ones() {
        let mut w = project_world();
        w.key("g d");
        assert_eq!(w.cwd(), Path::new("/home/me/Downloads"));
        w.key("~");
        assert_eq!(w.cwd(), Path::new("/home/me"));
        w.key("g c");
        assert_eq!(w.cwd(), Path::new("/home/me/.config/kestrel"));
        w.key("g h");
        assert_eq!(w.cwd(), Path::new("/home/me"));
        w.key("g D");
        assert_eq!(w.cwd(), Path::new("/home/me"));
        assert!(w.status().contains("Desktop"), "{}", w.status());
    }

    #[test]
    fn unreadable_directories_leave_the_view_alone() {
        let mut w = project_world();
        w.denied.push(p("/home/me/projects"));
        w.key("j l");
        assert_eq!(w.cwd(), Path::new("/home/me"));
        assert_eq!(w.names(), ["Downloads", "projects", "notes.txt"]);
        assert!(w.status().contains("Access is denied"), "{}", w.status());
        assert!(w.state.nav.is_none());
        // The failed visit left no history behind.
        w.key("ctrl-o");
        assert!(w.status().contains("oldest"), "{}", w.status());
    }

    #[test]
    fn failed_history_navigation_keeps_the_entry() {
        let mut w = project_world();
        w.key("j l");
        w.denied.push(p("/home/me"));
        w.key("ctrl-o");
        assert_eq!(w.cwd(), Path::new("/home/me/projects"));
        w.denied.clear();
        w.key("ctrl-o");
        assert_eq!(w.cwd(), Path::new("/home/me"));
    }

    #[test]
    fn stale_listings_are_dropped() {
        let mut w = project_world();
        let stale = w.state.view.gen;
        w.key("j l");
        let before = w.names();
        w.send(Msg::ListingChunk {
            gen: stale,
            entries: vec![Entry::file("ghost", 1)],
        });
        w.send(Msg::ListingFinished {
            gen: stale,
            error: Some(FsError::new(FsErrorKind::Other, "x")),
        });
        assert_eq!(w.names(), before);
        assert!(w.status().is_empty());
    }

    #[test]
    fn a_superseded_navigation_never_commits() {
        let mut w = project_world();
        // Two navigations back to back; deliver the results of the first one late.
        let first = update(&mut w.state, Msg::Key("j".parse().unwrap()));
        assert!(!first.iter().any(|e| matches!(e, Effect::ListDir { .. })));
        let e1 = update(&mut w.state, Msg::Key("l".parse().unwrap()));
        let Effect::ListDir { gen: g1, .. } = e1[0] else {
            panic!("expected a listing, got {e1:?}")
        };
        w.key("g d"); // supersedes it
        assert_eq!(w.cwd(), Path::new("/home/me/Downloads"));
        w.send(Msg::ListingChunk {
            gen: g1,
            entries: vec![Entry::file("late", 1)],
        });
        w.send(Msg::ListingFinished {
            gen: g1,
            error: None,
        });
        assert_eq!(w.cwd(), Path::new("/home/me/Downloads"));
        assert_eq!(w.names(), ["setup.exe"]);
    }

    #[test]
    fn streamed_listing_commits_on_the_first_chunk_and_finishes_later() {
        let mut w = project_world();
        let many: Vec<Entry> = (0..10).map(|i| Entry::file(format!("f{i}"), 1)).collect();
        w.dir("/home/me/big", many);
        w.chunk = 4;
        w.dirs
            .get_mut(Path::new("/home/me"))
            .unwrap()
            .push(Entry::dir("big"));
        w.send(Msg::DirChanged {
            gen: w.state.watch_gen.current(),
        });
        w.key("g g");
        assert_eq!(w.cursor(), "big");
        w.key("l");
        assert_eq!(w.cwd(), Path::new("/home/me/big"));
        assert_eq!(w.state.view.len(), 10);
        assert!(!w.state.view.loading);
    }

    #[test]
    fn partial_delivery_shows_progress_and_keeps_loading() {
        let mut w = project_world();
        w.key("j l");
        // Start another visit and deliver only the first chunk by hand.
        let effects = update(&mut w.state, Msg::Key("~".parse().unwrap()));
        let Effect::ListDir { gen, .. } = effects[0] else {
            panic!("{effects:?}")
        };
        assert_eq!(w.cwd(), Path::new("/home/me/projects"), "not committed yet");
        w.send(Msg::ListingChunk {
            gen,
            entries: vec![Entry::file("a", 1), Entry::file("b", 1)],
        });
        assert_eq!(w.cwd(), Path::new("/home/me"));
        assert!(w.state.view.loading);
        assert_eq!(w.names(), ["a", "b"]);
        w.send(Msg::ListingFinished { gen, error: None });
        assert!(!w.state.view.loading);
    }

    #[test]
    fn a_refresh_during_the_streamed_listing_keeps_pending_marks_and_finishes_the_view() {
        let mut w = project_world();
        w.key("j l");
        let effects = update(&mut w.state, Msg::Key("~".parse().unwrap()));
        let Effect::ListDir { gen, .. } = effects[0] else {
            panic!("{effects:?}")
        };
        w.send(Msg::ListingChunk {
            gen,
            entries: vec![Entry::file("a", 1)],
        });
        assert!(w.state.view.loading);
        w.state.search.pending_marks = Some((w.cwd().to_owned(), vec!["b".into()]));
        w.state.view.set_reveal(Some("b".into()));

        let watch = w.state.watch_gen.current();
        let effects = update(&mut w.state, Msg::DirChanged { gen: watch });
        let Some(Effect::ListDir { gen: g2, .. }) = effects
            .iter()
            .find(|e| matches!(e, Effect::ListDir { .. }))
            .cloned()
        else {
            panic!("{effects:?}")
        };
        assert!(
            w.state.search.pending_marks.is_some(),
            "refresh keeps marks"
        );
        w.send(Msg::ListingChunk {
            gen: g2,
            entries: vec![Entry::file("a", 1), Entry::file("b", 1)],
        });
        w.send(Msg::ListingFinished {
            gen: g2,
            error: None,
        });
        assert!(!w.state.view.loading);
        assert!(w.state.search.pending_marks.is_none());
        assert_eq!(
            w.cursor(),
            "b",
            "the visit's reveal is honoured by the refresh"
        );
        assert_eq!(w.state.view.marks_count(), 1, "b is marked");
        let marked: Vec<_> = w.state.view.marked_names().cloned().collect();
        assert_eq!(marked, [OsString::from("b")]);
    }

    #[test]
    fn find_file_on_an_attribute_hidden_file_unhides_from_the_listing_without_a_stat() {
        let mut w = project_world();
        let mut secret = Entry::file("secret.txt", 1);
        secret.attrs = crate::entry::Attrs::HIDDEN;
        w.dir(
            "/home/me/projects",
            vec![Entry::file("a.txt", 1), secret, Entry::file("z.txt", 1)],
        );
        assert!(!w.state.view.show_hidden);
        let mut effects = Vec::new();
        go_to_file(
            &mut w.state,
            &mut effects,
            Path::new("/home/me/projects/secret.txt"),
            Vec::new(),
        );
        // No hidden decision before the listing arrives: update() does no file I/O.
        assert!(!w.state.view.show_hidden);
        for e in &effects {
            for m in w.run(e).into_iter().rev() {
                w.send(m);
            }
        }
        assert!(w.state.view.show_hidden);
        assert_eq!(w.cursor(), "secret.txt");
        assert!(w.state.search.pending_unhide.is_none());
    }

    #[test]
    fn a_failed_visit_clears_the_pending_unhide() {
        let mut w = project_world();
        w.dir("/home/me/projects", vec![Entry::file("a.txt", 1)]);
        let mut effects = Vec::new();
        go_to_file(
            &mut w.state,
            &mut effects,
            Path::new("/home/me/projects/secret.txt"),
            Vec::new(),
        );
        assert!(w.state.search.pending_unhide.is_some());
        let Some(Effect::ListDir { gen, .. }) = effects
            .iter()
            .find(|e| matches!(e, Effect::ListDir { .. }))
            .cloned()
        else {
            panic!("{effects:?}")
        };
        w.send(Msg::ListingFinished {
            gen,
            error: Some(FsError::new(FsErrorKind::Other, "boom")),
        });
        assert!(w.state.search.pending_unhide.is_none());
        assert!(!w.state.view.show_hidden);
    }

    #[test]
    fn leaving_the_tab_clears_the_pending_unhide() {
        let mut w = project_world();
        w.state.search.pending_unhide = Some(OsString::from(".env"));
        let mut effects = Vec::new();
        crate::tabs::new_tab(&mut w.state, &mut effects);
        assert!(w.state.search.pending_unhide.is_none());
    }

    #[test]
    fn a_refresh_recounts_only_directories_whose_mtime_changed() {
        let mut w = project_world();
        let t = |secs| std::time::UNIX_EPOCH + Duration::from_secs(secs);
        let mut same = Entry::dir("same").with_mtime(t(10));
        same.children = Some(2);
        let mut changed = Entry::dir("changed").with_mtime(t(10));
        changed.children = Some(5);
        let list = w.dirs.get_mut(Path::new("/home/me")).unwrap();
        list.push(same);
        list.push(changed);
        w.send(Msg::DirChanged {
            gen: w.state.watch_gen.current(),
        });
        let fresh = |w: &mut World| -> Vec<OsString> {
            let watch = w.state.watch_gen.current();
            let effects = update(&mut w.state, Msg::DirChanged { gen: watch });
            let Some(Effect::ListDir { gen, .. }) = effects
                .iter()
                .find(|e| matches!(e, Effect::ListDir { .. }))
                .cloned()
            else {
                panic!("{effects:?}")
            };
            let kept: Vec<Entry> = w.dirs[Path::new("/home/me")].clone();
            w.send(Msg::ListingChunk { gen, entries: kept });
            update(&mut w.state, Msg::ListingFinished { gen, error: None })
                .iter()
                .filter_map(|e| match e {
                    Effect::CountChildren { names, .. } => Some(names.clone()),
                    _ => None,
                })
                .flatten()
                .collect()
        };
        // Give the view its counts (the world's listing carries them already).
        let _ = fresh(&mut w);
        for e in w.dirs.get_mut(Path::new("/home/me")).unwrap() {
            if e.name == "changed" {
                e.mtime = Some(t(99));
            }
        }
        let counted = fresh(&mut w);
        assert!(counted.contains(&OsString::from("changed")), "{counted:?}");
        assert!(
            !counted.contains(&OsString::from("same")),
            "unchanged dirs keep their count: {counted:?}"
        );
    }

    #[test]
    fn a_failed_refresh_drops_stale_pending_marks() {
        let mut w = project_world();
        w.state.search.pending_marks = Some((w.cwd().to_owned(), vec!["b".into()]));
        let watch = w.state.watch_gen.current();
        let effects = update(&mut w.state, Msg::DirChanged { gen: watch });
        let Some(Effect::ListDir { gen, .. }) = effects
            .iter()
            .find(|e| matches!(e, Effect::ListDir { .. }))
            .cloned()
        else {
            panic!("{effects:?}")
        };
        let error = FsError::new(FsErrorKind::PermissionDenied, "denied");
        w.send(Msg::ListingFinished {
            gen,
            error: Some(error),
        });
        assert!(w.state.search.pending_marks.is_none());
    }

    #[test]
    fn external_changes_refresh_in_place_and_keep_the_cursor_by_name() {
        let mut w = project_world();
        w.key("j");
        assert_eq!(w.cursor(), "projects");
        w.dirs
            .get_mut(Path::new("/home/me"))
            .unwrap()
            .push(Entry::file("aaa", 1));
        w.send(Msg::DirChanged {
            gen: w.state.watch_gen.current(),
        });
        assert_eq!(w.names(), ["Downloads", "projects", "aaa", "notes.txt"]);
        assert_eq!(w.cursor(), "projects");
        // Removing a row above the cursor must not move the cursor off its entry.
        w.dirs
            .get_mut(Path::new("/home/me"))
            .unwrap()
            .retain(|e| e.display_name() != "Downloads");
        w.send(Msg::DirChanged {
            gen: w.state.watch_gen.current(),
        });
        assert_eq!(w.cursor(), "projects");
        assert_eq!(w.state.view.cursor.index, 0);
    }

    #[test]
    fn watcher_events_of_an_old_directory_are_ignored() {
        let mut w = project_world();
        let old = w.state.watch_gen.current();
        w.key("j l");
        let before = w.log.len();
        w.send(Msg::DirChanged { gen: old });
        assert_eq!(
            w.log.len(),
            before,
            "a stale watch generation must do nothing"
        );
    }

    #[test]
    fn a_vanished_directory_climbs_to_the_nearest_parent() {
        let mut w = project_world();
        w.key("j l j l");
        assert_eq!(w.cwd(), Path::new("/home/me/projects/beta"));
        w.dirs.remove(Path::new("/home/me/projects/beta"));
        w.dirs.remove(Path::new("/home/me/projects"));
        w.send(Msg::DirChanged {
            gen: w.state.watch_gen.current(),
        });
        assert_eq!(w.cwd(), Path::new("/home/me"));
        assert!(
            w.status().contains("projects no longer exists"),
            "{}",
            w.status()
        );
        assert_eq!(w.cursor(), "projects");
    }

    #[test]
    fn marks_survive_refreshes_and_clear_on_navigation() {
        let mut w = project_world();
        w.key("space");
        assert_eq!(w.state.view.marks_count(), 1);
        w.send(Msg::DirChanged {
            gen: w.state.watch_gen.current(),
        });
        assert_eq!(w.state.view.marks_count(), 1);
        // `space` moved the cursor down onto `projects`.
        w.key("l");
        assert_eq!(w.cwd(), Path::new("/home/me/projects"));
        assert_eq!(w.state.view.marks_count(), 0);
    }

    #[test]
    fn visual_mode_marks_a_range_and_escape_leaves_it_applied() {
        let mut w = project_world();
        w.key("v");
        assert_eq!(w.state.mode, Mode::Visual);
        w.key("j j");
        assert_eq!(w.state.view.marks_count(), 3);
        w.key("esc");
        assert_eq!(w.state.mode, Mode::Normal);
        assert_eq!(w.state.view.marks_count(), 3);
        w.key("esc");
        assert_eq!(
            w.state.view.marks_count(),
            0,
            "a second escape clears the marks"
        );
    }

    #[test]
    fn visual_mode_ends_on_navigation_and_with_v() {
        let mut w = project_world();
        w.key("j v l");
        assert_eq!(w.state.mode, Mode::Normal);
        assert_eq!(w.state.view.marks_count(), 0);
        w.key("v");
        assert_eq!(w.state.mode, Mode::Visual);
        w.key("v");
        assert_eq!(w.state.mode, Mode::Normal);
        assert_eq!(w.state.view.marks_count(), 1);
    }

    #[test]
    fn ctrl_a_marks_everything_visible_and_again_clears() {
        let mut w = project_world();
        w.key("ctrl-a");
        assert_eq!(w.state.view.marks_count(), 3);
        w.key("ctrl-a");
        assert_eq!(w.state.view.marks_count(), 0);
    }

    #[test]
    fn hidden_toggle_shows_dotfiles_and_keeps_the_cursor() {
        let mut w = project_world();
        w.dirs
            .get_mut(Path::new("/home/me"))
            .unwrap()
            .push(Entry::file(".bashrc", 1));
        w.send(Msg::DirChanged {
            gen: w.state.watch_gen.current(),
        });
        assert_eq!(w.names().len(), 3);
        w.key("G");
        assert_eq!(w.cursor(), "notes.txt");
        w.key(".");
        assert_eq!(w.names(), ["Downloads", "projects", ".bashrc", "notes.txt"]);
        assert_eq!(w.cursor(), "notes.txt");
        assert!(w.status().contains("Showing"));
        w.key(".");
        assert_eq!(w.names().len(), 3);
    }

    #[test]
    fn sort_keys_select_and_reverse() {
        let mut w = project_world();
        w.key("S s");
        assert_eq!(w.state.view.sort.key, SortKey::Size);
        assert!(w.status().contains("size"));
        w.key("S s");
        assert!(w.state.view.sort.reverse);
        assert!(w.status().contains("reversed"));
        w.key("S n");
        assert_eq!(w.state.view.sort.key, SortKey::Name);
        assert!(!w.state.view.sort.reverse);
        w.key("S m");
        assert_eq!(w.state.view.sort.key, SortKey::Mtime);
        w.key("S e");
        assert_eq!(w.state.view.sort.key, SortKey::Ext);
        assert_eq!(w.state.view.len(), 3);
    }

    #[test]
    fn sort_survives_navigation() {
        let mut w = project_world();
        w.key("S s j l");
        assert_eq!(w.state.view.sort.key, SortKey::Size);
    }

    #[test]
    fn drives_view_from_a_root_and_back_into_a_drive() {
        let mut w = World::new("/");
        w.drives = vec![DriveInfo::new("/"), DriveInfo::new("/mnt/usb")];
        w.start();
        w.key("h");
        assert!(w.state.show_drives);
        assert_eq!(w.state.drives.cursor.index, 0);
        w.key("j");
        assert_eq!(w.state.drives.cursor.index, 1);
        w.key("j");
        assert_eq!(w.state.drives.cursor.index, 1, "clamped");
        w.key("h");
        assert!(w.state.show_drives, "nothing is above This PC");
        // Git is stopped while the drive list is shown, and the drives are watched instead
        // of the directory.
        assert!(w
            .log
            .iter()
            .any(|e| matches!(e, Effect::WatchDrives { .. })));
        w.key("k l");
        assert!(!w.state.show_drives);
        assert_eq!(w.cwd(), Path::new("/"));
        assert_eq!(w.names(), ["etc", "home"]);
    }

    fn last_drive_watch(w: &World) -> Gen {
        w.log
            .iter()
            .rev()
            .find_map(|e| match e {
                Effect::WatchDrives { gen } => Some(*gen),
                _ => None,
            })
            .expect("the drives are watched")
    }

    fn drive_listings(w: &World) -> usize {
        w.log
            .iter()
            .filter(|e| matches!(e, Effect::ListDrives { .. }))
            .count()
    }

    #[test]
    fn a_plugged_in_drive_appears_and_the_cursor_stays_on_its_drive() {
        let mut w = World::new("/");
        w.drives = vec![DriveInfo::new("/"), DriveInfo::new("/mnt/usb")];
        w.start();
        w.key("h j");
        assert_eq!(
            w.state.drives.current().unwrap().root,
            Path::new("/mnt/usb")
        );
        let gen = last_drive_watch(&w);
        w.drives = vec![
            DriveInfo::new("/"),
            DriveInfo::new("/mnt/a"),
            DriveInfo::new("/mnt/usb"),
        ];
        w.send(Msg::DrivesChanged { gen });
        assert!(w.state.show_drives);
        assert_eq!(w.state.drives.drives.len(), 3);
        assert_eq!(
            w.state.drives.current().unwrap().root,
            Path::new("/mnt/usb")
        );
        // Unplugging the drive under the cursor removes it.
        let gen = last_drive_watch(&w);
        w.drives = vec![DriveInfo::new("/"), DriveInfo::new("/mnt/a")];
        w.send(Msg::DrivesChanged { gen });
        assert_eq!(w.state.drives.drives.len(), 2);
        assert!(w.state.drives.current().is_some());
    }

    #[test]
    fn stale_or_misplaced_drive_changes_are_ignored() {
        let mut w = World::new("/");
        w.drives = vec![DriveInfo::new("/")];
        w.start();
        w.key("h");
        let old = last_drive_watch(&w);
        w.send(Msg::DrivesChanged { gen: old });
        let listings = drive_listings(&w);
        // `old` was replaced by the watch of the refresh.
        w.send(Msg::DrivesChanged { gen: old });
        assert_eq!(drive_listings(&w), listings);
        // Left "This PC": the change of its last watch does nothing.
        let gen = last_drive_watch(&w);
        w.key("l");
        assert!(!w.state.show_drives);
        w.send(Msg::DrivesChanged { gen });
        assert_eq!(drive_listings(&w), listings);
        assert!(!w.state.show_drives);
    }

    #[test]
    fn refreshing_this_pc_lists_the_drives_not_the_last_directory() {
        let mut w = World::new("/");
        w.drives = vec![DriveInfo::new("/")];
        w.start();
        w.key("h");
        let mut effects = Vec::new();
        refresh_if_idle(&mut w.state, &mut effects);
        assert!(matches!(effects[..], [Effect::ListDrives { .. }]));
    }

    #[test]
    fn go_drives_key_and_history() {
        let mut w = project_world();
        w.key("g /");
        assert!(w.state.show_drives);
        w.key("ctrl-o");
        assert!(!w.state.show_drives);
        assert_eq!(w.cwd(), Path::new("/home/me"));
        assert_eq!(w.cursor(), "Downloads");
        w.key("tab");
        assert!(w.state.show_drives);
    }

    #[test]
    fn marks_and_visual_are_ignored_in_the_drives_view() {
        let mut w = project_world();
        w.key("g /");
        w.key("space v ctrl-a");
        assert_eq!(w.state.mode, Mode::Normal);
        assert_eq!(w.state.view.marks_count(), 0);
    }

    #[test]
    fn paging_and_edges_work_in_the_drives_view() {
        let mut w = World::new("/");
        w.drives = (0..30).map(|i| DriveInfo::new(format!("/d{i}"))).collect();
        w.start();
        w.key("g /");
        w.key("G");
        assert_eq!(w.state.drives.cursor.index, 29);
        w.key("g g");
        assert_eq!(w.state.drives.cursor.index, 0);
        w.key("ctrl-d");
        assert_eq!(w.state.drives.cursor.index, 9);
    }

    #[test]
    fn child_counts_arrive_for_the_current_generation_only() {
        let mut w = project_world();
        let Some(Effect::CountChildren { gen, names, .. }) = w
            .log
            .iter()
            .rev()
            .find(|e| matches!(e, Effect::CountChildren { .. }))
            .cloned()
        else {
            panic!("no count effect")
        };
        assert_eq!(names.len(), 2);
        w.send(Msg::ChildCounts {
            gen,
            counts: vec![("projects".into(), 3)],
        });
        let projects = (0..w.state.view.len())
            .filter_map(|i| w.state.view.row(i))
            .find(|e| e.display_name() == "projects")
            .unwrap();
        assert_eq!(projects.children, Some(3));
        w.key("j l");
        w.send(Msg::ChildCounts {
            gen,
            counts: vec![("alpha".into(), 99)],
        });
        assert!(w
            .state
            .view
            .entries()
            .iter()
            .all(|e| e.children != Some(99)));
    }

    #[test]
    fn git_results_apply_only_for_the_latest_request() {
        let mut w = project_world();
        let gen = w.state.git_gen.current();
        let snap = GitSnapshot::default();
        w.send(Msg::GitStatus {
            gen,
            snapshot: Some(Box::new(snap.clone())),
        });
        assert!(w.state.git.is_some());
        w.key("j l");
        assert!(w.state.git.is_none(), "navigating drops the old status");
        w.send(Msg::GitStatus {
            gen,
            snapshot: Some(Box::new(snap)),
        });
        assert!(w.state.git.is_none(), "a stale status is ignored");
    }

    #[test]
    fn directory_changes_debounce_a_git_refresh_only_inside_a_repo() {
        let mut w = project_world();
        w.send(Msg::DirChanged {
            gen: w.state.watch_gen.current(),
        });
        assert_eq!(
            w.effects_of(|e| matches!(
                e,
                Effect::StartTimer {
                    kind: TimerKind::GitRefresh,
                    ..
                }
            )),
            0
        );
        let gen = w.state.git_gen.current();
        w.send(Msg::GitStatus {
            gen,
            snapshot: Some(Box::default()),
        });
        w.send(Msg::DirChanged {
            gen: w.state.watch_gen.current(),
        });
        let timer = w
            .log
            .iter()
            .rev()
            .find_map(|e| match e {
                Effect::StartTimer {
                    kind: TimerKind::GitRefresh,
                    gen,
                    delay,
                } => Some((*gen, *delay)),
                _ => None,
            })
            .expect("a git timer");
        assert_eq!(timer.1, GIT_DEBOUNCE);
        let before = w.effects_of(|e| matches!(e, Effect::GitStatus { .. }));
        w.send(Msg::Timer {
            kind: TimerKind::GitRefresh,
            gen: timer.0,
        });
        assert_eq!(
            w.effects_of(|e| matches!(e, Effect::GitStatus { .. })),
            before + 1
        );
        // A stale debounce timer does nothing.
        let stale = timer.0;
        w.send(Msg::DirChanged {
            gen: w.state.watch_gen.current(),
        });
        w.send(Msg::Timer {
            kind: TimerKind::GitRefresh,
            gen: stale,
        });
        assert_eq!(
            w.effects_of(|e| matches!(e, Effect::GitStatus { .. })),
            before + 1
        );
    }

    #[test]
    fn resizing_keeps_the_cursor_visible() {
        let mut w = World::new("/big");
        let names: Vec<Entry> = (0..100)
            .map(|i| Entry::file(format!("f{i:03}"), 1))
            .collect();
        w.dir("/big", names);
        w.start();
        w.key("G");
        let h = w.state.viewport_height();
        assert_eq!(w.state.view.cursor.offset, 100 - h);
        w.send(Msg::Resize(Size::new(80, 10)));
        let h = w.state.viewport_height();
        assert_eq!(h, 6);
        assert!(w.state.view.cursor.index < w.state.view.cursor.offset + h);
    }

    #[test]
    fn a_superseded_history_move_does_not_lose_its_entry() {
        let mut w = project_world();
        w.key("j l j l");
        assert_eq!(w.cwd(), Path::new("/home/me/projects/beta"));
        // Two quick `back` presses with no listing delivered in between.
        update(&mut w.state, Msg::Key("ctrl-o".parse().unwrap()));
        update(&mut w.state, Msg::Key("ctrl-o".parse().unwrap()));
        assert_eq!(w.state.history.back.len(), 1, "one entry is in flight");
        // Entering a directory while a back move is pending puts its entry back.
        update(&mut w.state, Msg::Key("~".parse().unwrap()));
        assert_eq!(w.state.history.back.len(), 2);
    }

    #[test]
    fn visual_mode_ends_when_its_anchor_disappears() {
        let mut w = project_world();
        w.dirs
            .get_mut(Path::new("/home/me"))
            .unwrap()
            .push(Entry::file(".hidden", 1));
        w.send(Msg::DirChanged {
            gen: w.state.watch_gen.current(),
        });
        w.key(".");
        w.key("g g j j");
        assert_eq!(w.cursor(), ".hidden");
        w.key("v");
        assert_eq!(w.state.mode, Mode::Visual);
        w.key(".");
        assert_eq!(w.state.mode, Mode::Normal);
    }

    #[test]
    fn a_failed_visit_relists_so_swallowed_changes_show_up() {
        let mut w = project_world();
        w.denied.push(p("/home/me/projects"));
        w.dirs
            .get_mut(Path::new("/home/me"))
            .unwrap()
            .push(Entry::file("aaa", 1));
        w.key("j l");
        assert!(w.names().contains(&"aaa".to_owned()), "{:?}", w.names());
    }

    #[test]
    fn entry_kinds_decide_navigability() {
        let mut w = World::new("/x");
        w.dir(
            "/x",
            vec![
                Entry::new("junction", EntryKind::Junction),
                Entry::new("linkdir", EntryKind::Symlink).with_dir_target(),
                Entry::new("linkfile", EntryKind::Symlink),
            ],
        );
        w.dir("/x/junction", vec![Entry::file("in", 1)]);
        w.dir("/x/linkdir", vec![Entry::file("in2", 1)]);
        w.start();
        w.key("l");
        assert_eq!(w.cwd(), Path::new("/x/junction"));
        w.key("h j l");
        assert_eq!(w.cwd(), Path::new("/x/linkdir"));
        w.key("h G l");
        assert_eq!(w.cwd(), Path::new("/x"), "a file link is not entered");
    }
}
