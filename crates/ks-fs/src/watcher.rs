//! Watching one directory for changes.
//!
//! `notify` uses `ReadDirectoryChangesW` on Windows. The watch is non-recursive: only direct
//! children of the directory are reported. Events are debounced, then collapsed into a
//! single "something changed" callback, because the application re-lists the directory
//! rather than applying individual events. The callback is trailing-edge: a burst of changes
//! produces one call after it has died down (capped at [`MAX_DELAY`] of continuous activity).
//!
//! `ReadDirectoryChangesW` never reports the removal or renaming of the watched directory
//! itself (the handle just goes quiet), so the parent directory is watched as well and only
//! events about the watched directory are let through from it.

use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::{self, RecvTimeoutError};
use std::sync::Arc;
use std::time::{Duration, Instant};

use notify::event::EventKind;
use notify::{Config, RecommendedWatcher, RecursiveMode};
use notify_debouncer_full::{
    new_debouncer_opt, DebounceEventResult, DebouncedEvent, Debouncer, NoCache,
};

/// How long events are collected before they are reported.
pub const DEBOUNCE: Duration = Duration::from_millis(100);

/// The debouncer releases events as each one matures, so a long burst arrives as a stream of
/// batches. They are coalesced: the callback fires once the stream has been quiet this long...
pub const QUIET: Duration = Duration::from_millis(50);

/// ...or after this long of continuous activity, so a big copy keeps the view live.
pub const MAX_DELAY: Duration = Duration::from_secs(1);

/// What the debouncer handler tells the coalescer.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Signal {
    /// A batch with relevant events.
    Changed,
    /// The watch broke: report at once.
    Broken,
}

/// Calls `on_change` once per burst: after `quiet` without a signal, after `max_delay` of
/// continuous signals, or at once for [`Signal::Broken`]. Returns when the channel closes
/// (pending changes are dropped: the watcher is gone) or `stopped` is set.
fn coalesce(
    rx: &mpsc::Receiver<Signal>,
    quiet: Duration,
    max_delay: Duration,
    stopped: &AtomicBool,
    on_change: impl Fn(),
) {
    while let Ok(first) = rx.recv() {
        let started = Instant::now();
        let mut broken = first == Signal::Broken;
        while !broken {
            let left = max_delay.saturating_sub(started.elapsed());
            if left.is_zero() {
                break;
            }
            match rx.recv_timeout(quiet.min(left)) {
                Ok(Signal::Broken) => broken = true,
                Ok(Signal::Changed) => {}
                Err(RecvTimeoutError::Timeout) => break,
                Err(RecvTimeoutError::Disconnected) => return,
            }
        }
        if stopped.load(Ordering::Acquire) {
            return;
        }
        on_change();
    }
}

/// Keeps a directory watch alive; dropping it stops the watch.
pub struct DirWatcher {
    debouncer: Option<Debouncer<RecommendedWatcher, NoCache>>,
    stopped: Arc<AtomicBool>,
}

/// The form of `path` handed to the OS watch: `\?\`-prefixed when it is long (a plain path
/// beyond MAX_PATH cannot be opened by `CreateFileW`), unchanged otherwise.
fn watch_form(path: &Path) -> PathBuf {
    #[cfg(windows)]
    {
        // Same rule as the listing code (`long_path_safe`). Absolutizing first also turns `/`
        // into `\`, which a verbatim path would not.
        crate::platform::long_path_safe(&crate::ops::paths::absolutize(path))
    }
    #[cfg(not(windows))]
    {
        path.to_path_buf()
    }
}

/// `path` without a `\?\` / `\?\UNC\` prefix, so paths compare equal in either form.
fn plain(path: &Path) -> PathBuf {
    let text = path.to_string_lossy();
    if let Some(rest) = text.strip_prefix(r"\\?\UNC\") {
        PathBuf::from(format!(r"\\{rest}"))
    } else if let Some(rest) = text.strip_prefix(r"\\?\") {
        PathBuf::from(rest)
    } else {
        path.to_path_buf()
    }
}

