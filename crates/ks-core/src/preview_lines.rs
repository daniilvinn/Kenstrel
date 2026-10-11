//! Line-range selection in the text preview.
//!
//! `V` starts a selection at the first visible line; `j`/`k` extend it, `esc` cancels, `C` or
//! `enter` sends `@path#L<a>-<b>` to the Claude pane. The selection lives in
//! [`PreviewState::select`](crate::preview::PreviewState::select) and gives the keymap its own
//! context ([`KeyContext::PreviewLines`](crate::mode::KeyContext::PreviewLines)) while it is active.

use std::path::PathBuf;

use ks_preview::PreviewBody;

use crate::action::Action;
use crate::effect::Effect;
use crate::mode::{KeyContext, Mode};
use crate::state::{AppState, Level};
use crate::update::set_status;

/// An active selection. Lines are 0-based indices into the text preview; file line numbers are
/// `index + 1`.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct LineSelect {
    /// The file the selection belongs to.
    pub path: PathBuf,
    /// Where the selection started.
    pub anchor: usize,
    /// The end that moves.
    pub cursor: usize,
    /// Hash of the text the indices refer to: a reload with other content drops the selection.
    fingerprint: u64,
}

impl LineSelect {
    /// First and last selected line index, in order.
    pub fn range(&self) -> (usize, usize) {
        (self.anchor.min(self.cursor), self.anchor.max(self.cursor))
    }

    /// 1-based inclusive file line numbers.
    pub fn file_lines(&self) -> (usize, usize) {
        let (a, b) = self.range();
        (a + 1, b + 1)
    }
}

/// The text result shown for the current target, if it is a plain text one.
fn text_of(state: &AppState) -> Option<&ks_preview::TextPreview> {
    let preview = state.preview.current.as_deref()?;
    if state.preview.target.as_deref() != Some(preview.path.as_path()) || state.preview.metadata {
        return None;
    }
    match &preview.body {
        PreviewBody::Text(text) => Some(text),
        _ => None,
    }
}

/// Number of lines of the text result shown for the current target, if it is a plain text one.
fn text_len(state: &AppState) -> Option<usize> {
    text_of(state).map(|text| text.lines.len())
}

fn fingerprint(text: &ks_preview::TextPreview) -> u64 {
    use std::hash::{Hash, Hasher};
    let mut hasher = std::collections::hash_map::DefaultHasher::new();
    for line in &text.lines {
        for span in &line.spans {
            span.text.hash(&mut hasher);
        }
        0xAu8.hash(&mut hasher);
    }
    hasher.finish()
}

/// Drops the selection when what it stands for is gone (another file, an overlay, a result that
/// is no longer text). Called after every `update`.
pub(crate) fn validate(state: &mut AppState) {
    let Some(select) = &state.preview.select else {
        return;
    };
    let keep = state.overlays.is_empty()
        && state.mode == Mode::Normal
        && state.preview.target.as_deref() == Some(select.path.as_path())
        && text_of(state).is_some_and(|text| {
            select.cursor < text.lines.len()
                && select.anchor < text.lines.len()
                && fingerprint(text) == select.fingerprint
        })
        && state.preview_visible();
    // While the result is still loading (same file refreshing) keep the selection.
    let loading =
        state.preview.loading && state.preview.target.as_deref() == Some(select.path.as_path());
    if !keep && !loading {
        state.preview.select = None;
        state.dirty = true;
    }
}

