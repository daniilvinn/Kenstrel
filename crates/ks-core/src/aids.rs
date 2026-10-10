//! Navigation aids: bookmarks (`m`, `'`), the bookmark list, and the frecency jump (`z`).
//!
//! Everything here is pure state and rules; reading and writing the state files is done by
//! the effect runner ([`Effect::LoadNavData`], [`Effect::WriteState`]).

use std::path::{Path, PathBuf};
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use nucleo_matcher::pattern::{CaseMatching, Normalization, Pattern};
use nucleo_matcher::{Config as MatcherConfig, Matcher, Utf32Str};

use crate::bookmarks::Bookmarks;
use crate::effect::Effect;
use crate::frecency::Frecency;
use crate::generation::{Gen, GenCounter};
use crate::key::{Key, KeyChord, Modifiers};
use crate::keymap::Resolution;
use crate::mode::{KeyContext, Overlay};
use crate::msg::TimerKind;
use crate::nav::{self, Location, NavKind};
use crate::state::{AppState, Level};
use crate::update::set_status;

/// How long after the first unsaved visit the frecency database is written (visits in
/// between are batched into the same write).
pub const FRECENCY_SAVE_DELAY: Duration = Duration::from_secs(5);
/// Rows kept in the jump overlay's result list.
pub const JUMP_RESULT_LIMIT: usize = 100;

/// The persisted files below the state directory.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum StateFile {
    /// `bookmarks.toml`
    Bookmarks,
    /// `frecency.toml`
    Frecency,
}

impl StateFile {
    pub const fn file_name(self) -> &'static str {
        match self {
            StateFile::Bookmarks => "bookmarks.toml",
            StateFile::Frecency => "frecency.toml",
        }
    }
}

/// What the state directory held at start-up.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct LoadedNavData {
    pub bookmarks: Bookmarks,
    pub frecency: Frecency,
    /// One line per file that could not be read or parsed (it was set aside, not lost).
    pub problems: Vec<String>,
    /// Files that exist but could not be read (locked, say). They are left untouched: nothing
    /// is written over them this session.
    pub unreadable: Vec<StateFile>,
}

// `Frecency` holds only finite floats (the parser rejects the rest), so equality is total.
impl Eq for LoadedNavData {}

/// One row of the jump overlay.
#[derive(Clone, Debug, PartialEq)]
pub struct JumpHit {
    pub path: PathBuf,
    /// The path as shown: forward slashes, `~` for the home directory.
    pub label: String,
    /// Character positions of `label` matched by the query.
    pub matched: Vec<u32>,
    pub score: f64,
}

/// The `z` overlay: a query and its ranked results.
#[derive(Clone, Debug, Default)]
pub struct JumpState {
    pub input: String,
    pub hits: Vec<JumpHit>,
    pub cursor: usize,
    /// Directories in the database (shown as the total next to the hit count).
    pub total: usize,
}

#[derive(Default)]
pub struct NavAids {
    pub bookmarks: Bookmarks,
    pub frecency: Frecency,
    /// Selected row of the bookmark list.
    pub bookmark_cursor: usize,
    pub jump: JumpState,
    frecency_dirty: bool,
    save_armed: bool,
    save_gen: GenCounter,
    /// The start-up load has been merged. Until then nothing is written, so the in-memory
    /// stub (one visit, one bookmark) can never replace the user's real files.
    loaded: bool,
    /// Files that could not be read: never written, so the intact original survives.
    write_blocked: Vec<StateFile>,
    /// Bookmarks changed before the load finished; written once it has.
    bookmarks_unsaved: bool,
    /// Bookmarks deleted before the load finished (must not return from disk).
    removed_early: Vec<char>,
    /// A new tab re-lists the directory it was opened in; that is not a visit.
    skip_visit: Option<PathBuf>,
}

impl NavAids {
    fn may_write(&self, file: StateFile) -> bool {
        self.loaded && !self.write_blocked.contains(&file)
    }

    /// Test helper: behave as if the start-up load had already finished.
    /// The next commit of `path` is a tab being opened there, not a visit.
    pub(crate) fn skip_next_visit(&mut self, path: PathBuf) {
        self.skip_visit = Some(path);
    }

    #[cfg(test)]
    pub(crate) fn mark_loaded(&mut self) {
        self.loaded = true;
    }
}

fn unix_secs(time: SystemTime) -> u64 {
    time.duration_since(UNIX_EPOCH).map_or(0, |d| d.as_secs())
}

