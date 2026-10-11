//! Find file (`ctrl-p`, PLAN 6.2): the overlay's state and its transitions.
//!
//! The fuzzy matcher itself lives in the effect runner (a `FinderSession` owned by a worker
//! thread). This module only holds what the screen shows: the query, the window of ranked
//! results the worker last reported, the selection, the marks and the toggles. Everything the
//! worker reports carries the session generation, so results of a replaced session are
//! dropped.

use std::ffi::OsString;
use std::path::{Path, PathBuf};

use ks_search::{EntryKind as WalkKind, FinderResult, Scope};

use super::geometry;
use super::input::{Edit, LineInput};
use crate::effect::Effect;
use crate::entry::{Entry, EntryKind};
use crate::key::KeyChord;
use crate::mode::Overlay;
use crate::nav;
use crate::state::{AppState, Level, Size};
use crate::update::set_status;

/// What the worker reports after matching: counters plus a window of ranked results.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct FinderSnapshot {
    /// The query the results answer. Results for an older query are ignored.
    pub query: String,
    /// The directory that was walked (the resolved scope).
    pub root: PathBuf,
    pub matched: u32,
    pub total: u32,
    /// The walker is still running.
    pub walking: bool,
    /// The walk was served from the cache.
    pub from_cache: bool,
    /// Directories the walk skipped as access denied (0 for a cache-served walk).
    pub denied: u64,
    /// Rank of `results[0]`.
    pub offset: u32,
    pub results: Vec<FinderResult>,
}

#[derive(Clone, Debug)]
pub struct FinderState {
    pub input: LineInput,
    pub scope: Scope,
    pub show_hidden: bool,
    pub respect_gitignore: bool,
    pub(crate) gen: u64,
    /// The resolved scope root, known after the first snapshot.
    pub root: Option<PathBuf>,
    pub matched: u32,
    pub total: u32,
    pub walking: bool,
    pub from_cache: bool,
    /// Directories skipped as access denied by the last finished walk.
    pub denied: u64,
    /// Results `window_offset..window_offset + results.len()` of the ranking.
    pub results: Vec<FinderResult>,
    pub window_offset: u32,
    /// Rank of the highlighted result.
    pub selected: u32,
    /// Rank of the first visible result.
    pub scroll: u32,
    /// Marked results in marking order.
    pub marks: Vec<FinderResult>,
    /// While `ctrl-r` is looking for a scope with a root of its own: the roots passed so far
    /// (a scope that resolves to one of them, such as the git root when already at it, is
    /// skipped).
    cycle_seen: Option<Vec<PathBuf>>,
}

impl FinderState {
    /// The highlighted result, when it is inside the reported window.
    pub fn current(&self) -> Option<&FinderResult> {
        self.result_at(self.selected)
    }

    /// The result of rank `rank`, when it is inside the reported window.
    pub fn result_at(&self, rank: u32) -> Option<&FinderResult> {
        let index = rank.checked_sub(self.window_offset)?;
        self.results.get(index as usize)
    }

    pub fn is_marked(&self, result: &FinderResult) -> bool {
        self.marks.iter().any(|m| m.path == result.path)
    }

    fn rows(size: Size) -> u32 {
        u32::from(geometry::finder_rows(size)).max(1)
    }

    /// The window to ask the worker for: the visible rows plus a page on either side, so
    /// small moves never show blank rows while the next window is on its way.
    fn window(&self, rows: u32) -> (u32, u32) {
        (self.scroll.saturating_sub(rows), rows * 3)
    }
}

pub(crate) fn open(state: &mut AppState, effects: &mut Vec<Effect>) {
    if matches!(state.overlays.last(), Some(Overlay::Finder)) {
        return;
    }
    let rows = FinderState::rows(state.size);
    let finder = FinderState {
        input: LineInput::default(),
        scope: state.search.finder_scope,
        show_hidden: state.search.finder_hidden,
        respect_gitignore: !state.search.finder_gitignore,
        gen: 0,
        root: None,
        matched: 0,
        total: 0,
        walking: true,
        from_cache: false,
        denied: 0,
        results: Vec::new(),
        window_offset: 0,
        selected: 0,
        scroll: 0,
        marks: Vec::new(),
        cycle_seen: None,
    };
    state.search.finder = Some(finder);
    state.overlays.push(Overlay::Finder);
    restart(state, effects, rows);
}

/// Starts (or restarts, after a scope or toggle change) the worker session.
fn restart(state: &mut AppState, effects: &mut Vec<Effect>, rows: u32) {
    state.search.finder_gen += 1;
    let gen = state.search.finder_gen;
    let cwd = state.cwd.clone();
    let Some(f) = state.search.finder.as_mut() else {
        return;
    };
    f.gen = gen;
    f.walking = true;
    let (offset, limit) = f.window(rows);
    effects.push(Effect::FinderStart {
        gen,
        scope: f.scope,
        cwd,
        show_hidden: f.show_hidden,
        respect_gitignore: f.respect_gitignore,
        query: f.input.text().to_owned(),
        offset,
        limit,
    });
    state.dirty = true;
}