/// Handles the actions of the selection mode. Returns false for any other action.
pub(crate) fn handle_action(
    state: &mut AppState,
    action: Action,
    effects: &mut Vec<Effect>,
) -> bool {
    if action == Action::PreviewSelectLines {
        if state.preview.select.is_some() {
            state.preview.select = None;
        } else {
            begin(state, effects);
        }
        state.dirty = true;
        return true;
    }
    if state.key_context() != KeyContext::PreviewLines {
        if action == Action::PreviewLinesSend {
            set_status(
                state,
                Level::Info,
                "Select lines first (V in a text preview)".to_owned(),
                effects,
            );
            return true;
        }
        return false;
    }
    let last = text_len(state).map_or(0, |len| len.saturating_sub(1));
    let half = (state.preview_rows() / 2).max(1);
    match action {
        Action::CursorDown => step(state, 1, last),
        Action::CursorUp => step(state, -1, last),
        Action::PageHalfDown => step(state, isize::try_from(half).unwrap_or(1), last),
        Action::PageHalfUp => step(state, -isize::try_from(half).unwrap_or(1), last),
        Action::CursorTop => set_cursor(state, 0),
        Action::CursorBottom => set_cursor(state, last),
        Action::Cancel => state.preview.select = None,
        Action::Accept | Action::PreviewLinesSend => send(state, effects),
        _ => return false,
    }
    state.dirty = true;
    true
}

/// `V`: start selecting at the first visible line.
fn begin(state: &mut AppState, effects: &mut Vec<Effect>) {
    let plain = state.overlays.is_empty() && state.mode == Mode::Normal;
    let len = if plain && state.preview_visible() {
        text_len(state)
    } else {
        None
    };
    let (Some(len), Some(path)) = (len, state.preview.target.clone()) else {
        set_status(
            state,
            Level::Info,
            "Line selection needs a text preview".to_owned(),
            effects,
        );
        return;
    };
    if len == 0 {
        return;
    }
    let start = state.preview.scroll.min(len - 1);
    state.preview.select = Some(LineSelect {
        path,
        anchor: start,
        cursor: start,
        fingerprint: text_of(state).map_or(0, fingerprint),
    });
}

fn step(state: &mut AppState, delta: isize, last: usize) {
    let Some(select) = &state.preview.select else {
        return;
    };
    let target = select.cursor.saturating_add_signed(delta).min(last);
    set_cursor(state, target);
}

/// Moves the moving end of the selection and scrolls it into view.
fn set_cursor(state: &mut AppState, to: usize) {
    let rows = state.preview_rows().max(1);
    let Some(select) = &mut state.preview.select else {
        return;
    };
    select.cursor = to;
    let scroll = &mut state.preview.scroll;
    if to < *scroll {
        *scroll = to;
    } else if to >= *scroll + rows {
        *scroll = to + 1 - rows;
    }
}

fn send(state: &mut AppState, effects: &mut Vec<Effect>) {
    let Some(select) = state.preview.select.clone() else {
        return;
    };
    if !state.claude.is_running() {
        // This context has no `c` binding and opening Claude drops the selection anyway, so
        // say what actually works instead of promising a way back.
        set_status(
            state,
            Level::Info,
            "Claude is not running: esc, c to open it, then V again".to_owned(),
            effects,
        );
        return;
    }
    let (start, end) = select.file_lines();
    state.preview.select = None;
    crate::claude::send_range(state, select.path, start, end, effects);
    let text = if start == end {
        format!("Sent line {start} to Claude")
    } else {
        format!("Sent lines {start}-{end} to Claude")
    };
    set_status(state, Level::Info, text, effects);
}

#[cfg(test)]
mod tests {
    use std::sync::Arc;

    use ks_preview::{Encoding, Preview, StyledLine, TextPreview};

    use super::*;
    use crate::claude::ClaudePreflight;
    use crate::config::Config;
    use crate::entry::Entry;
    use crate::key::KeyChord;
    use crate::keymap::Keymap;
    use crate::mode::Overlay;
    use crate::msg::Msg;
    use crate::state::{Places, Size, StartParams};
    use crate::update::update;

    fn key(s: &mut AppState, text: &str) -> Vec<Effect> {
        update(s, Msg::Key(text.parse::<KeyChord>().unwrap()))
    }

