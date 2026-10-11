//! Reading and writing Kestrel's state files (bookmarks, frecency; PLAN §9).
//!
//! Writes are atomic (temporary file in the same directory, then rename), so a crash or a
//! second instance never leaves a half-written file. Concurrent instances do not lose each
//! other's updates either: each write takes a lock file, re-reads the file and three-way merges
//! (what this instance last synced, what it wants now, what is on disk) before replacing it. A file that cannot be parsed is moved
//! aside to `<name>.bad` instead of being overwritten, and the app starts with empty data.

use std::collections::HashMap;
use std::fs;
use std::io;
use std::path::{Path, PathBuf};
use std::sync::{mpsc, Arc, Mutex};
use std::time::{Duration, Instant};

use ks_core::{Bookmarks, Frecency, LoadedNavData, StateFile};

/// A temporary file name no other writer uses: the process id keeps two running instances
/// apart, the counter two writers in one process (a lock that timed out).
fn temp_path(dir: &Path, name: &str) -> PathBuf {
    static SEQ: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
    let seq = SEQ.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
    dir.join(format!(".{name}.{}.{seq}.tmp", std::process::id()))
}

/// Writes `contents` to `path` atomically, creating parent directories.
pub fn atomic_write(path: &Path, contents: &str) -> io::Result<()> {
    let dir = path
        .parent()
        .filter(|d| !d.as_os_str().is_empty())
        .unwrap_or_else(|| Path::new("."));
    fs::create_dir_all(dir)?;
    let name = path
        .file_name()
        .map_or_else(|| "state".to_owned(), |n| n.to_string_lossy().into_owned());
    let tmp = temp_path(dir, &name);
    let written = (|| {
        use io::Write;
        let mut file = fs::File::create(&tmp)?;
        file.write_all(contents.as_bytes())?;
        file.sync_all()?;
        drop(file);
        rename_with_retry(&tmp, path)
    })();
    if written.is_err() {
        let _ = fs::remove_file(&tmp);
    }
    written
}

/// Replaces `to` with `from`. On Windows the rename fails for a moment when another process
/// (antivirus, an indexer, an editor) has the target open, so a few short retries are made.
fn rename_with_retry(from: &Path, to: &Path) -> io::Result<()> {
    let mut attempt = 0;
    loop {
        match fs::rename(from, to) {
            // About 2.4 s in all: a scanner or a starved disk can hold the target longer than
            // a quarter of a second, and a dropped save is worse than a short wait.
            Err(_) if attempt < 14 => {
                attempt += 1;
                std::thread::sleep(Duration::from_millis((25 * attempt).min(250)));
            }
            result => return result,
        }
    }
}

/// The state directory (`%LOCALAPPDATA%\kestrel\state`).
#[derive(Clone, Debug)]
pub struct StateStore {
    dir: PathBuf,
    /// Per file, the text this instance last synced with the disk (read at load, or the
    /// contents it last wrote): the common ancestor of the three-way merge on write.
    bases: Arc<Mutex<HashMap<StateFile, String>>>,
    /// How long a write waits for the file lock before proceeding without it.
    lock_wait: Duration,
    /// Age after which a lock file counts as abandoned.
    lock_stale: Duration,
}

/// Exclusive access to one state file among cooperating Kestrel instances: a `<file>.lock`
/// created with `create_new` and removed on drop. Best effort: after a timeout (or for a
/// stale lock) the writer proceeds, since a lost update is better than a stuck save.
struct FileLock {
    path: Option<PathBuf>,
}

impl FileLock {
    const STALE: Duration = Duration::from_secs(30);

    fn acquire(path: PathBuf, wait: Duration, stale_after: Duration) -> FileLock {
        let start = Instant::now();
        loop {
            match fs::OpenOptions::new()
                .write(true)
                .create_new(true)
                .open(&path)
            {
                Ok(_) => return FileLock { path: Some(path) },
                Err(e)
                    if matches!(
                        e.kind(),
                        io::ErrorKind::AlreadyExists | io::ErrorKind::PermissionDenied
                    ) =>
                {
                    let stale = fs::metadata(&path)
                        .and_then(|m| m.modified())
                        .ok()
                        .and_then(|t| t.elapsed().ok())
                        .is_some_and(|age| age > stale_after);
                    if stale {
                        let _ = fs::remove_file(&path);
                    } else if start.elapsed() > wait {
                        return FileLock { path: None };
                    } else {
                        std::thread::sleep(Duration::from_millis(5));
                    }
                }
                Err(_) => return FileLock { path: None },
            }
        }
    }
}