fn top_overlay(state: &AppState) -> Option<Overlay> {
    state.overlays.last().copied()
}

/// True while the topmost overlay is one of this module's.
pub(crate) fn owns_overlay(state: &AppState) -> bool {
    matches!(top_overlay(state), Some(Overlay::Bookmarks | Overlay::Jump))
}

// ----- frecency bookkeeping -----

/// A directory became the displayed location.
pub(crate) fn record_visit(state: &mut AppState, effects: &mut Vec<Effect>, path: &Path) {
    if state.aids.skip_visit.take().is_some_and(|p| p == path) {
        return;
    }
    let now = unix_secs(state.now);
    state.aids.frecency.visit(path, now);
    mark_frecency_dirty(state, effects);
}

/// A directory turned out not to exist any more: forget it.
pub(crate) fn forget_dir(state: &mut AppState, effects: &mut Vec<Effect>, path: &Path) {
    if state.aids.frecency.remove(path) {
        mark_frecency_dirty(state, effects);
    }
}

fn mark_frecency_dirty(state: &mut AppState, effects: &mut Vec<Effect>) {
    state.aids.frecency_dirty = true;
    if !state.aids.save_armed {
        state.aids.save_armed = true;
        effects.push(Effect::StartTimer {
            kind: TimerKind::StateSave,
            gen: state.aids.save_gen.bump(),
            delay: FRECENCY_SAVE_DELAY,
        });
    }
}

/// The debounce timer fired: write the database if it changed.
pub(crate) fn on_save_timer(state: &mut AppState, gen: Gen, effects: &mut Vec<Effect>) {
    if state.aids.save_gen.is_current(gen) {
        effects.extend(pending_writes(state));
    }
}

/// Writes still owed to the disk. The application runs this at shutdown so the last visits
/// are not lost to the debounce.
pub fn pending_writes(state: &mut AppState) -> Vec<Effect> {
    state.aids.save_armed = false;
    state.aids.save_gen.bump();
    if !state.aids.may_write(StateFile::Frecency) {
        // Keep the dirty flag: `on_loaded` re-arms the save once the real data is merged.
        return Vec::new();
    }
    if !std::mem::take(&mut state.aids.frecency_dirty) {
        return Vec::new();
    }
    vec![
        Effect::CancelTimer(TimerKind::StateSave),
        Effect::WriteState {
            file: StateFile::Frecency,
            contents: state.aids.frecency.to_toml(),
        },
    ]
}

fn write_bookmarks(state: &mut AppState, effects: &mut Vec<Effect>) {
    if !state.aids.may_write(StateFile::Bookmarks) {
        state.aids.bookmarks_unsaved = true;
        return;
    }
    effects.push(Effect::WriteState {
        file: StateFile::Bookmarks,
        contents: state.aids.bookmarks.to_toml(),
    });
}

/// `Effect::LoadNavData` finished.
pub(crate) fn on_loaded(state: &mut AppState, effects: &mut Vec<Effect>, loaded: LoadedNavData) {
    let mut disk_bookmarks = loaded.bookmarks;
    for key in std::mem::take(&mut state.aids.removed_early) {
        disk_bookmarks.remove(key);
    }
    state.aids.bookmarks.merge_loaded(disk_bookmarks);
    let had_visits = state.aids.frecency_dirty;
    state.aids.frecency.merge_loaded(loaded.frecency);
    state.aids.write_blocked = loaded.unreadable;
    state.aids.loaded = true;
    if std::mem::take(&mut state.aids.bookmarks_unsaved) {
        write_bookmarks(state, effects);
    }
    // What was visited before the load finished is written together with the loaded rest.
    if had_visits {
        mark_frecency_dirty(state, effects);
    }
    if let Some(problem) = loaded.problems.into_iter().next() {
        set_status(state, Level::Warning, problem, effects);
    }
    // An open overlay shows data that just changed.
    match top_overlay(state) {
        Some(Overlay::Jump) => recompute_jump(state),
        Some(Overlay::Bookmarks) => clamp_bookmark_cursor(state),
        _ => {}
    }
    state.dirty = true;
}

// ----- bookmarks -----

