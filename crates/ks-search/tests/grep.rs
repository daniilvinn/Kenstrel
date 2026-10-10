//! Correctness tests for content search on temp trees.
#![allow(clippy::print_stderr)]

use std::fs;
use std::path::Path;
use std::sync::Arc;
use std::time::{Duration, Instant};

use ks_search::{spawn_search, CancellationToken, FileMatches, GrepEvent, GrepQuery, GrepStats};

struct Outcome {
    files: Vec<FileMatches>,
    stats: GrepStats,
    capped: bool,
    cancelled: bool,
    error: Option<String>,
    events: Vec<GrepEvent>,
}

fn run_with(q: GrepQuery, generation: u64, cancel: CancellationToken) -> Outcome {
    let (tx, rx) = crossbeam_channel::unbounded();
    let sink = Arc::new(move |e: GrepEvent| {
        let _ = tx.send(e);
    });
    let handle = spawn_search(q, generation, cancel, sink).unwrap();
    let mut out = Outcome {
        files: vec![],
        stats: GrepStats::default(),
        capped: false,
        cancelled: false,
        error: None,
        events: vec![],
    };
    loop {
        let e = rx
            .recv_timeout(Duration::from_secs(30))
            .expect("search must terminate");
        assert_eq!(e.generation(), generation);
        out.events.push(e.clone());
        match e {
            GrepEvent::Matches { files, .. } => out.files.extend(files),
            GrepEvent::Progress { .. } => {}
            GrepEvent::Done { stats, capped, .. } => {
                out.stats = stats;
                out.capped = capped;
                break;
            }
            GrepEvent::Cancelled { stats, .. } => {
                out.stats = stats;
                out.cancelled = true;
                break;
            }
            GrepEvent::Error { message, .. } => {
                out.error = Some(message);
                break;
            }
        }
    }
    handle.join();
    out.files.sort_by(|a, b| a.rel.cmp(&b.rel));
    out
}

fn run(q: GrepQuery) -> Outcome {
    run_with(q, 1, CancellationToken::new())
}

fn write(root: &Path, rel: &str, content: impl AsRef<[u8]>) {
    let p = root.join(rel);
    fs::create_dir_all(p.parent().unwrap()).unwrap();
    fs::write(p, content).unwrap();
}

fn rels(o: &Outcome) -> Vec<String> {
    o.files
        .iter()
        .map(|f| f.rel.to_string_lossy().replace('\\', "/"))
        .collect()
}

fn q(root: &Path, pat: &str) -> GrepQuery {
    GrepQuery::new(root, pat)
}

#[test]
fn finds_lines_with_numbers_and_ranges() {
    let t = tempfile::tempdir().unwrap();
    write(t.path(), "a.txt", "one\ntwo needle two needle\nthree\n");
    write(t.path(), "b.txt", "nothing here\r\nneedle\r\n");
    let o = run(q(t.path(), "needle"));
    assert_eq!(rels(&o), vec!["a.txt", "b.txt"]);
    let a = &o.files[0].lines;
    assert_eq!(a.len(), 1);
    assert_eq!(a[0].line_number, 2);
    assert_eq!(a[0].text, "two needle two needle");
    assert_eq!(a[0].ranges, vec![4..10, 15..21]);
    let b = &o.files[1].lines;
    assert_eq!(b[0].line_number, 2);
    assert_eq!(b[0].text, "needle", "CRLF trimmed");
    assert_eq!(o.stats.matches, 2);
    assert_eq!(o.stats.files_with_matches, 2);
    assert!(!o.capped);
}

#[test]
fn smart_case() {
    let t = tempfile::tempdir().unwrap();
    write(t.path(), "a.txt", "Hello\nhello\nHELLO\n");
    let lower = run(q(t.path(), "hello"));
    assert_eq!(lower.files[0].lines.len(), 3);
    let upper = run(q(t.path(), "Hello"));
    assert_eq!(upper.files[0].lines.len(), 1);
    assert_eq!(upper.files[0].lines[0].line_number, 1);
}

