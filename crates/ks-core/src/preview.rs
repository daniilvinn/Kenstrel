//! The preview pane (PLAN §4.2, §8): which file is shown, the result delivered by the
//! preview engine, scrolling and the pane toggles.
//!
//! `update` stays free of I/O. After every message [`sync`] compares what the pane *should*
//! show (the entry under the cursor, the metadata toggle, the hidden toggle, the width) with
//! what was last requested and, when they differ, bumps a generation and emits an
//! [`Effect::RequestPreview`]. The engine (`ks-preview`) cancels the request it was working
//! on, so holding `j` never queues work. Results carry their generation; stale ones are
//! dropped in [`on_preview`].
//!
//! Terminal image protocol state is *not* here: [`PreviewState::image_id`] is only an opaque
//! handle that changes whenever a different image becomes current.

use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::SystemTime;

use ks_preview::{Preview, PreviewBody, PreviewEvent};

use crate::action::Action;
use crate::config::Config;
use crate::effect::Effect;
use crate::generation::{Gen, GenCounter};
use crate::mode::Overlay;
use crate::state::AppState;

/// Terminals narrower than this hide the preview unless `w` forces it (PLAN §4.2).
pub const PREVIEW_MIN_WIDTH: u16 = 80;
/// Terminals narrower than this never show the parent column.
pub const PARENT_MIN_TERMINAL_WIDTH: u16 = 100;
/// The smallest useful preview pane.
pub const PREVIEW_MIN_PANE: u16 = 20;
/// The smallest list pane once a preview or parent column is shown.
pub const LIST_MIN_PANE: u16 = 20;
/// Rows the preview spends on its header line and the rule below it.
pub const HEADER_ROWS: u16 = 2;

/// Which view of a file is wanted.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum PreviewKind {
    /// Pick by content.
    Auto,
    /// Only the metadata (the `i` toggle).
    Metadata,
}

/// What the effect runner asks the engine for.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct PreviewRequest {
    pub path: PathBuf,
    pub kind: PreviewKind,
    pub show_hidden: bool,
    /// `[preview] max_bytes`.
    pub max_bytes: u64,
    /// `[preview] max_lines`.
    pub max_lines: usize,
    /// Centre a text window on this 1-based line instead of reading from the top (a
    /// content-search hit deep in a big file).
    pub focus_line: Option<u64>,
}

/// A finished engine request on its way into `update`. `PartialEq` compares the generation,
/// the path and the identity of the result, which is all a test needs.
#[derive(Clone, Debug)]
pub struct PreviewMsg(pub PreviewEvent);

impl PreviewMsg {
    pub fn gen(&self) -> Gen {
        Gen::from_raw(self.0.generation)
    }
}

impl PartialEq for PreviewMsg {
    fn eq(&self, other: &Self) -> bool {
        self.0.generation == other.0.generation
            && self.0.path == other.0.path
            && Arc::ptr_eq(&self.0.result, &other.0.result)
    }
}

impl Eq for PreviewMsg {}

/// Widths of the panes for a terminal width. Dividers (one column) sit between neighbours
/// and are not included.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct PaneWidths {
    pub parent: u16,
    pub list: u16,
    pub preview: u16,
}

impl PaneWidths {
    pub const fn has_parent(self) -> bool {
        self.parent > 0
    }

    pub const fn has_preview(self) -> bool {
        self.preview > 0
    }
}

/// Splits `width` columns between the parent column, the list and the preview.
///
/// `list_percent` is the share of the list in the space left after the parent column. A
/// pane that would end up too small is dropped (the preview first, then the parent column).
pub fn pane_widths(
    width: u16,
    want_parent: bool,
    want_preview: bool,
    list_percent: u16,
) -> PaneWidths {
    let mut parent = if want_parent {
        ((u32::from(width) * 18 / 100) as u16).clamp(14, 30)
    } else {
        0
    };
    // One divider after the parent column.
    let parent_total = |p: u16| if p > 0 { p + 1 } else { 0 };
    if parent > 0 && width < parent_total(parent) + LIST_MIN_PANE {
        parent = 0;
    }
    let rest = width - parent_total(parent);
    if !want_preview {
        return PaneWidths {
            parent,
            list: rest,
            preview: 0,
        };
    }
    // The divider between list and preview.
    let usable = rest.saturating_sub(1);
    let list = (u32::from(usable) * u32::from(list_percent) / 100) as u16;
    let list = list.max(LIST_MIN_PANE).min(usable);
    let preview = usable - list;
    if preview < PREVIEW_MIN_PANE {
        return PaneWidths {
            parent,
            list: rest,
            preview: 0,
        };
    }
    PaneWidths {
        parent,
        list,
        preview,
    }
}

/// Identifies what the pane was asked to show.
#[derive(Clone, Debug, PartialEq, Eq)]
struct TargetKey {
    path: PathBuf,
    mtime: Option<SystemTime>,
    size: u64,
    metadata: bool,
    show_hidden: bool,
    epoch: u32,
    /// Line the window read is centred on so a content-search hit past the head limits is
    /// inside the preview (0 = the head). Bucketed, so moving between hits of one file does
    /// not request it again.
    focus: u64,
}

#[derive(Clone, Debug, PartialEq, Eq)]
struct ParentKey {
    /// Bumped by job-completion invalidation, like the main key's `epoch`.
    epoch: u32,
    path: PathBuf,
    show_hidden: bool,
}

