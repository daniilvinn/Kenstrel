//! Directory listing: `read_dir` plus per-entry metadata, delivered in chunks.
//!
//! On Windows `DirEntry::metadata` is built from the `WIN32_FIND_DATAW` that `FindNextFileW`
//! already returned, so reading size, times and attributes costs no extra system call per
//! entry. Links are the exception: their kind needs one extra call each (rare).

use std::fs::{self, DirEntry};
use std::io;
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

use ks_core::{Attrs, Entry, EntryKind, FsError};
use tokio_util::sync::CancellationToken;

use crate::{attrs, platform};

/// Entries in the first chunk, so something can be shown quickly in a huge directory.
pub const FIRST_CHUNK: usize = 1_000;
/// Entries in every later chunk.
pub const CHUNK: usize = 5_000;
/// A chunk is also sent when this much time passed since the last one, so a slow disk
/// shows progress instead of silence.
pub const CHUNK_INTERVAL: Duration = Duration::from_millis(60);

/// Reads `dir` and passes its entries to `sink` in chunks (unsorted).
///
/// Returns `Err` only when the directory itself cannot be read; an entry that cannot be
/// inspected (it vanished, or access is denied) is skipped. Stops quietly when `cancel`
/// fires or when `sink` returns `false` (the receiver is gone).
pub fn list_dir(
    dir: &Path,
    cancel: &CancellationToken,
    mut sink: impl FnMut(Vec<Entry>) -> bool,
) -> Result<(), FsError> {
    let reader = fs::read_dir(dir).map_err(|e| FsError::from(&e))?;
    let mut chunk = Vec::with_capacity(FIRST_CHUNK);
    let mut target = FIRST_CHUNK;
    let mut last_sent = Instant::now();

    for item in reader {
        if cancel.is_cancelled() {
            return Ok(());
        }
        let Ok(item) = item else { continue };
        if let Some(entry) = make_entry(&item) {
            chunk.push(entry);
        }
        if chunk.len() >= target || (!chunk.is_empty() && last_sent.elapsed() >= CHUNK_INTERVAL) {
            let full = std::mem::replace(&mut chunk, Vec::with_capacity(CHUNK));
            if !sink(full) {
                return Ok(());
            }
            target = CHUNK;
            last_sent = Instant::now();
        }
    }
    if !chunk.is_empty() && !cancel.is_cancelled() {
        sink(chunk);
    }
    Ok(())
}

/// Where the link at `path` points. Read on demand (the preview shows it) rather than for
/// every entry of a listing. A `\?\` prefix is stripped for display.
pub fn link_target(path: &Path) -> io::Result<PathBuf> {
    fs::read_link(path).map(|target| dunce::simplified(&target).to_path_buf())
}

/// Builds an [`Entry`] from a directory item, or `None` when it cannot be classified.
fn make_entry(item: &DirEntry) -> Option<Entry> {
    let file_type = item.file_type().ok()?;
    let metadata = item.metadata().ok();

    #[cfg(windows)]
    let raw_attrs = {
        use std::os::windows::fs::MetadataExt;
        metadata.as_ref().map_or(0, MetadataExt::file_attributes)
    };
    #[cfg(not(windows))]
    let raw_attrs = 0u32;

    let name = item.file_name();
    let mut attributes = attrs::from_windows(raw_attrs);
    let mut is_dir_target = file_type.is_dir();

    let kind = if file_type.is_symlink() {
        is_dir_target = link_points_to_dir(item, raw_attrs);
        if is_dir_target && is_junction(item) {
            EntryKind::Junction
        } else {
            EntryKind::Symlink
        }
    } else if file_type.is_dir() {
        EntryKind::Dir
    } else {
        EntryKind::File
    };

    if executable(&name, metadata.as_ref()) && kind == EntryKind::File {
        attributes = attributes | Attrs::EXEC;
    }
    #[cfg(not(windows))]
    if metadata
        .as_ref()
        .is_some_and(|m| m.permissions().readonly())
    {
        attributes = attributes | Attrs::READONLY;
    }

    let mut entry = Entry::new(name, kind).with_attrs(attributes);
    if is_dir_target {
        entry = entry.with_dir_target();
    }
    if let Some(metadata) = &metadata {
        if kind != EntryKind::Dir && kind != EntryKind::Junction {
            entry.size = metadata.len();
        }
        entry.mtime = metadata.modified().ok();
    }
    Some(entry)
}

/// Whether the link behind `item` points at a directory.
fn link_points_to_dir(item: &DirEntry, raw_attrs: u32) -> bool {
    if cfg!(windows) {
        // Directory links carry the directory attribute themselves.
        raw_attrs & attrs::win::DIRECTORY != 0
    } else {
        fs::metadata(item.path()).is_ok_and(|m| m.is_dir())
    }
}

