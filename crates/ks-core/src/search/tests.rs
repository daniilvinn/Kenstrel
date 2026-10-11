#![allow(clippy::single_range_in_vec_init)]

//! `update()` tests for the filter, find file and content search state machines.

use std::path::PathBuf;
use std::time::SystemTime;

use ks_search::{
    EntryKind as WalkKind, FileMatches, FinderResult, GrepEvent, GrepStats, LineMatch, PathChange,
    Scope,
};

use super::*;
use crate::config::Config;
use crate::keymap::Keymap;
use crate::mode::Overlay;
use crate::msg::{Msg, TimerKind};
use crate::state::{Places, Size, StartParams};
use crate::update::update;

fn cwd() -> PathBuf {
    PathBuf::from(if cfg!(windows) { "C:\\proj" } else { "/proj" })
}

fn state_with(names: &[&str]) -> AppState {
    let mut state = AppState::new(StartParams {
        cwd: cwd(),
        home: None,
        places: Places::default(),
        config: Config::default(),
        keymap: Keymap::defaults(),
        size: Size::new(80, 24),
        now: SystemTime::UNIX_EPOCH,
        utc_offset_secs: 0,
    });
    let height = state.viewport_height();
    state
        .view
        .push_chunk(names.iter().map(|n| Entry::file(*n, 1)).collect(), height);
    state.view.finish_loading();
    state.dirty = false;
    state
}

fn key(state: &mut AppState, text: &str) -> Vec<Effect> {
    update(state, Msg::Key(text.parse().unwrap()))
}

fn type_text(state: &mut AppState, text: &str) -> Vec<Effect> {
    let mut all = Vec::new();
    for c in text.chars() {
        let k = if c == ' ' {
            "space".to_owned()
        } else {
            c.to_string()
        };
        all.extend(key(state, &k));
    }
    all
}

fn shown(state: &AppState) -> Vec<String> {
    (0..state.view.len())
        .map(|i| state.view.row(i).unwrap().display_name().into_owned())
        .collect()
}

fn cursor_name(state: &AppState) -> String {
    state.view.current().unwrap().display_name().into_owned()
}

// ----- filter -----

#[test]
fn slash_opens_filter_mode_and_typing_narrows_the_list() {
    let mut s = state_with(&["main.rs", "lib.rs", "README.md", "Cargo.toml"]);
    key(&mut s, "/");
    assert_eq!(s.mode, Mode::Filter);
    type_text(&mut s, "rs");
    assert_eq!(shown(&s).len(), 2);
    assert!(shown(&s).contains(&"main.rs".to_owned()));
    assert!(
        !s.view.highlight(0).is_empty(),
        "matched chars are reported"
    );
    // Every keystroke refilters, no debounce: backspace widens again.
    key(&mut s, "backspace");
    assert!(shown(&s).len() >= 2);
    key(&mut s, "backspace");
    assert_eq!(shown(&s).len(), 4, "an empty query shows everything");
    assert_eq!(s.view.filter_query(), None);
}

#[test]
fn enter_keeps_the_filter_and_esc_clears_it() {
    let mut s = state_with(&["main.rs", "lib.rs", "README.md"]);
    key(&mut s, "/");
    type_text(&mut s, "rs");
    key(&mut s, "enter");
    assert_eq!(s.mode, Mode::Normal);
    assert_eq!(s.view.filter_query(), Some("rs"));
    assert_eq!(shown(&s).len(), 2);

    // Navigation works on the filtered list.
    key(&mut s, "j");
    assert_eq!(s.view.cursor.index, 1);
    key(&mut s, "j");
    assert_eq!(s.view.cursor.index, 1, "cannot move past the last match");
    let name = cursor_name(&s);

    key(&mut s, "esc");
    assert_eq!(s.view.filter_query(), None);
    assert_eq!(shown(&s).len(), 3);
    assert_eq!(cursor_name(&s), name, "the cursor stays on its entry");
}

#[test]
fn esc_in_filter_mode_clears_and_leaves_the_mode() {
    let mut s = state_with(&["a.txt", "b.txt"]);
    key(&mut s, "/");
    type_text(&mut s, "a");
    key(&mut s, "esc");
    assert_eq!(s.mode, Mode::Normal);
    assert_eq!(shown(&s).len(), 2);
}

#[test]
fn a_filter_opened_from_visual_mode_leaves_no_hidden_range() {
    let mut s = state_with(&["a", "b", "c", "d", "e"]);
    key(&mut s, "v");
    key(&mut s, "/");
    assert!(!s.view.in_visual_mode(), "the prompt replaces visual mode");
    type_text(&mut s, "a");
    key(&mut s, "enter");
    let before = s.view.marks_count();
    key(&mut s, "j");
    key(&mut s, "j");
    assert_eq!(
        s.view.marks_count(),
        before,
        "normal-mode motion marks nothing"
    );
}

#[test]
fn esc_in_visual_mode_over_a_kept_filter_ends_the_range_then_clears_the_filter() {
    let mut s = state_with(&["a1", "a2", "a3", "a4", "b"]);
    key(&mut s, "/");
    type_text(&mut s, "a");
    key(&mut s, "enter");
    key(&mut s, "v");
    assert_eq!(s.mode, Mode::Visual);
    key(&mut s, "esc");
    assert_eq!(s.mode, Mode::Normal);
    assert!(!s.view.in_visual_mode());
    assert_eq!(
        s.view.filter_query(),
        Some("a"),
        "first esc only ends visual"
    );
    let before = s.view.marks_count();
    key(&mut s, "j");
    assert_eq!(s.view.marks_count(), before);
    key(&mut s, "esc");
    assert_eq!(s.view.filter_query(), None, "second esc clears the filter");
}

#[test]
fn arrows_and_ctrl_n_p_move_inside_filter_mode() {
    let mut s = state_with(&["a1", "a2", "a3"]);
    key(&mut s, "/");
    type_text(&mut s, "a");
    key(&mut s, "down");
    key(&mut s, "ctrl-n");
    assert_eq!(s.view.cursor.index, 2);
    key(&mut s, "ctrl-p");
    key(&mut s, "up");
    assert_eq!(s.view.cursor.index, 0);
    assert_eq!(s.mode, Mode::Filter, "still typing");
}

#[test]
fn filter_survives_a_refresh_and_is_dropped_on_directory_change() {
    let mut s = state_with(&["main.rs", "lib.rs", "README.md"]);
    key(&mut s, "/");
    type_text(&mut s, "rs");
    key(&mut s, "enter");
    let height = s.viewport_height();
    s.view.replace_entries(
        vec![
            Entry::file("main.rs", 1),
            Entry::file("lib.rs", 1),
            Entry::file("new.rs", 1),
            Entry::file("note.txt", 1),
        ],
        height,
    );
    assert_eq!(s.view.filter_query(), Some("rs"));
    assert_eq!(
        shown(&s).len(),
        3,
        "the new .rs file matches, the .txt does not"
    );

    // Entering a directory builds a fresh view without the filter.
    let mut s = state_with(&["src", "main.rs"]);
    let height = s.viewport_height();
    s.view.push_chunk(vec![Entry::dir("sub")], height);
    key(&mut s, "/");
    type_text(&mut s, "sub");
    key(&mut s, "enter");
    let effects = key(&mut s, "l");
    let (gen, path) = effects
        .iter()
        .find_map(|e| match e {
            Effect::ListDir { gen, path } => Some((*gen, path.clone())),
            _ => None,
        })
        .expect("listing requested");
    assert_eq!(path, cwd().join("sub"));
    update(
        &mut s,
        Msg::ListingChunk {
            gen,
            entries: vec![Entry::file("inner.txt", 1)],
        },
    );
    assert_eq!(s.view.filter_query(), None);
    assert_eq!(s.mode, Mode::Normal);
}