/// State of the preview pane and the optional parent column.
#[derive(Debug)]
pub struct PreviewState {
    /// The user pressed `w`: `Some(true)` forces the pane on, `Some(false)` off. `None`
    /// means automatic: on when `[preview] enabled` and the terminal is at least 80 columns.
    pub user_visible: Option<bool>,
    /// The parent column (Miller columns) is on (`P`, `[layout] parent_column`).
    pub parent_column: bool,
    /// The `i` toggle: show metadata instead of content.
    pub metadata: bool,
    /// First visible content line (text, markdown, directory, archive, hex).
    pub scroll: usize,
    /// Path the pane currently stands for (its header follows the cursor immediately).
    pub target: Option<PathBuf>,
    /// The engine result for `target`; `None` while loading.
    pub current: Option<Arc<Preview>>,
    /// A request is in flight.
    pub loading: bool,
    /// Opaque handle of the current image result. Changes whenever a different image
    /// becomes current so the renderer knows to rebuild its protocol state.
    pub image_id: u64,
    /// Parent directory listing, when the parent column is shown.
    pub parent: Option<Arc<Preview>>,
    /// Bumped when the theme changes: syntax colours are baked into text results, so the
    /// current file is requested again with the new palette.
    pub(crate) theme_epoch: u32,
    /// The "hit is past the preview limit" notice was shown for this (file, line).
    beyond_noted: Option<(PathBuf, u64)>,
    /// Visual-lines selection in the text preview (`V`), see [`crate::preview_lines`].
    pub select: Option<crate::preview_lines::LineSelect>,
    key: Option<TargetKey>,
    pub(crate) gen: GenCounter,
    parent_key: Option<ParentKey>,
    parent_gen: GenCounter,
}

impl PreviewState {
    pub fn new(config: &Config) -> Self {
        Self {
            user_visible: None,
            parent_column: config.layout.parent_column,
            metadata: false,
            scroll: 0,
            target: None,
            current: None,
            loading: false,
            image_id: 0,
            parent: None,
            theme_epoch: 0,
            beyond_noted: None,
            select: None,
            key: None,
            gen: GenCounter::default(),
            parent_key: None,
            parent_gen: GenCounter::default(),
        }
    }

    /// The generation of the newest main-pane request.
    pub fn generation(&self) -> Gen {
        self.gen.current()
    }

    /// The generation of the newest parent-column request.
    pub fn parent_generation(&self) -> Gen {
        self.parent_gen.current()
    }
}

impl AppState {
    /// Whether the preview pane is drawn at the current size.
    pub fn preview_visible(&self) -> bool {
        self.pane_widths().has_preview()
    }

    /// The pane geometry at the current size.
    pub fn pane_widths(&self) -> PaneWidths {
        // A right-hand Claude pane narrows the area the list/preview split lives in.
        self.pane_widths_at(crate::claude::list_width(self, self.size.width))
    }

    /// The pane geometry for a terminal `width` columns wide (the renderer passes the width
    /// of the frame it actually draws into).
    pub fn pane_widths_at(&self, width: u16) -> PaneWidths {
        if self.show_drives {
            return PaneWidths {
                parent: 0,
                list: width,
                preview: 0,
            };
        }
        let want_preview = self
            .preview
            .user_visible
            .unwrap_or(self.config.preview.enabled && width >= PREVIEW_MIN_WIDTH);
        let want_parent = self.preview.parent_column && width >= PARENT_MIN_TERMINAL_WIDTH;
        pane_widths(
            width,
            want_parent,
            want_preview,
            self.config.layout.list_percent,
        )
    }

    /// Columns available to the preview content: the pane minus one cell of padding on
    /// each side. This is the width the renderer draws into.
    pub fn content_width(&self) -> u16 {
        if overlay_mode(self) {
            return crate::search::geometry::preview_width(self.size).saturating_sub(2);
        }
        self.pane_widths().preview.saturating_sub(2)
    }

    /// Rows of preview content below the header.
    pub fn preview_rows(&self) -> usize {
        if overlay_mode(self) {
            let grep = matches!(self.overlays.last(), Some(Overlay::Grep));
            let rows = crate::search::geometry::preview_rows(self.size, grep);
            return usize::from(rows.saturating_sub(HEADER_ROWS));
        }
        let body = crate::layout::body_height(self.size);
        usize::from(crate::claude::list_height(self, body).saturating_sub(HEADER_ROWS))
    }
}

/// Content width (the pane minus its padding, see [`AppState::content_width`]) from which
/// the hex view shows 16 bytes per row: 8 offset + 2 + 48 hex + 2 + 16 ascii = 76 cells.
/// Narrower content shows 8. Core (scroll limits) and UI (rows drawn) must pass the same width.
pub const HEX_WIDE_MIN_WIDTH: u16 = 76;

/// Bytes per row of the hex view for `pane_width` columns of content.
pub const fn hex_bytes_per_row(pane_width: u16) -> usize {
    if pane_width >= HEX_WIDE_MIN_WIDTH {
        16
    } else {
        8
    }
}

