//! Reading and writing Kestrel's state files (bookmarks, frecency; PLAN §9).
//!
//! Writes are atomic (temporary file in the same directory, then rename), so a crash or a
//! second instance never leaves a half-written file. A file that cannot be parsed is moved
//! aside to `<name>.bad` instead of being overwritten, and the app starts with empty data.

use std::fs;
use std::io;
use std::path::{Path, PathBuf};
use std::sync::mpsc;
use std::time::Duration;

use ks_core::{Bookmarks, Frecency, LoadedNavData, StateFile};

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
    // The process id keeps two running instances from sharing a temporary file.
    let tmp = dir.join(format!(".{name}.{}.tmp", std::process::id()));
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
            Err(_) if attempt < 4 => {
                attempt += 1;
                std::thread::sleep(Duration::from_millis(25 * attempt));
            }
            result => return result,
        }
    }
}

/// The state directory (`%LOCALAPPDATA%\kestrel\state`).
#[derive(Clone, Debug)]
pub struct StateStore {
    dir: PathBuf,
}

impl StateStore {
    pub fn new(dir: PathBuf) -> Self {
        Self { dir }
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
            Err(e) if e.kind() == io::ErrorKind::NotFound => return None,
            Err(e) => {
                tracing::warn!(path = %path.display(), error = %e, "cannot read state file");
                // The file is there but unreadable: it must not be overwritten by a save.
                problems.push(format!("Cannot read {name}: {e} (it is left untouched)"));
                unreadable.push(file);
                return None;
            }
        };
        match parse(&text) {
            Ok(value) => Some(value),
            Err(error) => {
                tracing::warn!(path = %path.display(), %error, "damaged state file");
                let aside = path.with_file_name(format!("{name}.bad"));
                let moved = fs::rename(&path, &aside).is_ok();
                problems.push(if moved {
                    format!("{name} was damaged and starts afresh (old copy: {name}.bad)")
                } else {
                    format!("{name} is damaged and was ignored")
                });
                None
            }
        }
    }

    pub fn write(&self, file: StateFile, contents: &str) -> io::Result<()> {
        atomic_write(&self.path(file), contents)
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
    fn the_writer_applies_writes_in_order_and_finish_waits() {
        let (_dir, store) = store();
        let writer = StateWriter::spawn(store.clone());
        for i in 0..50 {
            writer.send(StateFile::Frecency, format!("# {i}\n"));
        }
        writer.finish(Duration::from_secs(5));
        assert_eq!(
            fs::read_to_string(store.path(StateFile::Frecency)).unwrap(),
            "# 49\n"
        );
    }
}