// ----- find file -----

fn open_finder(s: &mut AppState) -> (u64, Vec<Effect>) {
    let effects = key(s, "ctrl-p");
    assert_eq!(s.overlays.last(), Some(&Overlay::Finder));
    let gen = effects
        .iter()
        .find_map(|e| match e {
            Effect::FinderStart { gen, .. } => Some(*gen),
            _ => None,
        })
        .expect("a finder start");
    (gen, effects)
}

fn result(rel: &str, indices: &[u32]) -> FinderResult {
    FinderResult {
        rel: rel.to_owned(),
        path: cwd().join(rel),
        kind: WalkKind::File,
        indices: indices.to_vec(),
    }
}

fn snapshot(query: &str, results: Vec<FinderResult>) -> Box<FinderSnapshot> {
    Box::new(FinderSnapshot {
        query: query.to_owned(),
        root: cwd(),
        matched: u32::try_from(results.len()).unwrap(),
        total: 100,
        walking: false,
        from_cache: false,
        denied: 0,
        offset: 0,
        results,
    })
}

#[test]
fn ctrl_p_and_f_open_the_finder_with_the_remembered_scope() {
    let mut s = state_with(&[]);
    let (gen, effects) = open_finder(&mut s);
    assert_eq!(
        effects[0],
        Effect::FinderStart {
            gen,
            scope: Scope::Cwd,
            cwd: cwd(),
            show_hidden: false,
            respect_gitignore: true,
            query: String::new(),
            offset: 0,
            limit: 45,
        }
    );
    key(&mut s, "esc");
    assert!(s.overlays.is_empty());
    let mut s2 = state_with(&[]);
    key(&mut s2, "f");
    assert_eq!(s2.overlays.last(), Some(&Overlay::Finder));
}

#[test]
fn typing_in_the_finder_sends_queries_and_resets_the_selection() {
    let mut s = state_with(&[]);
    let (gen, _) = open_finder(&mut s);
    let effects = type_text(&mut s, "ma");
    assert_eq!(
        effects.last(),
        Some(&Effect::FinderQuery {
            gen,
            query: "ma".into(),
            offset: 0,
            limit: 45
        })
    );
    // Overlay keys do not leak into the explorer: `q` is text here, not quit.
    type_text(&mut s, "q");
    assert_eq!(s.quit, None);
    assert_eq!(s.search.finder.as_ref().unwrap().input.text(), "maq");
}

#[test]
fn finder_results_for_old_generations_or_queries_are_dropped() {
    let mut s = state_with(&[]);
    let (gen, _) = open_finder(&mut s);
    type_text(&mut s, "a");
    update(
        &mut s,
        Msg::FinderResults {
            gen: gen + 7,
            snapshot: snapshot("a", vec![result("a.rs", &[0])]),
        },
    );
    assert!(s.search.finder.as_ref().unwrap().results.is_empty());
    update(
        &mut s,
        Msg::FinderResults {
            gen,
            snapshot: snapshot("", vec![result("old.rs", &[])]),
        },
    );
    assert!(s.search.finder.as_ref().unwrap().results.is_empty());
    update(
        &mut s,
        Msg::FinderResults {
            gen,
            snapshot: snapshot("a", vec![result("a.rs", &[0])]),
        },
    );
    let f = s.search.finder.as_ref().unwrap();
    assert_eq!(f.results.len(), 1);
    assert_eq!(f.matched, 1);
    assert_eq!(f.total, 100);
}

fn snapshot_at(query: &str, root: PathBuf) -> Box<FinderSnapshot> {
    let mut snap = snapshot(query, Vec::new());
    snap.root = root;
    snap
}

#[test]
fn ctrl_r_skips_scopes_that_resolve_to_the_same_root() {
    let mut s = state_with(&[]);
    let (gen, _) = open_finder(&mut s);
    // The cwd scope resolved to /home/me/proj, which is also the git root.
    update(
        &mut s,
        Msg::FinderResults {
            gen,
            snapshot: snapshot_at("", PathBuf::from("/home/me/proj")),
        },
    );
    let effects = key(&mut s, "ctrl-r");
    let Some(Effect::FinderStart {
        gen: git_gen,
        scope: Scope::GitRoot,
        ..
    }) = effects.first().cloned()
    else {
        panic!("expected a git-root session, got {effects:?}");
    };
    // The git root is the same directory: the session moves on to home by itself.
    let effects = update(
        &mut s,
        Msg::FinderResults {
            gen: git_gen,
            snapshot: snapshot_at("", PathBuf::from("/home/me/proj")),
        },
    );
    let Some(Effect::FinderStart {
        gen: home_gen,
        scope: Scope::Home,
        ..
    }) = effects.first().cloned()
    else {
        panic!("expected a home session, got {effects:?}");
    };
    assert!(home_gen > git_gen);
    assert_eq!(s.search.finder_scope, Scope::Home);
    // Home is a different directory: the cycle stops there.
    let effects = update(
        &mut s,
        Msg::FinderResults {
            gen: home_gen,
            snapshot: snapshot_at("", PathBuf::from("/home/me")),
        },
    );
    assert!(effects.is_empty(), "{effects:?}");
    let f = s.search.finder.as_ref().unwrap();
    assert_eq!(f.scope, Scope::Home);
    assert_eq!(f.root.as_deref(), Some(Path::new("/home/me")));
}

#[test]
fn ctrl_r_stops_when_every_scope_resolves_to_one_root() {
    let mut s = state_with(&[]);
    let (mut gen, _) = open_finder(&mut s);
    let root = PathBuf::from("/only");
    update(
        &mut s,
        Msg::FinderResults {
            gen,
            snapshot: snapshot_at("", root.clone()),
        },
    );
    let start_scope = s.search.finder.as_ref().unwrap().scope;
    let mut effects = key(&mut s, "ctrl-r");
    let mut starts = 1;
    while let Some(Effect::FinderStart { gen: g, .. }) = effects.first().cloned() {
        gen = g;
        effects = update(
            &mut s,
            Msg::FinderResults {
                gen,
                snapshot: snapshot_at("", root.clone()),
            },
        );
        starts += 1;
        assert!(starts < 10, "the cycle must end");
    }
    assert_eq!(
        s.search.finder.as_ref().unwrap().root.as_deref(),
        Some(root.as_path())
    );
    // Back on the scope it started from, not on a duplicate.
    assert_eq!(s.search.finder.as_ref().unwrap().scope, start_scope);
}

