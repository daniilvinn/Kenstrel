//! Regression tests for the review findings: safe overwrite, scan/progress events, stale
//! resolutions, panicking sinks, event order and very deep trees.

#[path = "ops_common.rs"]
mod common;

use std::fs;
use std::io::Write;
use std::path::Path;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::{channel, Receiver};
use std::sync::{Arc, OnceLock, Weak};
use std::time::Duration;

use common::*;
use ks_fs::ops::{
    ConflictChoice, Generation, JobEvent, JobManager, JobOptions, JobSpec, Resolution,
};

const T: Duration = Duration::from_secs(60);

fn write_big(path: &Path, mib: usize) {
    fs::create_dir_all(path.parent().unwrap()).unwrap();
    let mut f = fs::File::create(path).unwrap();
    let chunk = vec![9u8; 1024 * 1024];
    for _ in 0..mib {
        f.write_all(&chunk).unwrap();
    }
}

fn leftovers(dir: &Path) -> Vec<String> {
    fs::read_dir(dir)
        .unwrap()
        .map(|e| e.unwrap().file_name().to_string_lossy().into_owned())
        .filter(|n| n.contains(".ks-old-") || n.ends_with(".part"))
        .collect()
}

/// A manager whose sink cancels the job as soon as `pred` matches an event.
fn cancelling_manager(
    pred: impl Fn(&JobEvent) -> bool + Send + Sync + 'static,
) -> (Arc<JobManager>, Receiver<JobEvent>) {
    let (tx, rx) = channel();
    let cell: Arc<OnceLock<Weak<JobManager>>> = Arc::new(OnceLock::new());
    let cell2 = Arc::clone(&cell);
    let mgr = Arc::new(JobManager::new(move |ev: JobEvent| {
        if pred(&ev) {
            if let Some(m) = cell2.get().and_then(Weak::upgrade) {
                m.cancel(ev.job());
            }
        }
        let _ = tx.send(ev);
    }));
    let _ = cell.set(Arc::downgrade(&mgr));
    (mgr, rx)
}

fn final_event(rx: &Receiver<JobEvent>) -> JobEvent {
    loop {
        let ev = rx.recv_timeout(T).expect("job did not finish");
        if ev.is_final() {
            return ev;
        }
    }
}

#[test]
fn overwrite_move_replaces_and_leaves_no_backup() {
    let h = Harness::new();
    let td = tempfile::tempdir().unwrap();
    write(&td.path().join("src").join("f.txt"), "new");
    write(&td.path().join("dst").join("f.txt"), "old");
    // directory replaced by a file
    write(&td.path().join("src").join("g"), "file");
    write(&td.path().join("dst").join("g").join("inner"), "dir");
    let run = h.run_with(
        JobSpec::move_into(
            &[
                td.path().join("src").join("f.txt"),
                td.path().join("src").join("g"),
            ],
            &td.path().join("dst"),
        ),
        JobOptions::default(),
        |_| Resolution::all(ConflictChoice::Overwrite),
    );
    run.assert_done();
    assert_eq!(read(&td.path().join("dst").join("f.txt")), "new");
    assert_eq!(read(&td.path().join("dst").join("g")), "file");
    assert!(leftovers(&td.path().join("dst")).is_empty());
}

#[test]
fn cancelled_overwrite_restores_the_old_destination() {
    let td = tempfile::tempdir().unwrap();
    let src = td.path().join("src");
    let dst = td.path().join("dst");
    write_big(&src.join("item"), 64);
    // The destination "item" is a directory, so it is parked aside, not replaced in place.
    write(&dst.join("item").join("keep.txt"), "precious");
    let (mgr, rx) =
        cancelling_manager(|ev| matches!(ev, JobEvent::Progress(p) if p.bytes_done > 0));
    mgr.submit_with(
        JobSpec::copy_into(&[src.join("item")], &dst),
        Generation(1),
        JobOptions::default(),
    );
    let mut finished = None;
    while let Ok(ev) = rx.recv_timeout(T) {
        match &ev {
            JobEvent::ConflictPrompt(p) => {
                assert!(mgr.resolve(p.job, Resolution::once(ConflictChoice::Overwrite)));
            }
            e if e.is_final() => {
                finished = Some(ev);
                break;
            }
            _ => {}
        }
    }
    assert!(
        matches!(finished, Some(JobEvent::Cancelled(_))),
        "{finished:?}"
    );
    assert_eq!(read(&dst.join("item").join("keep.txt")), "precious");
    assert!(leftovers(&dst).is_empty(), "{:?}", leftovers(&dst));
    assert!(mgr.wait_idle(T));
}

