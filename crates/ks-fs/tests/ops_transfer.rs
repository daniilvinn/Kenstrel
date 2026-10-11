//! Copy / move / rename: trees, progress, cancel, conflicts, links.

#[path = "ops_common.rs"]
mod common;

use std::fs;
use std::io::Write;
use std::sync::{Arc, OnceLock, Weak};

use common::*;
use ks_fs::ops::{
    ConflictChoice, ConflictPolicy, Generation, JobEvent, JobKind, JobManager, JobOptions, JobSpec,
    Outcome, Resolution,
};

#[test]
fn copy_file_and_tree() {
    let h = Harness::new();
    let td = tempfile::tempdir().unwrap();
    let src = td.path().join("src");
    let dst = td.path().join("dst");
    make_tree(&src);
    fs::create_dir_all(&dst).unwrap();

    let run = h.run(JobSpec::copy_into(
        &[src.join("a.txt"), src.join("sub")],
        &dst,
    ));
    let rep = run.assert_done();
    assert_eq!(rep.kind, JobKind::Copy);
    assert_eq!(read(&dst.join("a.txt")), "alpha");
    assert_eq!(read(&dst.join("sub").join("файл ✓ 日本.txt")), "unicode");
    assert_eq!(listing(&dst.join("sub")), listing(&src.join("sub")));
    assert!(src.join("a.txt").exists(), "copy keeps the source");
    assert_eq!(rep.outcomes.len(), 2);
    assert!(rep
        .outcomes
        .iter()
        .all(|o| matches!(o, Outcome::Copied { .. })));
    assert!(!listing(&dst).iter().any(|n| n.contains(".part")));
}

#[test]
fn progress_events_are_sane() {
    let h = Harness::new();
    let td = tempfile::tempdir().unwrap();
    let src = td.path().join("src");
    let dst = td.path().join("dst");
    write(&src.join("big.bin"), &"x".repeat(9 * 1024 * 1024));
    write(&src.join("small.txt"), "tiny");
    fs::create_dir_all(&dst).unwrap();

    let run = h.run(JobSpec::copy_into(
        &[src.join("big.bin"), src.join("small.txt")],
        &dst,
    ));
    run.assert_done();
    assert!(matches!(run.events.first(), Some(JobEvent::Queued { .. })));
    assert!(run
        .events
        .iter()
        .any(|e| matches!(e, JobEvent::Started { .. })));
    let progress: Vec<_> = run
        .events
        .iter()
        .filter_map(|e| match e {
            JobEvent::Progress(p) => Some(p),
            _ => None,
        })
        .collect();
    assert!(progress.len() >= 2);
    let total = 9 * 1024 * 1024 + 4;
    let mut prev = 0;
    for p in &progress {
        assert!(p.bytes_done >= prev, "bytes_done must not go backwards");
        assert!(p.bytes_done <= p.bytes_total);
        assert!(p.files_done <= p.files_total);
        prev = p.bytes_done;
    }
    let last = progress.last().unwrap();
    assert_eq!((last.bytes_done, last.bytes_total), (total, total));
    assert_eq!((last.files_done, last.files_total), (2, 2));
    let rep = run.report();
    assert_eq!(rep.bytes_done, total);
}

#[test]
fn move_is_rename_on_same_volume() {
    let h = Harness::new();
    let td = tempfile::tempdir().unwrap();
    let src = td.path().join("src");
    let dst = td.path().join("dst");
    make_tree(&src);
    fs::create_dir_all(&dst).unwrap();
    let before = listing(&src.join("sub"));

    let run = h.run(JobSpec::move_into(
        &[src.join("sub"), src.join("a.txt")],
        &dst,
    ));
    let rep = run.assert_done();
    assert!(!src.join("sub").exists() && !src.join("a.txt").exists());
    assert_eq!(listing(&dst.join("sub")), before);
    assert!(rep
        .outcomes
        .iter()
        .all(|o| matches!(o, Outcome::Moved { .. })));
}