/// Number of scrollable content lines of a result in a pane `pane_width` columns wide
/// (0 for views that do not scroll).
pub fn content_len(preview: &Preview, pane_width: u16) -> usize {
    match &preview.body {
        PreviewBody::Text(t) | PreviewBody::Markdown(t) => t.lines.len() + usize::from(t.truncated),
        PreviewBody::Directory(d) => d.entries.len() + usize::from(d.truncated),
        PreviewBody::Archive(a) => a.entries.len() + usize::from(a.truncated),
        PreviewBody::Binary(b) => {
            let bytes: usize = b.rows.iter().map(|row| row.bytes.len()).sum();
            bytes.div_ceil(hex_bytes_per_row(pane_width)) + usize::from(b.note.is_some())
        }
        PreviewBody::Empty
        | PreviewBody::Image(_)
        | PreviewBody::Metadata
        | PreviewBody::Unavailable { .. } => 0,
    }
}

/// A find-file / content-search overlay is open on a terminal wide enough for its own preview
/// pane (PLAN 4.3): the pane then follows the overlay's highlighted result, not the cursor.
fn overlay_mode(state: &AppState) -> bool {
    matches!(state.overlays.last(), Some(Overlay::Finder | Overlay::Grep))
        && crate::search::geometry::shows_preview(state.size)
}

/// The highlighted result of the open search overlay, when the overlay shows a preview.
pub fn overlay_target(state: &AppState) -> Option<crate::search::PreviewTarget> {
    if overlay_mode(state) {
        crate::search::preview_target(state)
    } else {
        None
    }
}

fn wanted_key(state: &AppState) -> Option<TargetKey> {
    if overlay_mode(state) {
        // The overlay preview is independent of the `w` toggle and of the list's cursor.
        let target = crate::search::preview_target(state)?;
        return Some(TargetKey {
            path: target.path.clone(),
            mtime: None,
            size: 0,
            metadata: false,
            show_hidden: state.view.show_hidden,
            epoch: state.preview.theme_epoch,
            focus: target.line.map_or(0, |line| focus_for(&state.config, line)),
        });
    }
    if !state.preview_visible() || state.show_drives {
        return None;
    }
    let entry = state.view.current()?;
    Some(TargetKey {
        path: state.cwd.join(&entry.name),
        mtime: entry.mtime,
        size: entry.size,
        metadata: state.preview.metadata,
        show_hidden: state.view.show_hidden,
        epoch: state.preview.theme_epoch,
        focus: 0,
    })
}

/// Lines kept below a hit so it shows with context.
const FOCUS_MARGIN: u64 = 100;
/// Hits within one bucket share a window, so moving between them does not read the file again.
const FOCUS_BUCKET: u64 = 1_000;
/// Assumed bytes per line when deciding whether the head reaches a hit.
const FOCUS_BYTES_PER_LINE: u64 = 256;
/// Byte cap of a focused window read (the head cap, but at least this).
const FOCUS_MIN_BYTES: u64 = 1024 * 1024;

/// Centre line of the window to read for a content-search hit on `line`, or 0 when the head
/// preview within the configured limits already reaches it.
fn focus_for(config: &Config, line: u64) -> u64 {
    let lines_ok = line.saturating_add(FOCUS_MARGIN) <= config.preview.max_lines as u64;
    let bytes_ok = line.saturating_mul(FOCUS_BYTES_PER_LINE) <= config.preview.max_bytes;
    if lines_ok && bytes_ok {
        return 0;
    }
    let bucket = focus_bucket(config);
    (line / bucket) * bucket + bucket / 2
}

/// Bucket size: at most [`FOCUS_BUCKET`], and at most half the window (`max_lines`), so a hit
/// (up to half a bucket from its centre) always falls inside the window, even for a small
/// `preview.max_lines`.
fn focus_bucket(config: &Config) -> u64 {
    (config.preview.max_lines as u64 / 2).clamp(2, FOCUS_BUCKET)
}

fn request_for(state: &AppState, key: &TargetKey) -> PreviewRequest {
    let cfg = &state.config.preview;
    PreviewRequest {
        path: key.path.clone(),
        kind: if key.metadata {
            PreviewKind::Metadata
        } else {
            PreviewKind::Auto
        },
        show_hidden: key.show_hidden,
        max_bytes: if key.focus > 0 {
            cfg.max_bytes.max(FOCUS_MIN_BYTES)
        } else {
            cfg.max_bytes
        },
        max_lines: cfg.max_lines,
        focus_line: (key.focus > 0).then_some(key.focus),
    }
}

/// Brings the pane in line with the cursor. Called at the end of every `update`.
pub(crate) fn sync(state: &mut AppState, effects: &mut Vec<Effect>) {
    sync_main(state, effects);
    sync_parent(state, effects);
    crate::preview_lines::validate(state);
    follow_overlay_line(state, effects);
    // A resize changes the visible rows and the hex row width: keep the scroll in range.
    clamp_scroll(state);
}

/// A content-search hit shows its line: scrolls the overlay's preview so the line sits a third
/// of the way down, once the file's result is in.
fn follow_overlay_line(state: &mut AppState, effects: &mut Vec<Effect>) {
    let Some(target) = overlay_target(state) else {
        return;
    };
    let Some(line) = target.line else {
        return;
    };
    let ready = state
        .preview
        .current
        .as_deref()
        .is_some_and(|p| p.path == target.path);
    if !ready {
        return;
    }
    let beyond = state
        .preview
        .current
        .as_deref()
        .is_some_and(|p| match &p.body {
            PreviewBody::Text(t) => usize::try_from(line).map_or(true, |l| {
                t.truncated && (l <= t.first_line || l > t.first_line + t.line_count)
            }),
            _ => false,
        });
    if beyond && state.preview.beyond_noted.as_ref() != Some(&(target.path.clone(), line)) {
        state.preview.beyond_noted = Some((target.path.clone(), line));
        crate::update::set_status(
            state,
            crate::state::Level::Info,
            format!("Line {line} is beyond the preview limit"),
            effects,
        );
    }
    let rows = state.preview_rows();
    let offset = state
        .preview
        .current
        .as_deref()
        .map_or(0, |p| match &p.body {
            PreviewBody::Text(t) => t.first_line,
            _ => 0,
        });
    let first = usize::try_from(line.saturating_sub(1))
        .unwrap_or(usize::MAX)
        .saturating_sub(offset);
    let wanted = first.saturating_sub(rows / 3).min(max_scroll(state));
    if state.preview.scroll != wanted {
        state.preview.scroll = wanted;
        state.dirty = true;
    }
}

