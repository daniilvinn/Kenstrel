//! Tabs (`t`, `1`..`9`, `ctrl-w`).
//!
//! The navigation state of the *active* tab lives directly in [`AppState`] (`cwd`, `view`,
//! `history`, ...), so all of the single-pane code works unchanged. The other tabs are parked
//! in [`Tabs`] as [`TabSlot`]s and swapped in and out on a switch.
//!
//! Background results are routed to the right tab by their generation id: every tab owns
//! generation counters in a space of its own ([`GenCounter::in_space`]), so a listing, watch,
//! git or child-count result that belongs to a parked tab can never equal a current ticket of
//! the active tab and is dropped. A tab being parked also invalidates its own tickets, and a
//! tab being activated re-lists its directory (keeping the cursor on its entry by name),
//! re-arms the watcher and re-runs `git status`. Only the active tab is watched (deviation
//! from PLAN 7, which watches every tab: a parked tab is refreshed when it is activated, so
//! the user sees the same thing without a watcher per tab). Until the re-list arrives the tab
//! shows its old entries; a directory that vanished meanwhile is reported as a failed refresh
//! and the old entries stay. The effect protocol has a single watch slot, so watching every
//! tab needs a multi-path `WatchDir` (integration note in docs/progress/p7.md).

use std::collections::HashMap;
use std::ffi::OsString;
use std::path::PathBuf;

use crate::drives::DrivesView;
use crate::effect::Effect;
use crate::generation::GenCounter;
use crate::git::GitSnapshot;
use crate::mode::Mode;
use crate::msg::TimerKind;
use crate::nav::{self, History, Location, NavKind, PendingNav};
use crate::search;
use crate::state::{AppState, Level};
use crate::update::set_status;
use crate::view::DirView;

/// Most tabs that can be open (`1`..`9` address them).
pub const MAX_TABS: usize = 9;

/// The per-tab navigation state, moved between [`AppState`] and [`Tabs`].
pub(crate) struct TabSlot {
    cwd: PathBuf,
    view: DirView,
    show_drives: bool,
    drives: DrivesView,
    git: Option<GitSnapshot>,
    history: History,
    cursor_memory: HashMap<PathBuf, OsString>,
    nav: Option<PendingNav>,
    list_gen: GenCounter,
    watch_gen: GenCounter,
    git_gen: GenCounter,
    git_timer_gen: GenCounter,
    children_gen: GenCounter,
}

impl TabSlot {
    fn fresh(space: u32, cwd: PathBuf, view: DirView) -> Self {
        Self {
            cwd,
            view,
            show_drives: false,
            drives: DrivesView::default(),
            git: None,
            history: History::default(),
            cursor_memory: HashMap::new(),
            nav: None,
            list_gen: GenCounter::in_space(space),
            watch_gen: GenCounter::in_space(space),
            git_gen: GenCounter::in_space(space),
            git_timer_gen: GenCounter::in_space(space),
            children_gen: GenCounter::in_space(space),
        }
    }

    /// Exchanges this slot's contents with the active tab's fields of `state`.
    fn swap_with(&mut self, state: &mut AppState) {
        use std::mem::swap;
        swap(&mut self.cwd, &mut state.cwd);
        swap(&mut self.view, &mut state.view);
        swap(&mut self.show_drives, &mut state.show_drives);
        swap(&mut self.drives, &mut state.drives);
        swap(&mut self.git, &mut state.git);
        swap(&mut self.history, &mut state.history);
        swap(&mut self.cursor_memory, &mut state.cursor_memory);
        swap(&mut self.nav, &mut state.nav);
        swap(&mut self.list_gen, &mut state.list_gen);
        swap(&mut self.watch_gen, &mut state.watch_gen);
        swap(&mut self.git_gen, &mut state.git_gen);
        swap(&mut self.git_timer_gen, &mut state.git_timer_gen);
        swap(&mut self.children_gen, &mut state.children_gen);
    }
}

/// The tab list. The active tab's slot is empty (its data is in `AppState`).
pub struct Tabs {
    slots: Vec<Option<TabSlot>>,
    active: usize,
    next_space: u32,
}

impl Default for Tabs {
    fn default() -> Self {
        Self {
            slots: vec![None],
            active: 0,
            // Space 0 belongs to the first tab (the default counters).
            next_space: 1,
        }
    }
}

