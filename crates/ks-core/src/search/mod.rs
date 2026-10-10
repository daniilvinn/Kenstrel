//! Search features (phases P3 + P4): the in-directory filter (`/`), find file (`ctrl-p`) and
//! content search (`ctrl-f`).
//!
//! * [`finder`], [`grep`]: overlay state and transitions. The engines (`ks-search`) run in
//!   the effect runner and report back as [`Msg::FinderResults`](crate::Msg) and
//!   [`Msg::Grep`](crate::Msg).
//! * [`input`]: the single-line text input behind the query fields.
//! * [`geometry`]: overlay size computations shared with the renderer.
//! * This module: [`SearchState`], the filter, and the hooks `update` calls (the only
//!   places where this feature touches the shared dispatch code).

pub mod finder;
pub mod geometry;
pub mod grep;
pub mod input;

use std::collections::HashSet;
use std::ffi::{OsStr, OsString};
use std::path::{Path, PathBuf};

use ks_search::PathChange;

pub use finder::{FinderSnapshot, FinderState};
pub use grep::{GrepField, GrepRow, GrepState};
pub use input::{Edit, LineInput};
pub use ks_search::Scope;

use crate::action::Action;
use crate::effect::Effect;
use crate::entry::Entry;
use crate::generation::{Gen, GenCounter};
use crate::key::KeyChord;
use crate::keymap::Invocation;
use crate::mode::{KeyContext, Mode};
use crate::state::{AppState, Level};
use crate::update::set_status;

/// All search state of the application.
#[derive(Clone, Debug)]
pub struct SearchState {
    /// The text being typed in filter mode.
    pub filter_input: LineInput,
    pub finder: Option<FinderState>,
    pub grep: Option<GrepState>,
    /// Remembered between openings of the find-file overlay.
    pub finder_scope: Scope,
    pub finder_hidden: bool,
    /// "Show gitignored files" (the toggle is off by default: ignored files are skipped).
    pub finder_gitignore: bool,
    pub grep_regex: bool,
    pub grep_word: bool,
    /// Query and glob of the last content search, restored when the overlay reopens.
    pub last_grep: Option<(String, String)>,
    pub(crate) finder_gen: u64,
    pub grep_gen: u64,
    pub(crate) grep_timer: GenCounter,
    /// Entries to mark once the listing of the directory has finished loading.
    pub(crate) pending_marks: Option<(PathBuf, Vec<OsString>)>,
}

impl Default for SearchState {
    fn default() -> Self {
        Self {
            filter_input: LineInput::default(),
            finder: None,
            grep: None,
            finder_scope: Scope::Cwd,
            finder_hidden: false,
            finder_gitignore: false,
            grep_regex: false,
            grep_word: false,
            last_grep: None,
            finder_gen: 0,
            grep_gen: 0,
            grep_timer: GenCounter::default(),
            pending_marks: None,
        }
    }
}

impl SearchState {
    /// A search is in progress (walking, matching or waiting for the debounce), so the
    /// spinner should animate.
    pub fn is_busy(&self) -> bool {
        self.finder.as_ref().is_some_and(|f| f.walking)
            || self.grep.as_ref().is_some_and(|g| g.running || g.waiting)
    }
}

/// The file an open search overlay highlights: the hook through which a preview pane (P2)
/// can follow the selection. `line` is set for content-search hits.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct PreviewTarget {
    pub path: PathBuf,
    pub line: Option<u64>,
}

/// The highlighted result of the topmost search overlay.
pub fn preview_target(state: &AppState) -> Option<PreviewTarget> {
    match state.overlays.last()? {
        crate::mode::Overlay::Finder => {
            let result = state.search.finder.as_ref()?.current()?;
            Some(PreviewTarget {
                path: result.path.clone(),
                line: None,
            })
        }
        crate::mode::Overlay::Grep => {
            let (file, line) = state.search.grep.as_ref()?.current()?;
            Some(PreviewTarget {
                path: file.path.clone(),
                line: Some(line.line_number),
            })
        }
        _ => None,
    }
}

// ----- hooks called from `update` -----

