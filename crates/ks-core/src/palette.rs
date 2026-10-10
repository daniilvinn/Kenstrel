//! The command palette (`:`): state, fuzzy filtering and running the chosen command.
//!
//! The list is generated from the command registry and the live keymap each time the
//! palette opens, so rebinding keys is reflected immediately. Commands that need a theme or
//! icon-set choice open a second page of the same overlay.

use std::time::Duration;

use nucleo_matcher::pattern::{AtomKind, CaseMatching, Normalization, Pattern};
use nucleo_matcher::{Config as MatcherConfig, Matcher, Utf32Str};

use crate::action::Action;
use crate::command;
use crate::config::IconSet;
use crate::effect::Effect;
use crate::key::KeyChord;
use crate::keymap::{Invocation, KeyPattern, Keymap};
use crate::mode::{KeyContext, Overlay};
use crate::msg::TimerKind;
use crate::state::{AppState, Level, Size};
use crate::text_input::TextInput;
use crate::update;

/// How many recently run commands are remembered.
const RECENT_LIMIT: usize = 30;

/// Which list the palette is showing.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum Page {
    #[default]
    Commands,
    Themes,
    Icons,
}

impl Page {
    pub const fn title(self) -> &'static str {
        match self {
            Page::Commands => "Commands",
            Page::Themes => "Switch theme",
            Page::Icons => "Switch icon set",
        }
    }
}

/// What choosing an entry does.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum ItemKind {
    Command(Action, Option<char>),
    Theme(String),
    Icons(IconSet),
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Item {
    pub label: String,
    /// Current key bindings, shown right-aligned.
    pub keys: Vec<String>,
    pub kind: ItemKind,
    /// `true` for the entry that is currently in effect (theme and icon pages).
    pub current: bool,
}

/// A matching entry with the character positions to highlight.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Hit {
    pub item: usize,
    /// Char (not byte) positions in the label.
    pub indices: Vec<u32>,
}

#[derive(Clone, Debug, Default)]
pub struct PaletteState {
    pub input: TextInput,
    pub page: Page,
    pub items: Vec<Item>,
    pub hits: Vec<Hit>,
    pub selected: usize,
    pub scroll: usize,
    /// Most recent first. Survives closing the palette.
    recent: Vec<(Action, Option<char>)>,
}

impl PaletteState {
    pub fn selected_item(&self) -> Option<&Item> {
        self.hits.get(self.selected).map(|h| &self.items[h.item])
    }

    fn recent_rank(&self, kind: &ItemKind) -> Option<usize> {
        match kind {
            ItemKind::Command(action, arg) => {
                self.recent.iter().position(|r| r == &(*action, *arg))
            }
            _ => None,
        }
    }

    fn remember(&mut self, action: Action, arg: Option<char>) {
        self.recent.retain(|r| r != &(action, arg));
        self.recent.insert(0, (action, arg));
        self.recent.truncate(RECENT_LIMIT);
    }

