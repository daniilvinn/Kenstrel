#![allow(clippy::single_range_in_vec_init)]

//! Snapshots and unit tests of the filter, find-file and content-search screens.

use std::path::PathBuf;

use ks_core::{update, AppState, Effect, KeyChord, Msg};
use ks_search::{EntryKind, FileMatches, FinderResult, GrepEvent, GrepStats, LineMatch};
use ratatui::backend::TestBackend;
use ratatui::Terminal;

use super::*;
use crate::render::render;
use crate::testutil::{dump, listing_state};
use crate::theme::{ColorDepth, Theme};

const SIZES: [(u16, u16); 2] = [(80, 24), (160, 48)];
const THEMES: [&str; 2] = ["ember", "paper"];

fn theme(name: &str) -> Theme {
    Theme::builtin(name, ColorDepth::TrueColor).unwrap()
}

fn draw(state: &AppState, theme: &Theme) -> String {
    let backend = TestBackend::new(state.size.width, state.size.height);
    let mut terminal = Terminal::new(backend).unwrap();
    terminal.draw(|f| render(f, state, theme)).unwrap();
    dump(terminal.backend().buffer())
}

fn text_only(state: &AppState) -> String {
    draw(state, &theme("ember"))
        .split("--- styles")
        .next()
        .unwrap()
        .to_owned()
}

fn key(state: &mut AppState, text: &str) -> Vec<Effect> {
    update(state, Msg::Key(text.parse::<KeyChord>().unwrap()))
}

/// Types `text`, returning the effects of the last key.
fn type_text(state: &mut AppState, text: &str) -> Vec<Effect> {
    let mut last = Vec::new();
    for c in text.chars() {
        last = update(state, Msg::Key(KeyChord::char(c)));
    }
    last
}

fn root() -> PathBuf {
    PathBuf::from("/home/me/projects/omniforce/src/renderer")
}

// ----- filter -----

fn filter_state(w: u16, h: u16, keep: bool) -> AppState {
    let mut s = listing_state(w, h);
    key(&mut s, "/");
    type_text(&mut s, "rs");
    if keep {
        key(&mut s, "enter");
    }
    s
}

#[test]
fn filter_mode_snapshots() {
    for name in THEMES {
        for (w, h) in SIZES {
            let s = filter_state(w, h, false);
            insta::assert_snapshot!(
                format!("filter_typing_{name}_{w}x{h}"),
                draw(&s, &theme(name))
            );
            let s = filter_state(w, h, true);
            insta::assert_snapshot!(
                format!("filter_kept_{name}_{w}x{h}"),
                draw(&s, &theme(name))
            );
        }
    }
}

#[test]
fn filter_without_matches_says_so() {
    let mut s = listing_state(80, 24);
    key(&mut s, "/");
    type_text(&mut s, "zzzz");
    let text = text_only(&s);
    assert!(text.contains("no entries match /zzzz"), "{text}");
}

#[test]
fn filter_highlights_matched_characters_in_accent_bold() {
    let s = filter_state(80, 24, true);
    let th = theme("ember");
    let backend = TestBackend::new(80, 24);
    let mut terminal = Terminal::new(backend).unwrap();
    terminal.draw(|f| render(f, &s, &th)).unwrap();
    let buffer = terminal.backend().buffer();
    let bold_accent_cells = (2..21u16)
        .flat_map(|y| (5..60u16).map(move |x| (x, y)))
        .filter(|&(x, y)| {
            let cell = &buffer[(x, y)];
            cell.fg == th.accent && cell.modifier.contains(Modifier::BOLD) && cell.symbol() != " "
        })
        .count();
    assert!(
        bold_accent_cells >= 2,
        "matched characters should stand out"
    );
}

// ----- find file -----

fn finder_result(rel: &str, indices: &[u32]) -> FinderResult {
    FinderResult {
        rel: rel.to_owned(),
        path: root().join(rel),
        kind: EntryKind::File,
        indices: indices.to_vec(),
    }
}

