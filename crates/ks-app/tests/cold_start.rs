#![allow(clippy::print_stderr)]

//! Cold start (PLAN P8: first frame in under 60 ms). Headless: the real session build (config,
//! keymap, theme), the real event loop and effect runner on a `TestBackend`, a real directory.
//! The probe is the loop's "first frame drawn" hook, which is also where the real terminal
//! runs the graphics query and starts reading input, so nothing in this test waits for either.

use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant, SystemTime};

use crossterm::event::{Event, KeyCode, KeyEvent, KeyEventKind, KeyEventState, KeyModifiers};
use futures_util::{stream, StreamExt};
use ks_app::event_loop::{run_with_state, LoopOptions};
use ks_app::foreground::Foreground;
use ks_app::paths::AppPaths;
use ks_app::settings::{build_session, SessionInputs};
use ks_core::fileops::ForegroundSpec;
use ks_core::Size;
use ks_ui::ColorDepth;
use ratatui::backend::TestBackend;
use ratatui::Terminal;

struct Probe(Arc<Mutex<Vec<Instant>>>);

impl Foreground for Probe {
    fn run(&mut self, _: &ForegroundSpec) -> Result<Option<i32>, String> {
        Err("unused".into())
    }

    fn first_frame_drawn(&mut self) {
        self.0.lock().unwrap().push(Instant::now());
    }
}

fn press(c: char) -> std::io::Result<Event> {
    Ok(Event::Key(KeyEvent {
        code: KeyCode::Char(c),
        modifiers: KeyModifiers::NONE,
        kind: KeyEventKind::Press,
        state: KeyEventState::NONE,
    }))
}

async fn first_frame() -> Duration {
    let started = Instant::now();
    let root = tempfile::tempdir().unwrap();
    for i in 0..300 {
        std::fs::write(root.path().join(format!("file_{i:03}.txt")), "x").unwrap();
    }
    let paths = AppPaths::under(&root.path().join("cfg"));
    // The fixture is not part of the start-up cost.
    let started = started + started.elapsed();
    let session = build_session(SessionInputs {
        paths: &paths,
        cwd: root.path().to_owned(),
        home: None,
        places: ks_core::Places::default(),
        size: Size::new(120, 32),
        depth: ColorDepth::TrueColor,
        now: SystemTime::now(),
        utc_offset_secs: 0,
    });
    let mut state = session.state;
    let drawn = Arc::new(Mutex::new(Vec::new()));
    let mut terminal = Terminal::new(TestBackend::new(120, 32)).unwrap();
    // When the loop first takes an input event: never before the first-frame hook returned.
    let taken: Arc<Mutex<Vec<Instant>>> = Arc::default();
    let log = taken.clone();
    let events =
        stream::iter([press('q')]).inspect(move |_| log.lock().unwrap().push(Instant::now()));
    let options = LoopOptions {
        startup: session.startup,
        panic_after_first_frame: false,
        foreground: Some(Box::new(Probe(drawn.clone()))),
        claude: ks_app::claude::ClaudeOptions::default(),
    };
    run_with_state(
        &mut terminal,
        events,
        &mut state,
        &session.theme,
        options,
        None,
        None,
    )
    .await
    .unwrap();
    let marks = drawn.lock().unwrap();
    assert_eq!(marks.len(), 1, "the hook runs exactly once");
    let taken = taken.lock().unwrap();
    assert_eq!(taken.len(), 1, "the key was handled");
    assert!(
        marks[0] <= taken[0],
        "no input is read before the first-frame hook"
    );
    marks[0].duration_since(started)
}

#[tokio::test(flavor = "multi_thread")]
async fn the_first_frame_does_not_wait_for_the_listing_or_the_terminal() {
    // Warm the process once (first use of the file system, allocator), then measure.
    let _ = first_frame().await;
    let mut best = Duration::MAX;
    for _ in 0..5 {
        best = best.min(first_frame().await);
    }
    eprintln!("cold start: first frame after {best:?} (best of 5, session build included)");
    // The goal is 60 ms in a release build; unoptimised test builds get generous headroom.
    let budget = if cfg!(debug_assertions) { 400 } else { 60 };
    assert!(best < Duration::from_millis(budget), "{best:?}");
}