pub(crate) fn close(state: &mut AppState, effects: &mut Vec<Effect>) {
    if matches!(state.overlays.last(), Some(Overlay::Finder)) {
        state.overlays.pop();
    }
    state.search.finder = None;
    state.search.finder_gen += 1;
    effects.push(Effect::FinderStop);
    state.dirty = true;
}

/// A key the keymap did not bind: text editing.
pub(crate) fn on_text_key(state: &mut AppState, effects: &mut Vec<Effect>, key: KeyChord) -> bool {
    let rows = FinderState::rows(state.size);
    let Some(f) = state.search.finder.as_mut() else {
        return false;
    };
    match f.input.apply(key) {
        Edit::Ignored => false,
        Edit::Moved => true,
        Edit::Changed => {
            f.selected = 0;
            f.scroll = 0;
            let (offset, limit) = f.window(rows);
            effects.push(Effect::FinderQuery {
                gen: f.gen,
                query: f.input.text().to_owned(),
                offset,
                limit,
            });
            true
        }
    }
}

pub(crate) fn on_results(
    state: &mut AppState,
    effects: &mut Vec<Effect>,
    gen: u64,
    snapshot: FinderSnapshot,
) {
    let Some(f) = state.search.finder.as_mut() else {
        return;
    };
    if f.gen != gen || snapshot.query != f.input.text() {
        return;
    }
    // `ctrl-r` landed on a scope whose root is one it already passed: go on to the next.
    if let Some(mut seen) = f.cycle_seen.take() {
        let repeated = seen
            .iter()
            .any(|root| crate::fileops::same_path(root, &snapshot.root));
        // Four scopes exist; when all of them resolve to one root the cycle wraps back to the
        // scope it started on, so ctrl-r never leaves a scope label that shows the same results.
        if repeated && seen.len() < 4 {
            seen.push(snapshot.root);
            advance_scope(state, effects, seen);
            return;
        }
    }
    f.root = Some(snapshot.root);
    f.matched = snapshot.matched;
    f.total = snapshot.total;
    f.walking = snapshot.walking;
    f.from_cache = snapshot.from_cache;
    f.denied = snapshot.denied;
    f.window_offset = snapshot.offset;
    f.results = snapshot.results;
    f.selected = f.selected.min(f.matched.saturating_sub(1));
    state.dirty = true;
}

pub(crate) fn on_resize(state: &mut AppState, effects: &mut Vec<Effect>) {
    let rows = FinderState::rows(state.size);
    if let Some(f) = state.search.finder.as_mut() {
        keep_visible(f, rows);
        let (offset, limit) = f.window(rows);
        effects.push(Effect::FinderWindow {
            gen: f.gen,
            offset,
            limit,
        });
    }
}

/// Scrolls so the selection is inside the viewport of `rows` rows.
fn keep_visible(f: &mut FinderState, rows: u32) {
    if f.selected < f.scroll {
        f.scroll = f.selected;
    } else if f.selected >= f.scroll + rows {
        f.scroll = f.selected + 1 - rows;
    }
}

pub(crate) fn move_selection(state: &mut AppState, effects: &mut Vec<Effect>, delta: i64) {
    let rows = FinderState::rows(state.size);
    let Some(f) = state.search.finder.as_mut() else {
        return;
    };
    let last = i64::from(f.matched.saturating_sub(1));
    let target = (i64::from(f.selected) + delta).clamp(0, last);
    f.selected = u32::try_from(target).unwrap_or(0);
    let before = f.scroll;
    keep_visible(f, rows);
    if f.scroll != before || f.result_at(f.selected).is_none() {
        let (offset, limit) = f.window(rows);
        effects.push(Effect::FinderWindow {
            gen: f.gen,
            offset,
            limit,
        });
    }
    state.dirty = true;
}

pub(crate) fn half_page(state: &mut AppState, effects: &mut Vec<Effect>, down: bool) {
    let step = i64::from(FinderState::rows(state.size) / 2).max(1);
    move_selection(state, effects, if down { step } else { -step });
}

pub(crate) fn cycle_scope(state: &mut AppState, effects: &mut Vec<Effect>) {
    let seen = state
        .search
        .finder
        .as_ref()
        .map(|f| f.root.iter().cloned().collect())
        .unwrap_or_default();
    advance_scope(state, effects, seen);
}

/// Moves to the next scope and restarts the session; `seen` are the roots passed already.
fn advance_scope(state: &mut AppState, effects: &mut Vec<Effect>, seen: Vec<PathBuf>) {
    let rows = FinderState::rows(state.size);
    if let Some(f) = state.search.finder.as_mut() {
        f.scope = f.scope.next();
        f.root = None;
        f.marks.clear();
        f.cycle_seen = Some(seen);
        state.search.finder_scope = f.scope;
    }
    reset_view(state);
    restart(state, effects, rows);
}