fn finder_state(w: u16, h: u16, walking: bool) -> AppState {
    let mut s = listing_state(w, h);
    let effects = key(&mut s, "ctrl-p");
    let gen = effects
        .iter()
        .find_map(|e| match e {
            Effect::FinderStart { gen, .. } => Some(*gen),
            _ => None,
        })
        .unwrap();
    type_text(&mut s, "rend");
    let results = vec![
        finder_result("src/renderer/vulkan_device.rs", &[4, 5, 6, 7]),
        finder_result("src/renderer/render_graph.rs", &[4, 5, 6, 7]),
        finder_result("src/renderer/passes/pass_a.rs", &[4, 5, 6, 7]),
        finder_result("docs/renderer.md", &[5, 6, 7, 8]),
        finder_result(
            "docs/render-notes/backend/an/extremely/deeply/nested/path/vulkan_renderer_overview.md",
            &[71, 72, 73, 74],
        ),
        finder_result("tools/extend.rs", &[]),
    ];
    let matched = if walking { 1_234 } else { 6 };
    let snapshot = ks_core::FinderSnapshot {
        query: "rend".into(),
        root: root(),
        matched,
        total: 58_910,
        walking,
        from_cache: false,
        denied: 0,
        offset: 0,
        results,
    };
    update(
        &mut s,
        Msg::FinderResults {
            gen,
            snapshot: Box::new(snapshot),
        },
    );
    key(&mut s, "tab");
    if walking {
        s.spinner_frame = 3;
    }
    s
}

#[test]
fn finder_snapshots() {
    for name in THEMES {
        for (w, h) in SIZES {
            let s = finder_state(w, h, true);
            insta::assert_snapshot!(
                format!("finder_walking_{name}_{w}x{h}"),
                draw(&s, &theme(name))
            );
            let s = finder_state(w, h, false);
            insta::assert_snapshot!(
                format!("finder_done_{name}_{w}x{h}"),
                draw(&s, &theme(name))
            );
        }
    }
}

#[test]
fn finder_empty_states() {
    let mut s = listing_state(80, 24);
    key(&mut s, "ctrl-p");
    let text = text_only(&s);
    assert!(text.contains("walking…"), "{text}");
    assert!(text.contains("type to find a file"), "{text}");
}

#[test]
fn finder_shows_toggle_and_scope_state() {
    let mut s = finder_state(80, 24, false);
    let text = text_only(&s);
    assert!(text.contains("scope cwd"), "{text}");
    assert!(text.contains("hidden off"), "{text}");
    assert!(text.contains("ignored off"), "{text}");
    key(&mut s, "ctrl-r");
    key(&mut s, "alt-.");
    key(&mut s, "alt-g");
    let text = text_only(&s);
    assert!(text.contains("scope git root"), "{text}");
    assert!(text.contains("hidden on"), "{text}");
    assert!(text.contains("ignored on"), "{text}");
}

#[test]
fn finder_counter_uses_thousands_separators_and_a_spinner() {
    let s = finder_state(80, 24, true);
    let text = text_only(&s);
    assert!(text.contains("1,234 / 58,910"), "{text}");
    assert!(text.contains('⠸'), "{text}");
    let s = finder_state(80, 24, false);
    assert!(!text_only(&s).contains('⠸'));
}

#[test]
fn preview_rect_exists_only_on_wide_terminals() {
    let s = finder_state(80, 24, false);
    assert_eq!(preview_rect(&s), None);
    let s = finder_state(160, 48, false);
    let rect = preview_rect(&s).expect("a preview area");
    assert!(rect.width > 40);
    let s = listing_state(160, 48);
    assert_eq!(preview_rect(&s), None, "no search overlay is open");
}

// ----- content search -----

fn hit(n: u64, text: &str, ranges: &[std::ops::Range<usize>]) -> LineMatch {
    LineMatch {
        line_number: n,
        text: text.to_owned(),
        ranges: ranges.to_vec(),
    }
}

/// Opens the overlay, types `query`, lets the debounce fire and returns the search generation.
fn start_grep(s: &mut AppState, query: &str) -> u64 {
    key(s, "ctrl-f");
    let effects = type_text(s, query);
    let timer = effects
        .iter()
        .find_map(|e| match e {
            Effect::StartTimer { gen, .. } => Some(*gen),
            _ => None,
        })
        .unwrap();
    let started = update(
        s,
        Msg::Timer {
            kind: ks_core::TimerKind::GrepDebounce,
            gen: timer,
        },
    );
    started
        .iter()
        .find_map(|e| match e {
            Effect::GrepStart { gen, .. } => Some(*gen),
            _ => None,
        })
        .unwrap()
}

fn grep_state(w: u16, h: u16, capped: bool) -> AppState {
    grep_state_with(w, h, capped, true)
}

