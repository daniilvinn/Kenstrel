//! Content search (`ctrl-f`): the overlay's state and its transitions.
//!
//! Two fields (query, glob), regex and whole-word toggles, a 120 ms debounce, and the results
//! the engine streams in, grouped by file. Every engine event carries the search generation;
//! anything that is not the current generation is dropped.

use std::time::Duration;

use ks_search::{FileMatches, GrepEvent, GrepStats};

use super::geometry;
use super::input::{Edit, LineInput};
use crate::effect::Effect;
use crate::entry::{Entry, EntryKind};
use crate::generation::Gen;
use crate::key::KeyChord;
use crate::mode::Overlay;
use crate::msg::TimerKind;
use crate::nav;
use crate::state::{AppState, Size};

/// The quiet period before a changed query starts a search.
pub const DEBOUNCE: Duration = Duration::from_millis(120);

/// Which input field has the cursor.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum GrepField {
    Query,
    Glob,
}

/// One display line of the results: a file header or one matching line.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct GrepRow {
    /// Index into [`GrepState::files`].
    pub file: u32,
    /// Index into that file's lines, or `None` for the file's header.
    pub line: Option<u32>,
}

#[derive(Clone, Debug)]
pub struct GrepState {
    pub query: LineInput,
    pub glob: LineInput,
    pub field: GrepField,
    pub regex: bool,
    pub whole_word: bool,
    /// The directory searched.
    pub root: std::path::PathBuf,
    /// Results grouped by file, in arrival order (sorted by path once the search is done).
    pub files: Vec<FileMatches>,
    /// The display lines for `files` (headers and matches).
    pub rows: Vec<GrepRow>,
    pub stats: GrepStats,
    /// A search is running.
    pub running: bool,
    /// The query changed and the debounce has not fired yet (results are out of date).
    pub waiting: bool,
    /// More matches existed than the engine reports.
    pub capped: bool,
    /// The engine could not start the search (bad regex or glob).
    pub error: Option<String>,
    /// Index into `rows` of the highlighted match.
    pub selected: usize,
    /// Index into `rows` of the first visible display line.
    pub scroll: usize,
    /// Marked hits (path, line number) in marking order; `alt-c` sends them all.
    pub marks: Vec<(std::path::PathBuf, u64)>,
    pub(crate) gen: u64,
}

impl GrepState {
    /// Whether the hit on `line_number` of `path` is marked.
    pub fn is_marked(&self, path: &std::path::Path, line_number: u64) -> bool {
        self.marks
            .iter()
            .any(|(p, l)| p == path && *l == line_number)
    }

    /// The highlighted match: its file and line.
    pub fn current(&self) -> Option<(&FileMatches, &ks_search::LineMatch)> {
        let row = self.rows.get(self.selected)?;
        let file = self.files.get(row.file as usize)?;
        let line = file.lines.get(row.line? as usize)?;
        Some((file, line))
    }

    /// Generation of the current (or last started) search.
    pub fn generation(&self) -> u64 {
        self.gen
    }

    pub fn match_count(&self) -> usize {
        self.files.iter().map(|f| f.lines.len()).sum()
    }

    fn viewport(size: Size) -> usize {
        usize::from(geometry::grep_rows(size)).max(1)
    }

    fn push_files(&mut self, files: Vec<FileMatches>) {
        for file in files {
            if file.lines.is_empty() {
                continue;
            }
            let index = u32::try_from(self.files.len()).unwrap_or(u32::MAX);
            self.rows.push(GrepRow {
                file: index,
                line: None,
            });
            for line in 0..file.lines.len() {
                self.rows.push(GrepRow {
                    file: index,
                    line: Some(u32::try_from(line).unwrap_or(u32::MAX)),
                });
            }
            self.files.push(file);
        }
    }

    fn rebuild_rows(&mut self) {
        let files = std::mem::take(&mut self.files);
        self.rows.clear();
        self.push_files(files);
    }

