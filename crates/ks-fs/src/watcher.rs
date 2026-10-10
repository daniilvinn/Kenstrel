//! Watching one directory for changes (PLAN §7).
//!
//! `notify` uses `ReadDirectoryChangesW` on Windows. The watch is non-recursive: only direct
//! children of the directory are reported. Events are debounced, then collapsed into a
//! single "something changed" callback, because the application re-lists the directory
//! rather than applying individual events.
//!
//! `ReadDirectoryChangesW` never reports the removal or renaming of the watched directory
//! itself (the handle just goes quiet), so the parent directory is watched as well and only
//! events about the watched directory are let through from it.

use std::path::Path;
use std::time::Duration;

use notify::event::EventKind;
use notify::{Config, RecommendedWatcher, RecursiveMode};
use notify_debouncer_full::{
    new_debouncer_opt, DebounceEventResult, DebouncedEvent, Debouncer, NoCache,
};

/// How long events are collected before they are reported (PLAN §7).
pub const DEBOUNCE: Duration = Duration::from_millis(100);

/// Keeps a directory watch alive; dropping it stops the watch.
pub struct DirWatcher {
    debouncer: Option<Debouncer<RecommendedWatcher, NoCache>>,
}

impl DirWatcher {
    /// Watches `dir` and calls `on_change` (from a background thread) after each debounced
    /// burst of changes to its direct children.
    ///
    /// The file-id cache is disabled: the default one stats every file of the watched
    /// directory up front, which takes seconds in a directory with 100k entries, and the
    /// only thing it buys is rename correlation that re-listing makes unnecessary.
    pub fn new(dir: &Path, on_change: impl Fn() + Send + 'static) -> Result<Self, notify::Error> {
        let watched = dir.to_path_buf();
        let handler = move |result: DebounceEventResult| match result {
            Ok(events) => {
                if events.iter().any(|e| concerns(&watched, e)) {
                    on_change();
                }
            }
            // The watch broke (for example the directory was deleted): the listing
            // refresh that follows reports the real reason.
            Err(errors) => {
                tracing::warn!(?errors, "watcher error");
                on_change();
            }
        };
        let mut debouncer = new_debouncer_opt::<_, RecommendedWatcher, NoCache>(
            DEBOUNCE,
            None,
            handler,
            NoCache::new(),
            Config::default(),
        )?;
        debouncer.watch(dir, RecursiveMode::NonRecursive)?;
        if let Some(parent) = dir.parent().filter(|p| !p.as_os_str().is_empty()) {
            // Best effort: without it only the removal of `dir` itself goes unnoticed.
            if let Err(error) = debouncer.watch(parent, RecursiveMode::NonRecursive) {
                tracing::warn!(%error, parent = %parent.display(), "cannot watch the parent");
            }
        }
        Ok(Self {
            debouncer: Some(debouncer),
        })
    }
}

/// Whether `event` changes the listing of `dir`: something happened to one of its children
/// or to `dir` itself. Opening or reading a file is not a change, and neither is anything
/// that happens to the siblings of `dir` (reported through the parent watch).
fn concerns(dir: &Path, event: &DebouncedEvent) -> bool {
    !matches!(event.event.kind, EventKind::Access(_))
        && event
            .event
            .paths
            .iter()
            .any(|path| path == dir || path.parent() == Some(dir))
}

impl Drop for DirWatcher {
    fn drop(&mut self) {
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
    const WAIT: Duration = Duration::from_secs(3);

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
    fn a_burst_of_changes_is_one_notification() {
        let dir = tempfile::tempdir().unwrap();
        let (_watcher, rx) = watch(dir.path());
        std::thread::sleep(Duration::from_millis(100));
        for i in 0..20 {
            fs::write(dir.path().join(format!("f{i}")), "x").unwrap();
        }
        rx.recv_timeout(WAIT).expect("a notification");
        std::thread::sleep(Duration::from_millis(400));
        let extra = std::iter::from_fn(|| rx.try_recv().ok()).count();
        assert!(extra <= 1, "{extra} extra notifications for one burst");
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