#[test]
fn literal_vs_regex_with_special_chars() {
    let t = tempfile::tempdir().unwrap();
    write(t.path(), "a.txt", "a.c\nabc\nfoo(bar)\nx+y\n");
    // Literal: '.' is just a dot.
    let lit = run(q(t.path(), "a.c"));
    assert_eq!(lit.files[0].lines.len(), 1);
    // Regex: '.' matches any char.
    let mut rq = q(t.path(), "a.c");
    rq.regex = true;
    assert_eq!(run(rq).files[0].lines.len(), 2);
    // Literal parentheses and plus do not error.
    let paren = run(q(t.path(), "foo(bar)"));
    assert!(paren.error.is_none());
    assert_eq!(paren.files[0].lines[0].line_number, 3);
    assert_eq!(run(q(t.path(), "x+y")).files[0].lines.len(), 1);
    // Invalid regex reports an Error event and nothing else terminal.
    let mut bad = q(t.path(), "foo(");
    bad.regex = true;
    let o = run(bad);
    assert!(o.error.is_some());
}

#[test]
fn whole_word() {
    let t = tempfile::tempdir().unwrap();
    write(t.path(), "a.txt", "cat\nconcatenate\nthe cat sat\ncat_x\n");
    let plain = run(q(t.path(), "cat"));
    assert_eq!(plain.files[0].lines.len(), 4);
    let mut w = q(t.path(), "cat");
    w.whole_word = true;
    let o = run(w);
    let nums: Vec<u64> = o.files[0].lines.iter().map(|l| l.line_number).collect();
    assert_eq!(nums, vec![1, 3]);
}

#[test]
fn binary_files_skipped() {
    let t = tempfile::tempdir().unwrap();
    let mut bin = b"\0\0\0binary needle".to_vec();
    bin.extend_from_slice(&[0u8; 100]);
    write(t.path(), "bin.dat", bin);
    write(t.path(), "text.txt", "needle\n");
    let o = run(q(t.path(), "needle"));
    assert_eq!(rels(&o), vec!["text.txt"]);
    assert_eq!(o.stats.binary, 1);
}

#[test]
fn size_limit() {
    let t = tempfile::tempdir().unwrap();
    write(t.path(), "small.txt", "needle\n");
    write(t.path(), "big.txt", format!("needle\n{}", "x".repeat(5000)));
    let mut query = q(t.path(), "needle");
    query.max_file_size = 1000;
    let o = run(query);
    assert_eq!(rels(&o), vec!["small.txt"]);
    assert_eq!(o.stats.skipped_large, 1);
}

#[test]
fn gitignore_and_hidden() {
    let t = tempfile::tempdir().unwrap();
    write(t.path(), ".gitignore", "target/\n");
    write(t.path(), "src/a.rs", "needle\n");
    write(t.path(), "target/b.rs", "needle\n");
    write(t.path(), ".hidden/c.rs", "needle\n");
    let o = run(q(t.path(), "needle"));
    assert_eq!(rels(&o), vec!["src/a.rs"]);
    let mut ig = q(t.path(), "needle");
    ig.respect_gitignore = false;
    assert_eq!(rels(&run(ig)), vec!["src/a.rs", "target/b.rs"]);
    let mut hid = q(t.path(), "needle");
    hid.show_hidden = true;
    assert_eq!(rels(&run(hid)), vec![".hidden/c.rs", "src/a.rs"]);
}

#[test]
fn glob_filter_with_negation() {
    let t = tempfile::tempdir().unwrap();
    write(t.path(), "a.rs", "needle\n");
    write(t.path(), "b.txt", "needle\n");
    write(t.path(), "sub/c.rs", "needle\n");
    write(t.path(), "target/d.rs", "needle\n");
    let mut only_rs = q(t.path(), "needle");
    only_rs.glob = "*.rs".into();
    assert_eq!(rels(&run(only_rs)), vec!["a.rs", "sub/c.rs", "target/d.rs"]);
    let mut neg = q(t.path(), "needle");
    neg.glob = "*.rs !target/**".into();
    assert_eq!(rels(&run(neg)), vec!["a.rs", "sub/c.rs"]);
    let mut only_neg = q(t.path(), "needle");
    only_neg.glob = "!target/**".into();
    assert_eq!(rels(&run(only_neg)), vec!["a.rs", "b.txt", "sub/c.rs"]);
    let mut bad = q(t.path(), "needle");
    bad.glob = "[".into();
    assert!(run(bad).error.is_some());
}