    /// A 120x32 state showing a 50-line text file `notes.txt` in the preview.
    fn state() -> AppState {
        let mut s = AppState::new(StartParams {
            cwd: PathBuf::from(if cfg!(windows) { "C:/proj" } else { "/proj" }),
            home: None,
            places: Places::default(),
            config: Config::default(),
            keymap: Keymap::defaults(),
            size: Size::new(120, 32),
            now: std::time::SystemTime::UNIX_EPOCH,
            utc_offset_secs: 0,
        });
        let rows = s.viewport_height();
        s.view.push_chunk(vec![Entry::file("notes.txt", 100)], rows);
        update(&mut s, Msg::Tick);
        let path = s.cwd.join("notes.txt");
        let lines: Vec<StyledLine> = (0..50).map(|_| StyledLine::default()).collect();
        let text = TextPreview {
            first_line: 0,
            line_count: lines.len(),
            total_lines: Some(lines.len()),
            lines,
            truncated: false,
            encoding: Encoding::Utf8,
            syntax: None,
            bytes_read: 100,
        };
        s.preview.target = Some(path.clone());
        s.preview.current = Some(Arc::new(Preview {
            path,
            meta: None,
            body: PreviewBody::Text(text),
        }));
        s.preview.loading = false;
        s
    }

    fn start_claude(s: &mut AppState) {
        key(s, "c");
        let generation = s.claude.generation;
        update(
            s,
            Msg::ClaudePreflight {
                generation,
                result: ClaudePreflight::Ok,
            },
        );
        update(s, Msg::ClaudeStarted { generation });
        key(s, "ctrl-\\");
    }

    #[test]
    fn v_starts_a_selection_that_j_and_k_extend() {
        let mut s = state();
        key(&mut s, "V");
        assert_eq!(s.key_context(), KeyContext::PreviewLines);
        key(&mut s, "j");
        key(&mut s, "j");
        assert_eq!(s.preview.select.as_ref().unwrap().file_lines(), (1, 3));
        key(&mut s, "k");
        key(&mut s, "k");
        key(&mut s, "k");
        // The anchor stays: the range now runs from the cursor up to the anchor.
        let select = s.preview.select.clone().unwrap();
        assert_eq!((select.anchor, select.cursor), (0, 0));
        key(&mut s, "G");
        assert_eq!(s.preview.select.as_ref().unwrap().file_lines(), (1, 50));
        assert!(
            s.preview.scroll > 0,
            "the cursor line is scrolled into view"
        );
        key(&mut s, "g");
        key(&mut s, "g");
        assert_eq!(s.preview.select.as_ref().unwrap().file_lines(), (1, 1));
    }

    #[test]
    fn esc_cancels_and_other_files_drop_the_selection() {
        let mut s = state();
        key(&mut s, "V");
        key(&mut s, "esc");
        assert!(s.preview.select.is_none());
        assert_eq!(s.key_context(), KeyContext::Normal);

        key(&mut s, "V");
        s.preview.target = Some(s.cwd.join("other.txt"));
        update(&mut s, Msg::Tick);
        assert!(s.preview.select.is_none());
    }

    #[test]
    fn selection_needs_a_text_preview() {
        let mut s = state();
        s.preview.current = None;
        key(&mut s, "V");
        assert!(s.preview.select.is_none());
        assert!(s.status.as_ref().unwrap().text.contains("text preview"));
    }

    #[test]
    fn c_sends_the_range_to_the_pane_and_leaves_the_mode() {
        let mut s = state();
        start_claude(&mut s);
        key(&mut s, "V");
        key(&mut s, "j");
        key(&mut s, "j");
        key(&mut s, "j");
        let effects = key(&mut s, "C");
        assert!(effects.contains(&Effect::ClaudeSendRange {
            path: s.cwd.join("notes.txt"),
            start: 1,
            end: 4,
        }));
        assert!(s.preview.select.is_none());
        assert_eq!(s.overlays.last(), Some(&Overlay::Claude));
    }