impl Tabs {
    /// Number of open tabs (at least 1).
    pub fn count(&self) -> usize {
        self.slots.len()
    }

    /// Index of the active tab, from 0.
    pub fn active_index(&self) -> usize {
        self.active
    }
}

/// Compile-time guard: every [`AppState`] field is listed here, so adding one breaks the
/// build until it has been decided whether it is per tab (add it to [`TabSlot`] and
/// `swap_with`) or shared by all tabs.
#[allow(dead_code)]
fn classify_state_fields(state: &AppState) {
    let AppState {
        // Per tab (swapped by `TabSlot::swap_with`).
        cwd: _,
        view: _,
        show_drives: _,
        drives: _,
        git: _,
        history: _,
        cursor_memory: _,
        nav: _,
        list_gen: _,
        watch_gen: _,
        git_gen: _,
        git_timer_gen: _,
        children_gen: _,
        // Shared by all tabs: the pane follows the active tab's cursor (a different cursor
        // path makes `preview::sync` request it), and `w`/`P`/`i` are global toggles.
        preview: _,
        // Shared by all tabs.
        home: _,
        places: _,
        config: _,
        keymap: _,
        size: _,
        mode: _,
        overlays: _,
        pending: _,
        which_key_visible: _,
        status: _,
        jobs: _,
        spinner_frame: _,
        dirty: _,
        quit: _,
        now: _,
        utc_offset_secs: _,
        tabs: _,
        aids: _,
        // Overlay state (find file, content search) and the filter text box. The filter itself
        // (query, matches, highlights) lives in `view` and is therefore per tab; the overlays
        // are closed when a tab is left (`prepare_to_leave`).
        search: _,
        // Yank register, job table, prompt and dialogs: one set for the whole app. Marks live in
        // `view` and so are per tab.
        fileops: _,
        palette: _,
        help: _,
        themes: _,
        which_key_gen: _,
        status_gen: _,
    } = state;
}

/// Makes the state of the active tab safe to leave: nothing of it may react to results that
/// arrive while another tab is shown.
fn prepare_to_leave(state: &mut AppState, effects: &mut Vec<Effect>) {
    nav::abandon_history_nav(state);
    state.nav = None;
    // Late chunks of an unfinished listing must not extend the view; it is re-listed on
    // return anyway.
    state.view.gen = state.list_gen.bump();
    state.view.finish_loading();
    state.watch_gen.bump();
    state.git_gen.bump();
    state.git_timer_gen.bump();
    state.children_gen.bump();
    state.view.exit_visual();
    state.mode = Mode::Normal;
    // A find-file / content-search overlay belongs to the tab it was opened in.
    search::leave_tab(state, effects);
}

/// The newly shown tab refreshes itself: re-list, watch and ask git.
fn resume(state: &mut AppState, effects: &mut Vec<Effect>) {
    state.dirty = true;
    effects.push(Effect::CancelTimer(TimerKind::GitRefresh));
    if state.view.label.is_some() {
        // A results list is not a directory listing: nothing to refresh, watch or ask git.
        effects.push(Effect::StopWatching);
        return;
    }
    if state.show_drives {
        nav::request(state, effects, Location::Drives, NavKind::Refresh, None);
        effects.push(Effect::StopWatching);
        return;
    }
    nav::refresh(state, effects);
    effects.push(Effect::WatchDir {
        gen: state.watch_gen.bump(),
        path: state.cwd.clone(),
    });
    effects.push(Effect::GitStatus {
        gen: state.git_gen.bump(),
        dir: state.cwd.clone(),
    });
}

/// `t`: a new tab next to the current one, at the current directory.
pub(crate) fn new_tab(state: &mut AppState, effects: &mut Vec<Effect>) {
    if state.tabs.count() >= MAX_TABS {
        let text = format!("At most {MAX_TABS} tabs");
        set_status(state, Level::Warning, text, effects);
        return;
    }
    let at_drives = state.show_drives;
    let reveal = state.view.current().map(|e| e.name.clone());
    let view = DirView::new(state.view.sort.clone(), state.view.show_hidden);
    let space = state.tabs.next_space;
    state.tabs.next_space += 1;
    prepare_to_leave(state, effects);
    let mut slot = TabSlot::fresh(space, state.cwd.clone(), view);
    slot.swap_with(state);
    let at = state.tabs.active;
    state.tabs.slots[at] = Some(slot);
    state.tabs.slots.insert(at + 1, None);
    state.tabs.active = at + 1;
    // The new tab starts with an empty view and lists the directory like a first visit.
    if at_drives {
        nav::request(state, effects, Location::Drives, NavKind::Visit, None);
    } else {
        let cwd = state.cwd.clone();
        state.aids.skip_next_visit(cwd.clone());
        nav::request(state, effects, Location::Dir(cwd), NavKind::Visit, reveal);
    }
    effects.push(Effect::CancelTimer(TimerKind::GitRefresh));
    state.dirty = true;
}