#[test]
fn brace_alternation_globs_are_one_glob() {
    let t = tempfile::tempdir().unwrap();
    write(t.path(), "a.rs", "needle\n");
    write(t.path(), "b.toml", "needle\n");
    write(t.path(), "c.md", "needle\n");
    let mut braces = q(t.path(), "needle");
    braces.glob = "*.{rs,toml}".into();
    let o = run(braces);
    assert!(o.error.is_none(), "{:?}", o.error);
    assert_eq!(rels(&o), vec!["a.rs", "b.toml"]);
    let mut mixed = q(t.path(), "needle");
    mixed.glob = "*.{rs,md}, *.toml".into();
    assert_eq!(rels(&run(mixed)), vec!["a.rs", "b.toml", "c.md"]);
}

#[test]
fn byte_level_regex_in_a_long_multibyte_line_does_not_kill_workers() {
    let t = tempfile::tempdir().unwrap();
    let long = format!("{}é{}\n", "a".repeat(500), "b".repeat(500));
    for i in 0..8 {
        write(t.path(), &format!("f{i}.txt"), long.clone());
    }
    let mut query = q(t.path(), r"(?-u:\xA9)");
    query.regex = true;
    let o = run(query);
    assert!(o.error.is_none(), "{:?}", o.error);
    assert_eq!(o.stats.files_searched, 8);
}

#[test]
fn multi_line_regex_errors_are_a_single_short_line() {
    let t = tempfile::tempdir().unwrap();
    let mut query = q(t.path(), "(");
    query.regex = true;
    let msg = run(query).error.expect("an error");
    assert!(!msg.contains('\n'), "{msg:?}");
    assert!(msg.ends_with("unclosed group"), "{msg:?}");
}

#[test]
fn match_cap() {
    let t = tempfile::tempdir().unwrap();
    for i in 0..20 {
        write(t.path(), &format!("f{i}.txt"), "needle\n".repeat(100));
    }
    let mut query = q(t.path(), "needle");
    query.max_matches = 250;
    let o = run(query);
    let total: usize = o.files.iter().map(|f| f.lines.len()).sum();
    assert_eq!(total, 250);
    assert!(o.capped);
    assert!(!o.cancelled);
}

#[test]
fn cap_not_reported_when_exactly_reached() {
    let t = tempfile::tempdir().unwrap();
    write(t.path(), "a.txt", "needle\n".repeat(10));
    let mut query = q(t.path(), "needle");
    query.max_matches = 10;
    let o = run(query);
    assert_eq!(o.files[0].lines.len(), 10);
    assert!(!o.capped);
}

#[test]
fn generation_passed_through_and_terminal_last() {
    let t = tempfile::tempdir().unwrap();
    write(t.path(), "a.txt", "needle\n");
    let o = run_with(q(t.path(), "needle"), 9001, CancellationToken::new());
    assert!(o.events.iter().all(|e| e.generation() == 9001));
    assert!(o.events.last().unwrap().is_terminal());
    assert_eq!(o.events.iter().filter(|e| e.is_terminal()).count(), 1);
}

#[test]
fn empty_pattern_is_noop_done() {
    let t = tempfile::tempdir().unwrap();
    write(t.path(), "a.txt", "x\n");
    let o = run(q(t.path(), ""));
    assert!(o.files.is_empty() && o.error.is_none() && !o.cancelled);
}

#[test]
fn cancellation_stops_quickly() {
    let t = tempfile::tempdir().unwrap();
    let body = "filler line with the thing\n".repeat(20);
    for d in 0..40 {
        for f in 0..50 {
            write(t.path(), &format!("d{d}/f{f}.txt"), &body);
        }
    }
    // Deterministic: the sink cancels as soon as the first event is delivered, which always
    // precedes the terminal event, so the outcome must be Cancelled however fast the scan is.
    let cancel = CancellationToken::new();
    let c2 = cancel.clone();
    let (tx, rx) = crossbeam_channel::unbounded();
    let sink = Arc::new(move |e: GrepEvent| {
        c2.cancel();
        let _ = tx.send(e);
    });
    let mut query = q(t.path(), "thing");
    query.flush_interval = Duration::from_millis(1);
    let started = Instant::now();
    let handle = spawn_search(query, 5, cancel, sink).unwrap();
    let mut terminal = None;
    while let Ok(e) = rx.recv_timeout(Duration::from_secs(30)) {
        assert_eq!(e.generation(), 5);
        if e.is_terminal() {
            terminal = Some(e);
            break;
        }
    }
    handle.join();
    assert!(
        matches!(terminal, Some(GrepEvent::Cancelled { .. })),
        "{terminal:?}"
    );
    assert!(
        started.elapsed() < Duration::from_secs(5),
        "took {:?}",
        started.elapsed()
    );
}