#[test]
fn scope_cycles_and_toggles_restart_the_session_under_a_new_generation() {
    let mut s = state_with(&[]);
    let (gen, _) = open_finder(&mut s);
    type_text(&mut s, "x");

    let effects = key(&mut s, "ctrl-r");
    let Some(Effect::FinderStart {
        gen: g2,
        scope,
        query,
        ..
    }) = effects.first()
    else {
        panic!("expected a restart, got {effects:?}");
    };
    assert_eq!(*scope, Scope::GitRoot);
    assert_eq!(query, "x", "the query is kept");
    assert!(*g2 > gen);

    let effects = key(&mut s, "alt-.");
    assert!(matches!(
        effects.first(),
        Some(Effect::FinderStart {
            show_hidden: true,
            respect_gitignore: true,
            ..
        })
    ));
    let effects = key(&mut s, "alt-g");
    assert!(matches!(
        effects.first(),
        Some(Effect::FinderStart {
            show_hidden: true,
            respect_gitignore: false,
            scope: Scope::GitRoot,
            ..
        })
    ));
    // Results of the first session no longer apply.
    update(
        &mut s,
        Msg::FinderResults {
            gen,
            snapshot: snapshot("x", vec![result("x", &[])]),
        },
    );
    assert!(s.search.finder.as_ref().unwrap().results.is_empty());

    // Scope and toggles are remembered for the next opening.
    key(&mut s, "esc");
    let effects = key(&mut s, "ctrl-p");
    assert!(matches!(
        effects.first(),
        Some(Effect::FinderStart {
            scope: Scope::GitRoot,
            show_hidden: true,
            respect_gitignore: false,
            ..
        })
    ));
}

fn finder_gen(s: &AppState) -> u64 {
    s.search.finder.as_ref().unwrap().gen
}

fn selected_rel(s: &AppState) -> String {
    let f = s.search.finder.as_ref().unwrap();
    f.current().map(|r| r.rel.clone()).unwrap_or_default()
}

#[test]
fn toggling_hidden_files_keeps_the_highlighted_result() {
    let mut s = state_with(&[]);
    let (gen, _) = open_finder(&mut s);
    let results = || {
        vec![
            result("a.rs", &[]),
            result("b.rs", &[]),
            result("c.rs", &[]),
        ]
    };
    update(
        &mut s,
        Msg::FinderResults {
            gen,
            snapshot: snapshot("", results()),
        },
    );
    key(&mut s, "down");
    key(&mut s, "down");
    assert_eq!(selected_rel(&s), "c.rs");

    key(&mut s, "alt-.");
    let gen = finder_gen(&s);
    // Hidden files come in above it while the walk runs: the selection stays on c.rs.
    let mut walking = snapshot(
        "",
        vec![result(".x", &[]), result("a.rs", &[]), result("c.rs", &[])],
    );
    walking.walking = true;
    update(
        &mut s,
        Msg::FinderResults {
            gen,
            snapshot: walking,
        },
    );
    assert_eq!(selected_rel(&s), "c.rs");
    let mut more = results();
    more.insert(0, result(".x", &[]));
    more.insert(0, result(".y", &[]));
    update(
        &mut s,
        Msg::FinderResults {
            gen,
            snapshot: snapshot("", more.clone()),
        },
    );
    assert_eq!(selected_rel(&s), "c.rs");
    // The walk is over: later snapshots no longer move the selection.
    let mut reranked = more;
    reranked.swap(3, 4);
    update(
        &mut s,
        Msg::FinderResults {
            gen,
            snapshot: snapshot("", reranked),
        },
    );
    assert_eq!(s.search.finder.as_ref().unwrap().selected, 4);
}

#[test]
fn moving_or_typing_after_a_toggle_stops_following() {
    let mut s = state_with(&[]);
    let (gen, _) = open_finder(&mut s);
    let results = || {
        vec![
            result("a.rs", &[]),
            result("b.rs", &[]),
            result("c.rs", &[]),
        ]
    };
    update(
        &mut s,
        Msg::FinderResults {
            gen,
            snapshot: snapshot("", results()),
        },
    );
    key(&mut s, "down");
    key(&mut s, "down");
    key(&mut s, "alt-.");
    let gen = finder_gen(&s);
    let mut walking = snapshot("", results());
    walking.walking = true;
    update(
        &mut s,
        Msg::FinderResults {
            gen,
            snapshot: walking.clone(),
        },
    );
    assert_eq!(selected_rel(&s), "c.rs");
    key(&mut s, "up");
    assert_eq!(selected_rel(&s), "b.rs");
    update(
        &mut s,
        Msg::FinderResults {
            gen,
            snapshot: walking,
        },
    );
    assert_eq!(selected_rel(&s), "b.rs", "the user's move wins");
    // A result that is gone leaves the selection at the top.
    key(&mut s, "alt-.");
    let gen = finder_gen(&s);
    update(
        &mut s,
        Msg::FinderResults {
            gen,
            snapshot: snapshot("", vec![result("a.rs", &[])]),
        },
    );
    assert_eq!(selected_rel(&s), "a.rs");
}

#[test]
fn scope_wraps_around_after_four_presses() {
    let mut s = state_with(&[]);
    open_finder(&mut s);
    for _ in 0..4 {
        key(&mut s, "ctrl-r");
    }
    assert_eq!(s.search.finder.as_ref().unwrap().scope, Scope::Cwd);
}

#[test]
fn selection_moves_clamps_and_scrolls_requesting_new_windows() {
    let mut s = state_with(&[]);
    let (gen, _) = open_finder(&mut s);
    let results: Vec<FinderResult> = (0..40).map(|i| result(&format!("f{i}"), &[])).collect();
    let mut snap = snapshot("", results);
    snap.matched = 1000;
    update(
        &mut s,
        Msg::FinderResults {
            gen,
            snapshot: snap,
        },
    );

    key(&mut s, "up");
    assert_eq!(s.search.finder.as_ref().unwrap().selected, 0);
    for _ in 0..14 {
        key(&mut s, "ctrl-n");
    }
    let f = s.search.finder.as_ref().unwrap();
    assert_eq!((f.selected, f.scroll), (14, 0), "15 rows fit");
    let effects = key(&mut s, "down");
    let f = s.search.finder.as_ref().unwrap();
    assert_eq!((f.selected, f.scroll), (15, 1));
    assert!(matches!(
        effects.as_slice(),
        [Effect::FinderWindow {
            offset: 0,
            limit: 45,
            ..
        }]
    ));
    key(&mut s, "pagedown");
    assert_eq!(s.search.finder.as_ref().unwrap().selected, 22);
    key(&mut s, "ctrl-p");
    assert_eq!(s.search.finder.as_ref().unwrap().selected, 21);
}

#[test]
fn enter_navigates_to_the_directory_with_the_cursor_on_the_file() {
    let mut s = state_with(&[]);
    let (gen, _) = open_finder(&mut s);
    type_text(&mut s, "m");
    update(
        &mut s,
        Msg::FinderResults {
            gen,
            snapshot: snapshot("m", vec![result("src/deep/main.rs", &[])]),
        },
    );
    let effects = key(&mut s, "enter");
    assert!(s.overlays.is_empty());
    assert!(effects.contains(&Effect::FinderStop));
    let (lgen, path) = effects
        .iter()
        .find_map(|e| match e {
            Effect::ListDir { gen, path } => Some((*gen, path.clone())),
            _ => None,
        })
        .expect("a listing of the file's directory");
    assert_eq!(path, cwd().join("src/deep"));
    update(
        &mut s,
        Msg::ListingChunk {
            gen: lgen,
            entries: vec![
                Entry::file("a.rs", 1),
                Entry::file("main.rs", 1),
                Entry::file("z.rs", 1),
            ],
        },
    );
    update(
        &mut s,
        Msg::ListingFinished {
            gen: lgen,
            error: None,
        },
    );
    assert_eq!(s.cwd, cwd().join("src/deep"));
    assert_eq!(cursor_name(&s), "main.rs");
}