/// Runs the search actions and the overlay-context behaviour of the generic ones. Returns
/// false when the action is not a search matter.
pub(crate) fn run_action(
    state: &mut AppState,
    invocation: Invocation,
    effects: &mut Vec<Effect>,
) -> bool {
    let ctx = state.key_context();
    let overlay = matches!(ctx, KeyContext::Finder | KeyContext::Grep);
    match invocation.action {
        Action::FilterOpen => open_filter(state),
        Action::FindFile => finder::open(state, effects),
        Action::SearchContents => grep::open(state, effects),
        Action::FinderScope => in_finder(state, effects, "Cycling the scope", finder::cycle_scope),
        Action::FinderHidden => {
            in_finder(
                state,
                effects,
                "Toggling hidden files",
                finder::toggle_hidden,
            );
        }
        Action::FinderGitignore => in_finder(
            state,
            effects,
            "Toggling gitignored files",
            finder::toggle_gitignore,
        ),
        Action::FinderOpen => in_finder(state, effects, "Opening a result", finder::open_default),
        Action::FinderMark => in_finder(state, effects, "Marking a result", finder::toggle_mark),
        Action::GrepRegex => in_grep(state, effects, "The regex toggle", grep::toggle_regex),
        Action::GrepWholeWord => {
            in_grep(state, effects, "The whole-word toggle", grep::toggle_word)
        }
        Action::GrepNextField => in_grep(state, effects, "Switching fields", |s, _| {
            grep::next_field(s);
        }),
        Action::GrepToList => in_grep(state, effects, "Sending results to a list", grep::to_list),
        Action::Accept => match ctx {
            KeyContext::Filter => accept_filter(state),
            KeyContext::Finder => finder::accept(state, effects),
            KeyContext::Grep => grep::accept(state, effects),
            _ => return false,
        },
        Action::CursorDown | Action::CursorUp if overlay => {
            let down = invocation.action == Action::CursorDown;
            match ctx {
                KeyContext::Finder => {
                    finder::move_selection(state, effects, if down { 1 } else { -1 })
                }
                _ => grep::move_selection(state, if down { 1 } else { -1 }),
            }
        }
        Action::PageHalfDown | Action::PageHalfUp if overlay => {
            let down = invocation.action == Action::PageHalfDown;
            match ctx {
                KeyContext::Finder => finder::half_page(state, effects, down),
                _ => grep::half_page(state, down),
            }
        }
        _ => return false,
    }
    state.dirty = true;
    true
}

fn in_finder(
    state: &mut AppState,
    effects: &mut Vec<Effect>,
    what: &str,
    run: fn(&mut AppState, &mut Vec<Effect>),
) {
    if state.key_context() == KeyContext::Finder {
        run(state, effects);
    } else {
        finder::outside(state, effects, what);
    }
}

fn in_grep(
    state: &mut AppState,
    effects: &mut Vec<Effect>,
    what: &str,
    run: fn(&mut AppState, &mut Vec<Effect>),
) {
    if state.key_context() == KeyContext::Grep {
        run(state, effects);
    } else {
        set_status(
            state,
            Level::Info,
            format!("{what} works inside the content-search overlay"),
            effects,
        );
    }
}

/// A key without a binding in the active text-entry context.
pub(crate) fn on_unbound_key(
    state: &mut AppState,
    effects: &mut Vec<Effect>,
    key: KeyChord,
) -> bool {
    match state.key_context() {
        KeyContext::Filter => filter_text_key(state, key),
        KeyContext::Finder => finder::on_text_key(state, effects, key),
        KeyContext::Grep => grep::on_text_key(state, effects, key),
        _ => false,
    }
}

/// `esc`. Closes the search overlays and clears the filter; false when nothing of the
/// search feature was active.
pub(crate) fn cancel(state: &mut AppState, effects: &mut Vec<Effect>) -> bool {
    match state.overlays.last() {
        Some(crate::mode::Overlay::Finder) => {
            finder::close(state, effects);
            return true;
        }
        Some(crate::mode::Overlay::Grep) => {
            grep::close(state, effects);
            return true;
        }
        Some(_) => return false,
        None => {}
    }
    if state.mode == Mode::Filter || state.view.filter_query().is_some() {
        clear_filter(state);
        return true;
    }
    if state.view.label.is_some() {
        crate::nav::leave_results_list(state, effects);
        state.dirty = true;
        return true;
    }
    false
}

