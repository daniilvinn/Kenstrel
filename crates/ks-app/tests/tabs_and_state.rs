//! Headless end-to-end tests of phase P7 with the real effect runner, real directories and
//! real state files: tabs (listing and watcher follow the active tab), bookmarks and frecency
//! persisting across runs, damaged state files, and the cwd file written on `Q`.

use std::fs;
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant, SystemTime};

use crossterm::event::{Event, KeyCode, KeyEvent, KeyEventKind, KeyEventState, KeyModifiers};
use futures_util::{stream, StreamExt};
use ks_app::effects::EffectRunner;
use ks_app::event_loop::{run_with_state, LoopOptions};
use ks_app::persist::StateStore;
use ks_core::{
    pending_writes, update, AppState, Config, KeyChord, Keymap, Msg, Places, QuitKind, Size,
    StartParams, StateFile,
};
use ks_ui::{ColorDepth, Theme};
use ratatui::backend::TestBackend;
use ratatui::Terminal;
use tokio::sync::mpsc::{self, Receiver};

const SLACK: Duration = Duration::from_secs(5);

fn new_state(cwd: &Path) -> AppState {
    AppState::new(StartParams {
        cwd: cwd.to_owned(),
        home: None,
        places: Places::default(),
        config: Config::default(),
        keymap: Keymap::defaults(),
        size: Size::new(100, 30),
        now: SystemTime::now(),
        utc_offset_secs: 0,
    })
}

struct Harness {
    state: AppState,
    runner: EffectRunner,
    rx: Receiver<Msg>,
}