/// NTFS junctions are directory links whose reparse tag is "mount point".
fn is_junction(item: &DirEntry) -> bool {
    platform::reparse_tag(&item.path()) == Some(attrs::win::REPARSE_TAG_MOUNT_POINT)
}

fn executable(name: &std::ffi::OsStr, metadata: Option<&fs::Metadata>) -> bool {
    #[cfg(windows)]
    {
        let _ = metadata;
        attrs::is_windows_executable(name)
    }
    #[cfg(not(windows))]
    {
        use std::os::unix::fs::PermissionsExt;
        let _ = name;
        metadata.is_some_and(|m| m.is_file() && m.permissions().mode() & 0o111 != 0)
    }
}

#[cfg(test)]
mod tests {
    use std::fs::File;
    use std::io::Write;

    use ks_core::config::SortConfig;
    use ks_core::entry::sort_entries;

    use super::*;

    fn collect(dir: &Path) -> Result<Vec<Vec<Entry>>, FsError> {
        let mut chunks = Vec::new();
        list_dir(dir, &CancellationToken::new(), |c| {
            chunks.push(c);
            true
        })?;
        Ok(chunks)
    }

    fn flat(dir: &Path) -> Vec<Entry> {
        let mut all: Vec<Entry> = collect(dir).unwrap().into_iter().flatten().collect();
        sort_entries(&mut all, &SortConfig::default());
        all
    }

    #[test]
    fn lists_files_directories_sizes_and_times() {
        let dir = tempfile::tempdir().unwrap();
        File::create(dir.path().join("a.txt"))
            .unwrap()
            .write_all(b"hello")
            .unwrap();
        fs::create_dir(dir.path().join("sub")).unwrap();
        let entries = flat(dir.path());
        assert_eq!(entries.len(), 2);
        assert_eq!(entries[0].display_name(), "sub");
        assert_eq!(entries[0].kind, EntryKind::Dir);
        assert!(entries[0].is_dir);
        assert_eq!(entries[1].display_name(), "a.txt");
        assert_eq!(entries[1].kind, EntryKind::File);
        assert_eq!(entries[1].size, 5);
        assert!(entries[1].mtime.is_some());
    }

    #[test]
    fn empty_directory_yields_no_chunks() {
        let dir = tempfile::tempdir().unwrap();
        assert!(collect(dir.path()).unwrap().is_empty());
    }

    #[test]
    fn unicode_names_survive() {
        let dir = tempfile::tempdir().unwrap();
        for name in ["日本語.txt", "émoji 🎉.md", "with space"] {
            File::create(dir.path().join(name)).unwrap();
        }
        let names: Vec<String> = flat(dir.path())
            .iter()
            .map(|e| e.display_name().into_owned())
            .collect();
        assert_eq!(names.len(), 3);
        assert!(names.contains(&"日本語.txt".to_owned()));
        assert!(names.contains(&"émoji 🎉.md".to_owned()));
    }

    #[test]
    fn large_directories_arrive_in_chunks_of_the_documented_size() {
        let dir = tempfile::tempdir().unwrap();
        for i in 0..(FIRST_CHUNK + 30) {
            File::create(dir.path().join(format!("f{i}"))).unwrap();
        }
        let chunks = collect(dir.path()).unwrap();
        let total: usize = chunks.iter().map(Vec::len).sum();
        assert_eq!(total, FIRST_CHUNK + 30);
        assert!(chunks.len() >= 2, "{} chunks", chunks.len());
        assert!(chunks[0].len() >= FIRST_CHUNK || chunks[0].len() > 1);
    }

    #[test]
    fn missing_directory_is_a_not_found_error() {
        let dir = tempfile::tempdir().unwrap();
        let err = collect(&dir.path().join("nope")).unwrap_err();
        assert_eq!(err.kind, ks_core::FsErrorKind::NotFound);
    }

    /// A directory that exists but cannot be read maps to `PermissionDenied`.
    #[cfg(any(unix, windows))]
    #[test]
    fn an_unreadable_directory_is_a_permission_error() {
        let dir = tempfile::tempdir().unwrap();
        let locked = dir.path().join("locked");
        fs::create_dir(&locked).unwrap();
        File::create(locked.join("inside")).unwrap();

        #[cfg(unix)]
        let restore = {
            use std::os::unix::fs::PermissionsExt;
            fs::set_permissions(&locked, fs::Permissions::from_mode(0o000)).unwrap();
            // Root ignores mode bits; nothing to assert then.
            if fs::read_dir(&locked).is_ok() {
                fs::set_permissions(&locked, fs::Permissions::from_mode(0o755)).unwrap();
                return;
            }
            let locked = locked.clone();
            move || fs::set_permissions(&locked, fs::Permissions::from_mode(0o755)).unwrap()
        };
        #[cfg(windows)]
        let restore = {
            let user = std::env::var("USERNAME").unwrap();
            let status = std::process::Command::new("icacls")
                .arg(&locked)
                .args(["/deny", &format!("{user}:(RD)")])
                .output()
                .unwrap()
                .status;
            if !status.success() || fs::read_dir(&locked).is_ok() {
                return; // cannot set up the scenario here
            }
            let locked = locked.clone();
            move || {
                let _ = std::process::Command::new("icacls")
                    .arg(&locked)
                    .args(["/remove:d", &user])
                    .output();
            }
        };

        let result = collect(&locked);
        restore();
        let err = result.unwrap_err();
        assert_eq!(err.kind, ks_core::FsErrorKind::PermissionDenied, "{err}");
    }

