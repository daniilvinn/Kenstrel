#![allow(clippy::print_stderr, clippy::print_stdout)]

//! Headless end-to-end tests of the preview wiring: the real `update`, the real
//! effect runner with the real preview engine, and real files in a temporary directory.

use std::fs;
use std::path::Path;
use std::time::{Duration, Instant, SystemTime};

use ks_app::effects::EffectRunner;
use ks_core::{update, AppState, Config, KeyChord, Keymap, Msg, Places, Size, StartParams};
use ks_preview::PreviewBody;
use tokio::sync::mpsc::{self, Receiver};

const SLACK: Duration = Duration::from_secs(30);

struct Harness {
    state: AppState,
    runner: EffectRunner,
    rx: Receiver<Msg>,
    /// `from_cache` of every main-pane result that reached `update`.
    arrivals: Vec<bool>,
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
            size: Size::new(120, 30),
            now: SystemTime::now(),
            utc_offset_secs: 0,
        });
        Self {
            state,
            runner: EffectRunner::new(tx),
            rx,
            arrivals: Vec::new(),
        }
    }

    fn send(&mut self, msg: Msg) {
        if let Msg::Preview(preview) = &msg {
            self.arrivals.push(preview.0.from_cache);
        }
        let effects = update(&mut self.state, msg);
        self.runner.run(effects);
    }

    fn keys(&mut self, text: &str) {
        for key in text.split(' ') {
            self.send(Msg::Key(key.parse::<KeyChord>().unwrap()));
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
        self.until("the listing", |s| !s.view.loading && !s.view.is_empty())
            .await;
    }

    fn cursor(&self) -> String {
        self.state
            .view
            .current()
            .map(|e| e.display_name().into_owned())
            .unwrap_or_default()
    }

    fn shown(&self) -> Option<&ks_preview::Preview> {
        self.state.preview.current.as_deref()
    }

    /// Waits until the preview for the file under the cursor is on screen.
    async fn until_shown(&mut self) {
        let name = self.cursor();
        self.until("the preview of the cursor file", move |s| {
            s.preview.current.as_deref().is_some_and(|p| {
                p.path
                    .file_name()
                    .is_some_and(|n| n.to_string_lossy() == name)
            })
        })
        .await;
    }
}

fn png_bytes() -> Vec<u8> {
    let image = image::RgbImage::from_fn(32, 16, |x, _| image::Rgb([x as u8 * 8, 100, 200]));
    let mut out = std::io::Cursor::new(Vec::new());
    image::DynamicImage::ImageRgb8(image)
        .write_to(&mut out, image::ImageFormat::Png)
        .unwrap();
    out.into_inner()
}

fn project() -> tempfile::TempDir {
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path();
    fs::create_dir_all(root.join("a_dir")).unwrap();
    fs::write(root.join("a_dir/inner.txt"), "x").unwrap();
    fs::write(
        root.join("b_main.rs"),
        "pub fn main() {\n    let x = 42; // answer\n}\n",
    )
    .unwrap();
    fs::write(root.join("c_blob.bin"), [0u8, 1, 2, 3, 0xff, 0xfe, 0, 0, 7]).unwrap();
    fs::write(root.join("d_pic.png"), png_bytes()).unwrap();
    fs::write(root.join("e_empty.txt"), "").unwrap();
    dir
}

#[tokio::test(flavor = "multi_thread")]
async fn each_kind_of_file_gets_its_preview() {
    let dir = project();
    let mut h = Harness::new(dir.path());
    h.start().await;

    // a_dir
    assert_eq!(h.cursor(), "a_dir");
    h.until_shown().await;
    assert!(matches!(h.shown().unwrap().body, PreviewBody::Directory(_)));

    h.keys("j");
    h.until_shown().await;
    let PreviewBody::Text(text) = &h.shown().unwrap().body else {
        panic!("expected text, got {:?}", h.shown().unwrap().body);
    };
    assert_eq!(text.syntax.as_deref(), Some("Rust"));
    assert!(
        text.lines
            .iter()
            .flat_map(|l| &l.spans)
            .any(|s| s.style.fg.is_some()),
        "highlighted spans carry colours"
    );

    h.keys("j");
    h.until_shown().await;
    assert!(matches!(h.shown().unwrap().body, PreviewBody::Binary(_)));

    h.keys("j");
    h.until_shown().await;
    let id = h.state.preview.image_id;
    assert!(matches!(h.shown().unwrap().body, PreviewBody::Image(_)));
    assert!(id > 0, "an image result changes the opaque handle");

    h.keys("j");
    h.until_shown().await;
    assert!(matches!(h.shown().unwrap().body, PreviewBody::Empty));
}