/// Largest valid scroll offset for the current result at the current size.
fn max_scroll(state: &AppState) -> usize {
    let width = state.content_width();
    state
        .preview
        .current
        .as_deref()
        .map_or(0, |p| content_len(p, width))
        .saturating_sub(state.preview_rows())
}

fn clamp_scroll(state: &mut AppState) {
    if state.preview.scroll == 0 {
        return;
    }
    let max = max_scroll(state);
    if state.preview.scroll > max {
        state.preview.scroll = max;
        state.dirty = true;
    }
}

/// Whether the stored key already stands for what the pane should show. Compares borrowed
/// parts so the common case (nothing changed) allocates nothing.
fn key_is_current(state: &AppState) -> bool {
    if overlay_mode(state) {
        let target = crate::search::preview_target(state);
        return match (&state.preview.key, target) {
            (None, None) => true,
            (Some(key), Some(target)) => {
                key.path == target.path
                    && key.mtime.is_none()
                    && !key.metadata
                    && key.show_hidden == state.view.show_hidden
                    && key.epoch == state.preview.theme_epoch
            }
            _ => false,
        };
    }
    let Some(key) = &state.preview.key else {
        return wanted_visible_entry(state).is_none();
    };
    let Some(entry) = wanted_visible_entry(state) else {
        return false;
    };
    key.path.parent() == Some(state.cwd.as_path())
        && key.path.file_name() == Some(std::ffi::OsStr::new(&entry.name))
        && key.mtime == entry.mtime
        && key.size == entry.size
        && key.metadata == state.preview.metadata
        && key.show_hidden == state.view.show_hidden
        && key.epoch == state.preview.theme_epoch
}

fn wanted_visible_entry(state: &AppState) -> Option<&crate::entry::Entry> {
    if !state.preview_visible() || state.show_drives {
        return None;
    }
    state.view.current()
}

fn sync_main(state: &mut AppState, effects: &mut Vec<Effect>) {
    if key_is_current(state) {
        return;
    }
    let wanted = wanted_key(state);
    if wanted == state.preview.key {
        return;
    }
    let same_path = match (&wanted, &state.preview.key) {
        (Some(a), Some(b)) => a.path == b.path && a.metadata == b.metadata,
        _ => false,
    };
    let preview = &mut state.preview;
    preview.gen.bump();
    state.dirty = true;
    match wanted {
        Some(key) => {
            // The same file refreshed (it changed on disk): keep the old content on screen
            // and the scroll position until the new result arrives.
            if !same_path {
                preview.current = None;
                preview.scroll = 0;
            }
            preview.target = Some(key.path.clone());
            preview.loading = true;
            let gen = preview.gen.current();
            let request = request_for(state, &key);
            state.preview.key = Some(key);
            effects.push(Effect::RequestPreview { gen, request });
        }
        None => {
            let had_request = preview.key.take().is_some();
            preview.target = None;
            preview.current = None;
            preview.loading = false;
            preview.scroll = 0;
            if had_request {
                effects.push(Effect::CancelPreview);
            }
        }
    }
}

fn sync_parent(state: &mut AppState, effects: &mut Vec<Effect>) {
    let wanted = if state.pane_widths().has_parent() {
        state.cwd.parent().map(|parent| ParentKey {
            epoch: state.preview.theme_epoch,
            path: parent.to_owned(),
            show_hidden: state.view.show_hidden,
        })
    } else {
        None
    };
    if wanted == state.preview.parent_key {
        return;
    }
    state.preview.parent_gen.bump();
    state.dirty = true;
    match wanted {
        Some(key) => {
            let request = PreviewRequest {
                path: key.path.clone(),
                kind: PreviewKind::Auto,
                show_hidden: key.show_hidden,
                max_bytes: state.config.preview.max_bytes,
                max_lines: state.config.preview.max_lines,
                focus_line: None,
            };
            // The old listing stays until the new one arrives unless the directory changed.
            let same_dir = state
                .preview
                .parent_key
                .as_ref()
                .is_some_and(|old| old.path == key.path);
            if !same_dir {
                state.preview.parent = None;
            }
            state.preview.parent_key = Some(key);
            effects.push(Effect::RequestParent {
                gen: state.preview.parent_gen.current(),
                request,
            });
        }
        None => {
            let had = state.preview.parent_key.take().is_some();
            state.preview.parent = None;
            if had {
                effects.push(Effect::CancelParent);
            }
        }
    }
}

