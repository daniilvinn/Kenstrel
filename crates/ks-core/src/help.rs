//! The help overlay (`?`): every command grouped by category with its live key bindings,
//! scrollable and filterable.

use nucleo_matcher::pattern::{AtomKind, CaseMatching, Normalization, Pattern};
use nucleo_matcher::{Config as MatcherConfig, Matcher, Utf32Str};

use crate::action::Action;
use crate::command::{self, Category};
use crate::effect::Effect;
use crate::key::KeyChord;
use crate::keymap::{Binding, Invocation, KeyPattern, Keymap};
use crate::mode::{KeyContext, Overlay};
use crate::state::{AppState, Size};
use crate::text_input::TextInput;

/// One line of the help body.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum HelpLine {
    Heading(Category),
    Blank,
    Entry {
        /// Key sequences, e.g. `j, down`; empty when the command has no binding.
        keys: String,
        title: &'static str,
        description: &'static str,
    },
}

#[derive(Clone, Debug, Default)]
pub struct HelpState {
    pub filter: TextInput,
    /// Keys go to the filter field instead of the keymap.
    pub filtering: bool,
    /// Index of the first visible line of `lines`.
    pub scroll: usize,
    /// Lines for the current filter.
    pub lines: Vec<HelpLine>,
    all: Vec<(Category, HelpLine)>,
}

impl HelpState {
    fn rebuild_lines(&mut self) {
        let query = self.filter.text().trim().to_owned();
        let pattern = (!query.is_empty()).then(|| {
            Pattern::new(
                &query,
                CaseMatching::Smart,
                Normalization::Smart,
                AtomKind::Fuzzy,
            )
        });
        let mut matcher = Matcher::new(MatcherConfig::DEFAULT);
        let mut buf = Vec::new();
        let mut lines = Vec::new();
        for &category in Category::ALL {
            let mut section = Vec::new();
            for (cat, line) in &self.all {
                let HelpLine::Entry {
                    keys,
                    title,
                    description,
                } = line
                else {
                    continue;
                };
                if *cat != category {
                    continue;
                }
                if let Some(pattern) = &pattern {
                    let haystack = format!("{title} {keys} {description}");
                    if pattern
                        .score(Utf32Str::new(&haystack, &mut buf), &mut matcher)
                        .is_none()
                    {
                        continue;
                    }
                }
                section.push(line.clone());
            }
            if section.is_empty() {
                continue;
            }
            if !lines.is_empty() {
                lines.push(HelpLine::Blank);
            }
            lines.push(HelpLine::Heading(category));
            lines.extend(section);
        }
        self.lines = lines;
    }

    pub(crate) fn clamp_scroll(&mut self, rows: usize) {
        self.scroll = self
            .scroll
            .min(self.lines.len().saturating_sub(rows.max(1)));
    }

    /// Number of command entries currently shown.
    pub fn entry_count(&self) -> usize {
        self.lines
            .iter()
            .filter(|l| matches!(l, HelpLine::Entry { .. }))
            .count()
    }
}

/// Outer size of the help box.
pub fn box_size(size: Size) -> (u16, u16) {
    let width = size.width.saturating_sub(4).clamp(40, 110).min(size.width);
    let height = size.height.saturating_sub(4).max(6).min(size.height);
    (width, height)
}

/// Body rows: the box minus two borders and the filter line.
pub fn visible_rows(size: Size) -> usize {
    usize::from(box_size(size).1.saturating_sub(3))
}

/// Text for a command's key column. Consecutive digit bindings collapse (`1-9`).
pub fn keys_label(bindings: &[&Binding]) -> String {
    let digits: Vec<char> = bindings
        .iter()
        .filter_map(|b| match (b.arg, b.keys.as_slice()) {
            (Some(arg), [KeyPattern::Chord(c)]) if c.key == crate::key::Key::Char(arg) => {
                arg.is_ascii_digit().then_some(arg)
            }
            _ => None,
        })
        .collect();
    if digits.len() == bindings.len() && digits.len() > 3 {
        let (first, last) = (digits[0], digits[digits.len() - 1]);
        if digits.windows(2).all(|w| w[1] as u32 == w[0] as u32 + 1) {
            return format!("{first}-{last}");
        }
    }
    bindings
        .iter()
        .map(|b| b.keys_display())
        .collect::<Vec<_>>()
        .join(", ")
}

