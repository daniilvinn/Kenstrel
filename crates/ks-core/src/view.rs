//! The directory being browsed: entries, what is visible, the cursor, marks and visual mode.
//!
//! The entry list is kept sorted. Which entries are shown (the hidden-file filter) is a list
//! of indices, so toggling it never copies entries. Positions (cursor, visual anchor) are
//! tracked by entry name across every change that reorders the list, so a refresh, a sort
//! change or a streamed chunk never makes the cursor jump to a different file.

use std::collections::{HashMap, HashSet};
use std::ffi::{OsStr, OsString};

use crate::config::{SortConfig, SortKey};
use crate::cursor::Cursor;
use crate::entry::{compare, sort_entries, Entry};
use crate::generation::Gen;

#[derive(Clone, Debug)]
struct Visual {
    /// Index (into the visible list) where visual mode started.
    anchor: usize,
    anchor_name: OsString,
    /// Marks that existed before visual mode; the range is added on top of them.
    base: HashSet<OsString>,
}

#[derive(Clone, Debug)]
pub struct DirView {
    pub sort: SortConfig,
    pub show_hidden: bool,
    /// Sorted.
    entries: Vec<Entry>,
    /// Indices into `entries` of the rows that are shown.
    visible: Vec<u32>,
    /// Matched character indices of each shown row (parallel to `visible`; empty without a
    /// filter).
    highlights: Vec<Vec<u32>>,
    /// Entries hidden by the hidden-file rule (not counting the filter).
    hidden: usize,
    /// The in-directory fuzzy filter (`/`).
    filter: Option<String>,
    /// Set for a virtual listing (search results): the header shows it instead of the path.
    pub label: Option<String>,
    pub cursor: Cursor,
    marks: HashSet<OsString>,
    /// More chunks of the listing are expected.
    pub loading: bool,
    /// The listing generation that owns this view's incoming chunks.
    pub gen: Gen,
    /// Entry the cursor should land on as soon as it appears in the listing.
    reveal: Option<OsString>,
    /// False for a fresh listing until the user (or a reveal) moves the cursor, so streamed
    /// chunks that sort before the first row do not drag the cursor along.
    follow: bool,
    visual: Option<Visual>,
}

impl DirView {
    pub fn new(sort: SortConfig, show_hidden: bool) -> Self {
        Self {
            sort,
            show_hidden,
            entries: Vec::new(),
            visible: Vec::new(),
            highlights: Vec::new(),
            hidden: 0,
            filter: None,
            label: None,
            cursor: Cursor::default(),
            marks: HashSet::new(),
            loading: false,
            gen: Gen::default(),
            reveal: None,
            follow: false,
            visual: None,
        }
    }

    // ----- reading -----

    /// Every entry, including hidden ones.
    pub fn entries(&self) -> &[Entry] {
        &self.entries
    }

    /// Number of rows shown.
    pub fn len(&self) -> usize {
        self.visible.len()
    }

    pub fn is_empty(&self) -> bool {
        self.visible.is_empty()
    }

    /// Number of entries hidden by the hidden-file filter.
    pub fn hidden_count(&self) -> usize {
        self.hidden
    }

    /// The `row`-th shown entry.
    pub fn row(&self, row: usize) -> Option<&Entry> {
        self.visible
            .get(row)
            .and_then(|&i| self.entries.get(i as usize))
    }

    /// The entry under the cursor.
    pub fn current(&self) -> Option<&Entry> {
        self.row(self.cursor.index)
    }

    pub fn marks_count(&self) -> usize {
        self.marks.len()
    }

    pub fn is_marked(&self, entry: &Entry) -> bool {
        !self.marks.is_empty() && self.marks.contains(&entry.name)
    }

    /// Names of the marked entries (unordered).
    pub fn marked_names(&self) -> impl Iterator<Item = &OsString> {
        self.marks.iter()
    }

    pub fn in_visual_mode(&self) -> bool {
        self.visual.is_some()
    }

    fn position_of(&self, name: &OsStr) -> Option<usize> {
        self.visible
            .iter()
            .position(|&i| self.entries[i as usize].name == name)
    }

    // ----- listing changes -----

    /// Asks for the cursor to land on `name` once it shows up in the listing.
    pub fn set_reveal(&mut self, name: Option<OsString>) {
        self.reveal = name;
    }