/// A file job finished: previews of the paths it created or removed (and of the directories
/// holding them) are stale even when size and mtime happen to match. The engine cache is
/// dropped for them, and the pane asks again when it is showing one of them.
pub(crate) fn invalidate_paths(
    state: &mut AppState,
    created: &[PathBuf],
    removed: &[PathBuf],
    effects: &mut Vec<Effect>,
) {
    let changed: Vec<&PathBuf> = created.iter().chain(removed).collect();
    if changed.is_empty() {
        return;
    }
    // The paths themselves and the directories holding them: a directory preview is cached per
    // directory and its mtime may not move (coarse timestamps, same-size overwrite).
    let mut paths: Vec<PathBuf> = Vec::new();
    for p in &changed {
        for q in std::iter::once(&***p).chain(p.parent()) {
            if !paths.iter().any(|known| same_path(known, q)) {
                paths.push(q.to_owned());
            }
        }
    }
    let related = |shown: &Path| {
        changed.iter().any(|p| {
            p.ancestors().any(|a| same_path(a, shown)) || shown.ancestors().any(|a| same_path(a, p))
        })
    };
    let main_affected = state.preview.target.as_deref().is_some_and(related);
    let parent_affected = state.preview.parent_key.as_ref().is_some_and(|k| {
        changed
            .iter()
            .any(|p| p.parent().is_some_and(|dir| same_path(dir, &k.path)))
            || related(&k.path)
    });
    effects.push(Effect::InvalidatePreview { paths });
    if main_affected || parent_affected {
        // A new key makes `sync` request the same file again, past the dropped cache.
        state.preview.theme_epoch = state.preview.theme_epoch.wrapping_add(1);
        state.dirty = true;
    }
}

/// Path equality as Windows sees it: ASCII case-insensitive.
fn same_path(a: &Path, b: &Path) -> bool {
    a == b || a.as_os_str().eq_ignore_ascii_case(b.as_os_str())
}

/// A main-pane result arrived.
pub(crate) fn on_preview(state: &mut AppState, msg: PreviewMsg) {
    if !state.preview.gen.is_current(msg.gen()) {
        return;
    }
    let preview = &mut state.preview;
    if matches!(msg.0.result.body, PreviewBody::Image(_)) {
        preview.image_id = preview.image_id.wrapping_add(1);
    }
    preview.current = Some(msg.0.result);
    preview.loading = false;
    clamp_scroll(state);
    state.dirty = true;
}

/// A parent-column result arrived.
pub(crate) fn on_parent(state: &mut AppState, msg: PreviewMsg) {
    if !state.preview.parent_gen.is_current(msg.gen()) {
        return;
    }
    state.preview.parent = Some(msg.0.result);
    state.dirty = true;
}

/// The actions that belong to the preview pane. Returns false for any other action.
pub(crate) fn run_action(state: &mut AppState, action: Action, effects: &mut Vec<Effect>) -> bool {
    match action {
        Action::PreviewToggle => {
            let visible = state.preview_visible();
            state.preview.user_visible = Some(!visible);
            // The pane is dropped when it would be tiny; say so instead of looking dead.
            if !visible && !state.preview_visible() {
                crate::update::set_status(
                    state,
                    crate::state::Level::Info,
                    "terminal too narrow for the preview".to_owned(),
                    effects,
                );
            }
        }
        Action::ParentColumnToggle => state.preview.parent_column = !state.preview.parent_column,
        Action::MetadataToggle => {
            state.preview.metadata = !state.preview.metadata;
            state.preview.scroll = 0;
        }
        Action::PreviewScrollDown => scroll(state, true),
        Action::PreviewScrollUp => scroll(state, false),
        _ => return false,
    }
    true
}

/// Scrolls by half the visible content rows (at least one line).
fn scroll(state: &mut AppState, down: bool) {
    let step = (state.preview_rows() / 2).max(1);
    let max = max_scroll(state);
    let scroll = &mut state.preview.scroll;
    *scroll = if down {
        (*scroll + step).min(max)
    } else {
        scroll.saturating_sub(step)
    };
}

#[cfg(test)]
mod tests {
    use std::path::Path;
    use std::time::Duration;

    use ks_preview::{Encoding, StyledLine, TextPreview};

    use super::*;
    use crate::entry::Entry;
    use crate::key::KeyChord;
    use crate::keymap::Keymap;
    use crate::msg::Msg;
    use crate::state::{Places, Size, StartParams};
    use crate::update::update;

    fn state(width: u16, height: u16) -> AppState {
        let mut state = AppState::new(StartParams {
            cwd: PathBuf::from("/proj"),
            home: None,
            places: Places::default(),
            config: Config::default(),
            keymap: Keymap::defaults(),
            size: Size::new(width, height),
            now: SystemTime::UNIX_EPOCH,
            utc_offset_secs: 0,
        });
        let rows = state.viewport_height();
        state.view.push_chunk(
            vec![
                Entry::dir("src"),
                Entry::file("a.rs", 10).with_mtime(SystemTime::UNIX_EPOCH),
                Entry::file("b.rs", 20),
                Entry::file("c.rs", 30),
            ],
            rows,
        );
        state.view.finish_loading();
        // Start on the first file; the first `update` issues the initial request.
        state.view.move_cursor(1, rows);
        state
    }

    fn key(state: &mut AppState, text: &str) -> Vec<Effect> {
        update(state, Msg::Key(text.parse::<KeyChord>().unwrap()))
    }