impl Drop for FileLock {
    fn drop(&mut self) {
        if let Some(path) = &self.path {
            let _ = fs::remove_file(path);
        }
    }
}

/// Tries to read the state file this often (25 ms apart) before giving up.
const DISK_READ_ATTEMPTS: u32 = 40;

/// Reads `path`; `None` when it does not exist. A sharing violation (an antivirus scanner or
/// another instance's rename in flight) is retried before it becomes an error.
fn read_disk(path: &Path, attempts: u32) -> io::Result<Option<String>> {
    let mut attempt = 0;
    loop {
        match fs::read_to_string(path) {
            Ok(text) => return Ok(Some(text)),
            Err(e) if e.kind() == io::ErrorKind::NotFound => return Ok(None),
            Err(e) => {
                attempt += 1;
                if attempt >= attempts {
                    return Err(e);
                }
                std::thread::sleep(std::time::Duration::from_millis(25));
            }
        }
    }
}

impl StateStore {
    pub fn new(dir: PathBuf) -> Self {
        Self {
            dir,
            bases: Arc::default(),
            lock_wait: Duration::from_secs(8),
            lock_stale: FileLock::STALE,
        }
    }

    /// A store whose writes wait this long for the file lock (tests use a long wait so a
    /// loaded machine never falls through to an unlocked write).
    #[cfg(test)]
    fn with_lock_wait(mut self, wait: Duration) -> Self {
        self.lock_wait = wait;
        self
    }

    /// A store that treats a lock as abandoned only after `stale`: tests on a starved machine
    /// can hold the lock for longer than the production threshold without being robbed of it.
    #[cfg(test)]
    fn with_lock_stale(mut self, stale: Duration) -> Self {
        self.lock_stale = stale;
        self
    }

    fn set_base(&self, file: StateFile, text: &str) {
        if let Ok(mut bases) = self.bases.lock() {
            bases.insert(file, text.to_owned());
        }
    }

    fn base(&self, file: StateFile) -> String {
        self.bases
            .lock()
            .ok()
            .and_then(|b| b.get(&file).cloned())
            .unwrap_or_default()
    }

    pub fn path(&self, file: StateFile) -> PathBuf {
        self.dir.join(file.file_name())
    }

    /// Reads both files. Never fails: a missing file is empty data, a damaged one is set
    /// aside and reported in `problems`.
    pub fn load(&self) -> LoadedNavData {
        let mut problems = Vec::new();
        let mut unreadable = Vec::new();
        let bookmarks = self
            .read(
                StateFile::Bookmarks,
                Bookmarks::from_toml,
                &mut problems,
                &mut unreadable,
            )
            .unwrap_or_default();
        let frecency = self
            .read(
                StateFile::Frecency,
                Frecency::from_toml,
                &mut problems,
                &mut unreadable,
            )
            .unwrap_or_default();
        LoadedNavData {
            bookmarks,
            frecency,
            problems,
            unreadable,
        }
    }

    fn read<T>(
        &self,
        file: StateFile,
        parse: impl Fn(&str) -> Result<T, String>,
        problems: &mut Vec<String>,
        unreadable: &mut Vec<StateFile>,
    ) -> Option<T> {
        let path = self.path(file);
        let name = file.file_name();
        let text = match fs::read_to_string(&path) {
            Ok(text) => text,
            Err(e) if e.kind() == io::ErrorKind::NotFound => {
                self.set_base(file, "");
                return None;
            }
            Err(e) => {
                tracing::warn!(path = %path.display(), error = %e, "cannot read state file");
                // The file is there but unreadable: it must not be overwritten by a save.
                problems.push(format!("Cannot read {name}: {e} (it is left untouched)"));
                unreadable.push(file);
                return None;
            }
        };
        match parse(&text) {
            Ok(value) => {
                self.set_base(file, &text);
                Some(value)
            }
            Err(error) => {
                tracing::warn!(path = %path.display(), %error, "damaged state file");
                let aside = path.with_file_name(format!("{name}.bad"));
                let moved = fs::rename(&path, &aside).is_ok();
                self.set_base(file, "");
                problems.push(if moved {
                    format!("{name} was damaged and starts afresh (old copy: {name}.bad)")
                } else {
                    format!("{name} is damaged and was ignored")
                });
                None
            }
        }
    }