impl DirWatcher {
    /// Watches `dir` and calls `on_change` (from a background thread) once after each burst of
    /// changes to its direct children has died down.
    ///
    /// The file-id cache is disabled: the default one stats every file of the watched
    /// directory up front, which takes seconds in a directory with 100k entries, and the
    /// only thing it buys is rename correlation that re-listing makes unnecessary.
    pub fn new(dir: &Path, on_change: impl Fn() + Send + 'static) -> Result<Self, notify::Error> {
        let watched = plain(dir);
        let (tx, rx) = mpsc::channel();
        let stopped = Arc::new(AtomicBool::new(false));
        let flag = Arc::clone(&stopped);
        let spawned = std::thread::Builder::new()
            .name("ks-watch-coalesce".into())
            .spawn(move || coalesce(&rx, QUIET, MAX_DELAY, &flag, on_change));
        if let Err(error) = spawned {
            return Err(notify::Error::io(error));
        }
        let handler = move |result: DebounceEventResult| match result {
            Ok(events) => {
                if events.iter().any(|e| concerns(&watched, e)) {
                    let _ = tx.send(Signal::Changed);
                }
            }
            // The watch broke (for example the directory was deleted): the listing
            // refresh that follows reports the real reason.
            Err(errors) => {
                tracing::warn!(?errors, "watcher error");
                let _ = tx.send(Signal::Broken);
            }
        };
        let mut debouncer = new_debouncer_opt::<_, RecommendedWatcher, NoCache>(
            DEBOUNCE,
            None,
            handler,
            NoCache::new(),
            Config::default(),
        )?;
        debouncer.watch(watch_form(dir), RecursiveMode::NonRecursive)?;
        if let Some(parent) = dir.parent().filter(|p| !p.as_os_str().is_empty()) {
            // Best effort: without it only the removal of `dir` itself goes unnoticed.
            if let Err(error) = debouncer.watch(watch_form(parent), RecursiveMode::NonRecursive) {
                tracing::warn!(%error, parent = %parent.display(), "cannot watch the parent");
            }
        }
        Ok(Self {
            debouncer: Some(debouncer),
            stopped,
        })
    }
}

/// Whether `event` changes the listing of `dir` (given without a verbatim prefix): something
/// happened to one of its children or to `dir` itself. Opening or reading a file is not a
/// change, and neither is anything that happens to the siblings of `dir` (reported through the
/// parent watch). Event paths carry a `\?\` prefix when the watch was made with one.
fn concerns(dir: &Path, event: &DebouncedEvent) -> bool {
    !matches!(event.event.kind, EventKind::Access(_))
        && event.event.paths.iter().any(|path| {
            let path = plain(path);
            path == dir || path.parent() == Some(dir)
        })
}

impl Drop for DirWatcher {
    fn drop(&mut self) {
        self.stopped.store(true, Ordering::Release);
        if let Some(debouncer) = self.debouncer.take() {
            // Do not wait for the event thread: a drop must never stall the caller.
            debouncer.stop_nonblocking();
        }
    }
}

#[cfg(test)]
mod tests {
    #![allow(clippy::print_stderr)]

    use std::fs;
    use std::sync::mpsc;
    use std::time::Instant;

    use super::*;

    fn watch(dir: &Path) -> (DirWatcher, mpsc::Receiver<Instant>) {
        let (tx, rx) = mpsc::channel();
        let watcher = DirWatcher::new(dir, move || {
            let _ = tx.send(Instant::now());
        })
        .unwrap();
        (watcher, rx)
    }

    /// Generous: the debounce is 100 ms plus up to one tick; CI machines can be slow.
    const WAIT: Duration = Duration::from_secs(10);

    #[test]
    fn creating_a_file_is_reported_after_the_debounce() {
        let dir = tempfile::tempdir().unwrap();
        let (_watcher, rx) = watch(dir.path());
        std::thread::sleep(Duration::from_millis(100));
        let started = Instant::now();
        fs::write(dir.path().join("new.txt"), "x").unwrap();
        let at = rx.recv_timeout(WAIT).expect("a change notification");
        let latency = at.duration_since(started);
        assert!(
            latency >= Duration::from_millis(50),
            "reported before the debounce: {latency:?}"
        );
        eprintln!("create -> notification: {latency:?}");
    }