    /// Moves the cursor onto the shown entry called `name` right now (no pending reveal).
    /// Returns false when no shown entry has that name.
    pub fn reveal_now(&mut self, name: &OsStr, height: usize) -> bool {
        let Some(index) = self.position_of(name) else {
            return false;
        };
        self.cursor.set(index, self.visible.len());
        self.cursor.reveal(self.visible.len(), height);
        self.moved(height);
        true
    }

    /// When `name` exists in the listing but is hidden from the view, turns hidden entries on
    /// and puts the cursor on it. Returns whether it did.
    pub fn unhide_for(&mut self, name: &OsStr, height: usize) -> bool {
        if self.show_hidden || !self.entries.iter().any(|e| e.name == name && e.is_hidden()) {
            return false;
        }
        self.toggle_hidden(height);
        self.reveal_now(name, height);
        true
    }

    /// Adds a streamed chunk of a fresh listing.
    pub fn push_chunk(&mut self, mut chunk: Vec<Entry>, height: usize) {
        sort_entries(&mut chunk, &self.sort);
        self.reposition(height, |view| {
            view.entries.append(&mut chunk);
            // A stable sort merges the two sorted runs in linear time.
            sort_entries(&mut view.entries, &view.sort);
        });
    }

    /// Ends a streamed listing.
    /// Takes the pending reveal target (the listing is done or superseded).
    pub fn take_reveal(&mut self) -> Option<OsString> {
        self.reveal.take()
    }

    pub fn finish_loading(&mut self) {
        self.loading = false;
        self.reveal = None;
    }

    /// Swaps in a complete fresh listing of the same directory (watcher refresh). The cursor
    /// stays on the same name, marks of vanished entries are dropped and known child counts
    /// carry over for display. Returns the real directories whose count must be redone: new
    /// ones and those whose modification time changed (or is unknown), since a directory's
    /// mtime moves when entries are added to or removed from it.
    pub fn replace_entries(&mut self, mut fresh: Vec<Entry>, height: usize) -> Vec<OsString> {
        sort_entries(&mut fresh, &self.sort);
        let old: HashMap<&OsStr, (Option<u32>, Option<std::time::SystemTime>)> = self
            .entries
            .iter()
            .filter(|e| e.is_dir)
            .map(|e| (e.name.as_os_str(), (e.children, e.mtime)))
            .collect();
        let mut stale = Vec::new();
        for entry in fresh.iter_mut().filter(|e| e.is_dir) {
            let before = old.get(entry.name.as_os_str());
            if entry.children.is_none() {
                entry.children = before.and_then(|(count, _)| *count);
            }
            let unchanged = before.is_some_and(|(count, mtime)| {
                count.is_some() && mtime.is_some() && *mtime == entry.mtime
            });
            if !unchanged
                && entry.kind == crate::entry::EntryKind::Dir
                && !entry.is_cloud_placeholder()
            {
                stale.push(entry.name.clone());
            }
        }
        drop(old);
        if !self.marks.is_empty() {
            let present: HashSet<&OsStr> = fresh.iter().map(|e| e.name.as_os_str()).collect();
            self.marks.retain(|name| present.contains(name.as_os_str()));
        }
        self.loading = false;
        self.follow = true;
        self.reposition(height, |view| view.entries = fresh);
        stale
    }

    /// Drops the entries with these names (a results list after a file op removed them),
    /// keeping the cursor on the same name when it survives.
    pub fn remove_names(&mut self, names: &[OsString], height: usize) {
        if names.is_empty() {
            return;
        }
        self.marks.retain(|m| !names.contains(m));
        self.reposition(height, |view| {
            view.entries.retain(|e| !names.contains(&e.name));
        });
    }

    /// Renames entries of a results list in place (`old -> new` relative names): the row keeps
    /// its metadata under the new name. Rows that are not in the list are ignored.
    pub fn rename_names(&mut self, pairs: &[(OsString, OsString)], height: usize) {
        if pairs.is_empty() {
            return;
        }
        for (old, new) in pairs {
            if self.marks.remove(old) {
                self.marks.insert(new.clone());
            }
        }
        self.reposition(height, |view| {
            for (old, new) in pairs {
                if let Some(pos) = view.entries.iter().position(|e| &e.name == old) {
                    let old_entry = &view.entries[pos];
                    let mut fresh = Entry::new(new.clone(), old_entry.kind);
                    fresh.is_dir = old_entry.is_dir;
                    fresh.size = old_entry.size;
                    fresh.mtime = old_entry.mtime;
                    fresh.attrs = old_entry.attrs;
                    fresh.children = old_entry.children;
                    view.entries[pos] = fresh;
                }
            }
            sort_entries(&mut view.entries, &view.sort);
        });
    }