/// `m<char>`: bookmark the current directory.
pub(crate) fn set_bookmark(state: &mut AppState, effects: &mut Vec<Effect>, key: Option<char>) {
    let Some(key) = key else {
        return await_key(state, effects, 'm');
    };
    if state.show_drives {
        set_status(
            state,
            Level::Warning,
            "This PC cannot be bookmarked".into(),
            effects,
        );
        return;
    }
    if state.cwd.to_str().is_none() {
        let text = "This directory's name is not valid Unicode and cannot be bookmarked".into();
        set_status(state, Level::Error, text, effects);
        return;
    }
    let shown = display_path(&state.cwd, state.home.as_deref());
    let replaced = state.aids.bookmarks.set(key, state.cwd.clone());
    write_bookmarks(state, effects);
    let text = match replaced {
        Some(old) if old != state.cwd => format!("Bookmark {key} moved to {shown}"),
        _ => format!("Bookmark {key} set to {shown}"),
    };
    set_status(state, Level::Info, text, effects);
}

/// `'<char>`: go to a bookmark. A directory that no longer exists is reported by the
/// navigation itself ("Cannot open ...") and leaves the view alone.
pub(crate) fn jump_bookmark(state: &mut AppState, effects: &mut Vec<Effect>, key: Option<char>) {
    let Some(key) = key else {
        return await_key(state, effects, '\'');
    };
    match state.aids.bookmarks.get(key).map(Path::to_path_buf) {
        Some(path) => go_to(state, effects, path),
        None => set_status(state, Level::Warning, format!("No bookmark {key}"), effects),
    }
}

/// Invoked without its character (from the palette): wait for the key, as if the prefix had
/// been typed.
fn await_key(state: &mut AppState, effects: &mut Vec<Effect>, prefix: char) {
    let chord = KeyChord::char(prefix);
    if state
        .keymap
        .resolve(KeyContext::Normal, &[], chord)
        .eq(&Resolution::Pending)
    {
        state.pending.push(chord);
        state.dirty = true;
    } else {
        set_status(
            state,
            Level::Info,
            format!("Press {prefix} followed by a character"),
            effects,
        );
    }
}

pub(crate) fn open_bookmarks(state: &mut AppState) {
    state.aids.bookmark_cursor = 0;
    show_overlay(state, Overlay::Bookmarks);
}

fn clamp_bookmark_cursor(state: &mut AppState) {
    let last = state.aids.bookmarks.len().saturating_sub(1);
    state.aids.bookmark_cursor = state.aids.bookmark_cursor.min(last);
}

/// `d` in the bookmark list.
pub(crate) fn delete_bookmark(state: &mut AppState, effects: &mut Vec<Effect>) {
    if top_overlay(state) != Some(Overlay::Bookmarks) {
        return;
    }
    let Some((key, _)) = state.aids.bookmarks.nth(state.aids.bookmark_cursor) else {
        return;
    };
    state.aids.bookmarks.remove(key);
    if !state.aids.loaded {
        state.aids.removed_early.push(key);
    }
    clamp_bookmark_cursor(state);
    write_bookmarks(state, effects);
    set_status(
        state,
        Level::Info,
        format!("Bookmark {key} deleted"),
        effects,
    );
}

// ----- the frecency jump -----

pub(crate) fn open_jump(state: &mut AppState) {
    state.aids.jump = JumpState::default();
    show_overlay(state, Overlay::Jump);
    recompute_jump(state);
}

fn show_overlay(state: &mut AppState, overlay: Overlay) {
    state.overlays.retain(|o| *o != overlay);
    state.overlays.push(overlay);
    state.dirty = true;
}

/// `/`-separated, `~` for the home directory.
pub fn display_path(path: &Path, home: Option<&Path>) -> String {
    let text = path.to_string_lossy().replace('\\', "/");
    let Some(home) = home else {
        return text;
    };
    let home = home.to_string_lossy().replace('\\', "/");
    let home = home.trim_end_matches('/');
    if home.is_empty() {
        return text;
    }
    let fold = |s: &str| {
        if cfg!(windows) {
            s.to_lowercase()
        } else {
            s.to_owned()
        }
    };
    let (t, h) = (fold(&text), fold(home));
    if t == h {
        "~".to_owned()
    } else if t.starts_with(&h) && t.as_bytes().get(h.len()) == Some(&b'/') {
        // `fold` only changes ASCII case or keeps lengths for the paths seen in practice;
        // guard the slice against the exotic case of a length change.
        match text.get(home.len()..) {
            Some(rest) => format!("~{rest}"),
            None => text,
        }
    } else {
        text
    }
}