    #[test]
    fn deleting_and_renaming_are_reported() {
        let dir = tempfile::tempdir().unwrap();
        let file = dir.path().join("a.txt");
        fs::write(&file, "x").unwrap();
        let (_watcher, rx) = watch(dir.path());
        std::thread::sleep(Duration::from_millis(100));

        fs::rename(&file, dir.path().join("b.txt")).unwrap();
        rx.recv_timeout(WAIT).expect("rename reported");
        while rx.try_recv().is_ok() {}

        fs::remove_file(dir.path().join("b.txt")).unwrap();
        rx.recv_timeout(WAIT).expect("delete reported");
    }

    #[test]
    fn a_burst_of_changes_is_few_notifications() {
        let dir = tempfile::tempdir().unwrap();
        let (_watcher, rx) = watch(dir.path());
        std::thread::sleep(Duration::from_millis(100));
        let started = Instant::now();
        for i in 0..20 {
            fs::write(dir.path().join(format!("f{i}")), "x").unwrap();
        }
        let burst = started.elapsed();
        let mut count = 0;
        rx.recv_timeout(WAIT).expect("a notification");
        count += 1;
        // Collect until the watcher has been quiet for much longer than the debounce.
        while rx.recv_timeout(Duration::from_millis(800)).is_ok() {
            count += 1;
        }
        // Exact bound of the mechanism: a further notification needs a quiet gap inside the
        // burst or the continuous-activity cap, so it cannot exceed those (plus one for the
        // events that mature after the last write). The exact "one" case is covered by
        // `coalescing_*`, which has no disk timing in it.
        let bound = 2
            + (burst.as_millis() / QUIET.as_millis()) as usize
            + (burst.as_millis() / MAX_DELAY.as_millis()) as usize;
        assert!(
            count <= bound && count < 20,
            "{count} notifications (bound {bound}) for one {burst:?} burst"
        );
    }

    fn run_coalescer(
        quiet: Duration,
        cap: Duration,
    ) -> (
        mpsc::Sender<Signal>,
        Arc<std::sync::atomic::AtomicUsize>,
        Arc<AtomicBool>,
    ) {
        let (tx, rx) = mpsc::channel();
        let count = Arc::new(std::sync::atomic::AtomicUsize::new(0));
        let stopped = Arc::new(AtomicBool::new(false));
        let (c, s) = (Arc::clone(&count), Arc::clone(&stopped));
        std::thread::spawn(move || {
            coalesce(&rx, quiet, cap, &s, || {
                c.fetch_add(1, Ordering::SeqCst);
            });
        });
        (tx, count, stopped)
    }

    fn wait_for(count: &std::sync::atomic::AtomicUsize, at_least: usize) {
        let deadline = Instant::now() + WAIT;
        while count.load(Ordering::SeqCst) < at_least {
            assert!(Instant::now() < deadline, "no notification");
            std::thread::sleep(Duration::from_millis(5));
        }
    }

    #[test]
    fn coalescing_a_burst_of_batches_gives_exactly_one_call() {
        // Everything is queued before the coalescer starts, so no timing decides whether the
        // signals land inside one quiet window; the thread is joined instead of slept on.
        let (tx, rx) = mpsc::channel();
        for _ in 0..500 {
            tx.send(Signal::Changed).unwrap();
        }
        let count = Arc::new(std::sync::atomic::AtomicUsize::new(0));
        let stopped = Arc::new(AtomicBool::new(false));
        let (c, s) = (Arc::clone(&count), Arc::clone(&stopped));
        let worker = std::thread::spawn(move || {
            coalesce(
                &rx,
                Duration::from_millis(100),
                Duration::from_secs(60),
                &s,
                || {
                    c.fetch_add(1, Ordering::SeqCst);
                },
            );
        });
        wait_for(&count, 1);
        drop(tx); // disconnect: the worker returns without a further call
        worker.join().unwrap();
        assert_eq!(count.load(Ordering::SeqCst), 1);
    }

    #[test]
    fn continuous_activity_is_reported_after_the_cap() {
        let (tx, count, _stopped) =
            run_coalescer(Duration::from_secs(30), Duration::from_millis(200));
        let deadline = Instant::now() + WAIT;
        while count.load(Ordering::SeqCst) == 0 {
            assert!(Instant::now() < deadline, "the cap never fired");
            tx.send(Signal::Changed).unwrap();
            std::thread::sleep(Duration::from_millis(5));
        }
    }

