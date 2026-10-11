#![allow(clippy::print_stderr)]

//! Headless end-to-end tests of phase P1: the real `update`, the real effect runner and a
//! real temporary directory, with no terminal. A tiny harness plays the event loop's part:
//! it feeds messages to `update` and hands the resulting effects to the runner.

use std::fs;
use std::path::Path;
use std::process::Command;
use std::time::{Duration, Instant, SystemTime};

use ks_app::effects::EffectRunner;
use ks_core::{update, AppState, Config, KeyChord, Keymap, Msg, Places, Size, StartParams};
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

    fn start(&mut self) {
        self.send(Msg::Start);
    }

    /// Presses space-separated keys, one message each.
    fn keys(&mut self, text: &str) {
        for key in text.split(' ') {
            self.send(Msg::Key(key.parse::<KeyChord>().unwrap()));
        }
    }

    /// Processes background results until `done` holds; returns how long that took.
    async fn until(&mut self, what: &str, done: impl Fn(&AppState) -> bool) -> Duration {
        let started = Instant::now();
        while !done(&self.state) {
            let remaining = SLACK
                .checked_sub(started.elapsed())
                .unwrap_or_else(|| panic!("timed out waiting for {what}\n{}", self.describe()));
            let msg = tokio::time::timeout(remaining, self.rx.recv())
                .await
                .unwrap_or_else(|_| panic!("timed out waiting for {what}\n{}", self.describe()))
                .expect("the runner keeps a sender alive");
            self.send(msg);
        }
        started.elapsed()
    }

    /// Lets everything that is already queued (and what arrives within `wait`) take effect.
    async fn settle(&mut self, wait: Duration) {
        let deadline = Instant::now() + wait;
        while let Some(remaining) = deadline.checked_duration_since(Instant::now()) {
            match tokio::time::timeout(remaining, self.rx.recv()).await {
                Ok(Some(msg)) => self.send(msg),
                _ => break,
            }
        }
    }

    fn names(&self) -> Vec<String> {
        (0..self.state.view.len())
            .filter_map(|i| self.state.view.row(i))
            .map(|e| e.display_name().into_owned())
            .collect()
    }

    fn cursor(&self) -> Option<String> {
        self.state
            .view
            .current()
            .map(|e| e.display_name().into_owned())
    }

    fn describe(&self) -> String {
        format!(
            "cwd={} loading={} entries={:?}",
            self.state.cwd.display(),
            self.state.view.loading,
            self.names()
        )
    }
}

fn project() -> tempfile::TempDir {
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path();
    fs::create_dir_all(root.join("alpha/inner")).unwrap();
    fs::create_dir(root.join("beta")).unwrap();
    for name in ["notes.txt", "readme.md", "zeta.rs"] {
        fs::write(root.join(name), "content").unwrap();
    }
    fs::write(root.join("alpha/a.txt"), "a").unwrap();
    fs::write(root.join(".hidden"), "h").unwrap();
    dir
}

#[tokio::test(flavor = "multi_thread")]
async fn start_lists_the_directory_sorted_without_hidden_files() {
    let dir = project();
    let mut h = Harness::new(dir.path());
    h.start();
    h.until("the first listing", |s| {
        !s.view.loading && !s.view.is_empty()
    })
    .await;
    assert_eq!(
        h.names(),
        ["alpha", "beta", "notes.txt", "readme.md", "zeta.rs"]
    );
    h.keys(".");
    assert_eq!(
        h.names(),
        [
            "alpha",
            "beta",
            ".hidden",
            "notes.txt",
            "readme.md",
            "zeta.rs"
        ]
    );
}