    /// Moves the selection to the first match when nothing valid is selected.
    fn ensure_selection(&mut self) {
        if self
            .rows
            .get(self.selected)
            .is_some_and(|r| r.line.is_some())
        {
            return;
        }
        self.selected = self.rows.iter().position(|r| r.line.is_some()).unwrap_or(0);
    }

    fn clear_results(&mut self) {
        self.files.clear();
        self.marks.clear();
        self.rows.clear();
        self.stats = GrepStats::default();
        self.capped = false;
        self.error = None;
        self.selected = 0;
        self.scroll = 0;
    }

    /// Scrolls so the highlighted line (and its header, when it fits) is on screen.
    fn keep_visible(&mut self, viewport: usize) {
        if self.selected < self.scroll {
            self.scroll = self.selected;
        } else if self.selected >= self.scroll + viewport {
            self.scroll = self.selected + 1 - viewport;
        }
        // Prefer showing the file header above the first line of a file.
        if let Some(row) = self.rows.get(self.selected) {
            if row.line == Some(0) && self.selected >= 1 && self.selected - 1 < self.scroll {
                self.scroll = self.selected - 1;
            }
        }
    }
}

pub(crate) fn on_resize(state: &mut AppState) {
    let viewport = GrepState::viewport(state.size);
    if let Some(g) = state.search.grep.as_mut() {
        g.keep_visible(viewport);
    }
}

pub(crate) fn open(state: &mut AppState, effects: &mut Vec<Effect>) {
    if matches!(state.overlays.last(), Some(Overlay::Grep)) {
        return;
    }
    let (query, glob) = state.search.last_grep.clone().unwrap_or_default();
    let grep = GrepState {
        query: LineInput::new(query),
        glob: LineInput::new(glob),
        field: GrepField::Query,
        regex: state.search.grep_regex,
        whole_word: state.search.grep_word,
        root: state.cwd.clone(),
        files: Vec::new(),
        rows: Vec::new(),
        stats: GrepStats::default(),
        running: false,
        waiting: false,
        capped: false,
        error: None,
        selected: 0,
        scroll: 0,
        marks: Vec::new(),
        gen: 0,
    };
    let has_query = !grep.query.is_empty();
    state.search.grep = Some(grep);
    state.overlays.push(Overlay::Grep);
    state.dirty = true;
    if has_query {
        schedule(state, effects);
    }
}

pub(crate) fn close(state: &mut AppState, effects: &mut Vec<Effect>) {
    if matches!(state.overlays.last(), Some(Overlay::Grep)) {
        state.overlays.pop();
    }
    if let Some(g) = state.search.grep.take() {
        state.search.last_grep = Some((g.query.text().to_owned(), g.glob.text().to_owned()));
        state.search.grep_regex = g.regex;
        state.search.grep_word = g.whole_word;
    }
    state.search.grep_gen += 1;
    state.search.grep_timer.bump();
    effects.push(Effect::CancelTimer(TimerKind::GrepDebounce));
    effects.push(Effect::GrepCancel);
    state.dirty = true;
}

/// The query, glob or a toggle changed: cancel the running search, drop its late events and
/// start the debounce.
fn schedule(state: &mut AppState, effects: &mut Vec<Effect>) {
    state.search.grep_gen += 1;
    let timer = state.search.grep_timer.bump();
    let Some(g) = state.search.grep.as_mut() else {
        return;
    };
    g.gen = state.search.grep_gen;
    g.running = false;
    // Without search text nothing will run: no "searching…" and no stale hits meanwhile.
    g.waiting = !g.query.is_empty();
    if !g.waiting {
        g.clear_results();
    }
    effects.push(Effect::GrepCancel);
    effects.push(Effect::StartTimer {
        kind: TimerKind::GrepDebounce,
        gen: timer,
        delay: DEBOUNCE,
    });
    state.dirty = true;
}

