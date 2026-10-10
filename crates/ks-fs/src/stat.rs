//! Lazily reading size and modification time of listed paths, for virtual listings such as
//! the search results list whose entries come without file metadata.

use std::ffi::OsString;
use std::fs;
use std::path::Path;
use std::time::SystemTime;

use tokio_util::sync::CancellationToken;

/// Paths per reported batch.
pub const BATCH: usize = 64;

/// Size and modification time of one path.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct EntryStat {
    /// Size in bytes (0 for directories).
    pub size: u64,
    /// Last modification time, when the file system has one.
    pub mtime: Option<SystemTime>,
}

/// Reads the metadata of `dir/<name>` for each name and reports `(name, stat)` batches to
/// `sink`. A path that cannot be read (vanished, access denied) is left out. Links are
/// followed for their size and time, falling back to the link itself when it dangles. Stops
/// when `cancel` fires or `sink` returns `false`.
pub fn stat_entries(
    dir: &Path,
    names: Vec<OsString>,
    cancel: &CancellationToken,
    mut sink: impl FnMut(Vec<(OsString, EntryStat)>) -> bool,
) {
    let mut batch = Vec::with_capacity(BATCH);
    for name in names {
        if cancel.is_cancelled() {
            return;
        }
        let path = dir.join(&name);
        let Ok(meta) = fs::metadata(&path).or_else(|_| fs::symlink_metadata(&path)) else {
            continue;
        };
        let size = if meta.is_dir() { 0 } else { meta.len() };
        batch.push((
            name,
            EntryStat {
                size,
                mtime: meta.modified().ok(),
            },
        ));
        if batch.len() >= BATCH && !sink(std::mem::take(&mut batch)) {
            return;
        }
    }
    if !batch.is_empty() {
        sink(batch);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn run(dir: &Path, names: &[&str]) -> Vec<(OsString, EntryStat)> {
        let mut out = Vec::new();
        stat_entries(
            dir,
            names.iter().map(OsString::from).collect(),
            &CancellationToken::new(),
            |batch| {
                out.extend(batch);
                true
            },
        );
        out
    }

    #[test]
    fn reports_size_and_time_of_nested_relative_paths() {
        let dir = tempfile::tempdir().unwrap();
        fs::create_dir(dir.path().join("sub")).unwrap();
        fs::write(dir.path().join("sub").join("a.txt"), "12345").unwrap();
        let rel = Path::new("sub").join("a.txt");
        let got = run(dir.path(), &[rel.to_str().unwrap(), "sub"]);
        assert_eq!(got.len(), 2);
        assert_eq!(got[0].1.size, 5);
        assert!(got[0].1.mtime.is_some());
        assert_eq!(got[1].1.size, 0, "directories report no size");
    }

    #[test]
    fn missing_paths_are_skipped() {
        let dir = tempfile::tempdir().unwrap();
        fs::write(dir.path().join("here"), "x").unwrap();
        let got = run(dir.path(), &["gone", "here"]);
        assert_eq!(got.len(), 1);
        assert_eq!(got[0].0, OsString::from("here"));
    }

    #[test]
    fn batches_are_bounded_and_cancellation_stops() {
        let dir = tempfile::tempdir().unwrap();
        let names: Vec<String> = (0..BATCH * 2 + 3).map(|i| format!("f{i}")).collect();
        for n in &names {
            fs::write(dir.path().join(n), "x").unwrap();
        }
        let mut sizes = Vec::new();
        stat_entries(
            dir.path(),
            names.iter().map(OsString::from).collect(),
            &CancellationToken::new(),
            |b| {
                sizes.push(b.len());
                true
            },
        );
        assert_eq!(sizes, vec![BATCH, BATCH, 3]);

        let cancel = CancellationToken::new();
        cancel.cancel();
        let mut called = false;
        stat_entries(
            dir.path(),
            names.iter().map(OsString::from).collect(),
            &cancel,
            |_| {
                called = true;
                true
            },
        );
        assert!(!called);
    }
}