#[test]
fn ctrl_enter_and_ctrl_o_open_the_highlighted_result() {
    let mut s = state_with(&[]);
    let (gen, _) = open_finder(&mut s);
    update(
        &mut s,
        Msg::FinderResults {
            gen,
            snapshot: snapshot("", vec![result("a.txt", &[]), result("b.txt", &[])]),
        },
    );
    key(&mut s, "down");
    for k in ["ctrl-enter", "ctrl-o"] {
        let effects = key(&mut s, k);
        assert_eq!(
            effects,
            vec![Effect::OpenDefault {
                path: cwd().join("b.txt")
            }]
        );
    }
    assert_eq!(
        s.overlays.last(),
        Some(&Overlay::Finder),
        "the overlay stays"
    );
}

#[test]
fn marks_in_one_directory_are_marked_there_after_enter() {
    let mut s = state_with(&[]);
    let (gen, _) = open_finder(&mut s);
    update(
        &mut s,
        Msg::FinderResults {
            gen,
            snapshot: snapshot(
                "",
                vec![
                    result("d/a.txt", &[]),
                    result("d/b.txt", &[]),
                    result("d/c.txt", &[]),
                ],
            ),
        },
    );
    key(&mut s, "tab");
    key(&mut s, "down");
    key(&mut s, "tab");
    assert_eq!(s.search.finder.as_ref().unwrap().marks.len(), 2);
    let effects = key(&mut s, "enter");
    let lgen = effects
        .iter()
        .find_map(|e| match e {
            Effect::ListDir { gen, .. } => Some(*gen),
            _ => None,
        })
        .unwrap();
    update(
        &mut s,
        Msg::ListingChunk {
            gen: lgen,
            entries: vec![
                Entry::file("a.txt", 1),
                Entry::file("b.txt", 1),
                Entry::file("c.txt", 1),
            ],
        },
    );
    update(
        &mut s,
        Msg::ListingFinished {
            gen: lgen,
            error: None,
        },
    );
    assert_eq!(s.view.marks_count(), 2);
    assert_eq!(cursor_name(&s), "a.txt");
}

#[test]
fn marks_in_several_directories_open_a_results_list() {
    let mut s = state_with(&["old"]);
    let (gen, _) = open_finder(&mut s);
    type_text(&mut s, "t");
    update(
        &mut s,
        Msg::FinderResults {
            gen,
            snapshot: snapshot("t", vec![result("a/x.txt", &[]), result("b/y.txt", &[])]),
        },
    );
    key(&mut s, "tab");
    key(&mut s, "tab");
    key(&mut s, "enter");
    assert!(s.overlays.is_empty());
    assert_eq!(s.view.label.as_deref(), Some("Find file: t"));
    assert_eq!(shown(&s).len(), 2);
    assert_eq!(s.cwd, cwd());
}

#[test]
fn cache_diff_reports_additions_and_removals() {
    let old = vec![Entry::file("keep", 1), Entry::file("gone", 1)];
    let new = vec![Entry::file("keep", 1), Entry::dir("fresh")];
    let changes = diff_listing(&cwd(), &old, &new);
    assert_eq!(
        changes,
        vec![
            PathChange::Removed(cwd().join("gone")),
            PathChange::Added(cwd().join("fresh"), WalkKind::Dir),
        ]
    );
    assert!(diff_listing(&cwd(), &new, &new).is_empty());
}

#[test]
fn a_finished_job_feeds_the_walk_cache_with_its_destinations() {
    let mut effects = Vec::new();
    cache_after_job(&mut effects, &[], &[]);
    assert!(effects.is_empty());
    let dest = PathBuf::from("/elsewhere/copy");
    let src = cwd().join("old");
    cache_after_job(
        &mut effects,
        std::slice::from_ref(&dest),
        std::slice::from_ref(&src),
    );
    assert_eq!(
        effects,
        vec![Effect::WalkCacheChanges(vec![
            PathChange::Removed(src),
            PathChange::Touched(dest),
        ])]
    );
}

#[test]
fn a_watcher_refresh_feeds_the_walk_cache() {
    let mut s = state_with(&["a", "b"]);
    let watch = s.watch_gen.current();
    let effects = update(&mut s, Msg::DirChanged { gen: watch });
    let Some(Effect::ListDir { gen, .. }) = effects.first() else {
        panic!("expected a refresh listing, got {effects:?}");
    };
    let gen = *gen;
    update(
        &mut s,
        Msg::ListingChunk {
            gen,
            entries: vec![Entry::file("a", 1), Entry::file("c", 1)],
        },
    );
    let effects = update(&mut s, Msg::ListingFinished { gen, error: None });
    assert!(effects.contains(&Effect::WalkCacheChanges(vec![
        PathChange::Removed(cwd().join("b")),
        PathChange::Added(cwd().join("c"), WalkKind::File),
    ])));
}

// ----- content search -----

fn open_grep(s: &mut AppState) {
    key(s, "ctrl-f");
    assert_eq!(s.overlays.last(), Some(&Overlay::Grep));
}

fn timer_gen(effects: &[Effect]) -> crate::Gen {
    effects
        .iter()
        .rev()
        .find_map(|e| match e {
            Effect::StartTimer {
                kind: TimerKind::GrepDebounce,
                gen,
                ..
            } => Some(*gen),
            _ => None,
        })
        .expect("a debounce timer")
}

fn file_matches(rel: &str, lines: &[(u64, &str)]) -> FileMatches {
    FileMatches {
        path: cwd().join(rel),
        rel: PathBuf::from(rel),
        lines: lines
            .iter()
            .map(|(n, t)| LineMatch {
                line_number: *n,
                text: (*t).to_owned(),
                ranges: vec![0..1],
            })
            .collect(),
    }
}

fn fire_debounce(s: &mut AppState, effects: &[Effect]) -> (u64, Vec<Effect>) {
    let gen = timer_gen(effects);
    let effects = update(
        s,
        Msg::Timer {
            kind: TimerKind::GrepDebounce,
            gen,
        },
    );
    let started = effects
        .iter()
        .find_map(|e| match e {
            Effect::GrepStart { gen, .. } => Some(*gen),
            _ => None,
        })
        .expect("a search start");
    (started, effects)
}

#[test]
fn typing_debounces_120ms_and_cancels_the_running_search() {
    let mut s = state_with(&[]);
    open_grep(&mut s);
    let first = key(&mut s, "t");
    assert!(first.contains(&Effect::GrepCancel));
    assert!(first.iter().any(|e| matches!(
        e,
        Effect::StartTimer {
            kind: TimerKind::GrepDebounce,
            delay,
            ..
        } if *delay == std::time::Duration::from_millis(120)
    )));
    let stale_timer = timer_gen(&first);
    let second = key(&mut s, "o");
    // The first timer is superseded: nothing starts when it fires.
    let effects = update(
        &mut s,
        Msg::Timer {
            kind: TimerKind::GrepDebounce,
            gen: stale_timer,
        },
    );
    assert!(effects.is_empty());
    let (gen, effects) = fire_debounce(&mut s, &second);
    let Some(Effect::GrepStart {
        pattern,
        glob,
        regex,
        whole_word,
        root,
        ..
    }) = effects
        .iter()
        .find(|e| matches!(e, Effect::GrepStart { .. }))
    else {
        unreachable!()
    };
    assert_eq!((pattern.as_str(), glob.as_str()), ("to", ""));
    assert!(!regex && !whole_word);
    assert_eq!(root, &cwd());
    assert!(s.search.grep.as_ref().unwrap().running);
    assert!(gen > 0);
}