/// `chip`: switch whole-word on (which restarts the search, so its results are dropped).
fn grep_state_with(w: u16, h: u16, capped: bool, chip: bool) -> AppState {
    let mut s = listing_state(w, h);
    let gen = start_grep(&mut s, "handle");
    let files = vec![
        FileMatches {
            path: root().join("vulkan_device.rs"),
            rel: PathBuf::from("vulkan_device.rs"),
            lines: vec![
                hit(14, "    pub fn handle(&self) -> vk::Device {", &[11..17]),
                hit(
                    88,
                    "        let handle = self.logical.handle();",
                    &[12..18, 33..39],
                ),
            ],
        },
        FileMatches {
            path: root().join("passes/pass_a.rs"),
            rel: PathBuf::from("passes/pass_a.rs"),
            lines: vec![hit(
                3,
                "                                                  // a very long line where the handle is far to the right of the viewport",
                &[81..87],
            )],
        },
        FileMatches {
            path: root().join("日本語のメモ.txt"),
            rel: PathBuf::from("日本語のメモ.txt"),
            lines: vec![hit(1, "ハンドル handle メモ", &[13..19])],
        },
    ];
    update(
        &mut s,
        Msg::Grep(GrepEvent::Matches {
            generation: gen,
            files,
        }),
    );
    let stats = GrepStats {
        files_searched: 1_203,
        files_with_matches: 3,
        matches: 4,
        skipped_cloud: 2,
        skipped_large: 0,
        binary: 5,
        denied: 1,
        errors: 0,
    };
    if capped {
        update(
            &mut s,
            Msg::Grep(GrepEvent::Done {
                generation: gen,
                stats,
                capped: true,
            }),
        );
    } else {
        update(
            &mut s,
            Msg::Grep(GrepEvent::Progress {
                generation: gen,
                stats,
            }),
        );
        s.spinner_frame = 2;
    }
    // Highlight the second hit and switch a toggle on so a chip is visible.
    key(&mut s, "down");
    if chip {
        key(&mut s, "ctrl-w");
    }
    s
}

#[test]
fn grep_snapshots() {
    for name in THEMES {
        for (w, h) in SIZES {
            let s = grep_state(w, h, false);
            insta::assert_snapshot!(
                format!("grep_running_{name}_{w}x{h}"),
                draw(&s, &theme(name))
            );
            let s = grep_state(w, h, true);
            insta::assert_snapshot!(
                format!("grep_capped_{name}_{w}x{h}"),
                draw(&s, &theme(name))
            );
        }
    }
}

#[test]
fn grep_marked_hits_snapshots() {
    for name in THEMES {
        for (w, h) in SIZES {
            let mut s = grep_state(w, h, false);
            // Mark the highlighted hit (the cursor moves down) and the next one.
            key(&mut s, "alt-m");
            key(&mut s, "alt-m");
            let marks = s.search.grep.as_ref().unwrap().marks.len();
            assert_eq!(marks, 2);
            insta::assert_snapshot!(
                format!("grep_marked_{name}_{w}x{h}"),
                draw(&s, &theme(name))
            );
        }
    }
}

fn glob_focused_state(w: u16, h: u16) -> AppState {
    let mut s = grep_state(w, h, false);
    key(&mut s, "ctrl-r");
    key(&mut s, "tab");
    type_text(&mut s, "*.rs");
    s
}

#[test]
fn a_glob_without_search_text_explains_that_text_is_needed() {
    let mut s = listing_state(80, 24);
    key(&mut s, "ctrl-f");
    assert!(text_only(&s).contains("type to search file contents"));
    key(&mut s, "tab");
    type_text(&mut s, "*.rs");
    let text = text_only(&s);
    assert!(
        text.contains("type text to search the files the glob matches"),
        "{text}"
    );
}

#[test]
fn grep_glob_field_and_chips_snapshots() {
    for name in THEMES {
        for (w, h) in SIZES {
            let s = glob_focused_state(w, h);
            let text = draw(&s, &theme(name));
            assert!(text.contains("*.rs"), "{text}");
            insta::assert_snapshot!(format!("grep_glob_{name}_{w}x{h}"), text);
        }
    }
}

fn results_list_state(w: u16, h: u16, finished: bool) -> AppState {
    let mut s = grep_state_with(w, h, false, false);
    let gen = s.search.grep_gen;
    if finished {
        update(
            &mut s,
            Msg::Grep(GrepEvent::Done {
                generation: gen,
                stats: GrepStats::default(),
                capped: false,
            }),
        );
    }
    key(&mut s, "ctrl-l");
    s
}