fn same_dir(a: &Path, b: &Path) -> bool {
    let norm = |p: &Path| {
        let s = p.to_string_lossy().replace('\\', "/");
        let s = s.trim_end_matches('/').to_owned();
        if cfg!(windows) {
            s.to_lowercase()
        } else {
            s
        }
    };
    norm(a) == norm(b)
}

/// Re-ranks the database for the current query: score = frecency x fuzzy match score.
pub(crate) fn recompute_jump(state: &mut AppState) {
    let now = unix_secs(state.now);
    let query = state.aids.jump.input.clone();
    let mut matcher = Matcher::new(MatcherConfig::DEFAULT.match_paths());
    let pattern = Pattern::parse(&query, CaseMatching::Smart, Normalization::Smart);
    let mut buf = Vec::new();

    let mut hits: Vec<JumpHit> = Vec::new();
    let ranked = state.aids.frecency.ranked(now);
    let total = ranked.len();
    for (entry, frecency) in ranked {
        let path = PathBuf::from(&entry.path);
        if !state.show_drives && same_dir(&path, &state.cwd) {
            continue;
        }
        let label = display_path(&path, state.home.as_deref());
        let score = if query.trim().is_empty() {
            frecency
        } else {
            let Some(matched) = pattern.score(Utf32Str::new(&label, &mut buf), &mut matcher) else {
                continue;
            };
            frecency * f64::from(matched)
        };
        hits.push(JumpHit {
            path,
            label,
            matched: Vec::new(),
            score,
        });
    }
    hits.sort_by(|a, b| {
        b.score
            .total_cmp(&a.score)
            .then_with(|| a.label.cmp(&b.label))
    });
    hits.truncate(JUMP_RESULT_LIMIT);
    if !query.trim().is_empty() {
        for hit in &mut hits {
            let mut indices = Vec::new();
            let haystack = Utf32Str::new(&hit.label, &mut buf);
            pattern.indices(haystack, &mut matcher, &mut indices);
            indices.sort_unstable();
            indices.dedup();
            hit.matched = indices;
        }
    }
    let jump = &mut state.aids.jump;
    jump.hits = hits;
    jump.total = total;
    jump.cursor = jump.cursor.min(jump.hits.len().saturating_sub(1));
    state.dirty = true;
}

/// Text editing in the jump overlay: keys the keymap does not bind. Returns whether the key
/// was consumed.
pub(crate) fn on_unbound_key(state: &mut AppState, key: KeyChord) -> bool {
    if top_overlay(state) != Some(Overlay::Jump) {
        return false;
    }
    let input = &mut state.aids.jump.input;
    let changed = match (key.key, key.mods) {
        (Key::Char(c), m) if m.is_empty() && !c.is_control() => {
            input.push(c);
            true
        }
        (Key::Backspace, m) if m.is_empty() => input.pop().is_some(),
        (Key::Char('u'), Modifiers::CTRL) => {
            let had = !input.is_empty();
            input.clear();
            had
        }
        (Key::Char('w'), Modifiers::CTRL) => {
            let trimmed = input.trim_end().len();
            let cut = input[..trimmed]
                .rfind([' ', '/', '\\'])
                .map_or(0, |i| i + 1);
            let changed = cut < input.len();
            input.truncate(cut);
            changed
        }
        _ => return false,
    };
    if changed {
        state.aids.jump.cursor = 0;
        recompute_jump(state);
    }
    true
}

/// `down` / `up` inside this module's overlays.
pub(crate) fn move_selection(state: &mut AppState, delta: isize) {
    let (cursor, len) = match top_overlay(state) {
        Some(Overlay::Bookmarks) => (&mut state.aids.bookmark_cursor, state.aids.bookmarks.len()),
        Some(Overlay::Jump) => (&mut state.aids.jump.cursor, state.aids.jump.hits.len()),
        _ => return,
    };
    if len == 0 {
        return;
    }
    *cursor = cursor.saturating_add_signed(delta).min(len - 1);
    state.dirty = true;
}

/// `enter` inside this module's overlays: jump to the selected directory.
pub(crate) fn accept(state: &mut AppState, effects: &mut Vec<Effect>) {
    let target = match top_overlay(state) {
        Some(Overlay::Bookmarks) => state
            .aids
            .bookmarks
            .nth(state.aids.bookmark_cursor)
            .map(|(_, path)| path.to_path_buf()),
        Some(Overlay::Jump) => state
            .aids
            .jump
            .hits
            .get(state.aids.jump.cursor)
            .map(|hit| hit.path.clone()),
        _ => return,
    };
    if let Some(path) = target {
        state.overlays.pop();
        go_to(state, effects, path);
    }
}