#[test]
fn an_empty_query_clears_the_results_without_searching() {
    let mut s = state_with(&[]);
    open_grep(&mut s);
    let effects = key(&mut s, "a");
    let (gen, _) = fire_debounce(&mut s, &effects);
    update(
        &mut s,
        Msg::Grep(GrepEvent::Matches {
            generation: gen,
            files: vec![file_matches("a.rs", &[(1, "a")])],
        }),
    );
    assert_eq!(s.search.grep.as_ref().unwrap().files.len(), 1);
    let effects = key(&mut s, "backspace");
    let gen = timer_gen(&effects);
    let effects = update(
        &mut s,
        Msg::Timer {
            kind: TimerKind::GrepDebounce,
            gen,
        },
    );
    assert!(!effects
        .iter()
        .any(|e| matches!(e, Effect::GrepStart { .. })));
    let g = s.search.grep.as_ref().unwrap();
    assert!(g.files.is_empty() && !g.running);
}

#[test]
fn stale_generations_are_dropped_and_batches_group_by_file() {
    let mut s = state_with(&[]);
    open_grep(&mut s);
    let effects = key(&mut s, "x");
    let (gen, _) = fire_debounce(&mut s, &effects);

    update(
        &mut s,
        Msg::Grep(GrepEvent::Matches {
            generation: gen - 1,
            files: vec![file_matches("stale.rs", &[(1, "x")])],
        }),
    );
    assert!(s.search.grep.as_ref().unwrap().files.is_empty());

    update(
        &mut s,
        Msg::Grep(GrepEvent::Matches {
            generation: gen,
            files: vec![
                file_matches("b.rs", &[(3, "x = 1"), (9, "x = 2")]),
                file_matches("a.rs", &[(1, "x")]),
            ],
        }),
    );
    let g = s.search.grep.as_ref().unwrap();
    // Two headers and three matches; the selection skips headers.
    assert_eq!(g.rows.len(), 5);
    assert_eq!(g.selected, 1);
    assert_eq!(g.match_count(), 3);

    update(
        &mut s,
        Msg::Grep(GrepEvent::Progress {
            generation: gen,
            stats: GrepStats {
                files_searched: 10,
                skipped_cloud: 2,
                ..GrepStats::default()
            },
        }),
    );
    update(
        &mut s,
        Msg::Grep(GrepEvent::Done {
            generation: gen,
            stats: GrepStats {
                files_searched: 12,
                matches: 3,
                ..GrepStats::default()
            },
            capped: true,
        }),
    );
    let g = s.search.grep.as_ref().unwrap();
    assert!(!g.running && g.capped);
    assert_eq!(g.stats.files_searched, 12);
    // Sorted by path when done; the highlighted match (b.rs:3) is still highlighted.
    assert_eq!(g.files[0].rel, PathBuf::from("a.rs"));
    let (file, line) = g.current().unwrap();
    assert_eq!(
        (file.rel.clone(), line.line_number),
        (PathBuf::from("b.rs"), 3)
    );
}

#[test]
fn toggles_reschedule_and_are_remembered() {
    let mut s = state_with(&[]);
    open_grep(&mut s);
    key(&mut s, "a");
    let effects = key(&mut s, "ctrl-r");
    assert!(effects.contains(&Effect::GrepCancel));
    let (_, effects) = fire_debounce(&mut s, &effects);
    assert!(matches!(
        effects.first(),
        Some(Effect::GrepStart {
            regex: true,
            whole_word: false,
            ..
        })
    ));
    let effects = key(&mut s, "ctrl-w");
    let (_, effects) = fire_debounce(&mut s, &effects);
    assert!(matches!(
        effects.first(),
        Some(Effect::GrepStart {
            regex: true,
            whole_word: true,
            ..
        })
    ));
    assert!(s.quit.is_none(), "ctrl-w is overlay-local, not close-tab");

    // Closing and reopening restores query, toggles and searches again.
    key(&mut s, "esc");
    assert!(s.overlays.is_empty());
    let effects = key(&mut s, "ctrl-f");
    let g = s.search.grep.as_ref().unwrap();
    assert_eq!(g.query.text(), "a");
    assert!(g.regex && g.whole_word);
    assert!(effects.iter().any(|e| matches!(
        e,
        Effect::StartTimer {
            kind: TimerKind::GrepDebounce,
            ..
        }
    )));
}

#[test]
fn tab_switches_to_the_glob_field_which_is_part_of_the_query() {
    let mut s = state_with(&[]);
    open_grep(&mut s);
    key(&mut s, "a");
    key(&mut s, "tab");
    assert_eq!(s.search.grep.as_ref().unwrap().field, GrepField::Glob);
    let effects = type_text(&mut s, "*.rs");
    let (_, effects) = fire_debounce(&mut s, &effects);
    assert!(matches!(
        effects.first(),
        Some(Effect::GrepStart { pattern, glob, .. }) if pattern == "a" && glob == "*.rs"
    ));
    key(&mut s, "backtab");
    assert_eq!(s.search.grep.as_ref().unwrap().field, GrepField::Query);
}

#[test]
fn enter_opens_the_highlighted_hit_in_the_editor() {
    let mut s = state_with(&[]);
    open_grep(&mut s);
    let effects = key(&mut s, "x");
    let (gen, _) = fire_debounce(&mut s, &effects);
    update(
        &mut s,
        Msg::Grep(GrepEvent::Matches {
            generation: gen,
            files: vec![file_matches("a.rs", &[(4, "x"), (8, "xx")])],
        }),
    );
    key(&mut s, "down");
    let effects = key(&mut s, "enter");
    assert_eq!(
        effects,
        vec![Effect::OpenInEditor {
            path: cwd().join("a.rs"),
            line: Some(8)
        }]
    );
}

#[test]
fn ctrl_l_sends_the_matched_files_to_a_results_list() {
    let mut s = state_with(&["unrelated"]);
    open_grep(&mut s);
    let effects = type_text(&mut s, "todo");
    let (gen, _) = fire_debounce(&mut s, &effects);
    update(
        &mut s,
        Msg::Grep(GrepEvent::Matches {
            generation: gen,
            files: vec![
                file_matches("src/a.rs", &[(1, "todo")]),
                file_matches("b.rs", &[(2, "todo")]),
            ],
        }),
    );
    let effects = key(&mut s, "ctrl-l");
    assert!(s.overlays.is_empty());
    assert!(effects.contains(&Effect::GrepCancel));
    assert!(effects.contains(&Effect::StopWatching));
    assert_eq!(
        s.view.label.as_deref(),
        Some("Search results: todo (partial)")
    );
    assert_eq!(shown(&s).len(), 2);

    // A watcher notification must not replace the virtual listing.
    let watch = s.watch_gen.current();
    let effects = update(&mut s, Msg::DirChanged { gen: watch });
    assert!(effects.is_empty());
    assert!(s.view.label.is_some());

    // `esc` returns to the real listing.
    let effects = key(&mut s, "esc");
    assert!(effects.iter().any(|e| matches!(e, Effect::ListDir { .. })));
}

#[test]
fn bad_regex_error_is_shown_and_ends_the_search() {
    let mut s = state_with(&[]);
    open_grep(&mut s);
    key(&mut s, "ctrl-r");
    let effects = key(&mut s, "(");
    let (gen, _) = fire_debounce(&mut s, &effects);
    update(
        &mut s,
        Msg::Grep(GrepEvent::Error {
            generation: gen,
            message: "unclosed group".into(),
        }),
    );
    let g = s.search.grep.as_ref().unwrap();
    assert!(!g.running);
    assert_eq!(g.error.as_deref(), Some("unclosed group"));
}