#[tokio::test(flavor = "multi_thread")]
async fn the_metadata_toggle_requests_the_metadata_view() {
    let dir = project();
    let mut h = Harness::new(dir.path());
    h.start().await;
    h.keys("j");
    h.until_shown().await;
    h.keys("i");
    h.until("the metadata view", |s| {
        s.preview
            .current
            .as_deref()
            .is_some_and(|p| matches!(p.body, PreviewBody::Metadata))
    })
    .await;
    let meta = h.shown().unwrap().meta.as_ref().expect("metadata");
    assert!(meta.size > 0);
    h.keys("i");
    h.until("the content again", |s| {
        s.preview
            .current
            .as_deref()
            .is_some_and(|p| matches!(p.body, PreviewBody::Text(_)))
    })
    .await;
}

#[tokio::test(flavor = "multi_thread")]
async fn j_and_k_scroll_a_long_file() {
    let dir = tempfile::tempdir().unwrap();
    let body: String = (0..500).map(|i| format!("line {i}\n")).collect();
    fs::write(dir.path().join("long.txt"), body).unwrap();
    let mut h = Harness::new(dir.path());
    h.start().await;
    h.until_shown().await;
    h.keys("J J");
    assert!(h.state.preview.scroll > 0);
    let scrolled = h.state.preview.scroll;
    h.keys("K");
    assert!(h.state.preview.scroll < scrolled);
}

#[tokio::test(flavor = "multi_thread")]
async fn holding_j_through_large_files_stays_responsive_and_shows_only_the_last() {
    let dir = tempfile::tempdir().unwrap();
    // 60 files of ~240 KB of Rust-looking text: each preview is a real highlighting job.
    let line = "    let value = compute(alpha, beta) + 12345; // a comment about it\n";
    let big: String = line.repeat(4000);
    for i in 0..60 {
        fs::write(dir.path().join(format!("f{i:02}.rs")), &big).unwrap();
    }
    let mut h = Harness::new(dir.path());
    h.start().await;
    h.until_shown().await;

    // Hold `j`: 59 key presses as fast as the loop can process them.
    let started = Instant::now();
    let mut worst = Duration::ZERO;
    for _ in 0..59 {
        let t = Instant::now();
        h.keys("j");
        worst = worst.max(t.elapsed());
    }
    let burst = started.elapsed();
    println!("59 presses took {burst:?}, slowest single press {worst:?}");
    // The UI thread only does `update` and hands a request to the engine.
    let limit = if cfg!(debug_assertions) { 100 } else { 16 };
    assert!(
        worst.as_millis() < limit,
        "a keypress took {worst:?}, the engine must not block the loop"
    );

    assert_eq!(h.cursor(), "f59.rs");
    h.until_shown().await;
    // Let any straggler arrive; it must have been dropped as stale.
    h.settle(Duration::from_millis(300)).await;
    let shown = h.shown().unwrap();
    assert_eq!(shown.path.file_name().unwrap(), "f59.rs");
    assert!(
        h.arrivals.len() < 59,
        "the engine skipped superseded requests ({} results for 59 presses)",
        h.arrivals.len()
    );
}

#[tokio::test(flavor = "multi_thread")]
async fn revisiting_a_file_is_served_from_the_cache() {
    let dir = project();
    let mut h = Harness::new(dir.path());
    h.start().await;
    h.keys("j");
    h.until_shown().await;
    h.keys("j");
    h.until_shown().await;
    h.arrivals.clear();
    h.keys("k");
    let took = h
        .until("the cached preview", |s| s.preview.current.is_some())
        .await;
    assert_eq!(
        h.arrivals.last(),
        Some(&true),
        "second visit is a cache hit"
    );
    println!("cache hit delivered in {took:?}");
    assert!(took < Duration::from_millis(250));
}

#[tokio::test(flavor = "multi_thread")]
async fn hiding_the_pane_cancels_and_stops_requests() {
    let dir = project();
    let mut h = Harness::new(dir.path());
    h.start().await;
    h.until_shown().await;
    h.keys("w");
    assert!(h.state.preview.current.is_none());
    h.keys("j");
    h.settle(Duration::from_millis(200)).await;
    assert!(
        h.state.preview.current.is_none(),
        "nothing is requested while the pane is hidden"
    );
    h.keys("w");
    h.until_shown().await;
}