/// Opens `view` (a virtual listing such as the search results list) in a new tab next to the
/// current one, with `cwd` as its directory. Nothing is listed, watched or asked of git: the
/// tab shows exactly `view`. Callers check [`has_room`] first.
pub(crate) fn open_in_new_tab(
    state: &mut AppState,
    effects: &mut Vec<Effect>,
    cwd: PathBuf,
    view: DirView,
) {
    let space = state.tabs.next_space;
    state.tabs.next_space += 1;
    prepare_to_leave(state, effects);
    let mut slot = TabSlot::fresh(space, cwd, view);
    slot.swap_with(state);
    let at = state.tabs.active;
    state.tabs.slots[at] = Some(slot);
    state.tabs.slots.insert(at + 1, None);
    state.tabs.active = at + 1;
    effects.push(Effect::CancelTimer(TimerKind::GitRefresh));
    effects.push(Effect::StopWatching);
    state.dirty = true;
}

/// Whether another tab can be opened.
pub(crate) fn has_room(state: &AppState) -> bool {
    state.tabs.count() < MAX_TABS
}

/// `1`..`9`: switch to tab `number` (counted from 1).
pub(crate) fn goto(state: &mut AppState, effects: &mut Vec<Effect>, number: Option<char>) {
    let Some(number) = number.and_then(|c| c.to_digit(10)) else {
        // Run from the palette without a number: say what to press.
        set_status(
            state,
            Level::Info,
            "Press 1-9 to pick a tab".into(),
            effects,
        );
        return;
    };
    let target = (number as usize).saturating_sub(1);
    if number == 0 || target >= state.tabs.count() {
        let text = format!("No tab {number} ({} open)", state.tabs.count());
        set_status(state, Level::Info, text, effects);
        return;
    }
    switch_to(state, effects, target);
}

fn switch_to(state: &mut AppState, effects: &mut Vec<Effect>, target: usize) {
    if target == state.tabs.active {
        return;
    }
    let Some(mut slot) = state.tabs.slots[target].take() else {
        return;
    };
    prepare_to_leave(state, effects);
    slot.swap_with(state);
    let from = state.tabs.active;
    state.tabs.slots[from] = Some(slot);
    state.tabs.active = target;
    resume(state, effects);
}

/// `ctrl-w`: close the active tab. The last tab cannot be closed (`q` quits).
pub(crate) fn close(state: &mut AppState, effects: &mut Vec<Effect>) {
    if state.tabs.count() == 1 {
        set_status(
            state,
            Level::Info,
            "This is the last tab (q quits)".into(),
            effects,
        );
        return;
    }
    let at = state.tabs.active;
    prepare_to_leave(state, effects);
    state.tabs.slots.remove(at);
    // The tab to the right takes its place; closing the last one shows its left neighbour.
    let next = at.min(state.tabs.slots.len() - 1);
    let Some(mut slot) = state.tabs.slots[next].take() else {
        return;
    };
    // The closed tab's contents leave with the swap and are dropped.
    slot.swap_with(state);
    state.tabs.active = next;
    resume(state, effects);
}

#[cfg(test)]
mod tests {
    use std::path::Path;

    use super::*;
    use crate::entry::Entry;
    use crate::msg::Msg;
    use crate::testworld::project_world;

    #[test]
    fn goto_without_a_number_explains_itself() {
        let mut w = project_world();
        let mut effects = Vec::new();
        goto(&mut w.state, &mut effects, None);
        assert_eq!(w.status(), "Press 1-9 to pick a tab");
    }