    #[test]
    fn enter_sends_a_single_line_and_without_claude_a_toast_keeps_the_selection() {
        let mut s = state();
        key(&mut s, "V");
        let effects = key(&mut s, "enter");
        assert!(!effects
            .iter()
            .any(|e| matches!(e, Effect::ClaudeSendRange { .. })));
        assert!(s
            .status
            .as_ref()
            .unwrap()
            .text
            .contains("Claude is not running: esc, c to open it, then V again"));
        assert!(s.preview.select.is_some());

        key(&mut s, "V");
        assert!(s.preview.select.is_none(), "V toggles the mode off");
        start_claude(&mut s);
        key(&mut s, "V");
        let effects = key(&mut s, "enter");
        assert!(effects.contains(&Effect::ClaudeSendRange {
            path: s.cwd.join("notes.txt"),
            start: 1,
            end: 1,
        }));
    }

    fn reload(s: &mut AppState, text: &str, truncated: bool) {
        let path = s.cwd.join("notes.txt");
        let lines: Vec<StyledLine> = (0..50).map(|_| StyledLine::plain(text)).collect();
        s.preview.current = Some(Arc::new(Preview {
            path,
            meta: None,
            body: PreviewBody::Text(TextPreview {
                first_line: 0,
                line_count: lines.len(),
                total_lines: if truncated { None } else { Some(lines.len()) },
                lines,
                truncated,
                encoding: Encoding::Utf8,
                syntax: None,
                bytes_read: 100,
            }),
        }));
    }

    #[test]
    fn a_reload_with_other_content_drops_the_selection_but_the_same_content_keeps_it() {
        let mut s = state();
        reload(&mut s, "same", false);
        key(&mut s, "V");
        key(&mut s, "j");
        reload(&mut s, "same", false);
        update(&mut s, Msg::Tick);
        assert!(s.preview.select.is_some(), "identical text keeps it");
        reload(&mut s, "edited", false);
        update(&mut s, Msg::Tick);
        assert!(s.preview.select.is_none(), "other text drops it");
    }

    #[test]
    fn paging_moves_the_end_by_half_a_screen() {
        let mut s = state();
        key(&mut s, "V");
        key(&mut s, "ctrl-d");
        let down = s.preview.select.as_ref().unwrap().cursor;
        assert!(down > 1, "{down}");
        key(&mut s, "ctrl-u");
        assert_eq!(s.preview.select.as_ref().unwrap().cursor, 0);
    }

    #[test]
    fn an_overlay_or_a_hidden_pane_drops_the_selection() {
        let mut s = state();
        key(&mut s, "V");
        s.overlays.push(Overlay::Claude);
        update(&mut s, Msg::Tick);
        assert!(s.preview.select.is_none());

        let mut s = state();
        key(&mut s, "V");
        s.preview.user_visible = Some(false);
        update(&mut s, Msg::Tick);
        assert!(s.preview.select.is_none());
    }

    #[test]
    fn sending_outside_the_mode_says_to_select_first() {
        let mut s = state();
        let mut effects = Vec::new();
        assert!(handle_action(
            &mut s,
            Action::PreviewLinesSend,
            &mut effects
        ));
        assert!(s.status.as_ref().unwrap().text.contains("Select lines"));
        assert!(
            effects.is_empty()
                || !effects
                    .iter()
                    .any(|e| matches!(e, Effect::ClaudeSendRange { .. }))
        );
    }

    #[test]
    fn a_truncated_preview_bounds_the_selection_to_the_lines_shown() {
        let mut s = state();
        reload(&mut s, "t", true);
        start_claude(&mut s);
        key(&mut s, "V");
        key(&mut s, "G");
        let effects = key(&mut s, "C");
        assert!(effects.contains(&Effect::ClaudeSendRange {
            path: s.cwd.join("notes.txt"),
            start: 1,
            end: 50,
        }));
    }
}