#[tokio::test(flavor = "multi_thread")]
async fn the_parent_column_lists_the_parent_directory() {
    let dir = tempfile::tempdir().unwrap();
    let child = dir.path().join("child");
    fs::create_dir(&child).unwrap();
    fs::create_dir(dir.path().join("sibling")).unwrap();
    fs::write(child.join("f.txt"), "x").unwrap();
    let mut h = Harness::new(&child);
    h.state.size = Size::new(160, 40);
    h.start().await;
    h.keys("P");
    h.until("the parent listing", |s| s.preview.parent.is_some())
        .await;
    let parent = h.state.preview.parent.as_deref().unwrap();
    let PreviewBody::Directory(listing) = &parent.body else {
        panic!("expected a directory");
    };
    let names: Vec<_> = listing.entries.iter().map(|e| e.name.as_str()).collect();
    assert_eq!(names, ["child", "sibling"]);
}

#[tokio::test(flavor = "multi_thread")]
async fn a_changed_file_is_previewed_again() {
    let dir = tempfile::tempdir().unwrap();
    let file = dir.path().join("note.txt");
    fs::write(&file, "first version").unwrap();
    let mut h = Harness::new(dir.path());
    h.start().await;
    h.until_shown().await;
    let text_of = |h: &Harness| match &h.shown().unwrap().body {
        PreviewBody::Text(t) => t
            .lines
            .iter()
            .map(|l| l.text())
            .collect::<Vec<_>>()
            .join("\n"),
        other => panic!("unexpected {other:?}"),
    };
    assert_eq!(text_of(&h), "first version");

    // Simulate the watcher: the listing is refreshed with the new size and mtime.
    fs::write(&file, "second version, longer").unwrap();
    h.send(Msg::Start); // re-lists the directory
    h.until("the refreshed preview", |s| {
        s.preview.current.as_deref().is_some_and(|p| match &p.body {
            PreviewBody::Text(t) => t
                .lines
                .first()
                .is_some_and(|l| l.text().starts_with("second")),
            _ => false,
        })
    })
    .await;
}

#[cfg(windows)]
#[tokio::test(flavor = "multi_thread")]
async fn the_metadata_view_names_the_owner() {
    let dir = project();
    let mut h = Harness::new(dir.path());
    h.start().await;
    h.keys("j i");
    h.until("the metadata view", |s| {
        s.preview
            .current
            .as_deref()
            .is_some_and(|p| matches!(p.body, PreviewBody::Metadata))
    })
    .await;
    let owner = h
        .shown()
        .unwrap()
        .meta
        .as_ref()
        .and_then(|m| m.owner.clone())
        .expect("an owner on Windows");
    assert!(!owner.is_empty());
    eprintln!("owner: {owner}");
}

/// Job completion -> `Effect::InvalidatePreview` -> `Previewer::invalidate` -> a fresh request.
/// The file is overwritten with the same size and the same mtime, so only the invalidation (not
/// the cache key) can tell the previewer that the content changed.
#[tokio::test(flavor = "multi_thread")]
async fn a_finished_job_makes_the_pane_show_fresh_content() {
    use ks_core::fileops::{JobDone, JobMsg, JobStatus, OpKind};
    use ks_core::JobId;

    let dir = tempfile::tempdir().unwrap();
    let file = dir.path().join("note.txt");
    fs::write(&file, "AAAA").unwrap();
    let mtime = fs::metadata(&file).unwrap().modified().unwrap();
    let mut h = Harness::new(dir.path());
    h.start().await;
    h.until_shown().await;
    let first = |h: &Harness| match &h.shown().unwrap().body {
        PreviewBody::Text(t) => t.lines.first().map(|l| l.text()).unwrap_or_default(),
        other => panic!("unexpected {other:?}"),
    };
    assert_eq!(first(&h), "AAAA");

    fs::write(&file, "BBBB").unwrap();
    fs::File::options()
        .write(true)
        .open(&file)
        .unwrap()
        .set_modified(mtime)
        .unwrap();
    // Without a job nothing re-requests: the key (path, size, mtime) is unchanged.
    h.settle(Duration::from_millis(300)).await;
    assert_eq!(first(&h), "AAAA");

    h.send(Msg::Job(JobMsg::Finished(JobDone {
        id: JobId(1),
        kind: OpKind::Copy,
        status: JobStatus::Done,
        items_done: 1,
        items_skipped: 0,
        bytes_done: 4,
        errors: Vec::new(),
        created: vec![file.clone()],
        removed: Vec::new(),
        consumed_sources: Vec::new(),
        is_undo: false,
        elapsed_ms: 1,
    })));
    h.until("the fresh preview", |s| {
        s.preview.current.as_deref().is_some_and(|p| match &p.body {
            PreviewBody::Text(t) => t.lines.first().is_some_and(|l| l.text() == "BBBB"),
            _ => false,
        })
    })
    .await;
    assert_eq!(h.arrivals.last(), Some(&false), "not served from the cache");
}