    /// Saves `contents` (this instance's view of `file`), first folding in whatever other
    /// instances wrote since this one last synced (see the module docs).
    pub fn write(&self, file: StateFile, contents: &str) -> io::Result<()> {
        let path = self.path(file);
        fs::create_dir_all(&self.dir)?;
        let _lock = FileLock::acquire(
            path.with_extension("toml.lock"),
            self.lock_wait,
            self.lock_stale,
        );
        // A file that cannot be read (after retries) is not overwritten with this instance's
        // view: that would drop what another instance wrote. The next save tries again.
        let merged = self.merged(file, contents, &path)?;
        atomic_write(&path, &merged)?;
        self.set_base(file, contents);
        Ok(())
    }

    /// `contents` merged with the file on disk; `contents` itself when there is nothing to merge
    /// (no file, an unreadable or unparsable one, or no foreign changes).
    fn merged(&self, file: StateFile, contents: &str, path: &Path) -> io::Result<String> {
        let Some(disk_text) = read_disk(path, DISK_READ_ATTEMPTS)? else {
            return Ok(contents.to_owned());
        };
        Ok(self.merge_with_disk(file, contents, &disk_text))
    }

    fn merge_with_disk(&self, file: StateFile, contents: &str, disk_text: &str) -> String {
        let base_text = self.base(file);
        if disk_text == base_text || disk_text == contents {
            return contents.to_owned();
        }
        let merged = match file {
            StateFile::Bookmarks => {
                let (Ok(ours), Ok(disk)) = (
                    Bookmarks::from_toml(contents),
                    Bookmarks::from_toml(disk_text),
                ) else {
                    return contents.to_owned();
                };
                let base = Bookmarks::from_toml(&base_text).unwrap_or_default();
                let merged = Bookmarks::merge_three_way(&base, &ours, disk);
                if merged == ours {
                    return contents.to_owned();
                }
                merged.to_toml()
            }
            StateFile::Frecency => {
                let (Ok(ours), Ok(disk)) = (
                    Frecency::from_toml(contents),
                    Frecency::from_toml(disk_text),
                ) else {
                    return contents.to_owned();
                };
                let base = Frecency::from_toml(&base_text).unwrap_or_default();
                let merged = Frecency::merge_three_way(&base, &ours, disk);
                if merged.to_toml() == ours.to_toml() {
                    return contents.to_owned();
                }
                merged.to_toml()
            }
        };
        tracing::debug!(
            file = file.file_name(),
            "merged with changes from another instance"
        );
        merged
    }
}

type Job = (StateFile, String);

/// Applies state-file writes in order on a thread of its own, so the UI thread and the
/// async runtime never wait for the disk.
pub struct StateWriter {
    tx: mpsc::Sender<Job>,
    done: mpsc::Receiver<()>,
}

impl StateWriter {
    pub fn spawn(store: StateStore) -> Self {
        let (tx, rx) = mpsc::channel::<Job>();
        let (done_tx, done) = mpsc::channel();
        let spawned = std::thread::Builder::new()
            .name("ks-state-writer".into())
            .spawn(move || {
                for (file, contents) in rx {
                    if let Err(error) = store.write(file, &contents) {
                        tracing::warn!(file = file.file_name(), %error, "cannot save state");
                    }
                }
                let _ = done_tx.send(());
            });
        if let Err(error) = spawned {
            tracing::error!(%error, "cannot start the state writer");
        }
        Self { tx, done }
    }

    pub fn send(&self, file: StateFile, contents: String) {
        let _ = self.tx.send((file, contents));
    }