    #[test]
    fn switching_away_mid_navigation_cancels_it_without_a_visit() {
        let mut w = project_world();
        w.key("t");
        let visits = |w: &crate::testworld::World| {
            w.state
                .aids
                .frecency
                .get(Path::new("/home/me/projects"))
                .map(|e| e.rank)
        };
        w.hold = true;
        w.key("j l"); // Enter on projects: the listing is in flight
        assert!(w.state.is_navigating());
        w.key("1");
        let held = std::mem::take(&mut w.held);
        w.hold = false;
        for msg in held {
            w.send(msg);
        }
        w.key("2");
        assert_eq!(w.cwd(), Path::new("/home/me"), "the move was cancelled");
        assert_eq!(visits(&w), None, "no visit was recorded");
    }

    #[test]
    fn a_new_tab_opens_at_the_current_directory() {
        let mut w = project_world();
        w.key("G");
        assert_eq!(w.cursor(), "notes.txt");
        w.key("t");
        assert_eq!(w.state.tabs.count(), 2);
        assert_eq!(w.state.tabs.active_index(), 1);
        assert_eq!(w.cwd(), Path::new("/home/me"));
        assert_eq!(w.names(), ["Downloads", "projects", "notes.txt"]);
        assert_eq!(w.cursor(), "notes.txt", "the cursor is carried over");
        assert!(!w.state.view.loading);
    }

    #[test]
    fn tabs_navigate_independently() {
        let mut w = project_world();
        w.key("t j l");
        assert_eq!(w.cwd(), Path::new("/home/me/projects"));
        w.key("1");
        assert_eq!(w.state.tabs.active_index(), 0);
        assert_eq!(w.cwd(), Path::new("/home/me"));
        assert_eq!(w.cursor(), "Downloads");
        // History is per tab: tab 1 has none, tab 2 has one step.
        w.key("ctrl-o");
        assert!(w.status().contains("oldest"), "{}", w.status());
        w.key("2");
        assert_eq!(w.cwd(), Path::new("/home/me/projects"));
        w.key("ctrl-o");
        assert_eq!(w.cwd(), Path::new("/home/me"));
        // Cursor memory is per tab, too.
        assert_eq!(w.cursor(), "projects");
        w.key("l j j");
        assert_eq!(w.cursor(), "todo.md");
        w.key("1 l");
        assert_eq!(w.cwd(), Path::new("/home/me/Downloads"));
    }

    #[test]
    fn switching_back_refreshes_the_tab_and_rearms_watcher_and_git() {
        let mut w = project_world();
        w.key("j"); // cursor on projects
        w.key("t");
        w.dir(
            "/home/me",
            vec![
                Entry::dir("projects"),
                Entry::dir("Downloads"),
                Entry::file("new.txt", 1),
            ],
        );
        w.log.clear();
        w.key("1");
        assert_eq!(w.names(), ["Downloads", "projects", "new.txt"]);
        assert_eq!(
            w.cursor(),
            "projects",
            "the cursor stays on its entry by name"
        );
        assert_eq!(w.count(|e| matches!(e, Effect::ListDir { .. })), 1);
        assert_eq!(w.count(|e| matches!(e, Effect::WatchDir { .. })), 1);
        assert_eq!(w.count(|e| matches!(e, Effect::GitStatus { .. })), 1);
    }

    #[test]
    fn results_for_a_parked_tab_are_dropped() {
        let mut w = project_world();
        w.key("t");
        // Tab 2 starts a navigation whose results are still in flight ...
        w.hold = true;
        w.key("j l");
        assert!(w.state.is_navigating());
        assert!(!w.held.is_empty());
        // ... and the user switches back to tab 1 before they arrive.
        w.key("1");
        let before = w.names();
        let held = std::mem::take(&mut w.held);
        w.hold = false;
        // The switch itself listed tab 1 again; deliver the old results first.
        let (old, fresh): (Vec<_>, Vec<_>) = held.into_iter().partition(|m| match m {
            Msg::ListingChunk { entries, .. } => entries.iter().any(|e| e.name == "alpha"),
            _ => false,
        });
        for msg in old {
            w.send(msg);
        }
        assert_eq!(w.names(), before, "a chunk of tab 2 must not touch tab 1");
        assert_eq!(w.cwd(), Path::new("/home/me"));
        for msg in fresh {
            w.send(msg);
        }
        assert_eq!(w.names(), before);
        // Tab 2 never moved: its pending navigation was cancelled.
        w.key("2");
        assert_eq!(w.cwd(), Path::new("/home/me"));
        assert!(w.names().contains(&"projects".to_owned()));
    }