#[test]
fn scanning_events_precede_progress_and_match_totals() {
    let h = Harness::new();
    let td = tempfile::tempdir().unwrap();
    make_tree(&td.path().join("src"));
    fs::create_dir_all(td.path().join("dst")).unwrap();
    let run = h.run(JobSpec::copy_into(
        &[td.path().join("src")],
        &td.path().join("dst"),
    ));
    let rep = run.assert_done();
    let first_scan = run
        .events
        .iter()
        .position(|e| matches!(e, JobEvent::Scanning { .. }))
        .expect("a Scanning event");
    let first_progress = run
        .events
        .iter()
        .position(|e| matches!(e, JobEvent::Progress(_)))
        .unwrap();
    assert!(first_scan < first_progress);
    let (files, bytes) = run
        .events
        .iter()
        .filter_map(|e| match e {
            JobEvent::Scanning { files, bytes, .. } => Some((*files, *bytes)),
            _ => None,
        })
        .next_back()
        .unwrap();
    assert_eq!(bytes, rep.bytes_done);
    assert_eq!(files, 4);
}

#[test]
fn cancel_during_scan_copies_nothing() {
    let td = tempfile::tempdir().unwrap();
    make_tree(&td.path().join("src"));
    let dst = td.path().join("dst");
    fs::create_dir_all(&dst).unwrap();
    let (mgr, rx) = cancelling_manager(|ev| matches!(ev, JobEvent::Scanning { .. }));
    mgr.submit(
        JobSpec::copy_into(&[td.path().join("src")], &dst),
        Generation(1),
    );
    assert!(matches!(final_event(&rx), JobEvent::Cancelled(_)));
    assert_eq!(fs::read_dir(&dst).unwrap().count(), 0);
}

#[test]
fn progress_reports_throughput_and_sane_eta() {
    let h = Harness::new();
    let td = tempfile::tempdir().unwrap();
    write_big(&td.path().join("big.bin"), 128);
    fs::create_dir_all(td.path().join("out")).unwrap();
    let run = h.run(JobSpec::copy_into(
        &[td.path().join("big.bin")],
        &td.path().join("out"),
    ));
    run.assert_done();
    let prog: Vec<_> = run
        .events
        .iter()
        .filter_map(|e| match e {
            JobEvent::Progress(p) => Some(p),
            _ => None,
        })
        .collect();
    assert!(prog.iter().any(|p| p.throughput_bps > 0), "no throughput");
    for p in &prog {
        if p.eta.is_some() {
            assert!(p.throughput_bps > 0 && p.bytes_done < p.bytes_total);
        }
    }
    assert!(prog.last().unwrap().eta.is_none());
}

#[test]
fn resolve_without_pending_prompt_is_rejected() {
    let td = tempfile::tempdir().unwrap();
    for n in ["a", "b"] {
        write(&td.path().join("src").join(n), n);
        write(&td.path().join("dst").join(n), "old");
    }
    let (tx, rx) = channel();
    let mgr = JobManager::with_concurrency(tx, 1);
    // Job A blocks on a prompt and occupies the only slot; job B waits in the queue.
    let a = mgr.submit(
        JobSpec::copy_into(&[td.path().join("src").join("a")], &td.path().join("dst")),
        Generation(1),
    );
    loop {
        if matches!(rx.recv_timeout(T).unwrap(), JobEvent::ConflictPrompt(_)) {
            break;
        }
    }
    let b = mgr.submit(
        JobSpec::copy_into(&[td.path().join("src").join("b")], &td.path().join("dst")),
        Generation(1),
    );
    assert!(!mgr.resolve(b, Resolution::all(ConflictChoice::Overwrite)));
    assert!(mgr.resolve(a, Resolution::once(ConflictChoice::Skip)));
    assert!(!mgr.resolve(a, Resolution::all(ConflictChoice::Overwrite))); // duplicate answer
    let mut prompted = false;
    loop {
        match rx.recv_timeout(T).unwrap() {
            JobEvent::ConflictPrompt(p) if p.job == b => {
                prompted = true;
                assert!(mgr.resolve(b, Resolution::once(ConflictChoice::Skip)));
            }
            ev if ev.is_final() && ev.job() == b => break,
            _ => {}
        }
    }
    assert!(prompted, "B must ask for itself");
    assert_eq!(read(&td.path().join("dst").join("a")), "old");
    assert_eq!(read(&td.path().join("dst").join("b")), "old");
}