impl Harness {
    fn new(cwd: &Path, state_dir: Option<&Path>) -> Self {
        let (tx, rx) = mpsc::channel(256);
        let mut runner = EffectRunner::new(tx);
        if let Some(dir) = state_dir {
            runner.set_state_dir(dir.to_owned());
        }
        Self {
            state: new_state(cwd),
            runner,
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

    async fn until(&mut self, what: &str, done: impl Fn(&AppState) -> bool) {
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
    }

    async fn listed(&mut self, what: &str) {
        self.until(what, |s| {
            !s.view.loading && !s.is_navigating() && !s.view.is_empty()
        })
        .await;
    }

    fn names(&self) -> Vec<String> {
        (0..self.state.view.len())
            .filter_map(|i| self.state.view.row(i))
            .map(|e| e.display_name().into_owned())
            .collect()
    }

    fn describe(&self) -> String {
        format!(
            "cwd={} loading={} navigating={} entries={:?} status={:?}",
            self.state.cwd.display(),
            self.state.view.loading,
            self.state.is_navigating(),
            self.names(),
            self.state.status.as_ref().map(|s| &s.text)
        )
    }

    /// What quitting does: flush the debounced writes and wait for the disk.
    fn quit(mut self) {
        self.runner.run(pending_writes(&mut self.state));
        self.runner.finish_state_writes(Duration::from_secs(5));
    }
}

fn project() -> tempfile::TempDir {
    let dir = tempfile::tempdir().unwrap();
    fs::create_dir_all(dir.path().join("alpha/inner")).unwrap();
    fs::create_dir(dir.path().join("beta")).unwrap();
    fs::write(dir.path().join("notes.txt"), "n").unwrap();
    fs::write(dir.path().join("alpha/a.txt"), "a").unwrap();
    dir
}

/// The watcher is armed on a background thread right after a listing; give it a moment so
/// the change below is not made before it exists.
async fn arm_wait() {
    tokio::time::sleep(Duration::from_millis(400)).await;
}

fn same(a: &Path, b: &Path) -> bool {
    dunce::simplified(a).to_string_lossy().to_lowercase()
        == dunce::simplified(b).to_string_lossy().to_lowercase()
}

#[tokio::test(flavor = "multi_thread")]
async fn tabs_follow_the_file_system_and_keep_their_own_directories() {
    let dir = project();
    let root = dir.path().to_owned();
    let mut h = Harness::new(&root, None);
    h.send(Msg::Start);
    h.listed("the first listing").await;
    assert_eq!(h.names(), ["alpha", "beta", "notes.txt"]);

    // Tab 2 goes into alpha; tab 1 stays at the root.
    h.keys("t");
    h.listed("tab 2").await;
    assert_eq!(h.state.tabs.count(), 2);
    h.keys("l");
    h.until("alpha in tab 2", |s| same(&s.cwd, &root.join("alpha")))
        .await;
    h.listed("alpha").await;
    assert_eq!(h.names(), ["inner", "a.txt"]);

    // A change made while tab 1 is parked shows when it comes back.
    fs::write(root.join("added-while-away.txt"), "x").unwrap();
    h.keys("1");
    assert!(same(&h.state.cwd, &root));
    h.until("the refresh of tab 1", |s| {
        (0..s.view.len())
            .filter_map(|i| s.view.row(i))
            .any(|e| e.display_name() == "added-while-away.txt")
    })
    .await;

    // The watcher now follows tab 1, and again tab 2 after switching to it.
    arm_wait().await;
    fs::write(root.join("watched-in-tab-1.txt"), "x").unwrap();
    h.until("the watcher of tab 1", |s| {
        (0..s.view.len())
            .filter_map(|i| s.view.row(i))
            .any(|e| e.display_name() == "watched-in-tab-1.txt")
    })
    .await;
    h.keys("2");
    h.listed("tab 2 again").await;
    assert!(same(&h.state.cwd, &root.join("alpha")));
    arm_wait().await;
    fs::write(root.join("alpha").join("fresh.txt"), "x").unwrap();
    h.until("the watcher of tab 2", |s| {
        (0..s.view.len())
            .filter_map(|i| s.view.row(i))
            .any(|e| e.display_name() == "fresh.txt")
    })
    .await;

    // Closing tab 2 leaves tab 1 where it was.
    h.keys("ctrl-w");
    assert_eq!(h.state.tabs.count(), 1);
    h.listed("tab 1 after closing").await;
    assert!(same(&h.state.cwd, &root));
    assert!(h.names().contains(&"watched-in-tab-1.txt".to_owned()));
}

#[tokio::test(flavor = "multi_thread")]
async fn bookmarks_and_frecency_survive_a_restart() {
    let dir = project();
    let state_dir = tempfile::tempdir().unwrap();
    let root = dir.path().to_owned();

    // First run: visit alpha twice and bookmark it, then quit.
    let mut first = Harness::new(&root, Some(state_dir.path()));
    first.send(Msg::Start);
    first.listed("the first listing").await;
    first.keys("l");
    first
        .until("alpha", |s| same(&s.cwd, &root.join("alpha")))
        .await;
    first.keys("m a h");
    first.until("the root", |s| same(&s.cwd, &root)).await;
    first.listed("the root listing").await;
    first.keys("l");
    first
        .until("alpha again", |s| same(&s.cwd, &root.join("alpha")))
        .await;
    first.listed("alpha again").await;
    first.quit();

    let store = StateStore::new(state_dir.path().to_owned());
    assert!(store.path(StateFile::Bookmarks).exists());
    assert!(store.path(StateFile::Frecency).exists());

    // Second run: the data is loaded in the background after start.
    let mut second = Harness::new(&root, Some(state_dir.path()));
    second.send(Msg::Start);
    second
        .until("the persisted data", |s| !s.aids.bookmarks.is_empty())
        .await;
    assert!(second.state.aids.frecency.len() >= 2);
    second.listed("the listing").await;
    second.keys("' a");
    second
        .until("the bookmark jump", |s| same(&s.cwd, &root.join("alpha")))
        .await;
    let alpha = second.state.aids.frecency.get(&root.join("alpha")).unwrap();
    assert!(alpha.rank >= 3.0, "{alpha:?}");

    // `z` finds the directory, `enter` goes there.
    second.keys("g h");
    second.listed("the home listing").await;
    second.keys("z a l p");
    assert_eq!(second.state.aids.jump.hits.len(), 1);
    second.keys("enter");
    second
        .until("the z jump", |s| same(&s.cwd, &root.join("alpha")))
        .await;
}

#[tokio::test(flavor = "multi_thread")]
async fn damaged_state_files_do_not_stop_the_app() {
    let dir = project();
    let state_dir = tempfile::tempdir().unwrap();
    fs::write(state_dir.path().join("bookmarks.toml"), "[[[ nope").unwrap();
    fs::write(state_dir.path().join("frecency.toml"), "\u{0}\u{1}garbage").unwrap();

    let mut h = Harness::new(dir.path(), Some(state_dir.path()));
    h.send(Msg::Start);
    h.until("the warning", |s| {
        s.status
            .as_ref()
            .is_some_and(|st| st.text.contains("damaged"))
    })
    .await;
    h.listed("the listing").await;
    assert!(h.state.aids.bookmarks.is_empty());
    // The damaged files were set aside, and new data is saved normally.
    assert!(state_dir.path().join("bookmarks.toml.bad").exists());
    h.keys("m x");
    h.quit();
    let text = fs::read_to_string(state_dir.path().join("bookmarks.toml")).unwrap();
    assert!(text.contains('x'), "{text}");
}

fn key(c: char) -> std::io::Result<Event> {
    Ok(Event::Key(KeyEvent {
        code: KeyCode::Char(c),
        modifiers: KeyModifiers::NONE,
        kind: KeyEventKind::Press,
        state: KeyEventState::NONE,
    }))
}

/// The real loop: bookmark, visit, quit; the files exist when it returns.
#[tokio::test(flavor = "multi_thread")]
async fn the_event_loop_saves_state_on_exit() {
    let dir = project();
    let state_dir = tempfile::tempdir().unwrap();
    let mut state = new_state(dir.path());
    let mut terminal = Terminal::new(TestBackend::new(100, 30)).unwrap();
    let theme = Theme::builtin("ember", ColorDepth::TrueColor).unwrap();
    let events = stream::iter([key('m'), key('b')])
        .chain(stream::once(async {
            // Give the start-up listing time to commit (the visit counts then).
            tokio::time::sleep(Duration::from_millis(400)).await;
            key('Q')
        }))
        .chain(stream::pending());
    let quit = run_with_state(
        &mut terminal,
        events,
        &mut state,
        &theme,
        LoopOptions {
            startup: vec![Msg::Start],
            panic_after_first_frame: false,
        },
        None,
        Some(state_dir.path().to_owned()),
    )
    .await
    .unwrap();
    assert_eq!(quit, QuitKind::ChangeDir);

    let loaded = StateStore::new(state_dir.path().to_owned()).load();
    assert!(loaded.problems.is_empty(), "{:?}", loaded.problems);
    assert_eq!(
        loaded.bookmarks.get('b').map(PathBuf::from).as_deref(),
        Some(dir.path())
    );
    assert!(loaded.frecency.get(dir.path()).is_some());
}