/// The debounce elapsed: start the search for the current fields.
pub(crate) fn on_timer(state: &mut AppState, effects: &mut Vec<Effect>, gen: Gen) {
    if !state.search.grep_timer.is_current(gen) {
        return;
    }
    state.search.grep_gen += 1;
    let search_gen = state.search.grep_gen;
    // Follows the pane's runtime `.` toggle; ignore files are respected unless the config's
    // `[files] show_ignored` says otherwise (the find-file `alt-g` toggle is not shared).
    let (show_hidden, respect_gitignore) =
        (state.view.show_hidden, !state.config.files.show_ignored);
    let Some(g) = state.search.grep.as_mut() else {
        return;
    };
    g.waiting = false;
    g.gen = search_gen;
    g.clear_results();
    state.dirty = true;
    if g.query.is_empty() {
        g.running = false;
        return;
    }
    g.running = true;
    effects.push(Effect::GrepStart {
        gen: search_gen,
        root: g.root.clone(),
        pattern: g.query.text().to_owned(),
        regex: g.regex,
        whole_word: g.whole_word,
        glob: g.glob.text().to_owned(),
        show_hidden,
        respect_gitignore,
    });
}

/// An engine event; events of other generations are dropped.
pub(crate) fn on_event(state: &mut AppState, event: GrepEvent) {
    let viewport = GrepState::viewport(state.size);
    let Some(g) = state.search.grep.as_mut() else {
        return;
    };
    if event.generation() != g.gen {
        return;
    }
    match event {
        GrepEvent::Matches { files, .. } => {
            g.push_files(files);
            g.ensure_selection();
        }
        GrepEvent::Progress { stats, .. } => g.stats = stats,
        GrepEvent::Done { stats, capped, .. } => {
            g.stats = stats;
            g.capped = capped;
            g.running = false;
            sort_files(g);
        }
        GrepEvent::Cancelled { stats, .. } => {
            g.stats = stats;
            g.running = false;
        }
        GrepEvent::Error { message, .. } => {
            g.error = Some(message);
            g.running = false;
        }
    }
    g.keep_visible(viewport);
    state.dirty = true;
}

/// Orders the finished results by path, keeping the highlighted match highlighted.
fn sort_files(g: &mut GrepState) {
    let selected = g
        .current()
        .map(|(file, line)| (file.path.clone(), line.line_number));
    g.files.sort_by(|a, b| a.rel.cmp(&b.rel));
    g.rebuild_rows();
    g.selected = selected
        .and_then(|(path, number)| {
            g.rows.iter().position(|row| {
                row.line.is_some_and(|l| {
                    let file = &g.files[row.file as usize];
                    file.path == path && file.lines[l as usize].line_number == number
                })
            })
        })
        .unwrap_or(0);
    g.ensure_selection();
}

/// A key the keymap did not bind: text editing in the active field.
pub(crate) fn on_text_key(state: &mut AppState, effects: &mut Vec<Effect>, key: KeyChord) -> bool {
    let Some(g) = state.search.grep.as_mut() else {
        return false;
    };
    let field = match g.field {
        GrepField::Query => &mut g.query,
        GrepField::Glob => &mut g.glob,
    };
    match field.apply(key) {
        Edit::Ignored => false,
        Edit::Moved => true,
        Edit::Changed => {
            schedule(state, effects);
            true
        }
    }
}

pub(crate) fn toggle_regex(state: &mut AppState, effects: &mut Vec<Effect>) {
    if let Some(g) = state.search.grep.as_mut() {
        g.regex = !g.regex;
        state.search.grep_regex = g.regex;
        schedule(state, effects);
    }
}

pub(crate) fn toggle_word(state: &mut AppState, effects: &mut Vec<Effect>) {
    if let Some(g) = state.search.grep.as_mut() {
        g.whole_word = !g.whole_word;
        state.search.grep_word = g.whole_word;
        schedule(state, effects);
    }
}

pub(crate) fn next_field(state: &mut AppState) {
    if let Some(g) = state.search.grep.as_mut() {
        g.field = match g.field {
            GrepField::Query => GrepField::Glob,
            GrepField::Glob => GrepField::Query,
        };
        state.dirty = true;
    }
}