    fn requests(effects: &[Effect]) -> Vec<(Gen, PathBuf)> {
        effects
            .iter()
            .filter_map(|e| match e {
                Effect::RequestPreview { gen, request } => Some((*gen, request.path.clone())),
                _ => None,
            })
            .collect()
    }

    fn text_preview(path: &Path, lines: usize) -> Arc<Preview> {
        Arc::new(Preview {
            path: path.to_owned(),
            meta: None,
            body: PreviewBody::Text(TextPreview {
                lines: (0..lines)
                    .map(|i| StyledLine::plain(format!("line {i}")))
                    .collect(),
                first_line: 0,
                line_count: lines,
                total_lines: Some(lines),
                truncated: false,
                encoding: Encoding::Utf8,
                syntax: None,
                bytes_read: 0,
            }),
        })
    }

    fn deliver(state: &mut AppState, gen: Gen, path: &Path, result: Arc<Preview>) {
        update(
            state,
            Msg::Preview(PreviewMsg(PreviewEvent {
                generation: gen.raw(),
                path: path.to_owned(),
                result,
                from_cache: false,
            })),
        );
    }

    /// Drives the first sync (as `Msg::Start`'s result would) and returns its effects.
    fn first_sync(state: &mut AppState) -> Vec<Effect> {
        update(
            state,
            Msg::Clock {
                now: SystemTime::UNIX_EPOCH,
                utc_offset_secs: 0,
            },
        )
    }

    #[test]
    fn pane_widths_split_the_body() {
        let w = pane_widths(120, false, true, 40);
        assert_eq!(
            w,
            PaneWidths {
                parent: 0,
                list: 47,
                preview: 72
            }
        );
        assert_eq!(w.list + 1 + w.preview, 120);
        let w = pane_widths(160, true, true, 40);
        assert_eq!(w.parent, 28);
        assert_eq!(w.parent + 1 + w.list + 1 + w.preview, 160);
        // Without a preview the list takes everything left of the parent column.
        let w = pane_widths(100, true, false, 40);
        assert_eq!(w.parent + 1 + w.list, 100);
        assert_eq!(w.preview, 0);
        // Too narrow for a useful preview: dropped.
        assert_eq!(pane_widths(40, false, true, 40).preview, 0);
        assert_eq!(
            pane_widths(80, false, true, 40),
            PaneWidths {
                parent: 0,
                list: 31,
                preview: 48
            }
        );
    }

    #[test]
    fn the_first_cursor_position_requests_a_preview() {
        let mut s = state(120, 30);
        let effects = first_sync(&mut s);
        let reqs = requests(&effects);
        assert_eq!(reqs.len(), 1);
        assert_eq!(reqs[0].1, Path::new("/proj").join("a.rs"));
        assert!(s.preview.loading);
        assert_eq!(s.preview.target.as_deref(), Some(reqs[0].1.as_path()));
        // Nothing changed: no new request.
        let effects = first_sync(&mut s);
        assert!(requests(&effects).is_empty());
    }

    #[test]
    fn moving_the_cursor_supersedes_the_previous_request() {
        let mut s = state(120, 30);
        let first = requests(&first_sync(&mut s))[0].0;
        let moved = key(&mut s, "j");
        let second = requests(&moved);
        assert_eq!(second.len(), 1);
        assert_eq!(second[0].1, Path::new("/proj").join("b.rs"));
        assert!(second[0].0 > first);
        assert!(s.preview.current.is_none());
    }

    #[test]
    fn stale_results_are_dropped() {
        let mut s = state(120, 30);
        let first = requests(&first_sync(&mut s));
        let second = requests(&key(&mut s, "j"));
        s.dirty = false;

        let old = Path::new("/proj").join("a.rs");
        deliver(&mut s, first[0].0, &old, text_preview(&old, 5));
        assert!(
            s.preview.current.is_none(),
            "the old result must be ignored"
        );
        assert!(s.preview.loading);
        assert!(!s.dirty, "a stale result costs no redraw");

        let new = Path::new("/proj").join("b.rs");
        deliver(&mut s, second[0].0, &new, text_preview(&new, 5));
        assert!(s.preview.current.is_some());
        assert!(!s.preview.loading);
        assert!(s.dirty);
    }

    #[test]
    fn w_toggles_and_cancels() {
        let mut s = state(120, 30);
        first_sync(&mut s);
        assert!(s.preview_visible());
        let effects = key(&mut s, "w");
        assert!(!s.preview_visible());
        assert!(effects.contains(&Effect::CancelPreview));
        assert!(s.preview.target.is_none());
        let effects = key(&mut s, "w");
        assert!(s.preview_visible());
        assert_eq!(requests(&effects).len(), 1);
    }

    #[test]
    fn a_theme_switch_requests_the_current_file_again() {
        let mut s = state(120, 30);
        first_sync(&mut s);
        let effects = update(&mut s, Msg::ThemeApplied("paper".into()));
        assert_eq!(requests(&effects).len(), 1);
        // Same theme message twice still only re-requests once per message, not per update.
        let effects = key(&mut s, "i");
        assert_eq!(requests(&effects).len(), 1);
    }

    #[test]
    fn narrow_terminals_hide_the_preview_but_w_can_force_it() {
        let mut s = state(70, 24);
        let effects = first_sync(&mut s);
        assert!(!s.preview_visible());
        assert!(requests(&effects).is_empty(), "no work for a hidden pane");
        let effects = key(&mut s, "w");
        assert!(s.preview_visible());
        assert_eq!(requests(&effects).len(), 1);

        // Widening past 80 columns does not override the explicit choice.
        update(&mut s, Msg::Resize(Size::new(120, 30)));
        assert!(s.preview_visible());
        let effects = key(&mut s, "w");
        assert!(effects.contains(&Effect::CancelPreview));
        update(&mut s, Msg::Resize(Size::new(70, 24)));
        assert!(!s.preview_visible());
    }