    /// Records lazily computed child counts.
    pub fn apply_child_counts(&mut self, counts: Vec<(OsString, u32)>) {
        let by_name: HashMap<OsString, u32> = counts.into_iter().collect();
        for entry in self.entries.iter_mut().filter(|e| e.is_dir) {
            if let Some(&count) = by_name.get(&entry.name) {
                entry.children = Some(count);
            }
        }
    }

    /// Fills in size and modification time of the entries named in `stats` (a virtual listing
    /// whose entries start without metadata) and re-sorts when the sort key needs them.
    pub fn apply_stats(
        &mut self,
        stats: Vec<(OsString, u64, Option<std::time::SystemTime>)>,
        height: usize,
    ) {
        let by_name: HashMap<OsString, (u64, Option<std::time::SystemTime>)> = stats
            .into_iter()
            .map(|(name, size, mtime)| (name, (size, mtime)))
            .collect();
        for entry in self.entries.iter_mut() {
            if let Some(&(size, mtime)) = by_name.get(&entry.name) {
                entry.size = size;
                entry.mtime = mtime;
            }
        }
        // Size and mtime sorts depend on the values that just arrived.
        if matches!(self.sort.key, SortKey::Size | SortKey::Mtime) {
            self.reposition(height, |view| {
                view.entries.sort_by(|a, b| compare(a, b, &view.sort));
            });
        }
    }

    /// Names of up to `limit` real directories (not links, not cloud placeholders) whose
    /// children have not been counted yet, in display order.
    pub fn dirs_to_count(&self, limit: usize) -> Vec<OsString> {
        self.entries
            .iter()
            .filter(|e| {
                e.kind == crate::entry::EntryKind::Dir
                    && e.children.is_none()
                    && !e.is_cloud_placeholder()
            })
            .take(limit)
            .map(|e| e.name.clone())
            .collect()
    }

    pub fn set_sort(&mut self, sort: SortConfig, height: usize) {
        if sort == self.sort {
            return;
        }
        self.follow = true;
        self.reposition(height, |view| {
            view.sort = sort;
            view.entries.sort_by(|a, b| compare(a, b, &view.sort));
        });
    }

    pub fn toggle_hidden(&mut self, height: usize) {
        self.follow = true;
        self.reposition(height, |view| view.show_hidden = !view.show_hidden);
    }

    /// Applies `change`, rebuilds the visible list and puts the cursor (and the visual
    /// anchor) back on the entries they were on.
    fn reposition(&mut self, height: usize, change: impl FnOnce(&mut Self)) {
        let cursor_name = self.current().map(|e| e.name.clone());
        let anchor_name = self.visual.as_ref().map(|v| v.anchor_name.clone());
        change(self);
        let by_attr: Vec<u32> = self
            .entries
            .iter()
            .enumerate()
            .filter(|(_, e)| self.show_hidden || !e.is_hidden())
            .map(|(i, _)| u32::try_from(i).unwrap_or(u32::MAX))
            .collect();
        self.hidden = self.entries.len() - by_attr.len();
        match self.filter.as_deref() {
            Some(query) => {
                let names: Vec<std::borrow::Cow<'_, str>> = by_attr
                    .iter()
                    .map(|&i| self.entries[i as usize].display_name())
                    .collect();
                let matches = ks_search::filter_list(&names, query);
                self.visible = matches.iter().map(|m| by_attr[m.index]).collect();
                self.highlights = matches.into_iter().map(|m| m.indices).collect();
            }
            None => {
                self.visible = by_attr;
                self.highlights.clear();
            }
        }

        let revealed = self
            .reveal
            .as_deref()
            .and_then(|name| self.position_of(name));
        if let Some(index) = revealed {
            self.reveal = None;
            self.follow = true;
            self.cursor.set(index, self.visible.len());
            self.cursor.reveal(self.visible.len(), height);
        } else if self.follow {
            let index = cursor_name
                .as_deref()
                .and_then(|name| self.position_of(name))
                .unwrap_or(self.cursor.index);
            self.cursor.set(index, self.visible.len());
            self.cursor.ensure_visible(self.visible.len(), height);
        } else {
            self.cursor.set(0, self.visible.len());
            self.cursor.offset = 0;
        }

        if self.visual.is_some() {
            let anchor = anchor_name.as_deref().and_then(|n| self.position_of(n));
            match anchor {
                Some(index) => {
                    if let Some(visual) = self.visual.as_mut() {
                        visual.anchor = index;
                    }
                    self.apply_visual();
                }
                None => self.visual = None,
            }
        }
    }