    /// Recomputes `hits` for the current query and keeps the selection in range.
    pub fn refilter(&mut self, rows: usize) {
        let query = self.input.text().trim().to_owned();
        let mut hits: Vec<(Hit, u32, Option<usize>)> = Vec::new();
        if query.is_empty() {
            for (index, item) in self.items.iter().enumerate() {
                hits.push((
                    Hit {
                        item: index,
                        indices: Vec::new(),
                    },
                    0,
                    self.recent_rank(&item.kind),
                ));
            }
        } else {
            let pattern = Pattern::new(
                &query,
                CaseMatching::Smart,
                Normalization::Smart,
                AtomKind::Fuzzy,
            );
            let mut matcher = Matcher::new(MatcherConfig::DEFAULT);
            let mut buf = Vec::new();
            for (index, item) in self.items.iter().enumerate() {
                let mut indices = Vec::new();
                let Some(score) = pattern.indices(
                    Utf32Str::new(&item.label, &mut buf),
                    &mut matcher,
                    &mut indices,
                ) else {
                    // Fall back to the stable id (`file.trash`) so users can search by it;
                    // there is no label position to highlight then.
                    let id_score = match &item.kind {
                        ItemKind::Command(action, _) => pattern
                            .score(Utf32Str::new(action.id(), &mut buf), &mut matcher)
                            .map(|s| s / 2),
                        _ => None,
                    };
                    if let Some(score) = id_score {
                        hits.push((
                            Hit {
                                item: index,
                                indices: Vec::new(),
                            },
                            score,
                            self.recent_rank(&item.kind),
                        ));
                    }
                    continue;
                };
                indices.sort_unstable();
                indices.dedup();
                hits.push((
                    Hit {
                        item: index,
                        indices,
                    },
                    score,
                    self.recent_rank(&item.kind),
                ));
            }
        }
        // Empty query: recent commands first (most recent first), then registry order.
        // Query: best score first; equal scores prefer recently used commands.
        if query.is_empty() {
            hits.sort_by_key(|(hit, _, recent)| (recent.is_none(), recent.unwrap_or(0), hit.item));
        } else {
            hits.sort_by_key(|(hit, score, recent)| {
                (
                    std::cmp::Reverse(*score),
                    recent.is_none(),
                    recent.unwrap_or(0),
                    hit.item,
                )
            });
        }
        self.hits = hits.into_iter().map(|(hit, _, _)| hit).collect();
        self.selected = self.selected.min(self.hits.len().saturating_sub(1));
        self.keep_selection_visible(rows);
    }

    /// Re-clamps the scroll position after the terminal size changed.
    pub(crate) fn refit(&mut self, rows: usize) {
        self.keep_selection_visible(rows);
    }

    fn keep_selection_visible(&mut self, rows: usize) {
        let rows = rows.max(1);
        if self.selected < self.scroll {
            self.scroll = self.selected;
        } else if self.selected >= self.scroll + rows {
            self.scroll = self.selected + 1 - rows;
        }
        self.scroll = self.scroll.min(self.hits.len().saturating_sub(rows));
    }

    /// Moves the selection by `delta` rows, wrapping around at the ends.
    pub fn move_selection(&mut self, delta: isize, rows: usize) {
        let len = self.hits.len();
        if len == 0 {
            return;
        }
        let len = len as isize;
        self.selected = (self.selected as isize + delta).rem_euclid(len) as usize;
        self.keep_selection_visible(rows);
    }

    /// Moves without wrapping (paging).
    fn jump_selection(&mut self, delta: isize, rows: usize) {
        let last = self.hits.len().saturating_sub(1) as isize;
        self.selected = (self.selected as isize + delta).clamp(0, last) as usize;
        self.keep_selection_visible(rows);
    }
}

/// Outer size of the palette box: about 60 % of the width, at most 90 columns.
pub fn box_size(size: Size) -> (u16, u16) {
    let width = (size.width * 3 / 5).clamp(44, 90).min(size.width);
    let height = size.height.saturating_sub(4).clamp(8, 26).min(size.height);
    (width, height)
}

/// Result rows: the box minus two borders, the input line and its rule.
pub fn visible_rows(size: Size) -> usize {
    usize::from(box_size(size).1.saturating_sub(4))
}

/// Bindings of `action` with exactly this fixed argument in the normal keymap.
fn keys_of(keymap: &Keymap, action: Action, arg: Option<char>) -> Vec<String> {
    keymap
        .bindings(KeyContext::Normal)
        .iter()
        .filter(|b| b.action == action && b.arg == arg)
        .map(|b| b.keys_display())
        .collect()
}

/// All command entries. `Switch to tab` is expanded to one entry per tab number.
fn command_items(keymap: &Keymap) -> Vec<Item> {
    let mut items = Vec::new();
    for info in command::all() {
        if info.action == Action::TabGoto {
            for n in 1..=9u8 {
                let arg = char::from(b'0' + n);
                items.push(Item {
                    label: format!("{} {n}", info.title),
                    keys: keys_of(keymap, info.action, Some(arg)),
                    kind: ItemKind::Command(info.action, Some(arg)),
                    current: false,
                });
            }
            continue;
        }
        items.push(Item {
            label: info.title.to_owned(),
            keys: keys_of(keymap, info.action, None),
            kind: ItemKind::Command(info.action, None),
            current: false,
        });
    }
    items
}