    /// Waits (at most `timeout`) until everything sent so far is on disk.
    pub fn finish(self, timeout: Duration) {
        drop(self.tx);
        let _ = self.done.recv_timeout(timeout);
    }
}

#[cfg(test)]
mod tests {
    use std::path::Path;

    use super::*;

    fn store() -> (tempfile::TempDir, StateStore) {
        let dir = tempfile::tempdir().unwrap();
        let store = StateStore::new(dir.path().join("state"));
        (dir, store)
    }

    #[test]
    fn an_unreadable_state_file_is_an_error_not_a_reason_to_overwrite() {
        let dir = tempfile::tempdir().unwrap();
        // A directory cannot be read as a file: not "missing", so it must surface.
        assert!(read_disk(dir.path(), 2).is_err());
        assert_eq!(read_disk(&dir.path().join("none.toml"), 2).unwrap(), None);
        let file = dir.path().join("f.toml");
        fs::write(&file, "x").unwrap();
        assert_eq!(read_disk(&file, 2).unwrap().as_deref(), Some("x"));
    }

    #[test]
    fn atomic_write_creates_directories_and_replaces_files() {
        let dir = tempfile::tempdir().unwrap();
        let file = dir.path().join("a").join("b.toml");
        atomic_write(&file, "one").unwrap();
        assert_eq!(fs::read_to_string(&file).unwrap(), "one");
        atomic_write(&file, "two").unwrap();
        assert_eq!(fs::read_to_string(&file).unwrap(), "two");
        let leftovers: Vec<_> = fs::read_dir(file.parent().unwrap())
            .unwrap()
            .map(|e| e.unwrap().file_name())
            .collect();
        assert_eq!(leftovers.len(), 1, "no temporary file stays behind");
    }

    #[test]
    fn a_failed_write_leaves_the_old_file_alone() {
        let dir = tempfile::tempdir().unwrap();
        let file = dir.path().join("keep.toml");
        fs::write(&file, "old").unwrap();
        // A directory in the way of the rename makes the final step fail.
        let blocked = dir.path().join("blocked");
        fs::create_dir(&blocked).unwrap();
        assert!(atomic_write(&blocked, "new").is_err());
        assert_eq!(fs::read_to_string(&file).unwrap(), "old");
        assert!(fs::read_dir(dir.path()).unwrap().all(|e| !e
            .unwrap()
            .file_name()
            .to_string_lossy()
            .ends_with(".tmp")));
    }

    #[test]
    fn missing_files_load_as_empty_data() {
        let (_dir, store) = store();
        let loaded = store.load();
        assert!(loaded.bookmarks.is_empty() && loaded.frecency.is_empty());
        assert!(loaded.problems.is_empty());
    }

    #[test]
    fn bookmarks_and_frecency_round_trip_through_the_files() {
        let (_dir, store) = store();
        let mut bookmarks = Bookmarks::default();
        bookmarks.set('p', PathBuf::from("/some/where"));
        let mut frecency = Frecency::default();
        frecency.visit(Path::new("/a"), 10);
        frecency.visit(Path::new("/a"), 20);
        store
            .write(StateFile::Bookmarks, &bookmarks.to_toml())
            .unwrap();
        store
            .write(StateFile::Frecency, &frecency.to_toml())
            .unwrap();
        let loaded = store.load();
        assert_eq!(loaded.bookmarks, bookmarks);
        assert_eq!(loaded.frecency, frecency);
        assert!(loaded.problems.is_empty());
    }

    #[test]
    fn damaged_files_fall_back_to_empty_data_and_are_kept_aside() {
        let (_dir, store) = store();
        fs::create_dir_all(store.path(StateFile::Bookmarks).parent().unwrap()).unwrap();
        fs::write(store.path(StateFile::Bookmarks), "[bookmarks\nzzz").unwrap();
        fs::write(store.path(StateFile::Frecency), "entries = 5").unwrap();
        let loaded = store.load();
        assert!(loaded.bookmarks.is_empty() && loaded.frecency.is_empty());
        assert_eq!(loaded.problems.len(), 2);
        assert!(loaded.problems[0].contains("bookmarks.toml"));
        assert!(!store.path(StateFile::Bookmarks).exists());
        assert_eq!(
            fs::read_to_string(store.path(StateFile::Bookmarks).with_extension("toml.bad"))
                .unwrap(),
            "[bookmarks\nzzz"
        );
        // The next save starts a clean file.
        store.write(StateFile::Bookmarks, "").unwrap();
        assert!(store.load().problems.is_empty());
    }