#[test]
fn panicking_sink_does_not_leak_the_slot() {
    let td = tempfile::tempdir().unwrap();
    write(&td.path().join("x"), "x");
    let (tx, rx) = channel();
    let armed = Arc::new(AtomicBool::new(true));
    let armed2 = Arc::clone(&armed);
    let mgr = JobManager::with_concurrency(
        move |ev: JobEvent| {
            if ev.is_final() && armed2.swap(false, Ordering::SeqCst) {
                panic!("sink bug");
            }
            let _ = tx.send(ev);
        },
        1,
    );
    fs::create_dir_all(td.path().join("o1")).unwrap();
    fs::create_dir_all(td.path().join("o2")).unwrap();
    mgr.submit(
        JobSpec::copy_into(&[td.path().join("x")], &td.path().join("o1")),
        Generation(1),
    );
    assert!(mgr.wait_idle(T), "slot leaked after a sink panic");
    mgr.submit(
        JobSpec::copy_into(&[td.path().join("x")], &td.path().join("o2")),
        Generation(1),
    );
    assert!(mgr.wait_idle(T));
    assert!(matches!(final_event(&rx), JobEvent::Done(_)));
    assert!(td.path().join("o2").join("x").exists());
}

#[test]
fn per_job_event_order_under_concurrent_cancel() {
    let td = tempfile::tempdir().unwrap();
    write(&td.path().join("f"), "f");
    let (tx, rx) = channel();
    let mgr = Arc::new(JobManager::with_concurrency(tx, 2));
    let mut ids = Vec::new();
    let mut handles = Vec::new();
    for i in 0..30 {
        let out = td.path().join(format!("o{i}"));
        fs::create_dir_all(&out).unwrap();
        let id = mgr.submit(
            JobSpec::copy_into(&[td.path().join("f")], &out),
            Generation(1),
        );
        ids.push(id);
        let m = Arc::clone(&mgr);
        handles.push(std::thread::spawn(move || {
            m.cancel(id);
        }));
    }
    for h in handles {
        h.join().unwrap();
    }
    assert!(mgr.wait_idle(T));
    let events: Vec<_> = rx.try_iter().collect();
    for id in ids {
        let mine: Vec<_> = events.iter().filter(|e| e.job() == id).collect();
        assert!(
            matches!(mine.first(), Some(JobEvent::Queued { .. })),
            "{mine:?}"
        );
        assert!(mine.last().unwrap().is_final(), "{mine:?}");
        assert_eq!(mine.iter().filter(|e| e.is_final()).count(), 1);
    }
}

#[test]
fn very_deep_tree_copy_and_delete_do_not_overflow_the_stack() {
    let h = Harness::new();
    let td = tempfile::tempdir().unwrap();
    let mut deep = td.path().join("src");
    for _ in 0..1500 {
        deep.push("d");
    }
    fs::create_dir_all(&deep).unwrap();
    write(&deep.join("leaf.txt"), "bottom");
    let dst = td.path().join("dst");
    fs::create_dir_all(&dst).unwrap();
    h.run(JobSpec::copy_into(&[td.path().join("src")], &dst))
        .assert_done();
    let mut copied = dst.join("src");
    for _ in 0..1500 {
        copied.push("d");
    }
    assert_eq!(read(&copied.join("leaf.txt")), "bottom");
    h.run(JobSpec::DeletePermanent {
        paths: vec![td.path().join("src"), dst.join("src")],
    })
    .assert_done();
    assert!(!td.path().join("src").exists() && !dst.join("src").exists());
}