/// A tab is being left: its find-file / content-search overlays close (their sessions are
/// stopped) and a pending mark request is dropped. The kept filter lives in the tab's view.
pub(crate) fn leave_tab(state: &mut AppState, effects: &mut Vec<Effect>) {
    state
        .overlays
        .retain(|o| !matches!(o, crate::mode::Overlay::Finder | crate::mode::Overlay::Grep));
    if state.search.finder.is_some() {
        finder::close(state, effects);
    }
    if state.search.grep.is_some() {
        grep::close(state, effects);
    }
    state.search.pending_marks = None;
}

pub(crate) fn on_resize(state: &mut AppState, effects: &mut Vec<Effect>) {
    finder::on_resize(state, effects);
    grep::on_resize(state);
}

pub(crate) fn on_timer(state: &mut AppState, effects: &mut Vec<Effect>, gen: Gen) {
    grep::on_timer(state, effects, gen);
}

pub(crate) fn on_finder_results(state: &mut AppState, gen: u64, snapshot: FinderSnapshot) {
    finder::on_results(state, gen, snapshot);
}

// ----- the in-directory filter -----

fn open_filter(state: &mut AppState) {
    if state.show_drives {
        return;
    }
    state.mode = Mode::Filter;
    state.search.filter_input = LineInput::new(state.view.filter_query().unwrap_or_default());
}

fn filter_text_key(state: &mut AppState, key: KeyChord) -> bool {
    match state.search.filter_input.apply(key) {
        Edit::Ignored => false,
        Edit::Moved => true,
        Edit::Changed => {
            let query = state.search.filter_input.text().to_owned();
            let height = state.viewport_height();
            state
                .view
                .set_filter((!query.trim().is_empty()).then_some(query), height);
            true
        }
    }
}

/// `enter` in filter mode: keep the filter and return to normal navigation.
fn accept_filter(state: &mut AppState) {
    state.mode = Mode::Normal;
}

fn clear_filter(state: &mut AppState) {
    state.mode = Mode::Normal;
    state.search.filter_input.clear();
    let height = state.viewport_height();
    state.view.set_filter(None, height);
}

// ----- walk cache -----

/// The path changes between two listings of `dir`, for the walk cache (PLAN 6.2: the cache
/// is refreshed incrementally from what the watcher saw). A rename shows up as a removal
/// plus an addition.
pub(crate) fn diff_listing(dir: &Path, old: &[Entry], new: &[Entry]) -> Vec<PathChange> {
    let old_names: HashSet<&OsStr> = old.iter().map(|e| e.name.as_os_str()).collect();
    let new_names: HashSet<&OsStr> = new.iter().map(|e| e.name.as_os_str()).collect();
    let mut changes = Vec::new();
    for entry in old
        .iter()
        .filter(|e| !new_names.contains(e.name.as_os_str()))
    {
        changes.push(PathChange::Removed(dir.join(&entry.name)));
    }
    for entry in new
        .iter()
        .filter(|e| !old_names.contains(e.name.as_os_str()))
    {
        let kind = if entry.is_link() {
            ks_search::EntryKind::Symlink
        } else if entry.is_dir {
            ks_search::EntryKind::Dir
        } else {
            ks_search::EntryKind::File
        };
        changes.push(PathChange::Added(dir.join(&entry.name), kind));
    }
    changes
}

/// Marks the entries a find-file `enter` asked for, once their directory has loaded.
pub(crate) fn apply_pending_marks(state: &mut AppState) {
    let Some((dir, names)) = state.search.pending_marks.take() else {
        return;
    };
    if state.cwd == dir && !state.show_drives {
        state.view.mark_names(&names);
        state.dirty = true;
    }
}

#[cfg(test)]
mod tests;