    #[test]
    fn an_unreadable_file_is_reported_and_not_to_be_overwritten() {
        let (_dir, store) = store();
        // A directory where the file should be: reading it fails with something other than
        // "not found".
        fs::create_dir_all(store.path(StateFile::Bookmarks)).unwrap();
        let loaded = store.load();
        assert_eq!(loaded.unreadable, [StateFile::Bookmarks]);
        assert!(loaded.problems[0].contains("left untouched"));
        assert!(store.path(StateFile::Bookmarks).is_dir());
    }

    #[test]
    fn a_second_instance_does_not_wipe_the_first_ones_bookmarks() {
        let dir = tempfile::tempdir().unwrap();
        let a = StateStore::new(dir.path().join("state"));
        let b = StateStore::new(dir.path().join("state"));
        // Both start from an empty directory.
        a.load();
        b.load();
        let mut bm_a = Bookmarks::default();
        bm_a.set('a', PathBuf::from("/from/a"));
        a.write(StateFile::Bookmarks, &bm_a.to_toml()).unwrap();
        // B never saw 'a'; its table only holds 'b'.
        let mut bm_b = Bookmarks::default();
        bm_b.set('b', PathBuf::from("/from/b"));
        b.write(StateFile::Bookmarks, &bm_b.to_toml()).unwrap();
        let on_disk = StateStore::new(dir.path().join("state")).load().bookmarks;
        assert_eq!(on_disk.len(), 2, "{on_disk:?}");
        // A deletes its own bookmark: it goes, B's stays.
        a.write(StateFile::Bookmarks, &Bookmarks::default().to_toml())
            .unwrap();
        let on_disk = StateStore::new(dir.path().join("state")).load().bookmarks;
        assert!(on_disk.get('a').is_none() && on_disk.get('b').is_some());
        assert!(!a
            .path(StateFile::Bookmarks)
            .with_extension("toml.lock")
            .exists());
    }

    #[test]
    fn concurrent_instances_add_up_their_frecency_visits() {
        let dir = tempfile::tempdir().unwrap();
        let a = StateStore::new(dir.path().join("state"));
        let b = StateStore::new(dir.path().join("state"));
        a.load();
        b.load();
        let mut fa = Frecency::default();
        fa.visit(Path::new("/shared"), 10);
        fa.visit(Path::new("/shared"), 11);
        a.write(StateFile::Frecency, &fa.to_toml()).unwrap();
        let mut fb = Frecency::default();
        fb.visit(Path::new("/shared"), 20);
        fb.visit(Path::new("/only-b"), 21);
        b.write(StateFile::Frecency, &fb.to_toml()).unwrap();
        // A visits once more and saves again: its delta (1) is added, nothing is lost.
        fa.visit(Path::new("/shared"), 30);
        a.write(StateFile::Frecency, &fa.to_toml()).unwrap();
        let merged = StateStore::new(dir.path().join("state")).load().frecency;
        assert_eq!(merged.get(Path::new("/shared")).unwrap().rank, 4.0);
        assert!(merged.get(Path::new("/only-b")).is_some());
    }

    /// One instance's save: start from what it loaded, add its own bookmark, write.
    fn add_bookmark(store: &StateStore, key: char, path: &str) {
        let mut bm = store.load().bookmarks;
        bm.set(key, PathBuf::from(path));
        store.write(StateFile::Bookmarks, &bm.to_toml()).unwrap();
    }

    #[test]
    fn a_later_instance_keeps_the_bookmarks_an_earlier_one_saved() {
        let dir = tempfile::tempdir().unwrap();
        let state = dir.path().join("state");
        add_bookmark(&StateStore::new(state.clone()), 'a', "/pa");
        add_bookmark(&StateStore::new(state.clone()), 'b', "/pb");
        assert_eq!(StateStore::new(state).load().bookmarks.len(), 2);
    }

