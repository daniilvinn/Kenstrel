//! Throughput measurement, ignored by default:
//! `cargo test -p ks-fs --release --test ops_bench -- --ignored --nocapture`
//! Env: `KS_BENCH_DIR` (directory, e.g. on another volume), `KS_BENCH_MIB` (size, default 1024).
//! The source is freshly written, so the read side is served from the page cache; a cold-read
//! variant needs a cache drop or a separate volume and is not automated.

#[path = "ops_common.rs"]
mod common;

use std::fs;
use std::io::Write;
use std::time::Instant;

use common::*;
use ks_fs::ops::JobSpec;

#[test]
#[ignore = "copies ~1 GiB; run manually"]
#[allow(clippy::print_stderr)]
fn copy_one_gib_throughput() {
    let h = Harness::new();
    // KS_BENCH_DIR picks the volume (default: the system temp dir); KS_BENCH_MIB the size.
    let base = std::env::var_os("KS_BENCH_DIR")
        .map(std::path::PathBuf::from)
        .unwrap_or_else(std::env::temp_dir);
    let mib: usize = std::env::var("KS_BENCH_MIB")
        .ok()
        .and_then(|v| v.parse().ok())
        .unwrap_or(1024);
    let td = tempfile::tempdir_in(base).unwrap();
    let src = td.path().join("big.bin");
    let dst = td.path().join("out");
    fs::create_dir_all(&dst).unwrap();
    {
        let mut f = fs::File::create(&src).unwrap();
        // Pseudo-random, incompressible content so no filesystem trick short-circuits the copy.
        let mut chunk = vec![0u8; 1024 * 1024];
        let mut x: u64 = 0x9E37_79B9_7F4A_7C15;
        for _ in 0..mib {
            for b in chunk.iter_mut() {
                x ^= x << 13;
                x ^= x >> 7;
                x ^= x << 17;
                *b = x as u8;
            }
            f.write_all(&chunk).unwrap();
        }
        f.sync_all().unwrap();
    }
    let bytes = fs::metadata(&src).unwrap().len();
    let t = Instant::now();
    let run = h.run(JobSpec::copy_into(std::slice::from_ref(&src), &dst));
    let secs = t.elapsed().as_secs_f64();
    run.assert_done();
    assert_eq!(fs::metadata(dst.join("big.bin")).unwrap().len(), bytes);
    eprintln!(
        "COPY_1GIB: {:.2} s, {:.0} MiB/s ({} MiB, chunk 4 MiB)",
        secs,
        bytes as f64 / 1048576.0 / secs,
        bytes / 1048576
    );
}