/// Lines of the unfiltered help for `keymap`.
fn build_all(keymap: &Keymap) -> Vec<(Category, HelpLine)> {
    let mut all = Vec::new();
    for info in command::all() {
        let bindings: Vec<&Binding> = keymap
            .bindings(KeyContext::Normal)
            .iter()
            .filter(|b| b.action == info.action)
            .collect();
        all.push((
            info.category,
            HelpLine::Entry {
                keys: keys_label(&bindings),
                title: info.title,
                description: info.description,
            },
        ));
    }
    all
}

pub fn open(state: &mut AppState) {
    if state.overlays.last() != Some(&Overlay::Help) {
        state.overlays.push(Overlay::Help);
    }
    let all = build_all(&state.keymap);
    let help = &mut state.help;
    help.all = all;
    help.filter.clear();
    help.filtering = false;
    help.scroll = 0;
    help.rebuild_lines();
    state.dirty = true;
}

/// Handles keys while the filter field has focus. Returns true when the key was consumed.
pub(crate) fn on_filter_key(state: &mut AppState, chord: KeyChord) -> bool {
    use crate::key::Key;
    let rows = visible_rows(state.size);
    let help = &mut state.help;
    match chord.key {
        Key::Esc => {
            help.filter.clear();
            help.filtering = false;
            help.scroll = 0;
            help.rebuild_lines();
        }
        Key::Enter => help.filtering = false,
        _ => match help.filter.handle_key(chord) {
            Some(true) => {
                help.scroll = 0;
                help.rebuild_lines();
            }
            Some(false) => {}
            // Unknown keys are swallowed: nothing else may react while typing.
            None => return true,
        },
    }
    help.clamp_scroll(rows);
    state.dirty = true;
    true
}