    // ----- the in-directory filter -----

    /// The active filter query.
    pub fn filter_query(&self) -> Option<&str> {
        self.filter.as_deref()
    }

    /// Matched character indices (into the display name) of the `row`-th shown entry; empty
    /// without a filter.
    pub fn highlight(&self, row: usize) -> &[u32] {
        self.highlights.get(row).map_or(&[], Vec::as_slice)
    }

    /// Sets or clears the filter. A new query puts the cursor on the best match; clearing it
    /// keeps the cursor on the entry it was on.
    pub fn set_filter(&mut self, query: Option<String>, height: usize) {
        if self.filter == query {
            return;
        }
        let narrowing = query.is_some();
        self.follow = true;
        self.reposition(height, |view| view.filter = query);
        if narrowing {
            self.cursor.set(0, self.visible.len());
            self.cursor.offset = 0;
        }
        self.moved(height);
    }

    /// Marks the entries called `names` (those that exist).
    pub fn mark_names(&mut self, names: &[OsString]) {
        self.exit_visual();
        let present: HashSet<&OsStr> = self.entries.iter().map(|e| e.name.as_os_str()).collect();
        self.marks.extend(
            names
                .iter()
                .filter(|n| present.contains(n.as_os_str()))
                .cloned(),
        );
    }

    // ----- cursor movement -----

    fn moved(&mut self, height: usize) {
        self.follow = true;
        self.reveal = None;
        self.cursor.ensure_visible(self.visible.len(), height);
        self.apply_visual();
    }

    pub fn move_cursor(&mut self, delta: isize, height: usize) {
        self.cursor.move_by(delta, self.visible.len());
        self.moved(height);
    }

    pub fn cursor_top(&mut self, height: usize) {
        self.cursor.set(0, self.visible.len());
        self.moved(height);
    }

    pub fn cursor_bottom(&mut self, height: usize) {
        self.cursor.set(usize::MAX, self.visible.len());
        self.moved(height);
    }

    /// Moves half a page (`down` or up) and scrolls with the cursor, like vim's ctrl-d/u.
    pub fn half_page(&mut self, down: bool, height: usize) {
        let step = isize::try_from((height / 2).max(1)).unwrap_or(1);
        let len = self.visible.len();
        let delta = if down { step } else { -step };
        self.cursor.move_by(delta, len);
        // The viewport moves by the same amount as the cursor.
        let max_offset = len.saturating_sub(height);
        self.cursor.offset = self
            .cursor
            .offset
            .saturating_add_signed(delta)
            .min(max_offset);
        self.moved(height);
    }

    /// Re-clamps scrolling after the viewport size changed.
    pub fn fit_viewport(&mut self, height: usize) {
        self.cursor.ensure_visible(self.visible.len(), height);
    }

    // ----- marks and visual mode -----

    /// Toggles the mark of the entry under the cursor and moves down one row.
    pub fn toggle_mark(&mut self, height: usize) {
        self.exit_visual();
        let Some(name) = self.current().map(|e| e.name.clone()) else {
            return;
        };
        if !self.marks.remove(&name) {
            self.marks.insert(name);
        }
        self.move_cursor(1, height);
    }

    /// Marks every shown entry, or clears the marks when they are all marked already.
    pub fn toggle_mark_all(&mut self) {
        self.exit_visual();
        let all_marked = !self.visible.is_empty()
            && self
                .visible
                .iter()
                .all(|&i| self.marks.contains(&self.entries[i as usize].name));
        if all_marked {
            self.marks.clear();
        } else {
            self.marks = self
                .visible
                .iter()
                .map(|&i| self.entries[i as usize].name.clone())
                .collect();
        }
    }

    pub fn clear_marks(&mut self) {
        self.exit_visual();
        self.marks.clear();
    }

    /// Starts visual mode at the cursor: the entry under it is marked and the selection
    /// follows the cursor from here on.
    pub fn enter_visual(&mut self) {
        let Some(name) = self.current().map(|e| e.name.clone()) else {
            return;
        };
        self.visual = Some(Visual {
            anchor: self.cursor.index,
            anchor_name: name,
            base: self.marks.clone(),
        });
        self.apply_visual();
    }

