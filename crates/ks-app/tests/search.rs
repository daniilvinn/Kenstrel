#![allow(clippy::print_stderr)]

//! Headless end-to-end tests of search: the real `update`, the real effect runner, the
//! real search engines and a real temporary tree, with no terminal.

use std::fs;
use std::path::Path;
use std::time::{Duration, Instant, SystemTime};

use ks_app::effects::EffectRunner;
use ks_app::search::FinderWorker;
use ks_core::{
    update, AppState, Config, Effect, FinderSnapshot, KeyChord, Keymap, Msg, Overlay, Places, Size,
    StartParams,
};
use ks_search::finder::FinderItem;
use ks_search::{FinderOptions, FinderSession};
use tokio::sync::mpsc::{self, Receiver};

const SLACK: Duration = Duration::from_secs(30);

struct Harness {
    state: AppState,
    runner: EffectRunner,
    rx: Receiver<Msg>,
}

impl Harness {
    fn new(cwd: &Path) -> Self {
        let (tx, rx) = mpsc::channel(256);
        let state = AppState::new(StartParams {
            cwd: cwd.to_owned(),
            home: None,
            places: Places::default(),
            config: Config::default(),
            keymap: Keymap::defaults(),
            size: Size::new(100, 30),
            now: SystemTime::now(),
            utc_offset_secs: 0,
        });
        Self {
            state,
            runner: EffectRunner::new(tx),
            rx,
        }
    }

    fn send(&mut self, msg: Msg) {
        let effects = update(&mut self.state, msg);
        self.runner.run(effects);
    }

    fn keys(&mut self, text: &str) {
        for key in text.split(' ') {
            self.send(Msg::Key(key.parse::<KeyChord>().unwrap()));
        }
    }

    fn type_text(&mut self, text: &str) {
        for c in text.chars() {
            self.send(Msg::Key(KeyChord::char(c)));
        }
    }

    async fn until(&mut self, what: &str, done: impl Fn(&AppState) -> bool) -> Duration {
        let started = Instant::now();
        while !done(&self.state) {
            let remaining = SLACK
                .checked_sub(started.elapsed())
                .unwrap_or_else(|| panic!("timed out waiting for {what}"));
            let msg = tokio::time::timeout(remaining, self.rx.recv())
                .await
                .unwrap_or_else(|_| panic!("timed out waiting for {what}"))
                .expect("the runner keeps a sender alive");
            self.send(msg);
        }
        started.elapsed()
    }

    async fn settle(&mut self, wait: Duration) {
        let deadline = Instant::now() + wait;
        while let Some(remaining) = deadline.checked_duration_since(Instant::now()) {
            match tokio::time::timeout(remaining, self.rx.recv()).await {
                Ok(Some(msg)) => self.send(msg),
                _ => break,
            }
        }
    }

    async fn start(&mut self) {
        self.send(Msg::Start);
        self.until("the first listing", |s| {
            !s.view.loading && !s.is_navigating() && !s.view.is_empty()
        })
        .await;
    }

    fn finder_rels(&self) -> Vec<String> {
        self.state
            .search
            .finder
            .as_ref()
            .map(|f| f.results.iter().map(|r| r.rel.replace('\\', "/")).collect())
            .unwrap_or_default()
    }

    fn cursor(&self) -> Option<String> {
        self.state
            .view
            .current()
            .map(|e| e.display_name().into_owned())
    }
}

fn project() -> tempfile::TempDir {
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path();
    fs::create_dir_all(root.join("src/deep")).unwrap();
    fs::create_dir_all(root.join("docs")).unwrap();
    fs::write(root.join("README.md"), "# readme\nTODO: write docs\n").unwrap();
    fs::write(
        root.join("src/main.rs"),
        "fn main() {\n    // TODO later\n}\n",
    )
    .unwrap();
    fs::write(root.join("src/lib.rs"), "pub fn lib() {}\n").unwrap();
    fs::write(
        root.join("src/deep/handler.rs"),
        "// handler\nfn handle() {}\nfn Handle2() {}\n",
    )
    .unwrap();
    fs::write(root.join("docs/guide.md"), "the guide\n").unwrap();
    fs::write(root.join("docs/binary.bin"), b"TODO\0\0\0binary").unwrap();
    dir
}

