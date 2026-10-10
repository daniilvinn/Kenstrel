//! Benchmarks: walk, fuzzy keystroke latency and grep throughput.
//!
//! Scale with environment variables:
//! * `KS_BENCH_FILES`  files in the generated tree (default 200000)
//! * `KS_BENCH_PATHS`  synthetic paths for the fuzzy bench (default 500000)
//!
//! The tree is generated under the OS temp dir and removed afterwards.

#![allow(clippy::print_stderr)]

use std::sync::Arc;
use std::time::{Duration, Instant};

use criterion::{criterion_group, criterion_main, Criterion, Throughput};
use ks_search::finder::FinderItem;
use ks_search::testgen::{generate_tree, synthetic_paths};
use ks_search::{
    spawn_search, walk, CancellationToken, EntryKind, FinderOptions, FinderSession, GrepEvent,
    GrepQuery, WalkEntry, WalkOptions,
};

fn env_usize(name: &str, default: usize) -> usize {
    std::env::var(name)
        .ok()
        .and_then(|v| v.parse().ok())
        .unwrap_or(default)
}

fn settle(s: &mut FinderSession) {
    loop {
        let st = s.tick(10);
        if !st.busy() {
            return;
        }
    }
}

fn bench_fuzzy(c: &mut Criterion) {
    let n = env_usize("KS_BENCH_PATHS", 500_000);
    let items: Vec<FinderItem> = synthetic_paths(n, 42)
        .into_iter()
        .map(|p| FinderItem::new(p, EntryKind::File))
        .collect();
    let mut s =
        FinderSession::from_items(FinderOptions::new("C:\\bench"), 1, items, Arc::new(|| {}));
    settle(&mut s);

    let mut g = c.benchmark_group(format!("fuzzy_{n}_paths"));
    g.sample_size(30).measurement_time(Duration::from_secs(8));
    // Typing "handler cac" one keystroke at a time; each iteration times the keystroke that
    // extends the previous query (the common case), including fetching the top 30 rows.
    let typed = [
        "h",
        "ha",
        "han",
        "hand",
        "handl",
        "handle",
        "handler",
        "handler c",
        "handler ca",
        "handler cac",
    ];
    for (i, q) in typed.iter().enumerate() {
        let prev = if i == 0 { "" } else { typed[i - 1] };
        g.bench_function(format!("keystroke_{}_{q}", i + 1), |b| {
            b.iter_custom(|iters| {
                let mut total = Duration::ZERO;
                for _ in 0..iters {
                    s.set_query(prev);
                    settle(&mut s);
                    let t = Instant::now();
                    s.set_query(q);
                    settle(&mut s);
                    let rows = s.results(0, 30);
                    total += t.elapsed();
                    std::hint::black_box(rows);
                }
                total
            })
        });
    }
    g.bench_function("backspace_to_empty", |b| {
        b.iter_custom(|iters| {
            let mut total = Duration::ZERO;
            for _ in 0..iters {
                s.set_query("handler");
                settle(&mut s);
                let t = Instant::now();
                s.set_query("");
                settle(&mut s);
                std::hint::black_box(s.results(0, 30));
                total += t.elapsed();
            }
            total
        })
    });
    g.finish();
}

fn bench_tree(c: &mut Criterion) {
    let files = env_usize("KS_BENCH_FILES", 200_000);
    let dir = tempfile::tempdir().expect("tempdir");
    let t = Instant::now();
    let bytes = generate_tree(dir.path(), files, 1).expect("generate tree");
    eprintln!(
        "generated {files} files, {:.1} MB in {:.1?}",
        bytes as f64 / 1e6,
        t.elapsed()
    );

    let mut g = c.benchmark_group(format!("tree_{files}_files"));
    g.sample_size(10).measurement_time(Duration::from_secs(15));
    g.bench_function("walk", |b| {
        b.iter(|| {
            let count = std::sync::atomic::AtomicU64::new(0);
            let opts = WalkOptions::new(dir.path());
            walk(&opts, &CancellationToken::new(), || {
                let count = &count;
                move |e: WalkEntry| {
                    std::hint::black_box(e.path());
                    count.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
                    true
                }
            });
            assert!(count.into_inner() >= files as u64);
        })
    });

    g.throughput(Throughput::Bytes(bytes));
    for (name, pattern, regex) in [
        ("grep_rare_literal", "NEEDLE_MARKER", false),
        ("grep_no_match_literal", "zzzqqq_absent", false),
        ("grep_common_word_regex", r"hand\w+ cache", true),
    ] {
        g.bench_function(name, |b| {
            b.iter(|| {
                let mut q = GrepQuery::new(dir.path(), pattern);
                q.regex = regex;
                q.max_matches = usize::MAX;
                let (tx, rx) = crossbeam_channel::unbounded();
                let h = spawn_search(
                    q,
                    1,
                    CancellationToken::new(),
                    Arc::new(move |e: GrepEvent| {
                        let _ = tx.send(e);
                    }),
                )
                .expect("spawn");
                let mut matches = 0;
                while let Ok(e) = rx.recv() {
                    match e {
                        GrepEvent::Done { stats, .. } => {
                            matches = stats.matches;
                            break;
                        }
                        e if e.is_terminal() => panic!("unexpected {e:?}"),
                        _ => {}
                    }
                }
                h.join();
                std::hint::black_box(matches)
            })
        });
    }
    g.finish();
}

criterion_group!(benches, bench_fuzzy, bench_tree);
criterion_main!(benches);