fn go_to(state: &mut AppState, effects: &mut Vec<Effect>, path: PathBuf) {
    nav::request(state, effects, Location::Dir(path), NavKind::Visit, None);
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn home_is_abbreviated() {
        let home = Path::new("/home/me");
        assert_eq!(display_path(Path::new("/home/me"), Some(home)), "~");
        assert_eq!(
            display_path(Path::new("/home/me/code"), Some(home)),
            "~/code"
        );
        assert_eq!(
            display_path(Path::new("/home/mexico"), Some(home)),
            "/home/mexico"
        );
        assert_eq!(display_path(Path::new("C:\\a\\b"), None), "C:/a/b");
    }
}

#[cfg(test)]
mod world_tests {
    use super::*;
    use crate::entry::Entry;
    use crate::msg::Msg;
    use crate::testworld::{p, project_world};
    use crate::update::update;

    fn hit_labels(w: &crate::testworld::World) -> Vec<String> {
        w.state
            .aids
            .jump
            .hits
            .iter()
            .map(|h| h.label.clone())
            .collect()
    }

    #[test]
    fn m_sets_a_bookmark_and_persists_it() {
        let mut w = project_world();
        w.key("m a");
        assert_eq!(w.state.aids.bookmarks.get('a'), Some(Path::new("/home/me")));
        assert_eq!(w.status(), "Bookmark a set to ~");
        let text = w.last_write(StateFile::Bookmarks).expect("a write");
        assert_eq!(Bookmarks::from_toml(&text).unwrap(), w.state.aids.bookmarks);
    }

    #[test]
    fn the_captured_character_reaches_the_handler() {
        let mut w = project_world();
        w.key("j l"); // projects
        w.key("m Z");
        assert_eq!(
            w.state.aids.bookmarks.get('Z'),
            Some(Path::new("/home/me/projects"))
        );
        assert_eq!(w.state.aids.bookmarks.get('z'), None);
        w.key("g h");
        assert_eq!(w.cwd(), Path::new("/home/me"));
        w.key("' Z");
        assert_eq!(w.cwd(), Path::new("/home/me/projects"));
    }

    #[test]
    fn jumping_to_unknown_or_vanished_bookmarks_reports_an_error() {
        let mut w = project_world();
        w.key("' q");
        assert_eq!(w.status(), "No bookmark q");
        assert_eq!(w.cwd(), Path::new("/home/me"));

        w.state.aids.bookmarks.set('x', p("/gone/dir"));
        w.key("' x");
        assert!(
            w.status().starts_with("Cannot open /gone/dir"),
            "{}",
            w.status()
        );
        assert_eq!(w.cwd(), Path::new("/home/me"), "the view is left alone");
        assert_eq!(w.names(), ["Downloads", "projects", "notes.txt"]);
    }

    #[test]
    fn re_bookmarking_says_moved() {
        let mut w = project_world();
        w.key("m a j l m a");
        assert_eq!(w.status(), "Bookmark a moved to ~/projects");
    }

    #[test]
    fn this_pc_cannot_be_bookmarked() {
        let mut w = project_world();
        w.key("g / m a");
        assert!(w.state.aids.bookmarks.is_empty());
        assert!(w.status().contains("This PC"), "{}", w.status());
    }

    #[test]
    fn invoking_without_a_character_waits_for_one() {
        let mut w = project_world();
        let mut effects = Vec::new();
        set_bookmark(&mut w.state, &mut effects, None);
        assert_eq!(w.state.pending, [KeyChord::char('m')]);
        w.key("b");
        assert_eq!(w.state.aids.bookmarks.get('b'), Some(Path::new("/home/me")));
    }