#[test]
fn results_list_snapshots() {
    for name in THEMES {
        for (w, h) in SIZES {
            let s = results_list_state(w, h, true);
            let text = draw(&s, &theme(name));
            assert!(text.contains("Search results: handle"), "{text}");
            assert!(!text.contains("(partial)"), "{text}");
            insta::assert_snapshot!(format!("results_list_{name}_{w}x{h}"), text);
        }
    }
}

#[test]
fn unfinished_search_marks_the_results_list_partial() {
    let s = results_list_state(80, 24, false);
    assert!(text_only(&s).contains("(partial)"));
}

#[test]
fn grep_footer_reports_counts_skips_and_the_cap() {
    // Toggling the whole-word chip after the results restarts the search, which clears
    // them; look at the footer before that.
    let mut s = listing_state(160, 48);
    let gen = start_grep(&mut s, "handle");
    update(
        &mut s,
        Msg::Grep(GrepEvent::Matches {
            generation: gen,
            files: vec![FileMatches {
                path: root().join("a.rs"),
                rel: PathBuf::from("a.rs"),
                lines: vec![
                    hit(1, "handle", &[0..6]),
                    hit(2, "handle", &[0..6]),
                    hit(3, "handle", &[0..6]),
                    hit(4, "handle", &[0..6]),
                ],
            }],
        }),
    );
    update(
        &mut s,
        Msg::Grep(GrepEvent::Done {
            generation: gen,
            stats: GrepStats {
                files_searched: 1_203,
                skipped_cloud: 2,
                denied: 1,
                binary: 5,
                ..GrepStats::default()
            },
            capped: true,
        }),
    );
    let text = text_only(&s);
    assert!(text.contains("4 matches in 1 file"), "{text}");
    assert!(text.contains("searched 1,203"), "{text}");
    assert!(
        text.contains("skipped 2 cloud, 1 denied, 5 binary"),
        "{text}"
    );
    assert!(
        text.contains("10,000+ matches: refine your search"),
        "{text}"
    );
}

#[test]
fn grep_scrolls_a_far_match_into_view() {
    let s = grep_state(80, 24, false);
    let text = text_only(&s);
    assert!(text.contains('…'), "{text}");
    assert!(text.contains("handle is far"), "{text}");
}

#[test]
fn grep_error_replaces_the_footer() {
    let mut s = listing_state(80, 24);
    key(&mut s, "ctrl-f");
    key(&mut s, "ctrl-r");
    let effects = type_text(&mut s, "(");
    let timer = effects
        .iter()
        .find_map(|e| match e {
            Effect::StartTimer { gen, .. } => Some(*gen),
            _ => None,
        })
        .unwrap();
    let started = update(
        &mut s,
        Msg::Timer {
            kind: ks_core::TimerKind::GrepDebounce,
            gen: timer,
        },
    );
    let gen = started
        .iter()
        .find_map(|e| match e {
            Effect::GrepStart { gen, .. } => Some(*gen),
            _ => None,
        })
        .unwrap();
    update(
        &mut s,
        Msg::Grep(GrepEvent::Error {
            generation: gen,
            message: "invalid pattern: unclosed group".into(),
        }),
    );
    assert!(text_only(&s).contains("invalid pattern: unclosed group"));
}

// ----- helpers -----

#[test]
fn grouped_inserts_thousands_separators() {
    assert_eq!(grouped(0), "0");
    assert_eq!(grouped(999), "999");
    assert_eq!(grouped(1_000), "1,000");
    assert_eq!(grouped(58_910), "58,910");
    assert_eq!(grouped(1_234_567), "1,234,567");
}

#[test]
fn highlighted_splits_runs_by_style() {
    let base = Style::new();
    let hl = Style::new().add_modifier(Modifier::BOLD);
    let spans = highlighted("abcdef", &[1, 2, 4], base, hl);
    let parts: Vec<(&str, bool)> = spans
        .iter()
        .map(|s| (s.content.as_ref(), s.style == hl))
        .collect();
    assert_eq!(
        parts,
        [
            ("a", false),
            ("bc", true),
            ("d", false),
            ("e", true),
            ("f", false)
        ]
    );
    assert!(highlighted("", &[], base, hl).is_empty());
}

#[test]
fn long_paths_keep_the_file_name() {
    let th = theme("ember");
    let spans = path_spans(
        "a/very/long/directory/chain/file.rs",
        &[],
        16,
        &th,
        Style::new(),
    );
    let text: String = spans.iter().map(|s| s.content.as_ref()).collect();
    assert!(text.starts_with('…'));
    assert!(text.ends_with("file.rs"));
    assert!(width(&text) <= 16, "{text}");
}