fn page_items(state: &AppState, page: Page) -> Vec<Item> {
    match page {
        Page::Commands => command_items(&state.keymap),
        Page::Themes => state
            .themes
            .iter()
            .map(|name| Item {
                label: name.clone(),
                keys: Vec::new(),
                kind: ItemKind::Theme(name.clone()),
                current: *name == state.config.theme,
            })
            .collect(),
        Page::Icons => [
            (IconSet::Nerd, "nerd"),
            (IconSet::Unicode, "unicode"),
            (IconSet::Ascii, "ascii"),
        ]
        .into_iter()
        .map(|(set, label)| Item {
            label: label.to_owned(),
            keys: Vec::new(),
            kind: ItemKind::Icons(set),
            current: set == state.config.icons,
        })
        .collect(),
    }
}

fn show_page(state: &mut AppState, page: Page) {
    let rows = visible_rows(state.size);
    let items = page_items(state, page);
    let palette = &mut state.palette;
    palette.page = page;
    palette.items = items;
    palette.input.clear();
    palette.selected = 0;
    palette.scroll = 0;
    palette.refilter(rows);
    // Start the theme and icon pages on the active entry.
    if let Some(at) = palette
        .hits
        .iter()
        .position(|h| palette.items[h.item].current)
    {
        palette.selected = at;
        palette.keep_selection_visible(rows);
    }
    state.dirty = true;
}

/// Opens the palette on its command page.
pub fn open(state: &mut AppState) {
    if state.overlays.last() != Some(&Overlay::Palette) {
        state.overlays.push(Overlay::Palette);
    }
    show_page(state, Page::Commands);
}

/// Opens the palette directly on a sub-page (theme or icon set).
pub(crate) fn open_page(state: &mut AppState, page: Page) {
    if state.overlays.last() != Some(&Overlay::Palette) {
        state.overlays.push(Overlay::Palette);
    }
    show_page(state, page);
}

/// Handles an action while the palette is the top overlay. Returns false when the action is
/// not the palette's business.
pub(crate) fn on_action(
    state: &mut AppState,
    invocation: Invocation,
    effects: &mut Vec<Effect>,
) -> bool {
    let rows = visible_rows(state.size);
    match invocation.action {
        Action::CursorDown => state.palette.move_selection(1, rows),
        Action::CursorUp => state.palette.move_selection(-1, rows),
        Action::PageHalfDown => state
            .palette
            .jump_selection((rows / 2).max(1) as isize, rows),
        Action::PageHalfUp => state
            .palette
            .jump_selection(-((rows / 2).max(1) as isize), rows),
        Action::CursorTop => state.palette.jump_selection(isize::MIN / 2, rows),
        Action::CursorBottom => state.palette.jump_selection(isize::MAX / 2, rows),
        Action::Accept => accept(state, effects),
        Action::Cancel if state.palette.page != Page::Commands => show_page(state, Page::Commands),
        _ => return false,
    }
    state.dirty = true;
    true
}

/// Feeds an unbound key to the query field.
pub(crate) fn on_text_key(state: &mut AppState, chord: KeyChord) -> bool {
    match state.palette.input.handle_key(chord) {
        Some(changed) => {
            if changed {
                let rows = visible_rows(state.size);
                // A new query starts from the best match.
                state.palette.selected = 0;
                state.palette.refilter(rows);
            }
            state.dirty = true;
            true
        }
        None => false,
    }
}