    #[test]
    fn concurrent_instances_lose_no_bookmark() {
        for loaded_first in [false, true] {
            let dir = tempfile::tempdir().unwrap();
            let state = dir.path().join("state");
            let barrier = Arc::new(std::sync::Barrier::new(8));
            let handles: Vec<_> = (0..8u8)
                .map(|i| {
                    let state = state.clone();
                    let barrier = barrier.clone();
                    std::thread::spawn(move || {
                        let store = StateStore::new(state)
                            .with_lock_wait(Duration::from_secs(900))
                            .with_lock_stale(Duration::from_secs(900));
                        let mut bm = store.load().bookmarks;
                        if loaded_first {
                            // Every instance loaded before any wrote: a pure three-way merge.
                            barrier.wait();
                        }
                        bm.set((b'a' + i) as char, PathBuf::from(format!("/p{i}")));
                        store.write(StateFile::Bookmarks, &bm.to_toml()).unwrap();
                    })
                })
                .collect();
            for h in handles {
                h.join().unwrap();
            }
            assert_eq!(StateStore::new(state).load().bookmarks.len(), 8);
        }
    }

    #[test]
    fn consecutive_temp_paths_differ() {
        let dir = Path::new("d");
        assert_ne!(temp_path(dir, "f.toml"), temp_path(dir, "f.toml"));
    }

    #[test]
    fn a_held_lock_times_out_and_the_write_still_lands() {
        let (dir, store) = store();
        let store = store.with_lock_wait(Duration::from_millis(100));
        let lock = store.path(StateFile::Bookmarks).with_extension("toml.lock");
        fs::create_dir_all(lock.parent().unwrap()).unwrap();
        fs::write(&lock, "").unwrap();
        // A fresh lock held by someone else: the write waits, gives up, then proceeds.
        store
            .write(
                StateFile::Bookmarks,
                "# mine
",
            )
            .unwrap();
        assert_eq!(
            fs::read_to_string(store.path(StateFile::Bookmarks)).unwrap(),
            "# mine
"
        );
        assert!(
            lock.exists(),
            "a lock this writer did not take is left alone"
        );
        drop(dir);
    }

    #[test]
    fn a_stale_lock_is_removed() {
        let dir = tempfile::tempdir().unwrap();
        let lock = dir.path().join("x.lock");
        let file = fs::File::create(&lock).unwrap();
        file.set_modified(std::time::SystemTime::now() - FileLock::STALE - Duration::from_secs(5))
            .unwrap();
        drop(file);
        let taken = FileLock::acquire(lock.clone(), Duration::from_secs(30), FileLock::STALE);
        assert!(taken.path.is_some(), "the stale lock was replaced");
        drop(taken);
        assert!(!lock.exists());
    }

    #[test]
    fn atomic_writes_to_one_file_use_distinct_temp_names() {
        let dir = tempfile::tempdir().unwrap();
        let target = dir.path().join("f.toml");
        let handles: Vec<_> = (0..8)
            .map(|i| {
                let target = target.clone();
                std::thread::spawn(move || {
                    for j in 0..5 {
                        // A sharing violation on the shared target is fine; a temp file
                        // another writer renamed away (NotFound) is the bug.
                        if let Err(e) = atomic_write(
                            &target,
                            &format!(
                                "{i}-{j}
"
                            ),
                        ) {
                            assert_ne!(e.kind(), io::ErrorKind::NotFound, "{e}");
                        }
                    }
                })
            })
            .collect();
        for h in handles {
            h.join().unwrap();
        }
        assert!(fs::read_to_string(&target).unwrap().contains('-'));
        let leftovers = fs::read_dir(dir.path()).unwrap().count();
        assert_eq!(leftovers, 1, "no temp file is left behind");
    }

    #[test]
    fn the_writer_applies_writes_in_order_and_finish_waits() {
        let (_dir, store) = store();
        let writer = StateWriter::spawn(store.clone());
        for i in 0..50 {
            writer.send(StateFile::Frecency, format!("# {i}\n"));
        }
        writer.finish(Duration::from_secs(300));
        assert_eq!(
            fs::read_to_string(store.path(StateFile::Frecency)).unwrap(),
            "# 49\n"
        );
    }
}