    #[test]
    fn resizing_across_the_threshold_starts_and_stops_requests() {
        let mut s = state(120, 30);
        first_sync(&mut s);
        let effects = update(&mut s, Msg::Resize(Size::new(79, 30)));
        assert!(effects.contains(&Effect::CancelPreview));
        let effects = update(&mut s, Msg::Resize(Size::new(80, 30)));
        assert_eq!(requests(&effects).len(), 1);
    }

    #[test]
    fn i_requests_the_metadata_view_and_back() {
        let mut s = state(120, 30);
        first_sync(&mut s);
        let effects = key(&mut s, "i");
        let Some(Effect::RequestPreview { request, .. }) = effects.first() else {
            panic!("expected a request, got {effects:?}");
        };
        assert_eq!(request.kind, PreviewKind::Metadata);
        let effects = key(&mut s, "i");
        let Some(Effect::RequestPreview { request, .. }) = effects.first() else {
            panic!("expected a request, got {effects:?}");
        };
        assert_eq!(request.kind, PreviewKind::Auto);
    }

    #[test]
    fn toggling_hidden_refreshes_directory_previews() {
        let mut s = state(120, 30);
        first_sync(&mut s);
        let effects = key(&mut s, ".");
        let reqs: Vec<_> = effects
            .iter()
            .filter_map(|e| match e {
                Effect::RequestPreview { request, .. } => Some(request.show_hidden),
                _ => None,
            })
            .collect();
        assert_eq!(reqs, [true]);
    }

    #[test]
    fn scroll_moves_by_half_a_page_and_is_clamped() {
        let mut s = state(120, 30);
        let gen = requests(&first_sync(&mut s))[0].0;
        let path = Path::new("/proj").join("a.rs");
        deliver(&mut s, gen, &path, text_preview(&path, 100));
        let rows = s.preview_rows();
        assert_eq!(rows, 30 - 5 - 2);
        key(&mut s, "J");
        assert_eq!(s.preview.scroll, rows / 2);
        key(&mut s, "K");
        key(&mut s, "K");
        assert_eq!(s.preview.scroll, 0);
        for _ in 0..50 {
            key(&mut s, "J");
        }
        assert_eq!(s.preview.scroll, 100 - rows);
    }

    #[test]
    fn scroll_resets_per_file_but_not_on_refresh_of_the_same_file() {
        let mut s = state(120, 30);
        let gen = requests(&first_sync(&mut s))[0].0;
        let path = Path::new("/proj").join("a.rs");
        deliver(&mut s, gen, &path, text_preview(&path, 100));
        key(&mut s, "J");
        let scrolled = s.preview.scroll;
        assert!(scrolled > 0);

        // The file changes on disk: same path, new mtime. The pane keeps its place.
        let rows = s.viewport_height();
        let mut fresh = s.view.entries().to_vec();
        fresh[1] = fresh[1]
            .clone()
            .with_mtime(SystemTime::UNIX_EPOCH + Duration::from_secs(5));
        s.view.replace_entries(fresh, rows);
        let effects = first_sync(&mut s);
        assert_eq!(requests(&effects).len(), 1);
        assert_eq!(s.preview.scroll, scrolled);
        assert!(
            s.preview.current.is_some(),
            "old content stays until the new arrives"
        );

        // Another file starts at the top.
        let effects = key(&mut s, "j");
        assert_eq!(requests(&effects).len(), 1);
        assert_eq!(s.preview.scroll, 0);
    }

    #[test]
    fn parent_column_requests_the_parent_directory() {
        let mut s = state(160, 40);
        let effects = first_sync(&mut s);
        assert!(!effects
            .iter()
            .any(|e| matches!(e, Effect::RequestParent { .. })));
        let effects = key(&mut s, "P");
        let parent = effects.iter().find_map(|e| match e {
            Effect::RequestParent { request, .. } => Some(request.path.clone()),
            _ => None,
        });
        assert_eq!(parent.as_deref(), Some(Path::new("/")));
        let effects = key(&mut s, "P");
        assert!(effects.contains(&Effect::CancelParent));
        assert!(s.preview.parent.is_none());
    }

    #[test]
    fn the_parent_column_needs_a_wide_terminal() {
        let mut s = state(90, 30);
        key(&mut s, "P");
        assert!(!s.pane_widths().has_parent());
        update(&mut s, Msg::Resize(Size::new(140, 30)));
        assert!(s.pane_widths().has_parent());
    }

    #[test]
    fn the_drives_view_has_no_preview() {
        let mut s = state(120, 30);
        first_sync(&mut s);
        s.show_drives = true;
        let effects = first_sync(&mut s);
        assert!(effects.contains(&Effect::CancelPreview));
        assert!(!s.preview_visible());
    }