fn canonical(dir: &tempfile::TempDir) -> std::path::PathBuf {
    dunce::canonicalize(dir.path()).unwrap()
}

#[tokio::test(flavor = "multi_thread")]
async fn the_in_directory_filter_works_through_the_real_loop() {
    let dir = project();
    let mut h = Harness::new(&canonical(&dir));
    h.start().await;
    h.keys("/");
    h.type_text("md");
    let names: Vec<String> = (0..h.state.view.len())
        .map(|i| h.state.view.row(i).unwrap().display_name().into_owned())
        .collect();
    assert_eq!(names, ["README.md"]);
    h.keys("enter");
    assert_eq!(h.state.view.filter_query(), Some("md"));
    // Give the directory watcher time to be established.
    h.settle(Duration::from_millis(400)).await;
    // A change on disk refreshes the listing and the filter keeps applying.
    fs::write(dir.path().join("CHANGES.md"), "x").unwrap();
    h.until("the refresh", |s| s.view.len() == 2).await;
    h.keys("esc");
    assert!(h.state.view.len() >= 4);
}

#[tokio::test(flavor = "multi_thread")]
async fn find_file_streams_results_and_enter_navigates_with_the_cursor_on_the_file() {
    let dir = project();
    let root = canonical(&dir);
    let mut h = Harness::new(&root);
    h.start().await;
    h.keys("ctrl-p");
    assert_eq!(h.state.overlays.last(), Some(&Overlay::Finder));
    h.until("the walk to finish", |s| {
        s.search
            .finder
            .as_ref()
            .is_some_and(|f| !f.walking && f.total >= 6)
    })
    .await;
    h.type_text("handler");
    h.until("results for the query", |s| {
        s.search
            .finder
            .as_ref()
            .is_some_and(|f| f.matched == 1 && f.results.len() == 1)
    })
    .await;
    assert_eq!(h.finder_rels()[0], "src/deep/handler.rs");

    h.keys("enter");
    assert!(h.state.overlays.is_empty());
    h.until("the file's directory", |s| {
        s.cwd == root.join("src").join("deep") && !s.view.loading && !s.view.is_empty()
    })
    .await;
    assert_eq!(h.cursor().as_deref(), Some("handler.rs"));
}

#[tokio::test(flavor = "multi_thread")]
async fn typing_in_the_finder_narrows_and_the_scope_cycle_restarts_the_walk() {
    let dir = project();
    let root = canonical(&dir);
    let mut h = Harness::new(&root.join("src"));
    h.start().await;
    h.keys("ctrl-p");
    h.until("src files", |s| {
        s.search
            .finder
            .as_ref()
            .is_some_and(|f| !f.walking && f.total == 3)
    })
    .await;
    // Scope: cwd -> git root (none: falls back to cwd) -> home -> drive. The root reported
    // in the snapshot changes with the scope.
    h.keys("ctrl-r ctrl-r ctrl-r");
    h.until("the drive scope", |s| {
        s.search
            .finder
            .as_ref()
            .is_some_and(|f| f.root.as_ref().is_some_and(|r| r.parent().is_none()) && f.total > 0)
    })
    .await;
    h.keys("ctrl-r");
    h.until("back to cwd", |s| {
        s.search
            .finder
            .as_ref()
            .is_some_and(|f| f.root.as_deref() == Some(root.join("src").as_path()) && f.total == 3)
    })
    .await;
}