#[test]
fn search_actions_outside_their_overlay_explain_themselves() {
    let mut s = state_with(&[]);
    let invoke = crate::keymap::Invocation {
        action: Action::GrepRegex,
        arg: None,
    };
    let mut effects = Vec::new();
    assert!(run_action(&mut s, invoke, &mut effects));
    assert!(s.status.as_ref().unwrap().text.contains("content-search"));
}

#[test]
fn every_new_action_is_bound_by_default() {
    let map = Keymap::defaults();
    for (ctx, action) in [
        (KeyContext::Finder, Action::FinderScope),
        (KeyContext::Finder, Action::FinderHidden),
        (KeyContext::Finder, Action::FinderGitignore),
        (KeyContext::Finder, Action::FinderOpen),
        (KeyContext::Finder, Action::FinderMark),
        (KeyContext::Grep, Action::GrepRegex),
        (KeyContext::Grep, Action::GrepWholeWord),
        (KeyContext::Grep, Action::GrepNextField),
        (KeyContext::Grep, Action::GrepToList),
    ] {
        assert!(!map.keys_for(ctx, action).is_empty(), "{action}");
    }
}

// ----- integration with tabs and the preview pane -----

fn wide(s: &mut AppState) {
    update(s, Msg::Resize(Size::new(120, 40)));
}

fn tab_count(s: &AppState) -> usize {
    s.tabs.count()
}

fn grep_with_two_files(s: &mut AppState) {
    open_grep(s);
    let effects = type_text(s, "todo");
    let (gen, _) = fire_debounce(s, &effects);
    update(
        s,
        Msg::Grep(GrepEvent::Matches {
            generation: gen,
            files: vec![
                file_matches("src/a.rs", &[(1, "todo")]),
                file_matches("b.rs", &[(2, "todo")]),
            ],
        }),
    );
}

#[test]
fn the_results_list_opens_in_a_new_tab() {
    let mut s = state_with(&["unrelated"]);
    grep_with_two_files(&mut s);
    key(&mut s, "ctrl-l");
    assert_eq!(tab_count(&s), 2);
    assert_eq!(s.tabs.active_index(), 1);
    assert!(s.overlays.is_empty());
    assert_eq!(shown(&s).len(), 2);

    // The first tab still shows the directory it had.
    let effects = key(&mut s, "1");
    assert_eq!(shown(&s), ["unrelated"]);
    assert!(s.view.label.is_none());
    assert!(effects.iter().any(|e| matches!(e, Effect::ListDir { .. })));

    // Coming back does not re-list (and so does not replace) the virtual list.
    let effects = key(&mut s, "2");
    assert_eq!(
        s.view.label.as_deref(),
        Some("Search results: todo (partial)")
    );
    assert_eq!(shown(&s).len(), 2);
    assert!(
        !effects.iter().any(|e| matches!(
            e,
            Effect::ListDir { .. } | Effect::WatchDir { .. } | Effect::GitStatus { .. }
        )),
        "{effects:?}"
    );

    // `esc` turns the results tab into the real listing of the root.
    let effects = key(&mut s, "esc");
    assert!(effects.iter().any(|e| matches!(e, Effect::ListDir { .. })));
    assert_eq!(tab_count(&s), 2);
}

#[test]
fn results_list_entries_get_size_and_time_lazily() {
    let mut s = state_with(&["unrelated"]);
    grep_with_two_files(&mut s);
    let effects = key(&mut s, "ctrl-l");
    let Some(Effect::StatEntries { gen, dir, names }) = effects
        .iter()
        .find(|e| matches!(e, Effect::StatEntries { .. }))
        .cloned()
    else {
        panic!("expected a StatEntries effect, got {effects:?}");
    };
    assert_eq!(dir, cwd());
    assert_eq!(names.len(), 2);
    assert!(s
        .view
        .entries()
        .iter()
        .all(|e| e.size == 0 && e.mtime.is_none()));

    // A stale answer (another generation) is dropped.
    let stamp = std::time::UNIX_EPOCH + std::time::Duration::from_secs(1_000_000);
    update(
        &mut s,
        Msg::EntryStats {
            gen: Gen::default(),
            stats: vec![(names[0].clone(), 9, Some(stamp))],
        },
    );
    assert!(s.view.entries().iter().all(|e| e.size == 0));

    update(
        &mut s,
        Msg::EntryStats {
            gen,
            stats: vec![(names[0].clone(), 1234, Some(stamp))],
        },
    );
    let filled = s
        .view
        .entries()
        .iter()
        .find(|e| e.name == names[0])
        .unwrap();
    assert_eq!(filled.size, 1234);
    assert_eq!(filled.mtime, Some(stamp));
    let other = s
        .view
        .entries()
        .iter()
        .find(|e| e.name == names[1])
        .unwrap();
    assert_eq!(other.size, 0);
}

#[test]
fn the_results_list_replaces_the_pane_when_no_tab_is_free() {
    let mut s = state_with(&["unrelated"]);
    for _ in 1..crate::tabs::MAX_TABS {
        key(&mut s, "t");
    }
    assert_eq!(tab_count(&s), crate::tabs::MAX_TABS);
    grep_with_two_files(&mut s);
    key(&mut s, "ctrl-l");
    assert_eq!(tab_count(&s), crate::tabs::MAX_TABS);
    assert!(s.view.label.is_some());
}

#[test]
fn the_filter_belongs_to_its_tab() {
    let mut s = state_with(&["main.rs", "lib.rs", "README.md"]);
    key(&mut s, "/");
    type_text(&mut s, "rs");
    key(&mut s, "enter");
    assert_eq!(shown(&s).len(), 2);

    key(&mut s, "t");
    assert_eq!(s.view.filter_query(), None, "a new tab starts unfiltered");
    key(&mut s, "1");
    assert_eq!(s.view.filter_query(), Some("rs"), "tab 1 keeps its filter");
}

#[test]
fn leaving_a_tab_closes_the_search_overlays() {
    let mut s = state_with(&["a"]);
    let (_, _) = open_finder(&mut s);
    // `t` is text in the finder: leave through the palette-less path (tab API) instead.
    let mut effects = Vec::new();
    crate::tabs::new_tab(&mut s, &mut effects);
    assert!(s.overlays.is_empty());
    assert!(s.search.finder.is_none());
    assert!(effects.contains(&Effect::FinderStop));
}