pub(crate) fn move_selection(state: &mut AppState, delta: isize) {
    let viewport = GrepState::viewport(state.size);
    let Some(g) = state.search.grep.as_mut() else {
        return;
    };
    // Headers are skipped: step over rows until a match line is found.
    let mut index = g.selected;
    let mut remaining = delta.unsigned_abs();
    while remaining > 0 {
        let next = if delta > 0 {
            index + 1
        } else {
            match index.checked_sub(1) {
                Some(i) => i,
                None => break,
            }
        };
        match g.rows.get(next) {
            None => break,
            Some(row) => {
                index = next;
                if row.line.is_some() {
                    remaining -= 1;
                }
            }
        }
    }
    // Landing on a header (only possible at the edges) stays where we were.
    if g.rows.get(index).is_some_and(|r| r.line.is_some()) {
        g.selected = index;
    }
    g.keep_visible(viewport);
    state.dirty = true;
}

pub(crate) fn half_page(state: &mut AppState, down: bool) {
    let step = isize::try_from(GrepState::viewport(state.size) / 2)
        .unwrap_or(1)
        .max(1);
    move_selection(state, if down { step } else { -step });
}

/// `enter`: open the highlighted match in the editor at its line.
pub(crate) fn accept(state: &mut AppState, effects: &mut Vec<Effect>) {
    let Some(g) = state.search.grep.as_ref() else {
        return;
    };
    if let Some((file, line)) = g.current() {
        effects.push(Effect::OpenInEditor {
            path: file.path.clone(),
            line: u32::try_from(line.line_number).ok(),
        });
    }
}

/// `alt-m`: toggles the mark of the highlighted hit and moves down.
pub(crate) fn toggle_mark(state: &mut AppState, _effects: &mut Vec<Effect>) {
    if let Some(g) = state.search.grep.as_mut() {
        if let Some((path, line)) = g.current().map(|(f, l)| (f.path.clone(), l.line_number)) {
            if let Some(at) = g.marks.iter().position(|(p, l)| *p == path && *l == line) {
                g.marks.remove(at);
            } else {
                g.marks.push((path, line));
            }
        }
    }
    move_selection(state, 1);
}

/// `alt-c`: send the marked hits (or, with none, the highlighted hit) to the Claude pane as
/// `@path#L<n>` mentions. Marks whose hit is no longer in the results are skipped.
pub(crate) fn send_claude(state: &mut AppState, effects: &mut Vec<Effect>) {
    let Some(g) = state.search.grep.as_ref() else {
        return;
    };
    let mut hits: Vec<(std::path::PathBuf, u64)> = g
        .marks
        .iter()
        .filter(|(p, l)| {
            g.files
                .iter()
                .any(|f| f.path == *p && f.lines.iter().any(|m| m.line_number == *l))
        })
        .cloned()
        .collect();
    if hits.is_empty() {
        hits.extend(g.current().map(|(f, l)| (f.path.clone(), l.line_number)));
    }
    if hits.is_empty() {
        return;
    }
    if !state.claude.is_running() {
        crate::claude::not_running(state, effects);
        return;
    }
    close(state, effects);
    for (path, line) in hits {
        let line = usize::try_from(line).unwrap_or(usize::MAX);
        crate::claude::send_range(state, path, line, line, effects);
    }
}

/// `ctrl-l`: show the files with matches as a listing in the current pane.
pub(crate) fn to_list(state: &mut AppState, effects: &mut Vec<Effect>) {
    let Some(g) = state.search.grep.as_ref() else {
        return;
    };
    if g.files.is_empty() {
        return;
    }
    let root = g.root.clone();
    let partial = g.running || g.waiting || g.capped;
    let mut label = format!("Search results: {}", g.query.text());
    if partial {
        label.push_str(" (partial)");
    }
    let entries: Vec<Entry> = g
        .files
        .iter()
        .map(|f| Entry::new(f.rel.as_os_str().to_owned(), EntryKind::File))
        .collect();
    close(state, effects);
    nav::show_results_list(state, effects, root, label, entries);
    if partial {
        crate::update::set_status(
            state,
            crate::state::Level::Warning,
            "Search was not finished: the results list is partial".into(),
            effects,
        );
    }
}