#[test]
fn dropping_the_handle_cancels_and_detach_does_not() {
    let t = tempfile::tempdir().unwrap();
    let body = "filler line with the thing\n".repeat(20);
    for f in 0..300 {
        write(t.path(), &format!("f{f}.txt"), &body);
    }
    let run = |detach: bool| {
        let cancel = CancellationToken::new();
        let (tx, rx) = crossbeam_channel::unbounded();
        let gate = Arc::new(std::sync::Barrier::new(2));
        let g = gate.clone();
        let first = std::sync::atomic::AtomicBool::new(true);
        let sink = Arc::new(move |e: GrepEvent| {
            // Hold the pipeline at the first event until the handle has been dropped.
            if first.swap(false, std::sync::atomic::Ordering::SeqCst) {
                g.wait();
            }
            let _ = tx.send(e);
        });
        let mut query = q(t.path(), "thing");
        query.flush_interval = Duration::from_millis(1);
        let handle = spawn_search(query, 1, cancel.clone(), sink).unwrap();
        if detach {
            handle.detach();
        } else {
            drop(handle);
        }
        gate.wait();
        let mut last = None;
        while let Ok(e) = rx.recv_timeout(Duration::from_secs(30)) {
            if e.is_terminal() {
                last = Some(e);
                break;
            }
        }
        (cancel.is_cancelled(), last.expect("terminal event"))
    };
    let (cancelled, ev) = run(false);
    assert!(
        cancelled && matches!(ev, GrepEvent::Cancelled { .. }),
        "{ev:?}"
    );
    let (cancelled, ev) = run(true);
    assert!(!cancelled && matches!(ev, GrepEvent::Done { .. }), "{ev:?}");
}

#[test]
fn bidi_and_control_chars_never_reach_results() {
    let t = tempfile::tempdir().unwrap();
    write(
        t.path(),
        "a.txt",
        "pre \u{202E}needle\u{2066} \u{1b}[31m post\n",
    );
    let o = run(q(t.path(), "needle"));
    let l = &o.files[0].lines[0];
    assert!(
        !l.text.contains(['\u{202E}', '\u{2066}', '\u{1b}']),
        "{:?}",
        l.text
    );
    assert_eq!(&l.text[l.ranges[0].clone()], "needle");
}

#[test]
fn pre_cancelled_search_reports_cancelled() {
    let t = tempfile::tempdir().unwrap();
    write(t.path(), "a.txt", "needle\n");
    let cancel = CancellationToken::new();
    cancel.cancel();
    let o = run_with(q(t.path(), "needle"), 2, cancel);
    assert!(o.cancelled);
    assert!(o.files.is_empty());
}

#[test]
fn batching_flushes_progress() {
    let t = tempfile::tempdir().unwrap();
    for i in 0..30 {
        write(t.path(), &format!("f{i}.txt"), "needle\n");
    }
    let o = run(q(t.path(), "needle"));
    assert!(o
        .events
        .iter()
        .any(|e| matches!(e, GrepEvent::Progress { .. })));
    assert_eq!(o.stats.files_searched, 30);
}

#[test]
fn non_utf8_lines_do_not_break_ranges() {
    let t = tempfile::tempdir().unwrap();
    let mut content = b"caf\xE9 needle\n".to_vec();
    content.extend_from_slice(b"plain needle\n");
    write(t.path(), "latin1.txt", content);
    let o = run(q(t.path(), "needle"));
    let l = &o.files[0].lines[0];
    assert_eq!(&l.text[l.ranges[0].clone()], "needle");
}

#[test]
fn symlink_files_not_searched() {
    let t = tempfile::tempdir().unwrap();
    write(t.path(), "real/a.txt", "needle\n");
    let link = t.path().join("link");
    #[cfg(unix)]
    let made = std::os::unix::fs::symlink(t.path().join("real"), &link).is_ok();
    #[cfg(windows)]
    let made = std::os::windows::fs::symlink_dir(t.path().join("real"), &link).is_ok();
    if !made {
        eprintln!("SKIPPED symlink_files_not_searched: symlink_dir not permitted here");
        return;
    }
    let o = run(q(t.path(), "needle"));
    assert_eq!(rels(&o), vec!["real/a.txt"]);
}