    #[test]
    fn the_bookmark_list_jumps_and_deletes() {
        let mut w = project_world();
        w.state
            .aids
            .bookmarks
            .set('a', p("/home/me/projects/alpha"));
        w.state.aids.bookmarks.set('b', p("/home/me/projects/beta"));
        w.key("M");
        assert_eq!(w.state.overlays, [Overlay::Bookmarks]);
        assert_eq!(w.state.key_context(), KeyContext::Bookmarks);
        w.key("j");
        assert_eq!(w.state.aids.bookmark_cursor, 1);
        w.key("j");
        assert_eq!(w.state.aids.bookmark_cursor, 1, "clamped");
        w.key("d");
        assert_eq!(w.state.aids.bookmarks.len(), 1);
        assert_eq!(w.state.aids.bookmark_cursor, 0);
        let text = w.last_write(StateFile::Bookmarks).unwrap();
        assert!(!text.contains("beta"), "{text}");
        w.key("enter");
        assert!(w.state.overlays.is_empty());
        assert_eq!(w.cwd(), Path::new("/home/me/projects/alpha"));
    }

    #[test]
    fn an_empty_bookmark_list_is_harmless() {
        let mut w = project_world();
        w.key("M d enter j");
        assert_eq!(w.state.overlays, [Overlay::Bookmarks]);
        w.key("esc");
        assert!(w.state.overlays.is_empty());
    }

    fn state_save_timers(w: &crate::testworld::World) -> usize {
        w.count(|e| {
            matches!(
                e,
                Effect::StartTimer {
                    kind: TimerKind::StateSave,
                    ..
                }
            )
        })
    }

    fn frecency_writes(w: &crate::testworld::World) -> usize {
        w.count(|e| {
            matches!(
                e,
                Effect::WriteState {
                    file: StateFile::Frecency,
                    ..
                }
            )
        })
    }

    #[test]
    fn visits_are_recorded_and_the_write_is_debounced() {
        let mut w = project_world();
        // The start directory counts as a visit and armed the timer once.
        let rank = |w: &crate::testworld::World, path: &str| {
            w.state.aids.frecency.get(Path::new(path)).unwrap().rank
        };
        assert_eq!(rank(&w, "/home/me"), 1.0);
        assert_eq!(state_save_timers(&w), 1);
        w.key("j l h");
        assert_eq!(rank(&w, "/home/me"), 2.0);
        assert_eq!(rank(&w, "/home/me/projects"), 1.0);
        assert_eq!(state_save_timers(&w), 1, "more visits keep the debounce");
        assert_eq!(frecency_writes(&w), 0);

        let gen = w.state.aids.save_gen.current();
        w.send(Msg::Timer {
            kind: TimerKind::StateSave,
            gen,
        });
        let text = w
            .last_write(StateFile::Frecency)
            .expect("written when the timer fires");
        assert_eq!(Frecency::from_toml(&text).unwrap(), w.state.aids.frecency);

        // Nothing changed since: the same timer writes nothing; a new visit re-arms.
        w.send(Msg::Timer {
            kind: TimerKind::StateSave,
            gen,
        });
        assert_eq!(frecency_writes(&w), 1);
        w.key("k l");
        assert_eq!(state_save_timers(&w), 2);
    }

    #[test]
    fn pending_writes_flush_the_last_visits_at_shutdown() {
        let mut w = project_world();
        let effects = pending_writes(&mut w.state);
        assert!(effects.iter().any(|e| matches!(
            e,
            Effect::WriteState {
                file: StateFile::Frecency,
                ..
            }
        )));
        assert!(
            pending_writes(&mut w.state).is_empty(),
            "nothing left to write"
        );
    }

    fn visit(w: &mut crate::testworld::World, path: &str, times: u32) {
        for _ in 0..times {
            w.state.aids.frecency.visit(Path::new(path), 0);
        }
    }

    #[test]
    fn z_ranks_by_frecency_and_leaves_out_the_current_directory() {
        let mut w = project_world();
        visit(&mut w, "/home/me/projects/alpha", 5);
        visit(&mut w, "/home/me/projects/beta", 2);
        visit(&mut w, "/home/me/Downloads", 9);
        w.key("z");
        assert_eq!(w.state.overlays, [Overlay::Jump]);
        assert_eq!(
            hit_labels(&w),
            ["~/Downloads", "~/projects/alpha", "~/projects/beta"]
        );
        assert_eq!(w.state.aids.jump.total, 4);
    }