#[tokio::test(flavor = "multi_thread")]
async fn keyboard_only_navigation_down_and_back_up() {
    let dir = project();
    let mut h = Harness::new(dir.path());
    h.start();
    h.until("the first listing", |s| {
        !s.view.loading && !s.view.is_empty()
    })
    .await;

    // Into alpha, into inner, then back out; the cursor remembers where we came from.
    h.keys("l");
    let alpha = h.state.cwd.join("alpha");
    h.until("alpha", |s| s.cwd == alpha && !s.view.loading)
        .await;
    assert_eq!(h.names(), ["inner", "a.txt"]);
    h.keys("l");
    let inner = alpha.join("inner");
    h.until("inner", |s| s.cwd == inner && !s.view.loading)
        .await;
    assert!(h.names().is_empty());
    h.keys("h");
    h.until("alpha again", |s| s.cwd == alpha && !s.view.loading)
        .await;
    assert_eq!(h.cursor().as_deref(), Some("inner"));
    h.keys("h");
    h.until("the root again", |s| s.cwd == dir.path() && !s.view.loading)
        .await;
    assert_eq!(h.cursor().as_deref(), Some("alpha"));

    // History: back goes to alpha, forward returns to the root.
    h.keys("ctrl-o");
    h.until("history back", |s| s.cwd == alpha && !s.view.loading)
        .await;
    h.keys("tab");
    h.until("history forward", |s| {
        s.cwd == dir.path() && !s.view.loading
    })
    .await;
}

#[tokio::test(flavor = "multi_thread")]
async fn child_counts_fill_in_after_the_listing() {
    let dir = project();
    let mut h = Harness::new(dir.path());
    h.start();
    h.until("child counts", |s| {
        s.view
            .entries()
            .iter()
            .filter(|e| e.is_dir)
            .all(|e| e.children.is_some())
            && !s.view.is_empty()
    })
    .await;
    let count = |name: &str| {
        h.state
            .view
            .entries()
            .iter()
            .find(|e| e.display_name() == name)
            .and_then(|e| e.children)
    };
    assert_eq!(count("alpha"), Some(2));
    assert_eq!(count("beta"), Some(0));
}

#[tokio::test(flavor = "multi_thread")]
async fn external_changes_appear_quickly_and_the_cursor_stays_on_its_entry() {
    let dir = project();
    let mut h = Harness::new(dir.path());
    h.start();
    h.until("the first listing", |s| {
        !s.view.loading && !s.view.is_empty()
    })
    .await;
    h.settle(Duration::from_millis(300)).await; // let the watcher get established

    // Put the cursor on `notes.txt`.
    h.keys("j j");
    assert_eq!(h.cursor().as_deref(), Some("notes.txt"));
    let index_before = h.state.view.cursor.index;

    // A file sorting *before* the cursor appears: the cursor must stay on `notes.txt`.
    let created = Instant::now();
    fs::write(dir.path().join("aaa_new.txt"), "x").unwrap();
    let waited = h
        .until("the new file", |s| {
            s.view
                .entries()
                .iter()
                .any(|e| e.display_name() == "aaa_new.txt")
        })
        .await;
    let latency = created.elapsed();
    eprintln!("create -> visible in the list: {latency:?} (waited {waited:?})");
    assert!(
        latency < Duration::from_millis(1000),
        "external change took {latency:?}"
    );
    assert_eq!(h.cursor().as_deref(), Some("notes.txt"));
    assert_eq!(h.state.view.cursor.index, index_before + 1);

    // Deleting a file above the cursor shifts it back.
    fs::remove_file(dir.path().join("aaa_new.txt")).unwrap();
    h.until("the file to vanish", |s| {
        !s.view
            .entries()
            .iter()
            .any(|e| e.display_name() == "aaa_new.txt")
    })
    .await;
    assert_eq!(h.cursor().as_deref(), Some("notes.txt"));
    assert_eq!(h.state.view.cursor.index, index_before);

    // Renames show up as one name replacing another.
    fs::rename(dir.path().join("zeta.rs"), dir.path().join("omega.rs")).unwrap();
    h.until("the rename", |s| {
        s.view
            .entries()
            .iter()
            .any(|e| e.display_name() == "omega.rs")
            && !s
                .view
                .entries()
                .iter()
                .any(|e| e.display_name() == "zeta.rs")
    })
    .await;
}