    #[test]
    fn a_new_image_changes_the_opaque_handle() {
        let mut s = state(120, 30);
        let gen = requests(&first_sync(&mut s))[0].0;
        let path = Path::new("/proj").join("a.rs");
        let image = Arc::new(Preview {
            path: path.clone(),
            meta: None,
            body: PreviewBody::Image(ks_preview::ImagePreview {
                image: Arc::new(image::DynamicImage::new_rgb8(1, 1)),
                width: 1,
                height: 1,
                format: "PNG".into(),
                downscaled: false,
            }),
        });
        let before = s.preview.image_id;
        deliver(&mut s, gen, &path, image);
        assert_ne!(s.preview.image_id, before);
    }

    fn hex_preview(path: &Path, bytes: usize) -> Arc<Preview> {
        Arc::new(Preview {
            path: path.to_owned(),
            meta: None,
            body: PreviewBody::Binary(ks_preview::BinaryPreview {
                rows: vec![ks_preview::HexRow {
                    offset: 0,
                    bytes: vec![7; bytes],
                }],
                total_size: bytes as u64,
                type_hint: None,
                note: None,
            }),
        })
    }

    #[test]
    fn hex_scroll_limit_uses_the_content_width_the_renderer_draws_into() {
        // A terminal whose preview pane is 76 columns: the content (pane minus padding) is
        // 74, which is narrower than a 16-byte row, so the view shows 8 bytes per row.
        let width = (80..200)
            .find(|w| {
                let s = state(*w, 24);
                s.pane_widths().preview == 76
            })
            .expect("some width gives a 76 column pane");
        let mut s = state(width, 24);
        let gen = requests(&first_sync(&mut s))[0].0;
        let path = Path::new("/proj").join("a.rs");
        deliver(&mut s, gen, &path, hex_preview(&path, 800));
        assert_eq!(s.content_width(), 74);
        assert_eq!(hex_bytes_per_row(s.content_width()), 8);
        for _ in 0..40 {
            key(&mut s, "J");
        }
        assert_eq!(s.preview.scroll, 100 - s.preview_rows());
    }

    #[test]
    fn a_resize_keeps_the_scroll_in_range() {
        let mut s = state(120, 24);
        let gen = requests(&first_sync(&mut s))[0].0;
        let path = Path::new("/proj").join("a.rs");
        deliver(&mut s, gen, &path, text_preview(&path, 100));
        for _ in 0..40 {
            key(&mut s, "J");
        }
        let at_bottom = s.preview.scroll;
        assert_eq!(at_bottom, 100 - s.preview_rows());
        update(&mut s, Msg::Resize(Size::new(120, 48)));
        assert_eq!(s.preview.scroll, 100 - s.preview_rows());
        assert!(s.preview.scroll < at_bottom);
    }

    #[test]
    fn w_on_a_tiny_terminal_says_why_nothing_appeared() {
        let mut s = state(40, 24);
        first_sync(&mut s);
        key(&mut s, "w");
        assert!(!s.preview_visible());
        let status = s.status.as_ref().expect("a status message");
        assert!(status.text.contains("too narrow"), "{}", status.text);
    }

    #[test]
    fn config_layout_reaches_the_pane_geometry() {
        let config = Config::from_toml(
            "[layout]
list_percent = 30
parent_column = true",
        )
        .unwrap();
        let mut s = state(160, 24);
        s.config = config;
        s.preview = PreviewState::new(&s.config);
        let w = s.pane_widths();
        assert!(w.has_parent());
        assert!(w.list < w.preview, "30% list: {w:?}");
    }

    #[test]
    fn a_finished_job_on_the_shown_file_drops_its_cached_preview_and_asks_again() {
        let mut s = state(120, 30);
        let first = update(&mut s, Msg::Tick);
        assert_eq!(requests(&first).len(), 1);
        let shown = s.cwd.join("a.rs");
        let mut effects = Vec::new();
        invalidate_paths(&mut s, std::slice::from_ref(&shown), &[], &mut effects);
        assert_eq!(
            effects,
            vec![Effect::InvalidatePreview {
                paths: vec![shown.clone(), s.cwd.clone()]
            }]
        );
        let again = update(&mut s, Msg::Tick);
        assert_eq!(requests(&again).len(), 1, "{again:?}");

        // An unrelated path only drops the cache.
        let mut effects = Vec::new();
        let other = s.cwd.join("zzz.rs");
        invalidate_paths(&mut s, &[other], &[], &mut effects);
        assert_eq!(effects.len(), 1);
        assert!(requests(&update(&mut s, Msg::Tick)).is_empty());
    }

    #[test]
    fn invalidation_matches_deep_and_differently_cased_paths() {
        let mut s = state(120, 30);
        update(&mut s, Msg::Tick);
        let shown = s.preview.target.clone().expect("a target");
        // A removed ancestor of the shown path, in another case.
        let upper = PathBuf::from(shown.parent().unwrap().to_string_lossy().to_uppercase());
        let epoch = s.preview.theme_epoch;
        invalidate_paths(&mut s, &[], &[upper], &mut Vec::new());
        assert_ne!(s.preview.theme_epoch, epoch);

        // A change in the parent column's directory refreshes that column.
        let mut s = state(160, 40);
        first_sync(&mut s);
        key(&mut s, "P");
        let before = s.preview.parent_key.clone();
        assert!(before.is_some(), "the parent column is shown");
        let sibling = s.cwd.parent().unwrap().join("new-dir");
        invalidate_paths(&mut s, &[sibling], &[], &mut Vec::new());
        let again = update(&mut s, Msg::Tick);
        assert!(
            again
                .iter()
                .any(|e| matches!(e, Effect::RequestParent { .. })),
            "{again:?}"
        );
    }
}