    #[test]
    fn a_broken_watch_is_reported_at_once_and_a_stopped_one_never() {
        let (tx, count, stopped) = run_coalescer(Duration::from_secs(30), Duration::from_secs(60));
        tx.send(Signal::Broken).unwrap();
        wait_for(&count, 1);
        stopped.store(true, Ordering::SeqCst);
        tx.send(Signal::Broken).unwrap();
        drop(tx);
        std::thread::sleep(Duration::from_millis(200));
        assert_eq!(count.load(Ordering::SeqCst), 1);
    }

    #[test]
    fn verbatim_and_plain_paths_compare_equal() {
        assert_eq!(plain(Path::new(r"\\?\C:\a\b")), PathBuf::from(r"C:\a\b"));
        assert_eq!(
            plain(Path::new(r"\\?\UNC\srv\sh")),
            PathBuf::from(r"\\srv\sh")
        );
        assert_eq!(plain(Path::new("/a/b")), PathBuf::from("/a/b"));
    }

    /// `CreateFileW` rejects a plain path beyond MAX_PATH, so the watch needs the `\?\` form.
    #[cfg(windows)]
    #[test]
    fn a_directory_beyond_max_path_can_be_watched() {
        let base = tempfile::tempdir().unwrap();
        let mut deep = base.path().to_path_buf();
        while deep.as_os_str().len() < 320 {
            deep.push("a_rather_long_directory_name_for_depth");
        }
        fs::create_dir_all(&deep).unwrap();
        assert!(deep.as_os_str().len() > 300);
        let (_watcher, rx) = watch(&deep);
        std::thread::sleep(Duration::from_millis(200));
        while rx.try_recv().is_ok() {}
        fs::write(deep.join("new.txt"), "x").unwrap();
        rx.recv_timeout(WAIT)
            .expect("a change in the deep directory is reported");
    }

    #[test]
    fn changes_in_subdirectories_are_not_reported() {
        let dir = tempfile::tempdir().unwrap();
        let sub = dir.path().join("sub");
        fs::create_dir(&sub).unwrap();
        let (_watcher, rx) = watch(dir.path());
        std::thread::sleep(Duration::from_millis(200));
        while rx.try_recv().is_ok() {}
        fs::write(sub.join("deep.txt"), "x").unwrap();
        // Writing inside `sub` may touch the directory entry of `sub` itself on some file
        // systems, but a non-recursive watch must not report the file's creation as such.
        // Allow at most the parent-level modification notification.
        std::thread::sleep(Duration::from_millis(500));
        let count = std::iter::from_fn(|| rx.try_recv().ok()).count();
        assert!(count <= 1, "{count} notifications for a nested change");
    }

    #[test]
    fn removing_the_watched_directory_itself_is_reported() {
        let parent = tempfile::tempdir().unwrap();
        let dir = parent.path().join("watched");
        fs::create_dir(&dir).unwrap();
        let (_watcher, rx) = watch(&dir);
        std::thread::sleep(Duration::from_millis(200));
        while rx.try_recv().is_ok() {}
        fs::remove_dir(&dir).unwrap();
        rx.recv_timeout(WAIT).expect("removal reported");
    }

    #[test]
    fn changes_to_siblings_of_the_watched_directory_are_not_reported() {
        let parent = tempfile::tempdir().unwrap();
        let dir = parent.path().join("watched");
        fs::create_dir(&dir).unwrap();
        let (_watcher, rx) = watch(&dir);
        std::thread::sleep(Duration::from_millis(200));
        while rx.try_recv().is_ok() {}
        fs::write(parent.path().join("sibling.txt"), "x").unwrap();
        fs::create_dir(parent.path().join("sibling_dir")).unwrap();
        assert!(rx.recv_timeout(Duration::from_millis(700)).is_err());
    }

    #[test]
    fn watching_a_missing_directory_fails() {
        let dir = tempfile::tempdir().unwrap();
        assert!(DirWatcher::new(&dir.path().join("nope"), || {}).is_err());
    }

    #[test]
    fn dropping_the_watcher_stops_notifications() {
        let dir = tempfile::tempdir().unwrap();
        let (watcher, rx) = watch(dir.path());
        drop(watcher);
        std::thread::sleep(Duration::from_millis(100));
        fs::write(dir.path().join("late.txt"), "x").unwrap();
        assert!(rx.recv_timeout(Duration::from_millis(600)).is_err());
    }
}