#[tokio::test(flavor = "multi_thread")]
async fn marks_survive_a_refresh() {
    let dir = project();
    let mut h = Harness::new(dir.path());
    h.start();
    h.until("the first listing", |s| {
        !s.view.loading && !s.view.is_empty()
    })
    .await;
    h.settle(Duration::from_millis(300)).await;
    h.keys("j j space");
    assert_eq!(h.state.view.marks_count(), 1);
    fs::write(dir.path().join("another.txt"), "x").unwrap();
    h.until("the new file", |s| {
        s.view
            .entries()
            .iter()
            .any(|e| e.display_name() == "another.txt")
    })
    .await;
    assert_eq!(h.state.view.marks_count(), 1);
}

#[tokio::test(flavor = "multi_thread")]
async fn a_deleted_current_directory_moves_to_its_parent() {
    let dir = project();
    let mut h = Harness::new(dir.path());
    h.start();
    h.until("the first listing", |s| {
        !s.view.loading && !s.view.is_empty()
    })
    .await;
    h.keys("j l");
    let beta = dir.path().join("beta");
    h.until("beta", |s| s.cwd == beta && !s.view.loading).await;
    h.settle(Duration::from_millis(300)).await;

    fs::remove_dir(&beta).unwrap();
    h.until("the parent", |s| s.cwd == dir.path() && !s.view.loading)
        .await;
    // `beta` is gone, so the cursor falls back to the top of the list.
    assert_eq!(h.cursor().as_deref(), Some("alpha"));
}

#[tokio::test(flavor = "multi_thread")]
async fn going_to_a_missing_directory_reports_and_stays() {
    let dir = project();
    let mut h = Harness::new(dir.path());
    h.start();
    h.until("the first listing", |s| {
        !s.view.loading && !s.view.is_empty()
    })
    .await;
    // Remove `beta` behind the program's back, before the watcher notices, then enter it.
    h.keys("j");
    fs::remove_dir(dir.path().join("beta")).unwrap();
    h.keys("l");
    h.until("an error message", |s| s.status.is_some()).await;
    assert_eq!(h.state.cwd, dir.path());
    let text = h.state.status.as_ref().unwrap().text.clone();
    assert!(
        text.contains("beta") || text.contains("Cannot open"),
        "{text}"
    );
}

#[tokio::test(flavor = "multi_thread")]
async fn drives_view_lists_this_pc() {
    let dir = project();
    let mut h = Harness::new(dir.path());
    h.start();
    h.until("the first listing", |s| {
        !s.view.loading && !s.view.is_empty()
    })
    .await;
    h.keys("g /");
    h.until("the drives", |s| s.show_drives).await;
    assert!(!h.state.drives.drives.is_empty());
    // `l` on the selected drive enters its root.
    h.keys("l");
    h.until("a drive root", |s| !s.show_drives && !s.view.loading)
        .await;
    assert!(h.state.cwd.parent().is_none(), "{}", h.state.cwd.display());
}

#[tokio::test(flavor = "multi_thread")]
async fn going_up_from_a_drive_root_shows_this_pc() {
    let dir = project();
    let root = dunce::canonicalize(dir.path())
        .unwrap()
        .ancestors()
        .last()
        .map(Path::to_path_buf)
        .unwrap();
    let mut h = Harness::new(&root);
    h.start();
    h.until("the root listing", |s| !s.view.loading && s.cwd == root)
        .await;
    h.keys("h");
    h.until("This PC", |s| s.show_drives).await;
    // The cursor sits on the drive we came from.
    let current = h.state.drives.current().unwrap().root.clone();
    assert!(
        root.to_string_lossy()
            .to_lowercase()
            .starts_with(&current.to_string_lossy().to_lowercase()),
        "{} vs {}",
        root.display(),
        current.display()
    );
}

fn git_available() -> bool {
    Command::new("git")
        .arg("--version")
        .output()
        .is_ok_and(|o| o.status.success())
}

fn git(dir: &Path, args: &[&str]) {
    let out = Command::new("git")
        .arg("-C")
        .arg(dir)
        .args(["-c", "user.name=t", "-c", "user.email=t@example.com"])
        .args(["-c", "commit.gpgsign=false"])
        .args(args)
        .output()
        .unwrap();
    assert!(out.status.success(), "git {args:?}: {out:?}");
}