// ----- the preview pane inside the overlays -----

/// Makes a 150-line text file the engine result for the highlighted result of the open
/// overlay and scrolls like `ks_core::preview` does for a content-search hit.
fn with_overlay_preview(s: AppState) -> AppState {
    with_overlay_window(s, 0)
}

/// Like [`with_overlay_preview`], but the engine returned a window that starts after the first
/// `first_line` lines of the file (a hit beyond the head limits).
fn with_overlay_window(mut s: AppState, first_line: usize) -> AppState {
    use ks_preview::{Encoding, Preview, PreviewBody, StyledLine, TextPreview};
    let target = ks_core::preview_target(&s).expect("a highlighted result");
    let lines = 150;
    s.preview.target = Some(target.path.clone());
    s.preview.current = Some(std::sync::Arc::new(Preview {
        path: target.path.clone(),
        meta: None,
        body: PreviewBody::Text(TextPreview {
            lines: (0..lines)
                .map(|i| StyledLine::plain(format!("    let value_{i} = compute({i});")))
                .collect(),
            first_line,
            line_count: lines,
            total_lines: if first_line > 0 { None } else { Some(lines) },
            truncated: first_line > 0,
            encoding: Encoding::Utf8,
            syntax: None,
            bytes_read: 0,
        }),
    }));
    s.preview.loading = false;
    if let Some(line) = target.line {
        let first = usize::try_from(line - 1).unwrap();
        s.preview.scroll = first
            .saturating_sub(first_line)
            .saturating_sub(s.preview_rows() / 3);
    }
    s
}

#[test]
fn overlay_preview_snapshots() {
    for name in THEMES {
        let s = with_overlay_preview(finder_state(160, 48, false));
        insta::assert_snapshot!(
            format!("finder_preview_{name}_160x48"),
            draw(&s, &theme(name))
        );
        let s = with_overlay_preview(grep_state(160, 48, false));
        insta::assert_snapshot!(
            format!("grep_preview_{name}_160x48"),
            draw(&s, &theme(name))
        );
    }
}

#[test]
fn window_preview_snapshots() {
    for name in THEMES {
        let s = with_overlay_window(grep_state(160, 48, false), 40);
        insta::assert_snapshot!(
            format!("grep_window_preview_{name}_160x48"),
            draw(&s, &theme(name))
        );
    }
}

#[test]
fn the_grep_hit_line_is_marked_in_the_preview() {
    let s = with_overlay_preview(grep_state(160, 48, false));
    let text = text_only(&s);
    assert!(text.contains("line 88"), "{text}");
    assert!(text.contains("let value_87 = compute(87);"), "{text}");
    assert!(!text.contains("preview: wired by P2"), "{text}");
}

#[test]
fn finder_footer_counts_access_denied_skips() {
    let mut s = finder_state(160, 48, false);
    assert!(!text_only(&s).contains("denied"));
    s.search.finder.as_mut().unwrap().denied = 1_204;
    let text = text_only(&s);
    assert!(text.contains("1,204 denied"), "{text}");
}

#[test]
fn finder_denied_footer_snapshots_at_80x24() {
    for name in THEMES {
        let mut s = finder_state(80, 24, false);
        s.search.finder.as_mut().unwrap().denied = 1_204;
        insta::assert_snapshot!(
            format!("finder_denied_{name}_80x24"),
            draw(&s, &theme(name))
        );
    }
}

#[test]
fn path_spans_measure_zwj_sequences_as_one_cluster_and_highlight_it() {
    let t = theme("ember");
    // A family emoji is 2 cells; the over-counting char sum made it 6 and cut it early.
    let rel = "dir/👨‍👩‍👧.txt";
    let spans = path_spans(rel, &[], 20, &t, Style::new());
    let text: String = spans.iter().map(|s| s.content.as_ref()).collect();
    assert_eq!(text, rel);
    assert!(width(&text) <= 20);
    // A match on any char of the cluster highlights the whole cluster, never splitting it.
    let spans = path_spans(rel, &[6], 20, &t, Style::new());
    let hl: Vec<_> = spans
        .iter()
        .filter(|s| s.style == t.bold_accent())
        .map(|s| s.content.to_string())
        .collect();
    assert_eq!(hl, vec!["👨‍👩‍👧".to_owned()]);
    // Cut from the left, the result still fits the budget.
    let cut = path_spans(rel, &[], 8, &t, Style::new());
    let text: String = cut.iter().map(|s| s.content.as_ref()).collect();
    assert!(width(&text) <= 8, "{text}");
}