/// Handles an action while help is the top overlay.
pub(crate) fn on_action(
    state: &mut AppState,
    invocation: Invocation,
    _effects: &mut Vec<Effect>,
) -> bool {
    let rows = visible_rows(state.size);
    let half = (rows / 2).max(1);
    let help = &mut state.help;
    match invocation.action {
        Action::CursorDown => help.scroll += 1,
        Action::CursorUp => help.scroll = help.scroll.saturating_sub(1),
        Action::PageHalfDown => help.scroll += half,
        Action::PageHalfUp => help.scroll = help.scroll.saturating_sub(half),
        Action::CursorTop => help.scroll = 0,
        Action::CursorBottom => help.scroll = usize::MAX,
        Action::FilterOpen => help.filtering = true,
        // Esc first drops an applied filter, then closes the overlay.
        Action::Cancel if !help.filter.is_empty() => {
            help.filter.clear();
            help.scroll = 0;
            help.rebuild_lines();
        }
        _ => return false,
    }
    help.clamp_scroll(rows);
    state.dirty = true;
    true
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::msg::Msg;
    use crate::palette::tests::state;
    use crate::update::update;

    fn key(s: &mut AppState, text: &str) {
        update(s, Msg::Key(text.parse::<KeyChord>().unwrap()));
    }

    fn entry_titles(s: &AppState) -> Vec<&'static str> {
        s.help
            .lines
            .iter()
            .filter_map(|l| match l {
                HelpLine::Entry { title, .. } => Some(*title),
                _ => None,
            })
            .collect()
    }

    fn keys_of(s: &AppState, wanted: &str) -> String {
        s.help
            .lines
            .iter()
            .find_map(|l| match l {
                HelpLine::Entry { keys, title, .. } if *title == wanted => Some(keys.clone()),
                _ => None,
            })
            .unwrap()
    }

    #[test]
    fn question_mark_opens_a_complete_grouped_help() {
        let mut s = state();
        key(&mut s, "?");
        assert_eq!(s.overlays, [Overlay::Help]);
        assert_eq!(s.key_context(), KeyContext::Help);
        assert_eq!(s.help.entry_count(), Action::ALL.len());
        // Headings appear in category order, each followed by its entries.
        let headings: Vec<_> = s
            .help
            .lines
            .iter()
            .filter_map(|l| match l {
                HelpLine::Heading(c) => Some(*c),
                _ => None,
            })
            .collect();
        assert_eq!(headings, Category::ALL);
        assert_eq!(keys_of(&s, "Move cursor down"), "j, down");
        assert_eq!(keys_of(&s, "Jump to top"), "g g, home");
        assert_eq!(keys_of(&s, "Switch to tab"), "1-9");
        assert_eq!(keys_of(&s, "Reload config"), "");
        key(&mut s, "?");
        assert!(s.overlays.is_empty(), "? toggles");
    }

    #[test]
    fn help_reflects_rebinding_and_unbinding() {
        let mut s = state();
        s.keymap.apply_overrides(
            &[(
                "normal".to_owned(),
                [
                    ("T".to_owned(), "file.trash".to_owned()),
                    ("d".to_owned(), "none".to_owned()),
                ]
                .into(),
            )]
            .into(),
        );
        key(&mut s, "?");
        assert_eq!(keys_of(&s, "Move to Recycle Bin"), "T");
    }

    #[test]
    fn scrolling_is_clamped_and_pages() {
        let mut s = state();
        key(&mut s, "?");
        key(&mut s, "k");
        assert_eq!(s.help.scroll, 0);
        key(&mut s, "j");
        key(&mut s, "down");
        assert_eq!(s.help.scroll, 2);
        key(&mut s, "ctrl-d");
        assert_eq!(s.help.scroll, 2 + visible_rows(s.size) / 2);
        key(&mut s, "ctrl-u");
        assert_eq!(s.help.scroll, 2);
        key(&mut s, "G");
        assert_eq!(s.help.scroll, s.help.lines.len() - visible_rows(s.size));
        key(&mut s, "j");
        assert_eq!(s.help.scroll, s.help.lines.len() - visible_rows(s.size));
        key(&mut s, "g");
        key(&mut s, "g");
        assert_eq!(s.help.scroll, 0);
    }

    #[test]
    fn slash_filters_and_hides_empty_categories() {
        let mut s = state();
        key(&mut s, "?");
        key(&mut s, "/");
        assert!(s.help.filtering);
        // `q` and `?` are text while typing, not "close help".
        for c in "bookm".chars() {
            update(&mut s, Msg::Key(KeyChord::char(c)));
        }
        assert_eq!(s.overlays, [Overlay::Help]);
        assert_eq!(s.help.filter.text(), "bookm");
        let titles = entry_titles(&s);
        assert!(titles.contains(&"Set bookmark") && titles.contains(&"Jump to bookmark"));
        assert!(!titles.contains(&"Quit"));
        let headings = s
            .help
            .lines
            .iter()
            .filter(|l| matches!(l, HelpLine::Heading(_)))
            .count();
        assert_eq!(headings, 1);

        key(&mut s, "enter");
        assert!(!s.help.filtering);
        assert!(!s.help.filter.is_empty(), "enter keeps the filter");
        key(&mut s, "esc");
        assert!(s.help.filter.is_empty(), "first esc drops the filter");
        assert_eq!(s.overlays, [Overlay::Help]);
        key(&mut s, "esc");
        assert!(s.overlays.is_empty());
    }

    #[test]
    fn esc_while_typing_clears_and_leaves_the_field() {
        let mut s = state();
        key(&mut s, "?");
        key(&mut s, "/");
        update(&mut s, Msg::Key(KeyChord::char('x')));
        key(&mut s, "esc");
        assert!(!s.help.filtering);
        assert!(s.help.filter.is_empty());
        assert_eq!(s.overlays, [Overlay::Help]);
        assert_eq!(s.help.entry_count(), Action::ALL.len());
    }

    #[test]
    fn filter_matches_keys_and_descriptions() {
        let mut s = state();
        key(&mut s, "?");
        key(&mut s, "/");
        for c in "recycle".chars() {
            update(&mut s, Msg::Key(KeyChord::char(c)));
        }
        assert!(entry_titles(&s).contains(&"Move to Recycle Bin"));
    }
}