/// Real cross-volume move: the OS temp dir (usually C:) versus the build directory (E:).
#[test]
fn move_across_volumes_copies_then_deletes() {
    let a = tempfile::tempdir().unwrap();
    let b = tempfile::tempdir_in(env!("CARGO_MANIFEST_DIR")).unwrap();
    let root = |p: &std::path::Path| {
        p.components()
            .next()
            .map(|c| c.as_os_str().to_ascii_lowercase())
    };
    if root(a.path()) == root(b.path()) {
        return; // single-volume machine: nothing to test
    }
    let h = Harness::new();
    let src = a.path().join("src");
    make_tree(&src);
    let before = listing(&src.join("sub"));
    let run = h.run(JobSpec::move_into(&[src.join("sub")], b.path()));
    run.assert_done();
    assert!(
        !src.join("sub").exists(),
        "source removed after cross-volume move"
    );
    assert_eq!(listing(&b.path().join("sub")), before);
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
    assert!(last.bytes_total >= 10_000, "fallback copy adds to totals");
}

#[test]
fn cancel_mid_copy_removes_partial_file() {
    let td = tempfile::tempdir().unwrap();
    let src = td.path().join("big.bin");
    let dst = td.path().join("out");
    fs::create_dir_all(&dst).unwrap();
    {
        let mut f = fs::File::create(&src).unwrap();
        let chunk = vec![7u8; 1024 * 1024];
        for _ in 0..200 {
            f.write_all(&chunk).unwrap();
        }
    }
    // Cancel from inside the sink, on the job thread right after the first chunk, so the test
    // does not race against a fast disk.
    let (tx, rx) = std::sync::mpsc::channel();
    let cell: Arc<OnceLock<Weak<JobManager>>> = Arc::new(OnceLock::new());
    let cell2 = Arc::clone(&cell);
    let mgr = Arc::new(JobManager::new(move |ev: JobEvent| {
        if let JobEvent::Progress(p) = &ev {
            if p.bytes_done > 0 {
                if let Some(m) = cell2.get().and_then(Weak::upgrade) {
                    m.cancel(p.job);
                }
            }
        }
        let _ = tx.send(ev);
    }));
    let _ = cell.set(Arc::downgrade(&mgr));
    mgr.submit(
        JobSpec::copy_into(std::slice::from_ref(&src), &dst),
        Generation(1),
    );
    let mut last = None;
    while let Ok(ev) = rx.recv_timeout(std::time::Duration::from_secs(60)) {
        if ev.is_final() {
            last = Some(ev);
            break;
        }
    }
    match last.expect("job did not finish") {
        JobEvent::Cancelled(rep) => {
            assert!(rep.bytes_done < 200 * 1024 * 1024);
            assert!(rep.outcomes.is_empty());
        }
        other => panic!("expected Cancelled, got {other:?}"),
    }
    assert_eq!(
        fs::read_dir(&dst).unwrap().count(),
        0,
        "partial target must be gone"
    );
    assert!(src.exists());
}

#[test]
fn cancel_while_waiting_on_conflict() {
    let h = Harness::new();
    let td = tempfile::tempdir().unwrap();
    write(&td.path().join("a"), "1");
    write(&td.path().join("d").join("a"), "2");
    let id = h.mgr.submit(
        JobSpec::copy_into(&[td.path().join("a")], &td.path().join("d")),
        Generation(0),
    );
    loop {
        let ev =
            h.rx.recv_timeout(std::time::Duration::from_secs(10))
                .unwrap();
        if matches!(ev, JobEvent::ConflictPrompt(_)) {
            break;
        }
    }
    assert!(h.mgr.cancel(id));
    loop {
        let ev =
            h.rx.recv_timeout(std::time::Duration::from_secs(10))
                .unwrap();
        if let JobEvent::Cancelled(_) = ev {
            break;
        }
    }
    assert_eq!(read(&td.path().join("d").join("a")), "2");
}

fn conflict_setup() -> (tempfile::TempDir, std::path::PathBuf, std::path::PathBuf) {
    let td = tempfile::tempdir().unwrap();
    let src = td.path().join("src");
    let dst = td.path().join("dst");
    write(&src.join("f.txt"), "new");
    write(&dst.join("f.txt"), "old");
    (td, src, dst)
}

fn answer(choice: ConflictChoice) -> impl FnMut(&ks_fs::ops::ConflictPrompt) -> Resolution {
    move |_| Resolution::once(choice.clone())
}

