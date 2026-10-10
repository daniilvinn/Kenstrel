//! Lazily counting the children of directories, for the size column of directory rows.

use std::ffi::OsString;
use std::fs;
use std::path::Path;

use tokio_util::sync::CancellationToken;

/// Directories per reported batch.
pub const BATCH: usize = 32;

/// Counts the entries of `dir/<name>` for each name and reports `(name, count)` batches to
/// `sink`. A directory that cannot be read (access denied, vanished) gets no count. Stops
/// when `cancel` fires or `sink` returns `false`.
pub fn count_children(
    dir: &Path,
    names: Vec<OsString>,
    cancel: &CancellationToken,
    mut sink: impl FnMut(Vec<(OsString, u32)>) -> bool,
) {
    let mut batch = Vec::with_capacity(BATCH);
    for name in names {
        if cancel.is_cancelled() {
            return;
        }
        let Ok(reader) = fs::read_dir(dir.join(&name)) else {
            continue;
        };
        let mut count: u32 = 0;
        for item in reader {
            if item.is_ok() {
                count = count.saturating_add(1);
            }
            // Directories can be huge; stay responsive to cancellation inside one.
            if count.is_multiple_of(4096) && cancel.is_cancelled() {
                return;
            }
        }
        batch.push((name, count));
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

    #[test]
    fn counts_files_and_subdirectories() {
        let dir = tempfile::tempdir().unwrap();
        let a = dir.path().join("a");
        fs::create_dir(&a).unwrap();
        fs::write(a.join("1"), "").unwrap();
        fs::write(a.join("2"), "").unwrap();
        fs::create_dir(a.join("sub")).unwrap();
        fs::create_dir(dir.path().join("empty")).unwrap();

        let mut got = Vec::new();
        count_children(
            dir.path(),
            vec!["a".into(), "empty".into(), "missing".into()],
            &CancellationToken::new(),
            |batch| {
                got.extend(batch);
                true
            },
        );
        assert_eq!(
            got,
            vec![(OsString::from("a"), 3), (OsString::from("empty"), 0)]
        );
    }

    #[test]
    fn batches_are_bounded_and_cancellation_stops_work() {
        let dir = tempfile::tempdir().unwrap();
        let names: Vec<OsString> = (0..BATCH * 2 + 5)
            .map(|i| {
                let name = format!("d{i}");
                fs::create_dir(dir.path().join(&name)).unwrap();
                OsString::from(name)
            })
            .collect();
        let mut sizes = Vec::new();
        count_children(dir.path(), names, &CancellationToken::new(), |batch| {
            sizes.push(batch.len());
            true
        });
        assert_eq!(sizes, [BATCH, BATCH, 5]);

        let token = CancellationToken::new();
        token.cancel();
        let mut called = false;
        count_children(dir.path(), vec!["d0".into()], &token, |_| {
            called = true;
            true
        });
        assert!(!called);
    }
}