#[tokio::test(flavor = "multi_thread")]
async fn reopening_the_finder_on_the_same_root_is_served_from_the_cache() {
    let dir = project();
    let root = canonical(&dir);
    let mut h = Harness::new(&root);
    h.start().await;
    h.keys("ctrl-p");
    h.until("the first walk", |s| {
        s.search
            .finder
            .as_ref()
            .is_some_and(|f| !f.walking && f.total >= 6)
    })
    .await;
    assert!(!h.state.search.finder.as_ref().unwrap().from_cache);
    h.keys("esc");
    h.settle(Duration::from_millis(100)).await;

    h.keys("ctrl-p");
    let took = h
        .until("the cached results", |s| {
            s.search
                .finder
                .as_ref()
                .is_some_and(|f| f.from_cache && !f.walking && f.total >= 6)
        })
        .await;
    eprintln!("reopen from cache: {took:?}");
}

#[tokio::test(flavor = "multi_thread")]
async fn the_walk_cache_follows_directory_changes() {
    let dir = project();
    let root = canonical(&dir);
    let mut h = Harness::new(&root);
    h.start().await;
    h.keys("ctrl-p");
    h.until("the first walk", |s| {
        s.search
            .finder
            .as_ref()
            .is_some_and(|f| !f.walking && f.total >= 6)
    })
    .await;
    h.keys("esc");
    h.settle(Duration::from_millis(200)).await;

    // A new file appears in the displayed directory; the watcher refresh feeds the cache.
    fs::write(root.join("fresh_file.txt"), "x").unwrap();
    h.until("the refreshed listing", |s| {
        (0..s.view.len()).any(|i| {
            s.view
                .row(i)
                .is_some_and(|e| e.display_name() == "fresh_file.txt")
        })
    })
    .await;
    h.settle(Duration::from_millis(300)).await;

    h.keys("ctrl-p");
    h.type_text("fresh_file");
    h.until("the new file from the cache", |s| {
        s.search
            .finder
            .as_ref()
            .is_some_and(|f| f.from_cache && f.results.iter().any(|r| r.rel == "fresh_file.txt"))
    })
    .await;
}

#[tokio::test(flavor = "multi_thread")]
async fn content_search_streams_groups_by_file_and_counts() {
    let dir = project();
    let root = canonical(&dir);
    let mut h = Harness::new(&root);
    h.start().await;
    h.keys("ctrl-f");
    h.type_text("TODO");
    h.until("the search to finish", |s| {
        s.search
            .grep
            .as_ref()
            .is_some_and(|g| !g.waiting && !g.running && g.stats.files_searched > 0)
    })
    .await;
    let g = h.state.search.grep.as_ref().unwrap();
    let files: Vec<String> = g
        .files
        .iter()
        .map(|f| f.rel.to_string_lossy().replace('\\', "/"))
        .collect();
    assert_eq!(files, ["README.md", "src/main.rs"], "sorted when done");
    assert_eq!(g.match_count(), 2);
    assert_eq!(g.stats.binary, 1, "binary.bin is counted, not searched");
    assert!(!g.capped);

    // `enter` hands the hit to the editor launcher with its line (the runner decides between a
    // detached GUI editor and a suspended terminal one); the effect is checked without running it.
    let effects = update(&mut h.state, Msg::Key("enter".parse::<KeyChord>().unwrap()));
    let open = effects.iter().find_map(|e| match e {
        Effect::OpenInEditor { path, line } => Some((path.clone(), *line)),
        _ => None,
    });
    let (path, line) = open.expect("enter opens the hit in the editor");
    // Both hits (README.md, src/main.rs) are on line 2; which one is highlighted is not the point.
    assert!(
        path.ends_with("main.rs") || path.ends_with("README.md"),
        "{path:?}"
    );
    assert_eq!(line, Some(2));

    // The line reaches the launch command: a terminal editor gets its `+line` argument.
    let settings = ks_app::fileops::EditorSettings {
        open_at_line: "nvim +{line} {file}".into(),
        terminal: ks_core::config::EditorTerminal::Always,
        ..ks_app::fileops::EditorSettings::default()
    };
    let (spec, terminal) = ks_app::fileops::editor_command(&settings, &path, line);
    assert!(terminal, "nvim runs in the terminal (suspend/resume)");
    assert!(
        spec.program == "nvim" && spec.args.iter().any(|a| a == "+2"),
        "line missing from {spec:?}"
    );
}