#[test]
fn conflict_prompt_carries_metadata() {
    let h = Harness::new();
    let (_td, src, dst) = conflict_setup();
    let run = h.run_with(
        JobSpec::copy_into(&[src.join("f.txt")], &dst),
        JobOptions::default(),
        answer(ConflictChoice::Skip),
    );
    let p = run.prompts()[0].clone();
    assert_eq!(p.src, src.join("f.txt"));
    assert_eq!(p.dst, dst.join("f.txt"));
    assert_eq!((p.src_meta.len, p.dst_meta.len), (3, 3));
    assert!(!p.src_meta.is_dir && p.dst_meta.modified.is_some());
    assert_eq!(run.report().items_skipped, 1);
    assert_eq!(read(&dst.join("f.txt")), "old");
}

#[test]
fn conflict_overwrite_skip_rename_keepboth() {
    let h = Harness::new();
    for choice in ["overwrite", "skip", "rename", "keepboth"] {
        let (_td, src, dst) = conflict_setup();
        let c = match choice {
            "overwrite" => ConflictChoice::Overwrite,
            "skip" => ConflictChoice::Skip,
            "rename" => ConflictChoice::Rename("renamed.txt".into()),
            _ => ConflictChoice::KeepBoth,
        };
        let run = h.run_with(
            JobSpec::copy_into(&[src.join("f.txt")], &dst),
            JobOptions::default(),
            answer(c),
        );
        run.assert_done();
        let names = listing(&dst);
        match choice {
            "overwrite" => {
                assert_eq!(names, vec!["f.txt"]);
                assert_eq!(read(&dst.join("f.txt")), "new");
            }
            "skip" => {
                assert_eq!(names, vec!["f.txt"]);
                assert_eq!(read(&dst.join("f.txt")), "old");
            }
            "rename" => {
                assert_eq!(names, vec!["f.txt", "renamed.txt"]);
                assert_eq!(read(&dst.join("renamed.txt")), "new");
                assert_eq!(read(&dst.join("f.txt")), "old");
            }
            _ => {
                assert_eq!(names, vec!["f (1).txt", "f.txt"]);
                assert_eq!(read(&dst.join("f (1).txt")), "new");
            }
        }
    }
}

#[test]
fn rename_choice_with_invalid_name_is_an_item_error() {
    let h = Harness::new();
    let (_td, src, dst) = conflict_setup();
    let run = h.run_with(
        JobSpec::copy_into(&[src.join("f.txt")], &dst),
        JobOptions::default(),
        answer(ConflictChoice::Rename("a:b".into())),
    );
    assert!(matches!(run.last(), JobEvent::Failed(_)));
    assert_eq!(run.report().errors.len(), 1);
    assert_eq!(read(&dst.join("f.txt")), "old");
}

#[test]
fn rename_choice_to_another_existing_name_asks_again() {
    let h = Harness::new();
    let (_td, src, dst) = conflict_setup();
    write(&dst.join("g.txt"), "g");
    let mut n = 0;
    let run = h.run_with(
        JobSpec::copy_into(&[src.join("f.txt")], &dst),
        JobOptions::default(),
        |_| {
            n += 1;
            Resolution::once(if n == 1 {
                ConflictChoice::Rename("g.txt".into())
            } else {
                ConflictChoice::Rename("h.txt".into())
            })
        },
    );
    run.assert_done();
    assert_eq!(run.prompts().len(), 2);
    assert_eq!(read(&dst.join("h.txt")), "new");
}

#[test]
fn apply_to_all_asks_once() {
    for (choice, expect_new) in [
        (ConflictChoice::Overwrite, true),
        (ConflictChoice::Skip, false),
        (ConflictChoice::KeepBoth, false),
    ] {
        let h = Harness::new();
        let td = tempfile::tempdir().unwrap();
        let (src, dst) = (td.path().join("src"), td.path().join("dst"));
        let names = ["a.txt", "b.txt", "c.txt"];
        for n in names {
            write(&src.join(n), "new");
            write(&dst.join(n), "old");
        }
        write(&src.join("fresh.txt"), "new");
        let sources: Vec<_> = names
            .iter()
            .chain(&["fresh.txt"])
            .map(|n| src.join(n))
            .collect();
        let c = choice.clone();
        let run = h.run_with(
            JobSpec::copy_into(&sources, &dst),
            JobOptions::default(),
            move |_| Resolution::all(c.clone()),
        );
        run.assert_done();
        assert_eq!(
            run.prompts().len(),
            1,
            "apply_to_all must stop prompting ({choice:?})"
        );
        assert_eq!(read(&dst.join("fresh.txt")), "new");
        assert_eq!(read(&dst.join("a.txt")) == "new", expect_new);
        if choice == ConflictChoice::KeepBoth {
            assert!(dst.join("c (1).txt").exists());
            assert!(dst.join("a (1).txt").exists());
        }
    }
}

