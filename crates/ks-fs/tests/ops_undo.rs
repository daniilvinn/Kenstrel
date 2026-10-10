//! Undo log: every operation type, invalidation, bulk rename as one entry.

#[path = "ops_common.rs"]
mod common;

use std::fs;

use common::*;
use ks_fs::ops::{parse_create_input, BulkRename, JobEvent, JobSpec, Outcome, UndoError};

#[test]
fn undo_rename() {
    let h = Harness::new();
    let td = tempfile::tempdir().unwrap();
    write(&td.path().join("a.txt"), "x");
    h.run(JobSpec::rename(&td.path().join("a.txt"), "b.txt").unwrap())
        .assert_done();
    assert_eq!(
        h.mgr.undo_description().as_deref(),
        Some("rename a.txt to b.txt")
    );
    h.undo().assert_done();
    assert_eq!(listing(td.path()), vec!["a.txt"]);
    assert_eq!(h.mgr.undo_depth(), 0, "undo itself is not recorded");
    assert_eq!(h.mgr.undo(ks_fs::ops::Generation(0)), Err(UndoError::Empty));
}

#[test]
fn undo_move_recreates_parent() {
    let h = Harness::new();
    let td = tempfile::tempdir().unwrap();
    let (src, dst) = (td.path().join("src"), td.path().join("dst"));
    write(&src.join("only").join("f.txt"), "x");
    fs::create_dir_all(&dst).unwrap();
    h.run(JobSpec::move_into(&[src.join("only")], &dst))
        .assert_done();
    fs::remove_dir(&src).unwrap(); // the source directory disappears meanwhile
    h.undo().assert_done();
    assert_eq!(read(&src.join("only").join("f.txt")), "x");
    assert!(!dst.join("only").exists());
}

#[test]
fn undo_create_goes_to_trash() {
    let h = Harness::new();
    let td = tempfile::tempdir().unwrap();
    h.run(JobSpec::Create {
        entries: vec![parse_create_input(td.path(), "a/b/new.txt").unwrap()],
    })
    .assert_done();
    let run = h.undo();
    run.assert_done();
    assert!(matches!(run.report().outcomes[0], Outcome::Trashed { .. }));
    assert!(!td.path().join("a").exists(), "topmost created dir removed");
    purge_trash_under(td.path());
}

#[test]
fn undo_create_refuses_when_file_was_edited() {
    let h = Harness::new();
    let td = tempfile::tempdir().unwrap();
    h.run(JobSpec::Create {
        entries: vec![parse_create_input(td.path(), "n.txt").unwrap()],
    })
    .assert_done();
    write(&td.path().join("n.txt"), "user content");
    let err = h.mgr.undo(ks_fs::ops::Generation(0)).unwrap_err();
    assert!(
        matches!(err, UndoError::Changed(ref m) if m.contains("modified")),
        "{err}"
    );
    assert_eq!(read(&td.path().join("n.txt")), "user content");
    assert_eq!(h.mgr.undo_depth(), 0, "invalid entry is dropped");
}

#[test]
fn undo_copy_trashes_the_copies() {
    let h = Harness::new();
    let td = tempfile::tempdir().unwrap();
    let (src, dst) = (td.path().join("src"), td.path().join("dst"));
    make_tree(&src);
    fs::create_dir_all(&dst).unwrap();
    h.run(JobSpec::copy_into(
        &[src.join("a.txt"), src.join("sub")],
        &dst,
    ))
    .assert_done();
    h.undo().assert_done();
    assert!(listing(&dst).is_empty());
    assert!(src.join("a.txt").exists() && src.join("sub").exists());
    purge_trash_under(td.path());
}

#[test]
fn undo_copy_refuses_when_copy_was_modified_or_removed() {
    let h = Harness::new();
    let td = tempfile::tempdir().unwrap();
    let (src, dst) = (td.path().join("src"), td.path().join("dst"));
    write(&src.join("a.txt"), "x");
    fs::create_dir_all(&dst).unwrap();
    h.run(JobSpec::copy_into(&[src.join("a.txt")], &dst))
        .assert_done();
    fs::remove_file(dst.join("a.txt")).unwrap();
    assert!(matches!(
        h.mgr.undo(ks_fs::ops::Generation(0)),
        Err(UndoError::Changed(_))
    ));
}

#[test]
fn undo_rename_refuses_when_original_name_is_taken() {
    let h = Harness::new();
    let td = tempfile::tempdir().unwrap();
    write(&td.path().join("a"), "1");
    h.run(JobSpec::rename(&td.path().join("a"), "b").unwrap())
        .assert_done();
    write(&td.path().join("a"), "other");
    assert!(matches!(
        h.mgr.undo(ks_fs::ops::Generation(0)),
        Err(UndoError::Changed(_))
    ));
    assert_eq!(read(&td.path().join("a")), "other");
    assert_eq!(read(&td.path().join("b")), "1");
}

/// The VERIFY item: trash + restore through the `trash` crate's `os_limited` API, for real.
#[test]
fn undo_trash_restores_from_recycle_bin() {
    if !trash_supported() {
        return;
    }
    let h = Harness::new();
    let td = tempfile::tempdir().unwrap();
    let dir = td.path().join("restore me ✓");
    write(&dir.join("inner.txt"), "inner");
    write(&td.path().join("single.txt"), "single");
    let run = h.run(JobSpec::Trash {
        paths: vec![dir.clone(), td.path().join("single.txt")],
    });
    run.assert_done();
    assert!(!dir.exists() && !td.path().join("single.txt").exists());
    assert_eq!(
        h.mgr.undo_description().as_deref(),
        Some("delete of 2 item(s)")
    );

    let undo = h.undo();
    let rep = undo.assert_done();
    assert!(rep
        .outcomes
        .iter()
        .all(|o| matches!(o, Outcome::Restored { .. })));
    assert_eq!(read(&dir.join("inner.txt")), "inner");
    assert_eq!(read(&td.path().join("single.txt")), "single");
}