    #[test]
    fn typing_filters_by_fuzzy_match_and_keys_are_text() {
        let mut w = project_world();
        visit(&mut w, "/home/me/projects/alpha", 5);
        visit(&mut w, "/home/me/projects/beta", 2);
        visit(&mut w, "/home/me/Downloads", 9);
        w.key("z");
        w.key("p r o");
        assert_eq!(w.state.aids.jump.input, "pro");
        assert_eq!(hit_labels(&w), ["~/projects/alpha", "~/projects/beta"]);
        assert!(!w.state.aids.jump.hits[0].matched.is_empty());
        w.key("backspace backspace backspace");
        assert_eq!(hit_labels(&w).len(), 3);
        w.key("x y z");
        assert!(hit_labels(&w).is_empty());
        w.key("ctrl-u");
        assert_eq!(hit_labels(&w).len(), 3);
        w.key("b e t ctrl-w");
        assert_eq!(w.state.aids.jump.input, "");
    }

    #[test]
    fn a_much_better_match_beats_a_more_frequent_directory() {
        let mut w = project_world();
        visit(&mut w, "/home/me/Downloads", 3);
        visit(&mut w, "/home/me/projects/downtown", 3);
        w.key("z");
        w.key("d o w n t");
        assert_eq!(hit_labels(&w)[0], "~/projects/downtown");
    }

    #[test]
    fn enter_jumps_and_esc_closes() {
        let mut w = project_world();
        visit(&mut w, "/home/me/projects/alpha", 5);
        visit(&mut w, "/home/me/projects/beta", 2);
        w.key("z down enter");
        assert!(w.state.overlays.is_empty());
        assert_eq!(w.cwd(), Path::new("/home/me/projects/beta"));
        w.key("z");
        w.key("esc");
        assert!(w.state.overlays.is_empty());
        assert_eq!(w.cwd(), Path::new("/home/me/projects/beta"));
        // ctrl-n / ctrl-p move the selection.
        w.key("z ctrl-n");
        assert_eq!(w.state.aids.jump.cursor, 1);
        w.key("ctrl-p");
        assert_eq!(w.state.aids.jump.cursor, 0);
    }

    #[test]
    fn jumping_to_a_vanished_directory_prunes_it() {
        let mut w = project_world();
        visit(&mut w, "/gone/dir", 50);
        w.key("z enter");
        assert!(
            w.status().starts_with("Cannot open /gone/dir"),
            "{}",
            w.status()
        );
        assert!(w.state.aids.frecency.get(Path::new("/gone/dir")).is_none());
        assert_eq!(w.cwd(), Path::new("/home/me"));
    }

    #[test]
    fn an_empty_database_gives_an_empty_overlay() {
        let mut w = project_world();
        w.state.aids.frecency = Frecency::default();
        w.key("z enter");
        assert_eq!(w.state.overlays, [Overlay::Jump]);
        assert_eq!(w.cwd(), Path::new("/home/me"));
    }

    #[test]
    fn loaded_data_is_merged_with_what_happened_meanwhile() {
        let mut w = project_world();
        w.state.aids.loaded = false;
        w.state.aids.bookmarks.set('a', p("/new"));
        let mut loaded = LoadedNavData::default();
        loaded.bookmarks.set('a', p("/old"));
        loaded.bookmarks.set('b', p("/b"));
        loaded.frecency.visit(Path::new("/home/me"), 0);
        loaded.frecency.visit(Path::new("/elsewhere"), 0);
        loaded.problems.push("bookmarks.toml is damaged".into());
        w.key("z");
        w.send(Msg::NavDataLoaded(Box::new(loaded)));
        assert_eq!(w.state.aids.bookmarks.get('a'), Some(Path::new("/new")));
        assert_eq!(w.state.aids.bookmarks.get('b'), Some(Path::new("/b")));
        assert_eq!(
            w.state
                .aids
                .frecency
                .get(Path::new("/home/me"))
                .unwrap()
                .rank,
            2.0
        );
        assert_eq!(w.status(), "bookmarks.toml is damaged");
        // The open overlay shows the freshly loaded directory.
        assert_eq!(hit_labels(&w), ["/elsewhere"]);
        // What was visited before the load finished is saved together with the loaded data.
        assert!(state_save_timers(&w) >= 1);
    }

    #[test]
    fn opening_a_tab_is_not_a_visit() {
        let mut w = project_world();
        let rank = |w: &crate::testworld::World| {
            w.state
                .aids
                .frecency
                .get(Path::new("/home/me"))
                .unwrap()
                .rank
        };
        assert_eq!(rank(&w), 1.0);
        w.key("t");
        assert_eq!(rank(&w), 1.0);
        w.key("j l h");
        assert_eq!(rank(&w), 2.0, "real navigation still counts");
    }