#[test]
fn non_interactive_policies() {
    let h = Harness::new();
    for (policy, content, count, fails) in [
        (ConflictPolicy::Overwrite, "new", 1, false),
        (ConflictPolicy::Skip, "old", 1, false),
        (ConflictPolicy::KeepBoth, "old", 2, false),
        (ConflictPolicy::Fail, "old", 1, true),
    ] {
        let (_td, src, dst) = conflict_setup();
        let run = h.run_policy(JobSpec::copy_into(&[src.join("f.txt")], &dst), policy);
        assert_eq!(
            matches!(run.last(), JobEvent::Failed(_)),
            fails,
            "{policy:?}"
        );
        assert!(run.prompts().is_empty());
        assert_eq!(read(&dst.join("f.txt")), content, "{policy:?}");
        assert_eq!(listing(&dst).len(), count, "{policy:?}");
    }
}

#[test]
fn overwrite_replaces_read_only_target() {
    let h = Harness::new();
    let (_td, src, dst) = conflict_setup();
    let target = dst.join("f.txt");
    let mut perm = fs::metadata(&target).unwrap().permissions();
    perm.set_readonly(true);
    fs::set_permissions(&target, perm).unwrap();
    h.run_policy(
        JobSpec::copy_into(&[src.join("f.txt")], &dst),
        ConflictPolicy::Overwrite,
    )
    .assert_done();
    assert_eq!(read(&target), "new");
}

#[test]
fn directories_merge_and_conflict_per_file() {
    let h = Harness::new();
    let td = tempfile::tempdir().unwrap();
    let (src, dst) = (td.path().join("src"), td.path().join("dst"));
    write(&src.join("d").join("same.txt"), "new");
    write(&src.join("d").join("only_src.txt"), "s");
    write(&dst.join("d").join("same.txt"), "old");
    write(&dst.join("d").join("only_dst.txt"), "d");
    let run = h.run_with(
        JobSpec::copy_into(&[src.join("d")], &dst),
        JobOptions::default(),
        answer(ConflictChoice::KeepBoth),
    );
    run.assert_done();
    assert_eq!(run.prompts().len(), 1);
    assert_eq!(
        listing(&dst.join("d")),
        vec!["only_dst.txt", "only_src.txt", "same (1).txt", "same.txt"]
    );
    // Undo records the new leaves, not the pre-existing directory.
    let outcomes = &run.report().outcomes;
    assert_eq!(outcomes.len(), 2);
}

#[test]
fn file_onto_directory_conflict_overwrite() {
    let h = Harness::new();
    let td = tempfile::tempdir().unwrap();
    let (src, dst) = (td.path().join("src"), td.path().join("dst"));
    write(&src.join("x"), "file");
    write(&dst.join("x").join("inner.txt"), "inner");
    let run = h.run_with(
        JobSpec::copy_into(&[src.join("x")], &dst),
        JobOptions::default(),
        answer(ConflictChoice::Overwrite),
    );
    run.assert_done();
    assert_eq!(read(&dst.join("x")), "file");
}

#[test]
fn paste_into_same_directory_keeps_both() {
    let h = Harness::new();
    let td = tempfile::tempdir().unwrap();
    write(&td.path().join("a.txt"), "x");
    let run = h.run_policy(
        JobSpec::copy_into(&[td.path().join("a.txt")], td.path()),
        ConflictPolicy::KeepBoth,
    );
    run.assert_done();
    assert_eq!(listing(td.path()), vec!["a (1).txt", "a.txt"]);
}

#[test]
fn copy_dir_into_itself_is_rejected() {
    let h = Harness::new();
    let td = tempfile::tempdir().unwrap();
    write(&td.path().join("d").join("f"), "x");
    let run = h.run(JobSpec::copy_into(
        &[td.path().join("d")],
        &td.path().join("d").join("sub"),
    ));
    assert!(matches!(run.last(), JobEvent::Failed(_)));
    assert!(run.report().errors[0].message.contains("into itself"));
    assert_eq!(listing(td.path()), vec!["d/", "d/f"]);
}

