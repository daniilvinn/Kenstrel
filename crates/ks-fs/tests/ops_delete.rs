//! Permanent delete, trash, create.

#[path = "ops_common.rs"]
mod common;

use std::fs;

use common::*;
use ks_fs::ops::{parse_create_input, JobEvent, JobSpec, Outcome};

fn set_readonly(p: &std::path::Path) {
    let mut perm = fs::metadata(p).unwrap().permissions();
    perm.set_readonly(true);
    fs::set_permissions(p, perm).unwrap();
}

#[test]
fn permanent_delete_tree_including_read_only() {
    let h = Harness::new();
    let td = tempfile::tempdir().unwrap();
    let tree = td.path().join("tree");
    make_tree(&tree);
    set_readonly(&tree.join("a.txt"));
    set_readonly(&tree.join("sub").join("b.txt"));
    if cfg!(windows) {
        set_readonly(&tree.join("sub").join("deep")); // directory read-only attribute
    }
    write(&td.path().join("keep.txt"), "k");

    let run = h.run(JobSpec::DeletePermanent {
        paths: vec![tree.clone(), td.path().join("keep.txt")],
    });
    let rep = run.assert_done();
    assert!(!tree.exists() && !td.path().join("keep.txt").exists());
    assert_eq!(rep.items_done, 2);
    assert!(rep
        .outcomes
        .iter()
        .all(|o| matches!(o, Outcome::Deleted { .. })));
    let last = run
        .events
        .iter()
        .rev()
        .find_map(|e| {
            if let JobEvent::Progress(p) = e {
                Some(p)
            } else {
                None
            }
        })
        .unwrap();
    assert_eq!(last.files_done, last.files_total);
    assert_eq!(h.mgr.undo_depth(), 0, "permanent delete is not undoable");
}

#[test]
fn delete_missing_path_is_ok_and_dir_with_files() {
    let h = Harness::new();
    let td = tempfile::tempdir().unwrap();
    h.run(JobSpec::DeletePermanent {
        paths: vec![td.path().join("ghost")],
    })
    .assert_done();
}

#[test]
fn trash_removes_from_disk() {
    let h = Harness::new();
    let td = tempfile::tempdir().unwrap();
    write(&td.path().join("t.txt"), "bye");
    let run = h.run(JobSpec::Trash {
        paths: vec![td.path().join("t.txt")],
    });
    let rep = run.assert_done();
    assert!(!td.path().join("t.txt").exists());
    match &rep.outcomes[0] {
        Outcome::Trashed { item, .. } => {
            if ks_fs::ops::TRASH_RESTORE_SUPPORTED {
                assert!(item.is_some(), "trash id must be resolved on this platform");
            }
        }
        o => panic!("{o:?}"),
    }
    purge_trash_under(td.path());
}

#[test]
fn trash_missing_file_fails_cleanly() {
    let h = Harness::new();
    let td = tempfile::tempdir().unwrap();
    let run = h.run(JobSpec::Trash {
        paths: vec![td.path().join("ghost.txt")],
    });
    assert!(matches!(run.last(), JobEvent::Failed(_)));
    assert_eq!(h.mgr.undo_depth(), 0);
}

#[test]
fn create_files_and_dirs_nested() {
    let h = Harness::new();
    let td = tempfile::tempdir().unwrap();
    let entries = vec![
        parse_create_input(td.path(), "notes.md").unwrap(),
        parse_create_input(td.path(), "a/b/c.txt").unwrap(),
        parse_create_input(td.path(), "d1\\d2\\").unwrap(),
        parse_create_input(td.path(), "ünï/файл.txt").unwrap(),
    ];
    let run = h.run(JobSpec::Create { entries });
    let rep = run.assert_done();
    assert_eq!(
        listing(td.path()),
        vec![
            "a/",
            "a/b/",
            "a/b/c.txt",
            "d1/",
            "d1/d2/",
            "notes.md",
            "ünï/",
            "ünï/файл.txt"
        ]
    );
    // Outcomes name the topmost new entry (what undo removes).
    assert!(rep.outcomes.contains(&Outcome::Created {
        path: td.path().join("a"),
        is_dir: true
    }));
    assert!(rep.outcomes.contains(&Outcome::Created {
        path: td.path().join("notes.md"),
        is_dir: false
    }));
}

#[test]
fn create_existing_is_an_error_and_never_clobbers() {
    let h = Harness::new();
    let td = tempfile::tempdir().unwrap();
    write(&td.path().join("x.txt"), "precious");
    let run = h.run(JobSpec::Create {
        entries: vec![parse_create_input(td.path(), "x.txt").unwrap()],
    });
    assert!(matches!(run.last(), JobEvent::Failed(_)));
    assert_eq!(read(&td.path().join("x.txt")), "precious");
}
