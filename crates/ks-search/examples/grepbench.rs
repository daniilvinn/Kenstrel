#![allow(clippy::print_stdout, clippy::print_stderr)]
//! Content-search throughput probe, for comparing the engine with `rg`.
//!
//! Usage: `grepbench <root> <pattern> [--regex] [--gen <files>] [--runs <n>] [--threads <n>]`
//!
//! Searches `root` with the same defaults `rg` uses (smart case, gitignore and hidden rules,
//! binary detection) but without the match cap and size limit, and prints the best wall time
//! of `runs` runs together with the match and file counts. `--gen <files>` first writes a
//! synthetic tree of that many files into `root` (see `ks_search::testgen::generate_tree`).

use std::path::PathBuf;
use std::sync::Arc;
use std::time::{Duration, Instant};

use ks_search::{spawn_search, CancellationToken, GrepEvent, GrepQuery};

fn arg_value(args: &[String], name: &str) -> Option<String> {
    args.iter()
        .position(|a| a == name)
        .and_then(|i| args.get(i + 1))
        .cloned()
}

fn main() {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let (Some(root), Some(pattern)) = (args.first(), args.get(1)) else {
        eprintln!("usage: grepbench <root> <pattern> [--regex] [--gen <files>] [--runs <n>]");
        std::process::exit(2);
    };
    let root = PathBuf::from(root);
    if let Some(files) = arg_value(&args, "--gen").and_then(|v| v.parse().ok()) {
        std::fs::create_dir_all(&root).expect("create root");
        let bytes = ks_search::testgen::generate_tree(&root, files, 42).expect("generate");
        println!("generated {files} files, {} MB", bytes / 1_000_000);
    }
    let runs: usize = arg_value(&args, "--runs")
        .and_then(|v| v.parse().ok())
        .unwrap_or(5);
    let mut best = Duration::MAX;
    let mut last = (0u64, 0u64, 0u64);
    for _ in 0..runs {
        let mut query = GrepQuery::new(&root, pattern.clone());
        query.regex = args.iter().any(|a| a == "--regex");
        query.max_matches = usize::MAX;
        query.max_file_size = u64::MAX;
        if let Some(t) = arg_value(&args, "--threads").and_then(|v| v.parse().ok()) {
            query.threads = t;
        }
        let (tx, rx) = crossbeam_channel::unbounded();
        let sink = Arc::new(move |e: GrepEvent| {
            let _ = tx.send(e);
        });
        let started = Instant::now();
        let handle = spawn_search(query, 1, CancellationToken::new(), sink).expect("start");
        let mut matches = 0u64;
        loop {
            match rx.recv_timeout(Duration::from_secs(600)).expect("event") {
                GrepEvent::Done { stats, .. } => {
                    last = (
                        stats.matches,
                        stats.files_with_matches,
                        stats.files_searched,
                    );
                    break;
                }
                GrepEvent::Matches { files, .. } => {
                    matches += files.iter().map(|f| f.lines.len() as u64).sum::<u64>();
                }
                GrepEvent::Error { message, .. } => panic!("{message}"),
                GrepEvent::Cancelled { .. } => panic!("cancelled"),
                GrepEvent::Progress { .. } => {}
            }
        }
        let elapsed = started.elapsed();
        handle.join();
        assert_eq!(matches, last.0, "streamed matches equal the counted ones");
        best = best.min(elapsed);
    }
    println!(
        "ks best_ms={} matches={} files_with_matches={} files_searched={}",
        best.as_millis(),
        last.0,
        last.1,
        last.2
    );
}