#[test]
fn missing_source_is_an_item_error_and_others_continue() {
    let h = Harness::new();
    let td = tempfile::tempdir().unwrap();
    write(&td.path().join("ok.txt"), "x");
    let dst = td.path().join("dst");
    fs::create_dir(&dst).unwrap();
    let run = h.run(JobSpec::copy_into(
        &[td.path().join("nope.txt"), td.path().join("ok.txt")],
        &dst,
    ));
    assert!(matches!(run.last(), JobEvent::Failed(_)));
    assert_eq!(run.report().errors.len(), 1);
    assert_eq!(run.report().items_done, 1);
    assert!(dst.join("ok.txt").exists());
}

#[test]
fn rename_job_and_case_only_rename() {
    let h = Harness::new();
    let td = tempfile::tempdir().unwrap();
    write(&td.path().join("a.txt"), "x");
    let spec = JobSpec::rename(&td.path().join("a.txt"), "b.txt").unwrap();
    let run = h.run(spec);
    let rep = run.assert_done();
    assert!(matches!(rep.outcomes[0], Outcome::Renamed { .. }));
    assert_eq!(listing(td.path()), vec!["b.txt"]);
    h.run(JobSpec::rename(&td.path().join("b.txt"), "B.txt").unwrap())
        .assert_done();
    let names: Vec<_> = fs::read_dir(td.path())
        .unwrap()
        .map(|e| e.unwrap().file_name().to_string_lossy().into_owned())
        .collect();
    assert_eq!(names, vec!["B.txt"]);
    assert!(JobSpec::rename(&td.path().join("B.txt"), "bad|name").is_err());
}

#[test]
fn rename_onto_existing_prompts() {
    let h = Harness::new();
    let td = tempfile::tempdir().unwrap();
    write(&td.path().join("a"), "A");
    write(&td.path().join("b"), "B");
    let run = h.run_with(
        JobSpec::rename(&td.path().join("a"), "b").unwrap(),
        JobOptions::default(),
        answer(ConflictChoice::Overwrite),
    );
    run.assert_done();
    assert_eq!(read(&td.path().join("b")), "A");
    assert!(!td.path().join("a").exists());
}

#[test]
fn queued_jobs_run_and_can_be_cancelled() {
    let (tx, rx) = std::sync::mpsc::channel();
    let mgr = ks_fs::ops::JobManager::with_concurrency(tx, 1);
    let td = tempfile::tempdir().unwrap();
    write(&td.path().join("a"), "1");
    write(&td.path().join("d").join("a"), "2");
    // First job blocks on a conflict, holding the only slot.
    let j1 = mgr.submit(
        JobSpec::copy_into(&[td.path().join("a")], &td.path().join("d")),
        Generation(1),
    );
    let j2 = mgr.submit(
        JobSpec::Create {
            entries: vec![ks_fs::ops::parse_create_input(td.path(), "new.txt").unwrap()],
        },
        Generation(2),
    );
    let j3 = mgr.submit(
        JobSpec::Create {
            entries: vec![ks_fs::ops::parse_create_input(td.path(), "never.txt").unwrap()],
        },
        Generation(3),
    );
    loop {
        if matches!(
            rx.recv_timeout(std::time::Duration::from_secs(10)).unwrap(),
            JobEvent::ConflictPrompt(_)
        ) {
            break;
        }
    }
    assert_eq!(
        mgr.jobs()
            .iter()
            .filter(|j| j.state == ks_fs::ops::JobState::Queued)
            .count(),
        2
    );
    assert!(mgr.cancel(j3));
    assert!(mgr.resolve(j1, Resolution::once(ConflictChoice::Skip)));
    assert!(mgr.wait_idle(std::time::Duration::from_secs(20)));
    assert!(td.path().join("new.txt").exists());
    assert!(!td.path().join("never.txt").exists());
    let finals: Vec<_> = rx.try_iter().filter(|e| e.is_final()).collect();
    assert_eq!(finals.len(), 3);
    assert!(finals
        .iter()
        .any(|e| e.job() == j3 && matches!(e, JobEvent::Cancelled(_))));
    assert!(finals
        .iter()
        .any(|e| e.job() == j2 && matches!(e, JobEvent::Done(_))));
}