#[cfg(windows)]
#[test]
fn directory_junction_not_followed() {
    let t = tempfile::tempdir().unwrap();
    write(t.path(), "real/a.txt", "needle\n");
    let link = t.path().join("junc");
    let ok = std::process::Command::new("cmd")
        .args(["/C", "mklink", "/J"])
        .arg(&link)
        .arg(t.path().join("real"))
        .output()
        .map(|o| o.status.success())
        .unwrap_or(false);
    assert!(ok, "mklink /J failed");
    assert!(link.join("a.txt").exists());
    let o = run(q(t.path(), "needle"));
    assert_eq!(rels(&o), vec!["real/a.txt"]);
}

/// Sets a Windows attribute flag (for example `+O` = offline) through `attrib`.
#[cfg(windows)]
fn attrib(path: &Path, flag: &str) {
    let ok = std::process::Command::new("attrib")
        .arg(flag)
        .arg(path)
        .output()
        .map(|o| o.status.success())
        .unwrap_or(false);
    assert!(ok, "attrib {flag} failed");
}

#[cfg(windows)]
#[test]
fn cloud_placeholder_files_are_skipped_and_counted() {
    let t = tempfile::tempdir().unwrap();
    write(t.path(), "local.txt", "needle\n");
    write(t.path(), "cloud.txt", "needle\n");
    // FILE_ATTRIBUTE_OFFLINE is one of the placeholder attributes checked by the engine.
    attrib(&t.path().join("cloud.txt"), "+O");
    let o = run(q(t.path(), "needle"));
    assert_eq!(rels(&o), vec!["local.txt"]);
    assert_eq!(o.stats.skipped_cloud, 1);
    assert!(o.error.is_none());
}

#[cfg(windows)]
#[test]
fn cloud_placeholder_dirs_skipped_only_when_requested() {
    let t = tempfile::tempdir().unwrap();
    write(t.path(), "local.txt", "needle\n");
    write(t.path(), "cloud/inner.txt", "needle\n");
    attrib(&t.path().join("cloud"), "+O");
    let o = run(q(t.path(), "needle"));
    assert_eq!(rels(&o), vec!["cloud/inner.txt", "local.txt"]);
    let mut query = q(t.path(), "needle");
    query.skip_cloud_dirs = true;
    let o = run(query);
    assert_eq!(rels(&o), vec!["local.txt"]);
    assert_eq!(o.stats.skipped_cloud, 1);
}

/// Removes a deny ACE again so the temp dir can be cleaned up.
#[cfg(windows)]
struct DenyGuard(std::path::PathBuf, String);

#[cfg(windows)]
impl Drop for DenyGuard {
    fn drop(&mut self) {
        let _ = std::process::Command::new("icacls")
            .arg(&self.0)
            .args(["/remove:d", &self.1])
            .output();
    }
}

#[cfg(windows)]
#[test]
fn access_denied_file_is_counted_quietly() {
    let t = tempfile::tempdir().unwrap();
    write(t.path(), "ok.txt", "needle\n");
    write(t.path(), "secret.txt", "needle\n");
    let user = std::env::var("USERNAME").unwrap_or_default();
    let secret = t.path().join("secret.txt");
    let denied = std::process::Command::new("icacls")
        .arg(&secret)
        .args(["/deny", &format!("{user}:(R)")])
        .output()
        .map(|o| o.status.success())
        .unwrap_or(false);
    if !denied {
        eprintln!("SKIPPED access_denied_file: icacls /deny not possible here");
        return;
    }
    let _guard = DenyGuard(secret.clone(), user);
    if fs::read(&secret).is_ok() {
        eprintln!("SKIPPED access_denied_file: deny ACE not effective (elevated?)");
        return;
    }
    let o = run(q(t.path(), "needle"));
    assert!(o.error.is_none());
    assert_eq!(rels(&o), vec!["ok.txt"]);
    assert_eq!(o.stats.denied, 1, "{:?}", o.stats);
    assert_eq!(o.stats.errors, 0, "{:?}", o.stats);
}