fn text_result(path: &std::path::Path, lines: usize) -> std::sync::Arc<ks_preview::Preview> {
    use ks_preview::{Encoding, Preview, PreviewBody, StyledLine, TextPreview};
    std::sync::Arc::new(Preview {
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

fn preview_requests(effects: &[Effect]) -> Vec<(crate::Gen, PathBuf)> {
    effects
        .iter()
        .filter_map(|e| match e {
            Effect::RequestPreview { gen, request } => Some((*gen, request.path.clone())),
            _ => None,
        })
        .collect()
}

#[test]
fn the_finder_previews_the_highlighted_result_on_wide_terminals() {
    let mut s = state_with(&["a"]);
    wide(&mut s);
    let (gen, _) = open_finder(&mut s);
    let effects = update(
        &mut s,
        Msg::FinderResults {
            gen,
            snapshot: snapshot("", vec![result("src/a.rs", &[]), result("src/b.rs", &[])]),
        },
    );
    assert_eq!(
        preview_requests(&effects)
            .into_iter()
            .map(|(_, p)| p)
            .collect::<Vec<_>>(),
        [cwd().join("src/a.rs")]
    );
    // Moving the highlight asks for the next file.
    let effects = key(&mut s, "down");
    assert_eq!(
        preview_requests(&effects)
            .into_iter()
            .map(|(_, p)| p)
            .collect::<Vec<_>>(),
        [cwd().join("src/b.rs")]
    );
    // Closing the overlay returns the pane to the list's cursor.
    let effects = key(&mut s, "esc");
    assert_eq!(
        preview_requests(&effects)
            .into_iter()
            .map(|(_, p)| p)
            .collect::<Vec<_>>(),
        [cwd().join("a")]
    );
}

#[test]
fn narrow_terminals_do_not_preview_search_results() {
    let mut s = state_with(&["a"]);
    s.preview.user_visible = Some(false);
    let (gen, _) = open_finder(&mut s);
    let effects = update(
        &mut s,
        Msg::FinderResults {
            gen,
            snapshot: snapshot("", vec![result("src/a.rs", &[])]),
        },
    );
    assert!(preview_requests(&effects).is_empty());
}

#[test]
fn a_grep_hit_scrolls_the_preview_to_its_line() {
    let mut s = state_with(&["a"]);
    wide(&mut s);
    open_grep(&mut s);
    let effects = type_text(&mut s, "todo");
    let (gen, _) = fire_debounce(&mut s, &effects);
    let effects = update(
        &mut s,
        Msg::Grep(GrepEvent::Matches {
            generation: gen,
            files: vec![file_matches("big.rs", &[(80, "todo")])],
        }),
    );
    let path = cwd().join("big.rs");
    let (pgen, _) = preview_requests(&effects).pop().expect("a preview request");
    update(
        &mut s,
        Msg::Preview(crate::PreviewMsg(ks_preview::PreviewEvent {
            generation: pgen.raw(),
            path: path.clone(),
            result: text_result(&path, 200),
            from_cache: false,
        })),
    );
    let scroll = s.preview.scroll;
    assert!(
        scroll > 0 && scroll < 80,
        "scrolled towards line 80: {scroll}"
    );
    assert!(
        scroll + s.preview_rows() > 79,
        "line 80 is inside the visible rows ({scroll}+{})",
        s.preview_rows()
    );
}

fn grep_hit_request(line: u64) -> (AppState, crate::PreviewRequest) {
    let mut s = state_with(&["a"]);
    wide(&mut s);
    open_grep(&mut s);
    let effects = type_text(&mut s, "todo");
    let (gen, _) = fire_debounce(&mut s, &effects);
    let effects = update(
        &mut s,
        Msg::Grep(GrepEvent::Matches {
            generation: gen,
            files: vec![file_matches("big.rs", &[(line, "todo")])],
        }),
    );
    let request = effects
        .iter()
        .find_map(|e| match e {
            Effect::RequestPreview { request, .. } => Some(request.clone()),
            _ => None,
        })
        .expect("a preview request");
    (s, request)
}

#[test]
fn a_grep_hit_beyond_the_preview_cap_requests_a_window_around_it() {
    let cfg = crate::config::Config::default().preview;
    let (_, near) = grep_hit_request(80);
    assert_eq!(
        (near.max_lines, near.max_bytes, near.focus_line),
        (cfg.max_lines, cfg.max_bytes, None)
    );

    let (_, far) = grep_hit_request(50_000);
    assert_eq!(
        far.max_lines, cfg.max_lines,
        "the window is as big as the head"
    );
    let focus = far.focus_line.expect("a focus line");
    assert!(focus.abs_diff(50_000) <= 500, "{focus}");

    // A neighbouring hit in the same bucket asks for the same window.
    let (_, next) = grep_hit_request(50_300);
    assert_eq!(next.focus_line, far.focus_line);

    // Far hits are reachable too (no hard ceiling any more).
    let (_, huge) = grep_hit_request(5_000_000);
    assert!(huge.focus_line.is_some());
}

#[test]
fn a_small_preview_limit_keeps_every_hit_inside_the_window() {
    // With max_lines = 200 the window is +-100 lines; a fixed 1000-line bucket would centre a
    // hit at line 999 on 500 and miss it.
    for line in [201_u64, 999, 1_000, 1_499, 50_000, 50_099] {
        let mut s = state_with(&["a"]);
        s.config.preview.max_lines = 200;
        wide(&mut s);
        open_grep(&mut s);
        let effects = type_text(&mut s, "todo");
        let (gen, _) = fire_debounce(&mut s, &effects);
        let effects = update(
            &mut s,
            Msg::Grep(GrepEvent::Matches {
                generation: gen,
                files: vec![file_matches("big.rs", &[(line, "todo")])],
            }),
        );
        let request = effects
            .iter()
            .find_map(|e| match e {
                Effect::RequestPreview { request, .. } => Some(request.clone()),
                _ => None,
            })
            .expect("a preview request");
        let focus = request.focus_line.expect("a focus line");
        let half = request.max_lines as u64 / 2;
        assert!(
            focus.abs_diff(line) + 1 < half,
            "line {line} is outside the window centred on {focus} (half {half})"
        );
    }
}

#[test]
fn moving_between_hits_in_one_window_bucket_requests_nothing_new() {
    let mut s = state_with(&["a"]);
    wide(&mut s);
    open_grep(&mut s);
    let effects = type_text(&mut s, "todo");
    let (gen, _) = fire_debounce(&mut s, &effects);
    let effects = update(
        &mut s,
        Msg::Grep(GrepEvent::Matches {
            generation: gen,
            files: vec![file_matches(
                "big.rs",
                &[(50_000, "todo"), (50_100, "todo"), (50_200, "todo")],
            )],
        }),
    );
    assert_eq!(preview_requests(&effects).len(), 1);
    let effects = key(&mut s, "down");
    assert!(
        preview_requests(&effects).is_empty(),
        "same bucket, same window: {effects:?}"
    );
}

#[test]
fn a_window_preview_numbers_lines_from_its_offset_and_scrolls_to_the_hit() {
    let (mut s, request) = grep_hit_request(50_000);
    let path = cwd().join("big.rs");
    let first_line = request.focus_line.unwrap() as usize - 1000;
    let window = {
        let ks_preview::Preview { path, meta, body } = (*text_result(&path, 2000)).clone();
        let ks_preview::PreviewBody::Text(mut t) = body else {
            unreachable!()
        };
        t.first_line = first_line;
        t.truncated = true;
        t.total_lines = None;
        std::sync::Arc::new(ks_preview::Preview {
            path,
            meta,
            body: ks_preview::PreviewBody::Text(t),
        })
    };
    let gen = s.preview.generation();
    update(
        &mut s,
        Msg::Preview(crate::PreviewMsg(ks_preview::PreviewEvent {
            generation: gen.raw(),
            path,
            result: window,
            from_cache: false,
        })),
    );
    assert!(
        s.status.is_none(),
        "the hit is inside the window: {:?}",
        s.status
    );
    let rows = s.preview_rows();
    let want = (49_999 - first_line).saturating_sub(rows / 3);
    assert_eq!(s.preview.scroll, want);
}

#[test]
fn a_grep_hit_the_preview_still_cannot_reach_says_so() {
    let (mut s, _) = grep_hit_request(50_000);
    let path = cwd().join("big.rs");
    // The engine returned only the head (truncated): the hit is not in it.
    let truncated = {
        let ks_preview::Preview { path, meta, body } = (*text_result(&path, 100)).clone();
        let ks_preview::PreviewBody::Text(mut t) = body else {
            unreachable!()
        };
        t.truncated = true;
        t.total_lines = None;
        std::sync::Arc::new(ks_preview::Preview {
            path,
            meta,
            body: ks_preview::PreviewBody::Text(t),
        })
    };
    let gen = s.preview.generation();
    update(
        &mut s,
        Msg::Preview(crate::PreviewMsg(ks_preview::PreviewEvent {
            generation: gen.raw(),
            path,
            result: truncated,
            from_cache: false,
        })),
    );
    assert!(s
        .status
        .as_ref()
        .is_some_and(|st| st.text.contains("beyond the preview limit")));
}

// ----- sending a hit to Claude -----

fn start_claude(s: &mut AppState) {
    use crate::claude::ClaudePreflight;
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
    // Hand the focus back to the explorer.
    key(s, "ctrl-\\");
    assert!(s.overlays.is_empty());
}

#[test]
fn alt_c_sends_the_hit_to_the_running_pane_and_closes_the_overlay() {
    let mut s = state_with(&["unrelated"]);
    start_claude(&mut s);
    grep_with_two_files(&mut s);
    key(&mut s, "down");
    let effects = key(&mut s, "alt-c");
    let sent = effects.iter().find_map(|e| match e {
        Effect::ClaudeSendRange { path, start, end } => Some((path.clone(), *start, *end)),
        _ => None,
    });
    let (path, start, end) = sent.expect("a ClaudeSendRange effect");
    assert!(path.ends_with("b.rs"), "{path:?}");
    assert_eq!((start, end), (2, 2));
    assert!(s.search.grep.is_none(), "the overlay is closed");
    assert_eq!(
        s.overlays.last(),
        Some(&Overlay::Claude),
        "the pane has focus"
    );
}

#[test]
fn alt_c_without_claude_only_shows_a_toast() {
    let mut s = state_with(&["unrelated"]);
    grep_with_two_files(&mut s);
    let effects = key(&mut s, "alt-c");
    assert!(!effects
        .iter()
        .any(|e| matches!(e, Effect::ClaudeSendRange { .. })));
    assert!(s
        .status
        .as_ref()
        .is_some_and(|st| st.text.contains("Claude is not running")));
    assert_eq!(
        s.overlays.last(),
        Some(&Overlay::Grep),
        "the search stays open"
    );
}

#[test]
fn marked_hits_are_all_sent_to_claude() {
    let mut s = state_with(&["unrelated"]);
    start_claude(&mut s);
    grep_with_two_files(&mut s);
    key(&mut s, "alt-m");
    key(&mut s, "alt-m");
    let g = s.search.grep.as_ref().unwrap();
    assert_eq!(g.marks.len(), 2);
    let effects = key(&mut s, "alt-c");
    let sent: Vec<_> = effects
        .iter()
        .filter_map(|e| match e {
            Effect::ClaudeSendRange { path, start, end } => Some((path.clone(), *start, *end)),
            _ => None,
        })
        .collect();
    assert_eq!(sent.len(), 2, "{sent:?}");
    assert!(sent[0].0.ends_with("a.rs") && sent[1].0.ends_with("b.rs"));
}

#[test]
fn alt_m_toggles_a_mark() {
    let mut s = state_with(&["unrelated"]);
    grep_with_two_files(&mut s);
    key(&mut s, "alt-m");
    assert_eq!(s.search.grep.as_ref().unwrap().marks.len(), 1);
    key(&mut s, "up");
    key(&mut s, "alt-m");
    assert!(s.search.grep.as_ref().unwrap().marks.is_empty());
}

#[test]
fn a_new_search_drops_the_marks() {
    let mut s = state_with(&["unrelated"]);
    grep_with_two_files(&mut s);
    key(&mut s, "alt-m");
    assert_eq!(s.search.grep.as_ref().unwrap().marks.len(), 1);
    let effects = type_text(&mut s, "x");
    fire_debounce(&mut s, &effects);
    assert!(s.search.grep.as_ref().unwrap().marks.is_empty());
}

#[test]
fn lazy_stats_resort_a_size_sorted_results_list() {
    let mut s = state_with(&["unrelated"]);
    grep_with_two_files(&mut s);
    let effects = key(&mut s, "ctrl-l");
    let Some(Effect::StatEntries { gen, names, .. }) = effects
        .iter()
        .find(|e| matches!(e, Effect::StatEntries { .. }))
        .cloned()
    else {
        panic!("expected a StatEntries effect");
    };
    let h = s.viewport_height();
    s.view.set_sort(
        crate::config::SortConfig {
            key: crate::config::SortKey::Size,
            reverse: false,
            ..Default::default()
        },
        h,
    );
    assert_eq!(s.view.entries()[0].name, names[0]);
    update(
        &mut s,
        Msg::EntryStats {
            gen,
            stats: vec![(names[0].clone(), 10, None), (names[1].clone(), 500, None)],
        },
    );
    let order: Vec<_> = s.view.entries().iter().map(|e| e.size).collect();
    assert_eq!(order, vec![500, 10]);
}

// ----- [files] show_hidden / show_ignored defaults -----

fn state_with_files_config(show_hidden: bool, show_ignored: bool) -> AppState {
    let mut config = Config::default();
    config.files.show_hidden = show_hidden;
    config.files.show_ignored = show_ignored;
    AppState::new(StartParams {
        cwd: cwd(),
        home: None,
        places: Places::default(),
        config,
        keymap: Keymap::defaults(),
        size: Size::new(80, 24),
        now: SystemTime::UNIX_EPOCH,
        utc_offset_secs: 0,
    })
}

#[test]
fn config_files_defaults_feed_the_finder() {
    let mut s = state_with_files_config(true, true);
    let (_, effects) = open_finder(&mut s);
    assert!(effects.iter().any(|e| matches!(
        e,
        Effect::FinderStart {
            show_hidden: true,
            respect_gitignore: false,
            ..
        }
    )));
    let f = s.search.finder.as_ref().unwrap();
    assert!(f.show_hidden && !f.respect_gitignore);
    // The stock config keeps both off.
    let mut s = state_with_files_config(false, false);
    let (_, effects) = open_finder(&mut s);
    assert!(effects.iter().any(|e| matches!(
        e,
        Effect::FinderStart {
            show_hidden: false,
            respect_gitignore: true,
            ..
        }
    )));
}

#[test]
fn config_show_ignored_feeds_content_search() {
    let mut s = state_with_files_config(false, true);
    open_grep(&mut s);
    let first = type_text(&mut s, "x");
    let (_, effects) = fire_debounce(&mut s, &first);
    assert!(effects.iter().any(|e| matches!(
        e,
        Effect::GrepStart {
            respect_gitignore: false,
            ..
        }
    )));
}

#[test]
fn a_reloaded_config_reseeds_only_changed_finder_defaults() {
    let mut s = state_with_files_config(false, false);
    // A session toggle survives a reload that does not touch `[files]`.
    s.search.finder_hidden = true;
    let same = s.config.clone();
    update(
        &mut s,
        Msg::ConfigReloaded {
            config: Box::new(same),
            keymap: Box::new(Keymap::defaults()),
            issues: Vec::new(),
        },
    );
    assert!(s.search.finder_hidden);
    let mut changed = s.config.clone();
    changed.files.show_ignored = true;
    update(
        &mut s,
        Msg::ConfigReloaded {
            config: Box::new(changed),
            keymap: Box::new(Keymap::defaults()),
            issues: Vec::new(),
        },
    );
    assert!(s.search.finder_gitignore);
    assert!(s.search.finder_hidden);
}