#[cfg(windows)]
mod links {
    use super::*;
    use std::process::Command;

    fn junction(link: &std::path::Path, target: &std::path::Path) -> bool {
        Command::new("cmd")
            .args(["/C", "mklink", "/J"])
            .arg(link)
            .arg(target)
            .output()
            .map(|o| o.status.success())
            .unwrap_or(false)
    }

    #[test]
    fn copy_never_follows_junctions() {
        let h = Harness::new();
        let td = tempfile::tempdir().unwrap();
        let (src, dst) = (td.path().join("src"), td.path().join("dst"));
        let outside = td.path().join("outside");
        write(&outside.join("secret.txt"), "s");
        write(&src.join("real.txt"), "r");
        assert!(junction(&src.join("link"), &outside), "mklink /J failed");
        fs::create_dir_all(&dst).unwrap();

        let run = h.run(JobSpec::copy_into(std::slice::from_ref(&src), &dst));
        // The junction is either recreated as a link or skipped with an error, but its target's
        // files must never be copied as real content.
        assert!(
            !dst.join("src").join("link").join("secret.txt").exists()
                || fs::symlink_metadata(dst.join("src").join("link"))
                    .unwrap()
                    .file_type()
                    .is_symlink()
        );
        assert_eq!(read(&dst.join("src").join("real.txt")), "r");
        let _ = run.report();
        assert_eq!(read(&outside.join("secret.txt")), "s");
    }

    #[test]
    fn permanent_delete_removes_junction_not_target() {
        let h = Harness::new();
        let td = tempfile::tempdir().unwrap();
        let tree = td.path().join("tree");
        let outside = td.path().join("outside");
        write(&outside.join("keep.txt"), "keep");
        write(&tree.join("f.txt"), "f");
        assert!(junction(&tree.join("link"), &outside), "mklink /J failed");

        h.run(JobSpec::DeletePermanent {
            paths: vec![tree.clone()],
        })
        .assert_done();
        assert!(!tree.exists());
        assert_eq!(
            read(&outside.join("keep.txt")),
            "keep",
            "junction target must survive"
        );
    }

    #[test]
    fn move_renames_the_junction_itself() {
        let h = Harness::new();
        let td = tempfile::tempdir().unwrap();
        let outside = td.path().join("outside");
        write(&outside.join("keep.txt"), "keep");
        assert!(junction(&td.path().join("link"), &outside));
        fs::create_dir(td.path().join("dst")).unwrap();
        h.run(JobSpec::move_into(
            &[td.path().join("link")],
            &td.path().join("dst"),
        ))
        .assert_done();
        assert!(!td.path().join("link").exists());
        assert_eq!(
            read(&td.path().join("dst").join("link").join("keep.txt")),
            "keep"
        );
        assert_eq!(read(&outside.join("keep.txt")), "keep");
    }
}

/// A move whose rename fails (source locked) after the old destination was parked must put
/// the old destination back under its own name.
#[cfg(windows)]
#[test]
fn failed_overwrite_restores_the_previous_destination() {
    use std::os::windows::fs::OpenOptionsExt;
    let h = Harness::new();
    let td = tempfile::tempdir().unwrap();
    let src = td.path().join("src");
    let dst = td.path().join("dst");
    write(&src.join("f.txt"), "new");
    write(&dst.join("f.txt"), "old");
    // Hold the source open without delete sharing: rename fails with a sharing violation.
    let _lock = fs::OpenOptions::new()
        .read(true)
        .share_mode(1 | 2)
        .open(src.join("f.txt"))
        .unwrap();
    let run = h.run_with(
        JobSpec::move_into(&[src.join("f.txt")], &dst),
        JobOptions::default(),
        answer(ConflictChoice::Overwrite),
    );
    assert!(!run.report().errors.is_empty(), "the failure is reported");
    assert_eq!(read(&dst.join("f.txt")), "old", "old destination restored");
    assert_eq!(listing(&dst), vec!["f.txt"], "no parked leftovers");
    assert_eq!(read(&src.join("f.txt")), "new");
}