    #[test]
    fn nothing_is_written_before_the_load_has_been_merged() {
        let mut w = project_world();
        w.state.aids.loaded = false;
        // Quitting early: the one in-memory visit must not replace the real database.
        assert!(pending_writes(&mut w.state).is_empty());
        w.key("m a");
        assert!(w.last_write(StateFile::Bookmarks).is_none());
        // After the load both are written, together with what the disk held.
        let mut loaded = LoadedNavData::default();
        loaded.bookmarks.set('b', p("/b"));
        loaded.frecency.visit(Path::new("/elsewhere"), 0);
        w.send(Msg::NavDataLoaded(Box::new(loaded)));
        let text = w.last_write(StateFile::Bookmarks).expect("bookmarks saved");
        let saved = Bookmarks::from_toml(&text).unwrap();
        assert!(saved.get('a').is_some() && saved.get('b').is_some());
        let effects = pending_writes(&mut w.state);
        let Some(Effect::WriteState { contents, .. }) = effects.iter().find(|e| {
            matches!(
                e,
                Effect::WriteState {
                    file: StateFile::Frecency,
                    ..
                }
            )
        }) else {
            panic!("the frecency write is still owed");
        };
        let saved = Frecency::from_toml(contents).unwrap();
        assert!(saved.get(Path::new("/elsewhere")).is_some());
    }

    #[test]
    fn unreadable_files_are_never_written_over() {
        let mut w = project_world();
        w.state.aids.loaded = false;
        let loaded = LoadedNavData {
            unreadable: vec![StateFile::Bookmarks, StateFile::Frecency],
            ..LoadedNavData::default()
        };
        w.send(Msg::NavDataLoaded(Box::new(loaded)));
        w.key("m a");
        assert!(w.last_write(StateFile::Bookmarks).is_none());
        assert!(pending_writes(&mut w.state).is_empty());
    }

    #[test]
    fn bookmarks_deleted_before_the_load_do_not_come_back() {
        let mut w = project_world();
        w.state.aids.loaded = false;
        w.state.aids.bookmarks.set('a', p("/a"));
        w.key("M d");
        assert!(w.state.aids.bookmarks.is_empty());
        let mut loaded = LoadedNavData::default();
        loaded.bookmarks.set('a', p("/a"));
        loaded.bookmarks.set('b', p("/b"));
        w.send(Msg::NavDataLoaded(Box::new(loaded)));
        assert_eq!(w.state.aids.bookmarks.get('a'), None);
        assert!(w.state.aids.bookmarks.get('b').is_some());
    }

    #[test]
    fn ctrl_w_in_the_jump_overlay_edits_the_query_and_keeps_the_tabs() {
        let mut w = project_world();
        w.key("t");
        assert_eq!(w.state.tabs.count(), 2);
        w.key("z a b");
        assert_eq!(w.state.aids.jump.input, "ab");
        w.key("ctrl-w");
        assert_eq!(w.state.aids.jump.input, "");
        assert_eq!(w.state.tabs.count(), 2);
        assert_eq!(w.state.overlays, [Overlay::Jump]);
    }

    /// `cargo test -p ks-core --release -- --ignored --nocapture z_ranking_time`
    #[test]
    #[ignore = "measurement"]
    #[allow(clippy::print_stderr)]
    fn z_ranking_time() {
        let mut w = project_world();
        for i in 0..crate::frecency::MAX_ENTRIES {
            let path = format!(
                "/home/me/projects/group{}/repo{i}/src/module{}",
                i % 50,
                i % 7
            );
            w.state
                .aids
                .frecency
                .visit(Path::new(&path), (i % 1000) as u64);
        }
        let started = std::time::Instant::now();
        w.key("z");
        let empty = started.elapsed();
        let started = std::time::Instant::now();
        w.key("r e p o 1 2");
        let typed = started.elapsed();
        eprintln!(
            "z with {} entries: open {empty:?}; six keystrokes {typed:?} ({:?} each)",
            w.state.aids.frecency.len(),
            typed / 6
        );
    }

    #[test]
    fn start_asks_for_the_persisted_data() {
        let mut state = project_world().state;
        let effects = update(&mut state, Msg::Start);
        assert!(effects.contains(&Effect::LoadNavData));
    }

    #[test]
    fn unrelated_unbound_keys_are_still_ignored() {
        let mut w = project_world();
        w.log.clear();
        w.key("ctrl-z");
        assert!(w.log.is_empty());
        let _ = Entry::file("unused", 1);
    }
}