fn accept(state: &mut AppState, effects: &mut Vec<Effect>) {
    let Some(item) = state.palette.selected_item().cloned() else {
        return;
    };
    match item.kind {
        ItemKind::Command(Action::ThemeSwitch, _) => show_page(state, Page::Themes),
        ItemKind::Command(Action::IconsSwitch, _) => show_page(state, Page::Icons),
        ItemKind::Command(action, arg) => {
            state.overlays.retain(|o| *o != Overlay::Palette);
            state.palette.remember(action, arg);
            run_command(state, Invocation { action, arg }, effects);
        }
        ItemKind::Theme(name) => {
            state.overlays.retain(|o| *o != Overlay::Palette);
            effects.push(Effect::SetTheme {
                name,
                announce: true,
            });
        }
        ItemKind::Icons(set) => {
            state.overlays.retain(|o| *o != Overlay::Palette);
            state.config.icons = set;
            let label = match set {
                IconSet::Nerd => "nerd",
                IconSet::Unicode => "unicode",
                IconSet::Ascii => "ascii",
            };
            update::set_status(state, Level::Info, format!("Icon set: {label}"), effects);
        }
    }
}

/// Runs a command picked from the palette. Commands that need a character (`m <char>`) arm
/// their key prefix, so the next key press completes them exactly as if typed.
fn run_command(state: &mut AppState, invocation: Invocation, effects: &mut Vec<Effect>) {
    if command::takes_char(invocation.action) && invocation.arg.is_none() {
        arm_prefix(state, invocation.action, effects);
        return;
    }
    update::run_action(state, invocation, effects);
}

fn arm_prefix(state: &mut AppState, action: Action, effects: &mut Vec<Effect>) {
    let prefix = state
        .keymap
        .bindings(KeyContext::Normal)
        .iter()
        .filter(|b| b.action == action)
        .find_map(|b| {
            let at = b.keys.iter().position(|k| *k == KeyPattern::AnyChar)?;
            b.keys[..at]
                .iter()
                .map(|k| match k {
                    KeyPattern::Chord(c) => Some(*c),
                    KeyPattern::AnyChar => None,
                })
                .collect::<Option<Vec<KeyChord>>>()
        });
    match prefix {
        Some(prefix) if !prefix.is_empty() => {
            state.pending = prefix;
            state.which_key_visible = false;
            let gen = state.which_key_gen.bump();
            effects.push(Effect::StartTimer {
                kind: TimerKind::WhichKey,
                gen,
                delay: Duration::from_millis(state.config.ui.which_key_delay_ms),
            });
            update::set_status(
                state,
                Level::Info,
                format!("{}: press a character", action.title()),
                effects,
            );
        }
        _ => update::set_status(
            state,
            Level::Warning,
            format!("{} needs a key binding like `m <char>`", action.title()),
            effects,
        ),
    }
}

#[cfg(test)]
pub(crate) mod tests {
    use std::path::PathBuf;

    use super::*;
    use crate::config::Config;
    use crate::keymap::Keymap;
    use crate::msg::Msg;
    use crate::state::{Places, StartParams};
    use crate::update::update;

    pub(crate) fn state() -> AppState {
        let mut s = AppState::new(StartParams {
            cwd: PathBuf::from("/home/me/project"),
            home: Some(PathBuf::from("/home/me")),
            places: Places::default(),
            config: Config::default(),
            keymap: Keymap::defaults(),
            size: Size::new(80, 24),
            now: std::time::SystemTime::UNIX_EPOCH,
            utc_offset_secs: 0,
        });
        s.themes = vec!["ember".into(), "paper".into(), "solar".into()];
        s.dirty = false;
        s
    }

    fn key(s: &mut AppState, text: &str) -> Vec<Effect> {
        update(s, Msg::Key(text.parse::<KeyChord>().unwrap()))
    }

    fn type_text(s: &mut AppState, text: &str) {
        for c in text.chars() {
            update(s, Msg::Key(KeyChord::char(c)));
        }
    }

    fn top_label(s: &AppState) -> String {
        s.palette.selected_item().unwrap().label.clone()
    }

