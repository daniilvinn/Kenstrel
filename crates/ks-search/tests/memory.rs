//! Memory budget guard (a 1M-path finder cache plus an open session stays under
//! 250 MB peak working set). The workspace forbids `unsafe`, so the measurement is taken from
//! outside: the `mem` example fills the cache, opens a session and idles; this test reads the
//! peak working set of that process with PowerShell. Windows only.
//! Run: `cargo test -p ks-search --release --test memory -- --ignored --nocapture`.
#![allow(clippy::print_stdout)]
#![cfg(windows)]

use std::io::{BufRead, BufReader};
use std::process::{Command, Stdio};

#[test]
#[ignore = "memory guard; run in release mode"]
fn finder_cache_1m_paths_under_250mb() {
    // target/<profile>/deps/memory-xxxx.exe -> target/<profile>/examples/mem.exe
    let exe = std::env::current_exe().unwrap();
    let profile_dir = exe.parent().unwrap().parent().unwrap();
    let mem = profile_dir.join("examples").join("mem.exe");
    assert!(mem.exists(), "build the example first: {}", mem.display());
    let mut child = Command::new(&mem)
        .arg("1000000")
        .stdout(Stdio::piped())
        .spawn()
        .unwrap();
    let mut lines = BufReader::new(child.stdout.take().unwrap()).lines();
    while let Some(Ok(line)) = lines.next() {
        if line == "READY" {
            break;
        }
    }
    let out = Command::new("powershell")
        .args([
            "-NoProfile",
            "-Command",
            &format!("(Get-Process -Id {}).PeakWorkingSet64", child.id()),
        ])
        .output()
        .unwrap();
    let _ = child.kill();
    let _ = child.wait();
    let peak: u64 = String::from_utf8_lossy(&out.stdout).trim().parse().unwrap();
    let mb = peak / (1024 * 1024);
    println!("peak working set: {mb} MB");
    assert!(
        mb < 250,
        "peak working set {mb} MB exceeds the 250 MB budget"
    );
}