    #[test]
    fn a_file_is_not_a_directory() {
        let dir = tempfile::tempdir().unwrap();
        let file = dir.path().join("f");
        File::create(&file).unwrap();
        assert!(collect(&file).is_err());
    }

    #[test]
    fn cancellation_stops_the_listing_without_error() {
        let dir = tempfile::tempdir().unwrap();
        for i in 0..50 {
            File::create(dir.path().join(format!("f{i}"))).unwrap();
        }
        let token = CancellationToken::new();
        token.cancel();
        let mut sent = 0;
        let result = list_dir(dir.path(), &token, |c| {
            sent += c.len();
            true
        });
        assert!(result.is_ok());
        assert_eq!(sent, 0);
    }

    #[test]
    fn a_closed_receiver_stops_the_listing() {
        let dir = tempfile::tempdir().unwrap();
        for i in 0..(FIRST_CHUNK * 3) {
            File::create(dir.path().join(format!("f{i}"))).unwrap();
        }
        let mut calls = 0;
        list_dir(dir.path(), &CancellationToken::new(), |_| {
            calls += 1;
            false
        })
        .unwrap();
        assert_eq!(calls, 1);
    }

    #[cfg(unix)]
    #[test]
    fn symlinks_and_executables_on_unix() {
        use std::os::unix::fs::{symlink, PermissionsExt};
        let dir = tempfile::tempdir().unwrap();
        fs::create_dir(dir.path().join("target_dir")).unwrap();
        symlink(dir.path().join("target_dir"), dir.path().join("link_dir")).unwrap();
        File::create(dir.path().join("plain")).unwrap();
        symlink(dir.path().join("plain"), dir.path().join("link_file")).unwrap();
        let script = dir.path().join("run.sh");
        File::create(&script).unwrap();
        fs::set_permissions(&script, fs::Permissions::from_mode(0o755)).unwrap();

        let entries = flat(dir.path());
        let get = |name: &str| entries.iter().find(|e| e.display_name() == name).unwrap();
        assert_eq!(get("link_dir").kind, EntryKind::Symlink);
        assert!(get("link_dir").is_dir);
        assert_eq!(get("link_file").kind, EntryKind::Symlink);
        assert!(!get("link_file").is_dir);
        assert!(get("run.sh").is_exec());
        assert_eq!(
            link_target(&dir.path().join("link_dir")).unwrap(),
            dir.path().join("target_dir")
        );
        assert!(!get("plain").is_exec());
    }

    #[cfg(windows)]
    #[test]
    fn windows_attributes_executables_and_junctions() {
        use std::process::Command;

        let dir = tempfile::tempdir().unwrap();
        File::create(dir.path().join("tool.exe")).unwrap();
        File::create(dir.path().join("notes.txt")).unwrap();
        let hidden = dir.path().join("secret.txt");
        File::create(&hidden).unwrap();
        let status = Command::new("attrib")
            .arg("+h")
            .arg(&hidden)
            .status()
            .unwrap();
        assert!(status.success());

        let real = dir.path().join("real");
        fs::create_dir(&real).unwrap();
        let junction = dir.path().join("junc");
        // `mklink /J` needs no elevation, unlike symbolic links.
        let status = Command::new("cmd")
            .args(["/C", "mklink", "/J"])
            .arg(&junction)
            .arg(&real)
            .output()
            .unwrap()
            .status;
        assert!(status.success(), "mklink /J failed");

        let entries = flat(dir.path());
        let get = |name: &str| entries.iter().find(|e| e.display_name() == name).unwrap();
        assert!(get("tool.exe").is_exec());
        assert!(!get("notes.txt").is_exec());
        assert!(get("secret.txt").is_hidden());
        assert!(!get("notes.txt").is_hidden());
        assert_eq!(get("junc").kind, EntryKind::Junction);
        assert!(get("junc").is_dir);
        let target = link_target(&junction).unwrap();
        assert_eq!(
            target.to_string_lossy().to_lowercase(),
            real.to_string_lossy().to_lowercase()
        );
        assert_eq!(get("real").kind, EntryKind::Dir);
    }
}