    #[test]
    fn colon_opens_and_esc_closes() {
        let mut s = state();
        key(&mut s, ":");
        assert_eq!(s.overlays, [Overlay::Palette]);
        assert_eq!(s.key_context(), KeyContext::Palette);
        assert!(s.dirty);
        key(&mut s, "esc");
        assert!(s.overlays.is_empty());
    }

    #[test]
    fn every_action_is_listed_with_its_bindings() {
        let mut s = state();
        key(&mut s, ":");
        for &action in Action::ALL {
            let listed = s
                .palette
                .items
                .iter()
                .any(|i| matches!(i.kind, ItemKind::Command(a, _) if a == action));
            assert!(listed, "{action} missing from the palette");
        }
        let trash = s
            .palette
            .items
            .iter()
            .find(|i| i.kind == ItemKind::Command(Action::Trash, None))
            .unwrap();
        assert_eq!(trash.keys, ["d"]);
        let tab3 = s
            .palette
            .items
            .iter()
            .find(|i| i.kind == ItemKind::Command(Action::TabGoto, Some('3')))
            .unwrap();
        assert_eq!(tab3.label, "Switch to tab 3");
        assert_eq!(tab3.keys, ["3"]);
        let home = s
            .palette
            .items
            .iter()
            .find(|i| i.kind == ItemKind::Command(Action::GoHome, None))
            .unwrap();
        assert_eq!(home.keys, ["~", "g h"]);
    }

    #[test]
    fn typing_filters_fuzzily_and_highlights_matches() {
        let mut s = state();
        key(&mut s, ":");
        type_text(&mut s, "srtsz");
        assert_eq!(top_label(&s), "Sort by size");
        let hit = &s.palette.hits[0];
        assert_eq!(hit.indices.len(), 5);
        assert!(hit.indices.windows(2).all(|w| w[0] < w[1]));

        // Backspace widens the match again.
        key(&mut s, "ctrl-u");
        type_text(&mut s, "sort by");
        assert!(s.palette.hits.len() >= 4);
        // Searching by the stable id works too.
        key(&mut s, "ctrl-u");
        type_text(&mut s, "file.trash");
        assert_eq!(top_label(&s), "Move to Recycle Bin");
    }

    #[test]
    fn no_match_shows_nothing_and_enter_is_harmless() {
        let mut s = state();
        key(&mut s, ":");
        type_text(&mut s, "zzzqqq");
        assert!(s.palette.hits.is_empty());
        key(&mut s, "enter");
        assert_eq!(s.overlays, [Overlay::Palette], "nothing chosen, still open");
    }

    #[test]
    fn navigation_wraps_and_pages() {
        let mut s = state();
        key(&mut s, ":");
        key(&mut s, "up");
        assert_eq!(
            s.palette.selected,
            s.palette.hits.len() - 1,
            "wraps to the end"
        );
        key(&mut s, "down");
        assert_eq!(s.palette.selected, 0);
        key(&mut s, "ctrl-n");
        key(&mut s, "ctrl-n");
        assert_eq!(s.palette.selected, 2);
        key(&mut s, "ctrl-p");
        assert_eq!(s.palette.selected, 1);
        key(&mut s, "pagedown");
        assert!(s.palette.selected > 5);
        assert!(s.palette.scroll > 0 || s.palette.selected < visible_rows(s.size));
        key(&mut s, "pageup");
        key(&mut s, "pageup");
        key(&mut s, "pageup");
        assert_eq!(s.palette.selected, 0);
        assert_eq!(s.palette.scroll, 0);
    }

    #[test]
    fn palette_reaches_the_tab_and_bookmark_overlays() {
        for (query, expected) in [
            ("list bookmarks", Overlay::Bookmarks),
            ("frecency", Overlay::Jump),
        ] {
            let mut s = state();
            key(&mut s, ":");
            type_text(&mut s, query);
            key(&mut s, "enter");
            assert_eq!(s.overlays, [expected], "{query}");
        }
        let mut s = state();
        key(&mut s, ":");
        type_text(&mut s, "new tab");
        key(&mut s, "enter");
        assert_eq!(s.tabs.count(), 2);
    }