pub(crate) fn toggle_hidden(state: &mut AppState, effects: &mut Vec<Effect>) {
    let rows = FinderState::rows(state.size);
    if let Some(f) = state.search.finder.as_mut() {
        f.show_hidden = !f.show_hidden;
        state.search.finder_hidden = f.show_hidden;
    }
    reset_view(state);
    restart(state, effects, rows);
}

pub(crate) fn toggle_gitignore(state: &mut AppState, effects: &mut Vec<Effect>) {
    let rows = FinderState::rows(state.size);
    if let Some(f) = state.search.finder.as_mut() {
        f.respect_gitignore = !f.respect_gitignore;
        // `finder_gitignore` remembers "show ignored files".
        state.search.finder_gitignore = !f.respect_gitignore;
    }
    reset_view(state);
    restart(state, effects, rows);
}

fn reset_view(state: &mut AppState) {
    if let Some(f) = state.search.finder.as_mut() {
        f.selected = 0;
        f.scroll = 0;
        f.window_offset = 0;
        f.results.clear();
    }
}

/// `tab`: toggles the mark of the highlighted result and moves down.
pub(crate) fn toggle_mark(state: &mut AppState, effects: &mut Vec<Effect>) {
    if let Some(f) = state.search.finder.as_mut() {
        if let Some(result) = f.current().cloned() {
            if let Some(at) = f.marks.iter().position(|m| m.path == result.path) {
                f.marks.remove(at);
            } else {
                f.marks.push(result);
            }
        }
    }
    move_selection(state, effects, 1);
}

/// The most results `ctrl-o` launches in one go.
const MAX_OPEN: usize = 10;

/// `ctrl-enter` / `ctrl-o`: opens the marked results, or the highlighted one.
pub(crate) fn open_default(state: &mut AppState, effects: &mut Vec<Effect>) {
    let Some(f) = state.search.finder.as_ref() else {
        return;
    };
    let paths: Vec<PathBuf> = if f.marks.is_empty() {
        f.current().map(|r| r.path.clone()).into_iter().collect()
    } else {
        f.marks.iter().map(|r| r.path.clone()).collect()
    };
    if paths.len() > MAX_OPEN {
        crate::update::set_status(
            state,
            crate::state::Level::Warning,
            format!(
                "Too many marked results to open at once ({} > {MAX_OPEN})",
                paths.len()
            ),
            effects,
        );
        return;
    }
    effects.extend(paths.into_iter().map(|path| Effect::OpenDefault { path }));
}

/// `enter`.
///
/// * No marks: go to the directory of the highlighted result with the cursor on it.
/// * Marks that all live in one directory: go there, cursor on the first mark, all marked.
/// * Marks in several directories: show them as a results list.
pub(crate) fn accept(state: &mut AppState, effects: &mut Vec<Effect>) {
    let Some(f) = state.search.finder.as_ref() else {
        return;
    };
    let query = f.input.text().to_owned();
    let root = f.root.clone().unwrap_or_else(|| state.cwd.clone());
    let marks = f.marks.clone();
    let current = f.current().cloned();
    if marks.is_empty() {
        let Some(result) = current else {
            return;
        };
        close(state, effects);
        nav::go_to_file(state, effects, &result.path, Vec::new());
        return;
    }
    let dirs_equal = marks
        .iter()
        .all(|m| m.path.parent() == marks[0].path.parent());
    close(state, effects);
    if dirs_equal {
        let names: Vec<OsString> = marks
            .iter()
            .filter_map(|m| m.path.file_name().map(ToOwned::to_owned))
            .collect();
        nav::go_to_file(state, effects, &marks[0].path, names);
    } else {
        let label = if query.is_empty() {
            "Marked files".to_owned()
        } else {
            format!("Find file: {query}")
        };
        let entries = marks
            .iter()
            .map(|m| result_entry(&root, &m.path, m.kind))
            .collect();
        nav::show_results_list(state, effects, root, label, entries);
    }
}

/// A listing entry for a search result: the name is the path relative to the search root,
/// so `root.join(name)` is the file (the results list is a listing of `root`).
pub(crate) fn result_entry(root: &Path, path: &Path, kind: WalkKind) -> Entry {
    let rel = path.strip_prefix(root).unwrap_or(path);
    let kind = match kind {
        WalkKind::File => EntryKind::File,
        WalkKind::Dir => EntryKind::Dir,
        WalkKind::Symlink => EntryKind::Symlink,
    };
    Entry::new(rel.as_os_str().to_owned(), kind)
}

/// Report that a finder-only action ran outside the overlay.
pub(crate) fn outside(state: &mut AppState, effects: &mut Vec<Effect>, title: &str) {
    set_status(
        state,
        Level::Info,
        format!("{title} works inside the find-file overlay"),
        effects,
    );
}
