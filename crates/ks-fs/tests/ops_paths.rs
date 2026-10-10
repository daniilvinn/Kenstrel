//! Long paths (> 260 chars) and Unicode names through every operation.

#[path = "ops_common.rs"]
mod common;

use std::fs;
use std::path::{Path, PathBuf};

use common::*;
use ks_fs::ops::{paths, JobSpec};

/// A directory path well beyond MAX_PATH, created with std (which adds the `\\?\` prefix itself).
fn deep_dir(root: &Path, name: &str) -> PathBuf {
    let mut p = root.join(name);
    for i in 0..6 {
        p.push(format!("level-{i}-{}", "ж".repeat(40)));
    }
    fs::create_dir_all(&p).unwrap();
    assert!(
        p.to_string_lossy().chars().count() > 260,
        "path must exceed MAX_PATH"
    );
    p
}

#[test]
fn copy_move_delete_deep_tree() {
    let h = Harness::new();
    let td = tempfile::tempdir().unwrap();
    let deep = deep_dir(td.path(), "src");
    write(&deep.join("leaf ✓.txt"), "deep");
    let dst = td.path().join("dst");
    fs::create_dir_all(&dst).unwrap();

    // copy: the destination is just as deep
    h.run(JobSpec::copy_into(&[td.path().join("src")], &dst))
        .assert_done();
    let copied = dst
        .join("src")
        .join(deep.strip_prefix(td.path().join("src")).unwrap());
    assert_eq!(read(&copied.join("leaf ✓.txt")), "deep");

    // move (rename of a deep tree) into a deeper place
    let dst2 = deep_dir(td.path(), "dst2");
    h.run(JobSpec::move_into(&[dst.join("src")], &dst2))
        .assert_done();
    assert!(!dst.join("src").exists());

    // permanent delete of everything
    h.run(JobSpec::DeletePermanent {
        paths: vec![td.path().join("src"), td.path().join("dst2")],
    })
    .assert_done();
    assert!(!td.path().join("src").exists() && !td.path().join("dst2").exists());
}

#[test]
fn create_and_rename_in_deep_dir_with_unicode() {
    let h = Harness::new();
    let td = tempfile::tempdir().unwrap();
    let deep = deep_dir(td.path(), "d");
    let entry = ks_fs::ops::parse_create_input(&deep, "новая папка/ファイル.txt").unwrap();
    h.run(JobSpec::Create {
        entries: vec![entry.clone()],
    })
    .assert_done();
    assert!(entry.path.exists());
    h.run(JobSpec::rename(&entry.path, "переименовано 🎉.txt").unwrap())
        .assert_done();
    assert!(entry.path.with_file_name("переименовано 🎉.txt").exists());
    h.undo().assert_done();
    assert!(entry.path.exists());
}

#[test]
fn trash_and_restore_deep_file() {
    if !trash_supported() {
        return;
    }
    let h = Harness::new();
    let td = tempfile::tempdir().unwrap();
    let deep = deep_dir(td.path(), "t");
    let file = deep.join("trashme.txt");
    write(&file, "x");
    let run = h.run(JobSpec::Trash {
        paths: vec![file.clone()],
    });
    // The Recycle Bin may refuse very long paths; either outcome must be clean.
    if run.assert_done_or_failed() {
        assert!(!file.exists());
        // Windows' shell restore API rejects originals over MAX_PATH (E_INVALIDARG): the undo
        // must then fail cleanly, reporting the error and leaving the item in the bin.
        let undo = h.undo();
        match undo.last() {
            ks_fs::ops::JobEvent::Done(_) => {
                #[cfg(windows)]
                panic!("documented: restore of >260 chars fails on Windows");
                #[cfg(not(windows))]
                assert_eq!(read(&file), "x");
            }
            ks_fs::ops::JobEvent::Failed(rep) => {
                assert!(!rep.errors.is_empty());
                assert!(!file.exists(), "failed restore leaves the item in the bin");
                assert_eq!(h.mgr.undo_depth(), 0, "the entry was consumed");
            }
            other => panic!("unexpected final event {other:?}"),
        }
    } else {
        assert!(file.exists(), "failed trash must leave the file alone");
    }
    purge_trash_under(td.path());
}

trait DoneOrFailed {
    fn assert_done_or_failed(&self) -> bool;
}
impl DoneOrFailed for Run {
    fn assert_done_or_failed(&self) -> bool {
        matches!(self.last(), ks_fs::ops::JobEvent::Done(_))
    }
}

#[test]
fn verbatim_and_display_helpers_for_deep_paths() {
    let td = tempfile::tempdir().unwrap();
    let deep = deep_dir(td.path(), "v");
    let v = paths::verbatim(&deep);
    if cfg!(windows) {
        assert!(v.to_string_lossy().starts_with(r"\\?\"));
        assert!(v.exists());
        // dunce keeps the prefix when the path is too long to be usable without it.
        assert_eq!(paths::display(&v), v.to_string_lossy());
        assert_eq!(paths::path_key(&v), paths::path_key(&deep));
    }
}

#[test]
fn unicode_names_roundtrip_through_copy_and_listing() {
    let h = Harness::new();
    let td = tempfile::tempdir().unwrap();
    let names = [
        "café.txt",
        "日本語.md",
        "Ω≈ç√.rs",
        "emoji 😀.txt",
        "ﬁ ligature.txt",
        "a b  c.txt",
    ];
    for n in names {
        write(&td.path().join("s").join(n), n);
    }
    fs::create_dir(td.path().join("d")).unwrap();
    let sources: Vec<_> = names.iter().map(|n| td.path().join("s").join(n)).collect();
    h.run(JobSpec::copy_into(&sources, &td.path().join("d")))
        .assert_done();
    for n in names {
        assert_eq!(read(&td.path().join("d").join(n)), n);
    }
}