    #[test]
    fn enter_runs_the_command_and_closes() {
        let mut s = state();
        key(&mut s, ":");
        type_text(&mut s, "hidden");
        assert_eq!(top_label(&s), "Toggle hidden files");
        let before = s.view.show_hidden;
        key(&mut s, "enter");
        assert!(s.overlays.is_empty());
        assert_ne!(s.view.show_hidden, before);
    }

    #[test]
    fn unimplemented_commands_report_not_available() {
        let mut s = state();
        key(&mut s, ":");
        type_text(&mut s, "bulk");
        key(&mut s, "enter");
        let status = s.status.as_ref().unwrap();
        assert_eq!(status.text, "Bulk rename in editor is not available yet");
    }

    #[test]
    fn recent_commands_come_first_when_the_query_is_empty() {
        let mut s = state();
        key(&mut s, ":");
        assert_eq!(top_label(&s), "Move cursor down", "registry order at first");
        type_text(&mut s, "sort by size");
        key(&mut s, "enter");
        key(&mut s, ":");
        assert_eq!(top_label(&s), "Sort by size");
        type_text(&mut s, "sort by");
        // The same score for every sort command; the recent one wins the tie.
        assert_eq!(top_label(&s), "Sort by size");
        key(&mut s, "ctrl-u");
        type_text(&mut s, "sort by name");
        key(&mut s, "enter");
        key(&mut s, ":");
        assert_eq!(top_label(&s), "Sort by name");
        key(&mut s, "down");
        assert_eq!(top_label(&s), "Sort by size");
    }

    #[test]
    fn character_commands_arm_their_prefix() {
        let mut s = state();
        key(&mut s, ":");
        type_text(&mut s, "set bookmark");
        assert_eq!(top_label(&s), "Set bookmark");
        let effects = key(&mut s, "enter");
        assert_eq!(s.pending, [KeyChord::char('m')]);
        assert!(effects.iter().any(|e| matches!(
            e,
            Effect::StartTimer {
                kind: TimerKind::WhichKey,
                ..
            }
        )));
        // The next key completes `m <char>` as the keymap would.
        key(&mut s, "a");
        assert!(s.pending.is_empty());
    }

    #[test]
    fn character_command_without_binding_explains_itself() {
        let mut s = state();
        s.keymap.apply_overrides(
            &[(
                "normal".to_owned(),
                [("m <char>".to_owned(), "none".to_owned())].into(),
            )]
            .into(),
        );
        key(&mut s, ":");
        type_text(&mut s, "set bookmark");
        key(&mut s, "enter");
        assert!(s.pending.is_empty());
        let status = s.status.as_ref().unwrap();
        assert!(
            status.text.contains("needs a key binding"),
            "{}",
            status.text
        );
    }

    #[test]
    fn theme_page_lists_themes_and_requests_the_switch() {
        let mut s = state();
        key(&mut s, ":");
        type_text(&mut s, "switch theme");
        key(&mut s, "enter");
        assert_eq!(s.overlays, [Overlay::Palette]);
        assert_eq!(s.palette.page, Page::Themes);
        let labels: Vec<_> = s.palette.items.iter().map(|i| i.label.as_str()).collect();
        assert_eq!(labels, ["ember", "paper", "solar"]);
        assert_eq!(top_label(&s), "ember", "starts on the active theme");
        key(&mut s, "down");
        let effects = key(&mut s, "enter");
        assert!(s.overlays.is_empty());
        assert_eq!(
            effects,
            [Effect::SetTheme {
                name: "paper".into(),
                announce: true
            }]
        );
    }

    #[test]
    fn esc_on_a_sub_page_goes_back_before_closing() {
        let mut s = state();
        key(&mut s, ":");
        type_text(&mut s, "switch icon");
        key(&mut s, "enter");
        assert_eq!(s.palette.page, Page::Icons);
        key(&mut s, "esc");
        assert_eq!(s.palette.page, Page::Commands);
        assert_eq!(s.overlays, [Overlay::Palette]);
        key(&mut s, "esc");
        assert!(s.overlays.is_empty());
    }