#[tokio::test(flavor = "multi_thread")]
async fn smart_case_regex_and_glob_filter() {
    let dir = project();
    let root = canonical(&dir);
    let mut h = Harness::new(&root);
    h.start().await;
    h.keys("ctrl-f");
    // Lowercase query: case-insensitive, so `handle` and `Handle2` both match.
    h.type_text("handle");
    h.until("results", |s| {
        s.search
            .grep
            .as_ref()
            .is_some_and(|g| !g.waiting && !g.running && g.match_count() > 0)
    })
    .await;
    assert_eq!(h.state.search.grep.as_ref().unwrap().match_count(), 3);

    // Restrict by glob: nothing matches in *.md.
    h.keys("tab");
    h.type_text("*.md");
    h.until("glob results", |s| {
        s.search.grep.as_ref().is_some_and(|g| {
            !g.waiting && !g.running && g.stats.files_searched > 0 && g.files.is_empty()
        })
    })
    .await;

    // Regex toggle on a pattern that is only valid as a regex.
    h.keys("ctrl-u tab ctrl-r ctrl-u");
    h.type_text("fn \\w+\\(\\)");
    h.until("regex results", |s| {
        s.search
            .grep
            .as_ref()
            .is_some_and(|g| !g.waiting && !g.running && g.regex && g.match_count() > 0)
    })
    .await;
}

#[tokio::test(flavor = "multi_thread")]
async fn a_bad_regex_reports_instead_of_hanging() {
    let dir = project();
    let mut h = Harness::new(&canonical(&dir));
    h.start().await;
    h.keys("ctrl-f ctrl-r");
    h.type_text("(");
    h.until("the error", |s| {
        s.search.grep.as_ref().is_some_and(|g| g.error.is_some())
    })
    .await;
    assert!(!h.state.search.grep.as_ref().unwrap().running);
}

#[tokio::test(flavor = "multi_thread")]
async fn fast_typing_cancels_earlier_searches_and_only_the_last_one_reports() {
    // A larger tree so the first searches would still be running.
    let dir = tempfile::tempdir().unwrap();
    let root = canonical(&dir);
    ks_search::testgen::generate_tree(&root, 3_000, 7).unwrap();
    let mut h = Harness::new(&root);
    h.start().await;
    h.keys("ctrl-f");
    // Keys faster than the 120 ms debounce: only one search may start.
    for c in "NEEDLE_MARKER".chars() {
        h.send(Msg::Key(KeyChord::char(c)));
        tokio::time::sleep(Duration::from_millis(10)).await;
    }
    let gen_after_typing = h.state.search.grep.as_ref().unwrap().generation();
    h.until("the final search", |s| {
        s.search
            .grep
            .as_ref()
            .is_some_and(|g| !g.waiting && !g.running && g.stats.files_searched > 0)
    })
    .await;
    let g = h.state.search.grep.as_ref().unwrap();
    assert!(
        g.match_count() > 0,
        "the marker occurs in the generated tree"
    );
    assert_eq!(g.query.text(), "NEEDLE_MARKER");
    assert!(g.generation() > gen_after_typing);
    // Changing the query mid-search drops the old results and finishes cleanly.
    h.keys("backspace");
    h.until("the new search to finish", |s| {
        s.search
            .grep
            .as_ref()
            .is_some_and(|g| !g.waiting && !g.running && g.query.text() == "NEEDLE_MARKE")
    })
    .await;
}