#[test]
fn undo_trash_refuses_when_original_path_reoccupied() {
    if !trash_supported() {
        return;
    }
    let h = Harness::new();
    let td = tempfile::tempdir().unwrap();
    write(&td.path().join("t.txt"), "first");
    h.run(JobSpec::Trash {
        paths: vec![td.path().join("t.txt")],
    })
    .assert_done();
    write(&td.path().join("t.txt"), "second");
    assert!(matches!(
        h.mgr.undo(ks_fs::ops::Generation(0)),
        Err(UndoError::Changed(_))
    ));
    assert_eq!(read(&td.path().join("t.txt")), "second");
    purge_trash_under(td.path());
    // Clean up our trash entry so the user's bin does not accumulate test files.
}

#[test]
fn bulk_rename_executes_and_undoes_as_one_entry() {
    let h = Harness::new();
    let td = tempfile::tempdir().unwrap();
    for n in ["a", "b", "c", "d"] {
        write(&td.path().join(n), n);
    }
    let session = BulkRename::new(td.path(), ["a", "b", "c", "d"].map(String::from).to_vec());
    // swap a<->b, rotate c->d->e... d->c is a swap too: c<->d plus new names
    let plan = session.plan("b\na\nd\nc\n").unwrap();
    assert!(
        plan.steps.len() > plan.changes.len(),
        "swaps need temp names"
    );
    assert_eq!(plan.preview().len(), 4);

    let run = h.run(plan.into_spec());
    run.assert_done();
    assert_eq!(read(&td.path().join("a")), "b");
    assert_eq!(read(&td.path().join("b")), "a");
    assert_eq!(read(&td.path().join("c")), "d");
    assert_eq!(read(&td.path().join("d")), "c");
    assert_eq!(listing(td.path()).len(), 4, "no temp names left behind");
    assert_eq!(h.mgr.undo_depth(), 1);

    h.undo().assert_done();
    for n in ["a", "b", "c", "d"] {
        assert_eq!(read(&td.path().join(n)), n);
    }
    assert_eq!(listing(td.path()).len(), 4);
    assert_eq!(h.mgr.undo_depth(), 0);
}

#[test]
fn bulk_rename_three_cycle_and_undo() {
    let h = Harness::new();
    let td = tempfile::tempdir().unwrap();
    for n in ["x", "y", "z"] {
        write(&td.path().join(n), n);
    }
    let s = BulkRename::new(td.path(), ["x", "y", "z"].map(String::from).to_vec());
    h.run(s.plan("y\nz\nx\n").unwrap().into_spec())
        .assert_done();
    assert_eq!(read(&td.path().join("y")), "x");
    assert_eq!(read(&td.path().join("z")), "y");
    assert_eq!(read(&td.path().join("x")), "z");
    h.undo().assert_done();
    for n in ["x", "y", "z"] {
        assert_eq!(read(&td.path().join(n)), n);
    }
}

#[test]
fn bulk_rename_failure_stops_and_reports() {
    let h = Harness::new();
    let td = tempfile::tempdir().unwrap();
    write(&td.path().join("a"), "a");
    write(&td.path().join("b"), "b");
    let s = BulkRename::new(td.path(), vec!["a".into(), "b".into()]);
    let plan = s.plan("a2\nb2\n").unwrap();
    // The world changes between planning and executing: a2 appears.
    write(&td.path().join("a2"), "intruder");
    let run = h.run(plan.into_spec());
    assert!(matches!(run.last(), JobEvent::Failed(_)));
    assert_eq!(read(&td.path().join("a2")), "intruder");
    assert_eq!(read(&td.path().join("a")), "a");
}

#[test]
fn multiple_undo_steps_in_order() {
    let h = Harness::new();
    let td = tempfile::tempdir().unwrap();
    write(&td.path().join("a"), "1");
    h.run(JobSpec::rename(&td.path().join("a"), "b").unwrap())
        .assert_done();
    h.run(JobSpec::rename(&td.path().join("b"), "c").unwrap())
        .assert_done();
    h.undo().assert_done();
    assert_eq!(listing(td.path()), vec!["b"]);
    h.undo().assert_done();
    assert_eq!(listing(td.path()), vec!["a"]);
}

/// With `track_trash_ids = false` the trash still works but undo cannot restore the items.
#[test]
fn trash_without_id_tracking_is_not_restorable() {
    if !trash_supported() {
        return;
    }
    let h = Harness::new();
    let td = tempfile::tempdir().unwrap();
    let f = td.path().join("untracked.txt");
    write(&f, "x");
    h.run_with(
        JobSpec::Trash {
            paths: vec![f.clone()],
        },
        ks_fs::ops::JobOptions {
            track_trash_ids: false,
            ..Default::default()
        },
        |p| panic!("unexpected prompt {p:?}"),
    )
    .assert_done();
    assert!(!f.exists());
    // Either undo is refused up front or the restore job reports a failure; never a silent success.
    match h.mgr.undo(ks_fs::ops::Generation(42)) {
        Err(_) => {}
        Ok(id) => loop {
            let ev =
                h.rx.recv_timeout(std::time::Duration::from_secs(60))
                    .unwrap();
            if ev.job() == id && ev.is_final() {
                assert!(!matches!(ev, JobEvent::Done(_)), "must not report success");
                break;
            }
        },
    }
    assert!(!f.exists());
    purge_trash_under(td.path());
}