#[tokio::test(flavor = "multi_thread")]
async fn git_status_follows_the_work_tree() {
    if !git_available() {
        eprintln!("git not installed; skipping");
        return;
    }
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path();
    git(root, &["init", "-q", "-b", "main"]);
    fs::write(root.join("tracked.txt"), "one\n").unwrap();
    fs::write(root.join("other.txt"), "x\n").unwrap();
    git(root, &["add", "."]);
    git(root, &["commit", "-q", "-m", "init"]);

    let mut h = Harness::new(root);
    h.start();
    h.until("a clean git status", |s| {
        s.git.as_ref().is_some_and(|g| g.counts.is_clean())
    })
    .await;
    assert_eq!(h.state.git.as_ref().unwrap().branch.label(), "main");
    h.settle(Duration::from_millis(300)).await;

    // Editing a tracked file shows up through the watcher, the 300 ms debounce and git.
    let edited = Instant::now();
    fs::write(root.join("tracked.txt"), "two\n").unwrap();
    h.until("the modified glyph", |s| {
        s.git
            .as_ref()
            .and_then(|g| g.state_of("tracked.txt"))
            .is_some()
    })
    .await;
    eprintln!("edit -> git glyph: {:?}", edited.elapsed());
    assert_eq!(
        h.state.git.as_ref().unwrap().state_of("tracked.txt"),
        Some(ks_core::GitState::Modified)
    );
    assert_eq!(h.state.git.as_ref().unwrap().state_of("other.txt"), None);
}

/// An always-on guard against a listing or merge step turning quadratic: 20k files must be
/// listed and sorted well inside a generous bound, even in a debug build.
#[tokio::test(flavor = "multi_thread")]
async fn a_directory_of_20k_files_lists_in_reasonable_time() {
    const FILES: usize = 20_000;
    let dir = tempfile::tempdir().unwrap();
    let big = dir.path().join("big");
    fs::create_dir(&big).unwrap();
    for i in 0..FILES {
        fs::write(big.join(format!("file_{i:05}.dat")), b"").unwrap();
    }
    let mut h = Harness::new(&big);
    let started = Instant::now();
    h.start();
    h.until("the complete listing", |s| {
        !s.view.loading && s.view.len() == FILES
    })
    .await;
    let total = started.elapsed();
    assert!(total < Duration::from_secs(10), "20k listing: {total:?}");
    assert_eq!(h.cursor().as_deref(), Some("file_00000.dat"));
}

/// The performance budget of PLAN §11: entering a directory with 100k entries shows the first
/// rows in under 150 ms and has listed everything in under a second. Creating 100k files
/// takes a while, so this runs on demand:
///
/// `cargo test -p ks-app --release --test navigation -- --ignored --nocapture`
#[tokio::test(flavor = "multi_thread")]
#[ignore = "creates 100,000 files; run with --ignored"]
async fn a_directory_of_100k_files_meets_the_budget() {
    const FILES: usize = 100_000;
    let dir = tempfile::tempdir().unwrap();
    let big = dir.path().join("big");
    fs::create_dir(&big).unwrap();
    let created = Instant::now();
    for i in 0..FILES {
        fs::write(big.join(format!("file_{i:06}.dat")), b"").unwrap();
    }
    eprintln!("created {FILES} files in {:?}", created.elapsed());

    let mut h = Harness::new(&big);
    let started = Instant::now();
    h.start();
    let first = h.until("the first rows", |s| !s.view.is_empty()).await;
    let first_at = started.elapsed();
    let full = h
        .until("the complete listing", |s| {
            !s.view.loading && s.view.len() == FILES
        })
        .await;
    let total = started.elapsed();
    eprintln!(
        "100k entries: first rows after {first_at:?} (waited {first:?}), \
         complete and sorted after {total:?} (waited {full:?})"
    );
    assert_eq!(h.state.view.len(), FILES);
    assert_eq!(h.cursor().as_deref(), Some("file_000000.dat"));
    if !cfg!(debug_assertions) {
        assert!(
            first_at < Duration::from_millis(150),
            "first rows: {first_at:?}"
        );
        assert!(total < Duration::from_secs(1), "full listing: {total:?}");
    }
}