    #[test]
    fn icon_page_changes_the_icon_set_for_the_session() {
        let mut s = state();
        key(&mut s, ":");
        type_text(&mut s, "switch icon");
        key(&mut s, "enter");
        type_text(&mut s, "ascii");
        key(&mut s, "enter");
        assert_eq!(s.config.icons, IconSet::Ascii);
        assert!(s.overlays.is_empty());
        assert_eq!(s.status.as_ref().unwrap().text, "Icon set: ascii");
    }

    #[test]
    fn rebinding_shows_up_in_the_palette() {
        let mut s = state();
        s.keymap.apply_overrides(
            &[(
                "normal".to_owned(),
                [("T".to_owned(), "file.trash".to_owned())].into(),
            )]
            .into(),
        );
        key(&mut s, ":");
        let trash = s
            .palette
            .items
            .iter()
            .find(|i| i.kind == ItemKind::Command(Action::Trash, None))
            .unwrap();
        assert_eq!(trash.keys, ["d", "T"]);
    }

    #[test]
    fn palette_keys_do_not_leak_to_the_main_screen() {
        let mut s = state();
        key(&mut s, ":");
        type_text(&mut s, "q");
        assert!(s.quit.is_none(), "q is text in the palette");
        assert_eq!(s.palette.input.text(), "q");
    }

    #[test]
    fn query_characters_are_literal_not_nucleo_syntax() {
        let mut s = state();
        key(&mut s, ":");
        type_text(&mut s, "^sort");
        assert!(
            s.palette.hits.is_empty(),
            "^ is not an anchor: no title has it"
        );
        let mut s = state();
        key(&mut s, ":");
        type_text(&mut s, "!sort");
        assert!(s.palette.hits.is_empty(), "! is not a negation");
        let mut s = state();
        key(&mut s, ":");
        type_text(&mut s, "sort name");
        assert!(
            !s.palette.hits.is_empty(),
            "words still match as separate atoms"
        );
    }

    #[test]
    fn resizing_smaller_keeps_the_selection_on_screen() {
        let mut s = state();
        s.size = Size::new(160, 48);
        key(&mut s, ":");
        for _ in 0..20 {
            key(&mut s, "down");
        }
        update(&mut s, Msg::Resize(Size::new(80, 12)));
        let rows = visible_rows(s.size);
        assert!(
            s.palette.selected >= s.palette.scroll,
            "selected above view"
        );
        assert!(
            s.palette.selected < s.palette.scroll + rows,
            "selected below view"
        );
    }

    #[test]
    fn resizing_larger_does_not_leave_help_scrolled_past_the_end() {
        let mut s = state();
        key(&mut s, "?");
        for _ in 0..40 {
            key(&mut s, "ctrl-d");
        }
        update(&mut s, Msg::Resize(Size::new(200, 100)));
        let rows = crate::help::visible_rows(s.size);
        assert!(s.help.scroll + rows >= s.help.lines.len() || s.help.scroll == 0);
        assert!(s.help.scroll <= s.help.lines.len().saturating_sub(rows));
    }

    #[test]
    fn reloading_the_config_discards_session_icon_and_theme_choices() {
        let mut s = state();
        s.config.icons = IconSet::Ascii;
        s.config.theme = "paper".into();
        let effects = update(
            &mut s,
            Msg::ConfigReloaded {
                config: Box::new(Config::default()),
                keymap: Box::new(Keymap::defaults()),
                issues: Vec::new(),
            },
        );
        // The file is the source of truth on reload: icons revert and the theme is
        // re-requested from the file's setting.
        assert_eq!(s.config.icons, IconSet::Nerd);
        assert!(effects.iter().any(
            |e| matches!(e, Effect::SetTheme { name, announce: false } if *name == Config::default().theme)
        ));
    }
}