#[tokio::test(flavor = "multi_thread")]
async fn ctrl_l_shows_the_matched_files_as_a_listing() {
    let dir = project();
    let root = canonical(&dir);
    let mut h = Harness::new(&root);
    h.start().await;
    h.keys("ctrl-f");
    h.type_text("TODO");
    h.until("results", |s| {
        s.search
            .grep
            .as_ref()
            .is_some_and(|g| !g.waiting && !g.running && g.match_count() == 2)
    })
    .await;
    h.keys("ctrl-l");
    assert_eq!(h.state.view.label.as_deref(), Some("Search results: TODO"));
    let names: Vec<String> = (0..h.state.view.len())
        .map(|i| {
            h.state
                .view
                .row(i)
                .unwrap()
                .display_name()
                .replace('\\', "/")
        })
        .collect();
    assert_eq!(names, ["README.md", "src/main.rs"]);
    // The directory watcher is off: a change in the directory must not replace the list.
    fs::write(root.join("zzz.txt"), "x").unwrap();
    h.settle(Duration::from_millis(500)).await;
    assert_eq!(h.state.view.len(), 2);

    h.keys("esc");
    h.until("the real listing", |s| {
        s.view.label.is_none() && s.view.len() > 2
    })
    .await;
}

// ----- performance -----

fn measure_keystrokes(items: usize) -> Vec<(String, Duration)> {
    let paths = ks_search::testgen::synthetic_paths(items, 1);
    let (tx, mut rx) = mpsc::channel(64);
    let root = std::path::PathBuf::from(if cfg!(windows) { "C:\\bench" } else { "/bench" });
    let make_root = root.clone();
    let worker = FinderWorker::spawn(
        1,
        move |notify| {
            let session = FinderSession::from_items(
                FinderOptions::new(make_root.clone()),
                1,
                paths
                    .iter()
                    .map(|p| FinderItem::new(p.as_str(), ks_search::EntryKind::File)),
                notify,
            );
            (session, make_root)
        },
        String::new(),
        0,
        45,
        tx,
    );
    let wait_for = |rx: &mut Receiver<Msg>, query: &str| -> FinderSnapshot {
        let deadline = Instant::now() + Duration::from_secs(30);
        loop {
            match rx.try_recv() {
                Ok(Msg::FinderResults { snapshot, .. }) if snapshot.query == query => {
                    return *snapshot;
                }
                Ok(_) => {}
                Err(_) => {
                    assert!(Instant::now() < deadline, "no results for `{query}`");
                    std::thread::sleep(Duration::from_micros(200));
                }
            }
        }
    };
    // Wait for the initial load, then type the query one character at a time.
    let initial = wait_for(&mut rx, "");
    assert_eq!(initial.total as usize, items);
    let mut timings = Vec::new();
    let mut typed = String::new();
    for c in "handler cache".chars() {
        typed.push(c);
        let started = Instant::now();
        worker.set_query(typed.clone(), 0, 45);
        let snapshot = wait_for(&mut rx, &typed);
        timings.push((typed.clone(), started.elapsed()));
        assert!(snapshot.matched <= snapshot.total);
    }
    drop(worker);
    timings
}

/// Keystroke to results on 500k synthetic paths, through the real worker thread and message
/// channel. Run with `cargo test -p ks-app --release -- --ignored finder_keystroke`.
#[test]
#[ignore = "performance measurement; run in release mode"]
fn finder_keystroke_latency_on_500k_paths() {
    let timings = measure_keystrokes(500_000);
    let worst = timings.iter().map(|(_, t)| *t).max().unwrap();
    for (query, took) in &timings {
        eprintln!("keystroke -> results: `{query}` {took:?}");
    }
    eprintln!("worst: {worst:?}");
    assert!(
        worst < Duration::from_millis(25),
        "budget is 25 ms, worst was {worst:?}"
    );
}

#[test]
fn finder_worker_answers_a_keystroke_on_a_small_list() {
    let timings = measure_keystrokes(5_000);
    assert_eq!(timings.len(), "handler cache".len());
    // A generous bound (the real 25 ms budget is checked in release by the ignored test):
    // catches an accidental O(n^2) or a blocking call in the default run.
    let worst = timings.iter().map(|(_, t)| *t).max().unwrap();
    assert!(
        worst < Duration::from_millis(500),
        "5k paths took {worst:?}"
    );
}