    /// Leaves visual mode; the marks stay applied.
    pub fn exit_visual(&mut self) {
        self.visual = None;
    }

    /// Marks = marks from before visual mode + the rows between the anchor and the cursor.
    fn apply_visual(&mut self) {
        let Some(visual) = &self.visual else { return };
        let (lo, hi) = if visual.anchor <= self.cursor.index {
            (visual.anchor, self.cursor.index)
        } else {
            (self.cursor.index, visual.anchor)
        };
        let mut marks = visual.base.clone();
        let end = hi.min(self.visible.len().saturating_sub(1));
        if let Some(rows) = self.visible.get(lo..=end) {
            marks.extend(rows.iter().map(|&i| self.entries[i as usize].name.clone()));
        }
        self.marks = marks;
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::entry::{Attrs, Entry};

    const H: usize = 10;

    fn view_with(names: &[&str]) -> DirView {
        let mut view = DirView::new(SortConfig::default(), false);
        view.push_chunk(names.iter().map(|n| Entry::file(*n, 1)).collect(), H);
        view.finish_loading();
        view
    }

    fn numbered(n: usize) -> DirView {
        let names: Vec<String> = (0..n).map(|i| format!("f{i:04}")).collect();
        let refs: Vec<&str> = names.iter().map(String::as_str).collect();
        view_with(&refs)
    }

    fn shown(view: &DirView) -> Vec<String> {
        (0..view.len())
            .map(|i| view.row(i).unwrap().display_name().into_owned())
            .collect()
    }

    fn cursor_name(view: &DirView) -> String {
        view.current().unwrap().display_name().into_owned()
    }

    #[test]
    fn arriving_stats_re_sort_by_size_and_keep_the_cursor_entry() {
        let mut view = DirView::new(
            SortConfig {
                key: crate::config::SortKey::Size,
                ..SortConfig::default()
            },
            false,
        );
        view.push_chunk(
            vec![
                Entry::file("a", 0),
                Entry::file("b", 0),
                Entry::file("c", 0),
            ],
            H,
        );
        view.finish_loading();
        view.move_cursor(2, H);
        assert_eq!(cursor_name(&view), "c");
        view.apply_stats(
            vec![
                ("a".into(), 30, None),
                ("b".into(), 10, None),
                ("c".into(), 20, None),
            ],
            H,
        );
        assert_eq!(shown(&view), ["a", "c", "b"]);
        assert_eq!(cursor_name(&view), "c");
    }

    #[test]
    fn chunks_merge_into_sorted_order() {
        let mut view = DirView::new(SortConfig::default(), false);
        view.push_chunk(vec![Entry::file("c", 1), Entry::file("a", 1)], H);
        view.push_chunk(vec![Entry::dir("z"), Entry::file("b", 1)], H);
        assert_eq!(shown(&view), ["z", "a", "b", "c"]);
    }

    #[test]
    fn fresh_listing_keeps_the_cursor_at_the_top_while_streaming() {
        let mut view = DirView::new(SortConfig::default(), false);
        view.push_chunk(vec![Entry::file("m", 1)], H);
        view.push_chunk(vec![Entry::file("a", 1)], H);
        assert_eq!(view.cursor.index, 0);
        assert_eq!(cursor_name(&view), "a");
    }

    #[test]
    fn cursor_follows_its_entry_once_the_user_moved() {
        let mut view = view_with(&["b", "c", "d"]);
        view.move_cursor(1, H);
        assert_eq!(cursor_name(&view), "c");
        view.push_chunk(vec![Entry::file("a", 1)], H);
        assert_eq!(cursor_name(&view), "c");
        assert_eq!(view.cursor.index, 2);
    }

    #[test]
    fn reveal_lands_on_the_named_entry_when_it_arrives() {
        let mut view = DirView::new(SortConfig::default(), false);
        view.loading = true;
        view.set_reveal(Some("m".into()));
        view.push_chunk(vec![Entry::file("a", 1), Entry::file("b", 1)], H);
        assert_eq!(cursor_name(&view), "a");
        view.push_chunk(vec![Entry::file("m", 1)], H);
        assert_eq!(cursor_name(&view), "m");
        // Later chunks no longer move it.
        view.push_chunk(vec![Entry::file("0", 1)], H);
        assert_eq!(cursor_name(&view), "m");
    }

    #[test]
    fn missing_reveal_target_is_dropped_when_loading_finishes() {
        let mut view = DirView::new(SortConfig::default(), false);
        view.set_reveal(Some("gone".into()));
        view.push_chunk(vec![Entry::file("a", 1)], H);
        view.finish_loading();
        view.push_chunk(vec![Entry::file("gone", 1)], H);
        assert_eq!(cursor_name(&view), "a");
    }

    #[test]
    fn hidden_entries_are_filtered_and_toggle_keeps_the_cursor() {
        let mut view = DirView::new(SortConfig::default(), false);
        view.push_chunk(
            vec![
                Entry::file(".env", 1),
                Entry::file("a", 1),
                Entry::file("sys", 1).with_attrs(Attrs::SYSTEM),
                Entry::file("b", 1),
            ],
            H,
        );
        assert_eq!(shown(&view), ["a", "b"]);
        assert_eq!(view.hidden_count(), 2);
        view.move_cursor(1, H);
        view.toggle_hidden(H);
        assert_eq!(shown(&view), [".env", "a", "b", "sys"]);
        assert_eq!(cursor_name(&view), "b");
        view.toggle_hidden(H);
        assert_eq!(shown(&view), ["a", "b"]);
        assert_eq!(cursor_name(&view), "b");
    }

    #[test]
    fn cursor_on_an_entry_that_becomes_hidden_stays_in_range() {
        let mut view = DirView::new(SortConfig::default(), true);
        view.push_chunk(vec![Entry::file(".a", 1), Entry::file("b", 1)], H);
        view.cursor.set(0, 2);
        view.follow = true;
        view.toggle_hidden(H);
        assert_eq!(shown(&view), ["b"]);
        assert_eq!(view.cursor.index, 0);
    }

    #[test]
    fn sort_change_keeps_the_cursor_on_the_same_entry() {
        let mut view = DirView::new(SortConfig::default(), false);
        view.push_chunk(
            vec![
                Entry::file("a", 1),
                Entry::file("b", 99),
                Entry::file("c", 50),
            ],
            H,
        );
        view.move_cursor(1, H);
        assert_eq!(cursor_name(&view), "b");
        view.set_sort(
            SortConfig {
                key: crate::config::SortKey::Size,
                ..SortConfig::default()
            },
            H,
        );
        assert_eq!(shown(&view), ["b", "c", "a"]);
        assert_eq!(cursor_name(&view), "b");
    }

    #[test]
    fn half_page_moves_cursor_and_viewport_together() {
        let mut view = numbered(100);
        view.half_page(true, H);
        assert_eq!(view.cursor.index, 5);
        view.half_page(true, H);
        assert_eq!(view.cursor.index, 10);
        assert!(view.cursor.offset >= 5);
        view.half_page(false, H);
        assert_eq!(view.cursor.index, 5);
        for _ in 0..40 {
            view.half_page(true, H);
        }
        assert_eq!(view.cursor.index, 99);
        assert_eq!(view.cursor.offset, 90);
    }

    #[test]
    fn top_and_bottom() {
        let mut view = numbered(30);
        view.cursor_bottom(H);
        assert_eq!(view.cursor.index, 29);
        assert_eq!(view.cursor.offset, 20);
        view.cursor_top(H);
        assert_eq!((view.cursor.index, view.cursor.offset), (0, 0));
    }

    #[test]
    fn empty_view_is_safe() {
        let mut view = view_with(&[]);
        view.move_cursor(1, H);
        view.cursor_bottom(H);
        view.half_page(true, H);
        view.toggle_mark(H);
        view.toggle_mark_all();
        view.enter_visual();
        assert!(view.current().is_none());
        assert_eq!(view.marks_count(), 0);
    }

    #[test]
    fn toggle_mark_marks_and_advances() {
        let mut view = view_with(&["a", "b", "c"]);
        view.toggle_mark(H);
        view.toggle_mark(H);
        assert_eq!(view.marks_count(), 2);
        assert_eq!(cursor_name(&view), "c");
        view.move_cursor(-1, H);
        view.move_cursor(-1, H);
        view.toggle_mark(H);
        assert_eq!(view.marks_count(), 1);
        assert!(!view.is_marked(view.row(0).unwrap()));
        assert!(view.is_marked(view.row(1).unwrap()));
    }

    #[test]
    fn mark_all_toggles_between_all_and_none() {
        let mut view = view_with(&["a", "b", ".h"]);
        view.toggle_mark_all();
        assert_eq!(view.marks_count(), 2, "hidden entries are not marked");
        view.toggle_mark_all();
        assert_eq!(view.marks_count(), 0);
    }

    #[test]
    fn visual_mode_extends_and_shrinks_with_the_cursor() {
        let mut view = view_with(&["a", "b", "c", "d", "e"]);
        view.move_cursor(1, H);
        view.enter_visual();
        assert_eq!(view.marks_count(), 1);
        view.move_cursor(2, H);
        assert_eq!(view.marks_count(), 3);
        view.move_cursor(-1, H);
        assert_eq!(view.marks_count(), 2);
        // Crossing over the anchor selects upwards.
        view.move_cursor(-2, H);
        assert_eq!(view.marks_count(), 2);
        assert!(view.is_marked(view.row(0).unwrap()));
        assert!(view.is_marked(view.row(1).unwrap()));
        view.exit_visual();
        view.move_cursor(3, H);
        assert_eq!(view.marks_count(), 2, "marks stay applied after leaving");
    }

    #[test]
    fn visual_range_adds_to_earlier_marks() {
        let mut view = view_with(&["a", "b", "c", "d", "e"]);
        view.cursor_bottom(H);
        view.toggle_mark(H);
        view.cursor_top(H);
        view.enter_visual();
        view.move_cursor(1, H);
        assert_eq!(view.marks_count(), 3);
        view.move_cursor(-1, H);
        assert_eq!(view.marks_count(), 2);
    }

    #[test]
    fn visual_anchor_survives_a_refresh() {
        let mut view = view_with(&["b", "c", "d"]);
        view.enter_visual();
        view.move_cursor(1, H);
        view.replace_entries(
            vec![
                Entry::file("a", 1),
                Entry::file("b", 1),
                Entry::file("c", 1),
                Entry::file("d", 1),
            ],
            H,
        );
        assert_eq!(view.marks_count(), 2);
        assert_eq!(cursor_name(&view), "c");
    }

    #[test]
    fn refresh_keeps_cursor_by_name_and_prunes_marks() {
        let mut view = view_with(&["a", "b", "c"]);
        view.toggle_mark(H);
        view.toggle_mark(H);
        view.move_cursor(-1, H);
        assert_eq!(cursor_name(&view), "b");
        view.replace_entries(
            vec![
                Entry::file("0", 1),
                Entry::file("b", 1),
                Entry::file("c", 1),
            ],
            H,
        );
        assert_eq!(cursor_name(&view), "b");
        assert_eq!(view.marks_count(), 1, "a vanished");
    }

    #[test]
    fn refresh_when_the_cursor_entry_vanished_clamps() {
        let mut view = view_with(&["a", "b", "c"]);
        view.cursor_bottom(H);
        view.replace_entries(vec![Entry::file("a", 1)], H);
        assert_eq!(view.cursor.index, 0);
        view.replace_entries(vec![], H);
        assert!(view.current().is_none());
    }

    #[test]
    fn child_counts_apply_and_survive_refresh() {
        let mut view = DirView::new(SortConfig::default(), false);
        view.push_chunk(vec![Entry::dir("d"), Entry::file("f", 1)], H);
        assert_eq!(view.dirs_to_count(10), [OsString::from("d")]);
        view.apply_child_counts(vec![("d".into(), 7), ("f".into(), 3)]);
        assert_eq!(view.row(0).unwrap().children, Some(7));
        assert_eq!(view.row(1).unwrap().children, None, "files have no count");
        assert!(view.dirs_to_count(10).is_empty());
        view.replace_entries(vec![Entry::dir("d")], H);
        assert_eq!(view.row(0).unwrap().children, Some(7));
    }

    #[test]
    fn links_and_cloud_dirs_are_not_counted() {
        let mut view = DirView::new(SortConfig::default(), false);
        view.push_chunk(
            vec![
                Entry::dir("real"),
                Entry::dir("cloud").with_attrs(Attrs::CLOUD),
                Entry::new("junction", crate::entry::EntryKind::Junction),
            ],
            H,
        );
        assert_eq!(view.dirs_to_count(10), [OsString::from("real")]);
        assert_eq!(view.dirs_to_count(0).len(), 0);
    }
}