    #[test]
    fn late_chunks_of_the_tab_that_was_left_do_not_leak_after_returning() {
        let mut w = project_world();
        w.key("t");
        w.hold = true;
        w.key("j l");
        let stale = std::mem::take(&mut w.held);
        w.hold = false;
        w.key("1 2");
        // Back in tab 2: results of the abandoned navigation arrive now.
        for msg in stale {
            w.send(msg);
        }
        assert_eq!(w.cwd(), Path::new("/home/me"));
        assert_eq!(w.names(), ["Downloads", "projects", "notes.txt"]);
    }

    #[test]
    fn closing_a_tab_shows_its_right_neighbour_or_the_left_one() {
        let mut w = project_world();
        w.key("t t"); // tabs: 1 2 3, active 3
        assert_eq!(w.state.tabs.count(), 3);
        w.key("2 j l"); // tab 2 -> projects
        w.key("3 j l j l"); // tab 3 -> projects/beta
        assert_eq!(w.cwd(), Path::new("/home/me/projects/beta"));

        w.key("ctrl-w"); // close 3 (last): shows tab 2
        assert_eq!(w.state.tabs.count(), 2);
        assert_eq!(w.state.tabs.active_index(), 1);
        assert_eq!(w.cwd(), Path::new("/home/me/projects"));

        w.key("1 ctrl-w"); // close 1: tab 2 takes its place
        assert_eq!(w.state.tabs.count(), 1);
        assert_eq!(w.state.tabs.active_index(), 0);
        assert_eq!(w.cwd(), Path::new("/home/me/projects"));
    }

    #[test]
    fn the_last_tab_cannot_be_closed() {
        let mut w = project_world();
        w.key("ctrl-w");
        assert_eq!(w.state.tabs.count(), 1);
        assert!(w.status().contains("last tab"), "{}", w.status());
        assert!(w.state.quit.is_none());
        assert_eq!(w.names(), ["Downloads", "projects", "notes.txt"]);
    }

    #[test]
    fn switching_to_a_missing_tab_or_the_same_tab_is_harmless() {
        let mut w = project_world();
        w.key("5");
        assert!(w.status().contains("No tab 5"), "{}", w.status());
        w.log.clear();
        w.key("1");
        assert!(w.log.is_empty(), "switching to the active tab does nothing");
        assert_eq!(w.state.tabs.active_index(), 0);
    }

    #[test]
    fn there_is_a_tab_limit() {
        let mut w = project_world();
        for _ in 0..MAX_TABS + 2 {
            w.key("t");
        }
        assert_eq!(w.state.tabs.count(), MAX_TABS);
        assert!(w.status().contains("At most"), "{}", w.status());
    }

    #[test]
    fn marks_and_visual_mode_do_not_follow_to_another_tab() {
        let mut w = project_world();
        w.key("space space");
        w.key("v");
        let marked = w.state.view.marks_count();
        assert_eq!(w.state.mode, Mode::Visual);
        w.key("t");
        assert_eq!(w.state.view.marks_count(), 0);
        assert_eq!(w.state.mode, Mode::Normal);
        w.key("1");
        assert_eq!(
            w.state.view.marks_count(),
            marked,
            "marks stay with their tab"
        );
        assert_eq!(w.state.mode, Mode::Normal);
    }

    #[test]
    fn a_tab_in_the_drives_view_round_trips() {
        let mut w = project_world();
        w.key("g /");
        assert!(w.state.show_drives);
        w.key("t");
        assert!(w.state.show_drives, "the new tab opens on This PC too");
        w.key("1");
        assert!(w.state.show_drives);
        w.key("2 l");
        assert!(!w.state.show_drives);
        w.key("1");
        assert!(w.state.show_drives, "tab 1 is still on This PC");
    }

    #[test]
    fn a_tab_keeps_its_own_sort_and_hidden_setting_for_new_tabs() {
        let mut w = project_world();
        w.key(".");
        let shown = w.state.view.show_hidden;
        w.key("t");
        assert_eq!(w.state.view.show_hidden, shown);
    }
}