/// Copies (and so cross-volume moves) keep alternate data streams and the creation time.
#[cfg(windows)]
#[test]
fn copy_preserves_alternate_streams_and_creation_time() {
    let h = Harness::new();
    let td = tempfile::tempdir().unwrap();
    let src = td.path().join("f.txt");
    let dst = td.path().join("out");
    write(&src, "main");
    fs::write(td.path().join("f.txt:note"), "stream data").unwrap();
    fs::create_dir_all(&dst).unwrap();
    std::thread::sleep(std::time::Duration::from_millis(50));
    let created = fs::metadata(&src).unwrap().created().unwrap();
    h.run(JobSpec::copy_into(std::slice::from_ref(&src), &dst))
        .assert_done();
    assert_eq!(read(&dst.join("f.txt")), "main");
    assert_eq!(
        fs::read_to_string(dst.join("f.txt:note")).unwrap(),
        "stream data"
    );
    assert_eq!(
        fs::metadata(dst.join("f.txt")).unwrap().created().unwrap(),
        created
    );
}

/// A cross-volume move cancelled part-way keeps the whole source and leaves no partial or
/// temporary file behind (the data-loss guard of copy-then-delete).
#[cfg(windows)]
#[test]
fn cancelled_cross_volume_move_keeps_the_source() {
    let a = tempfile::tempdir().unwrap();
    let b = tempfile::tempdir_in(env!("CARGO_MANIFEST_DIR")).unwrap();
    let root = |p: &std::path::Path| {
        p.components()
            .next()
            .map(|c| c.as_os_str().to_ascii_lowercase())
    };
    if root(a.path()) == root(b.path()) {
        return; // single-volume machine: nothing to test
    }
    let src = a.path().join("big.bin");
    {
        let mut f = fs::File::create(&src).unwrap();
        let chunk = vec![7u8; 1024 * 1024];
        for _ in 0..200 {
            f.write_all(&chunk).unwrap();
        }
    }
    let (tx, rx) = std::sync::mpsc::channel();
    let cell: Arc<OnceLock<Weak<JobManager>>> = Arc::new(OnceLock::new());
    let cell2 = Arc::clone(&cell);
    let mgr = Arc::new(JobManager::new(move |ev: JobEvent| {
        if let JobEvent::Progress(p) = &ev {
            if p.bytes_done > 0 {
                if let Some(m) = cell2.get().and_then(Weak::upgrade) {
                    m.cancel(p.job);
                }
            }
        }
        let _ = tx.send(ev);
    }));
    let _ = cell.set(Arc::downgrade(&mgr));
    mgr.submit(
        JobSpec::move_into(std::slice::from_ref(&src), b.path()),
        Generation(1),
    );
    let mut last = None;
    while let Ok(ev) = rx.recv_timeout(std::time::Duration::from_secs(60)) {
        if ev.is_final() {
            last = Some(ev);
            break;
        }
    }
    match last.expect("job did not finish") {
        JobEvent::Cancelled(rep) => assert!(rep.consumed_sources.is_empty()),
        other => panic!("expected Cancelled, got {other:?}"),
    }
    assert_eq!(fs::metadata(&src).unwrap().len(), 200 * 1024 * 1024);
    assert_eq!(
        fs::read_dir(b.path()).unwrap().count(),
        0,
        "no partial or .tmp"
    );
}

/// Moves that replace or merge record no outcome for the top-level source, but the report
/// still lists it as consumed (so a cut register can drop it).
#[test]
fn move_reports_replaced_and_merged_sources_as_consumed() {
    let h = Harness::new();
    let td = tempfile::tempdir().unwrap();
    let src = td.path().join("src");
    let dst = td.path().join("dst");
    write(&src.join("f.txt"), "new");
    write(&dst.join("f.txt"), "old");
    let run = h.run_with(
        JobSpec::move_into(&[src.join("f.txt")], &dst),
        JobOptions::default(),
        answer(ConflictChoice::Overwrite),
    );
    let rep = run.assert_done();
    assert!(rep.outcomes.is_empty(), "replacement records no outcome");
    assert_eq!(rep.consumed_sources, vec![src.join("f.txt")]);

    write(&src.join("d").join("x.txt"), "x");
    write(&dst.join("d").join("y.txt"), "y");
    let run = h.run(JobSpec::move_into(&[src.join("d")], &dst));
    let rep = run.assert_done();
    assert!(
        rep.consumed_sources.contains(&src.join("d")),
        "{:?}",
        rep.consumed_sources
    );
}
