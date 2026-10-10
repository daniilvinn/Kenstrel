//! 10b spike: run `claude` in a ConPTY for a few seconds, feed vt100, dump screen + sequences.
//! Sends NO input to claude. Usage: cargo run -p ks-claude --example pty_spike [secs]
#![allow(clippy::print_stdout, clippy::print_stderr)]
use std::io::Read;
use std::sync::mpsc;
use std::time::{Duration, Instant};

use portable_pty::{native_pty_system, CommandBuilder, PtySize};

fn main() {
    let secs: u64 = std::env::args()
        .nth(1)
        .and_then(|s| s.parse().ok())
        .unwrap_or(7);
    let dir = tempfile::tempdir().expect("tempdir");
    let pty = native_pty_system()
        .openpty(PtySize {
            rows: 40,
            cols: 120,
            pixel_width: 0,
            pixel_height: 0,
        })
        .expect("openpty");
    let mut cmd = CommandBuilder::new("claude");
    for v in ks_claude::DEFAULT_STRIP_ENV {
        cmd.env_remove(v);
    }
    match std::env::args().nth(2) {
        Some(d) => cmd.cwd(d),
        None => cmd.cwd(dir.path()),
    }
    let mut child = pty.slave.spawn_command(cmd).expect("spawn");
    drop(pty.slave);
    let mut reader = pty.master.try_clone_reader().expect("reader");
    let mut writer = pty.master.take_writer().expect("writer");
    let (tx, rx) = mpsc::channel::<Vec<u8>>();
    std::thread::spawn(move || {
        let mut buf = [0u8; 8192];
        while let Ok(n) = reader.read(&mut buf) {
            if n == 0 || tx.send(buf[..n].to_vec()).is_err() {
                break;
            }
        }
    });
    let mut raw = Vec::new();
    let end = Instant::now() + Duration::from_secs(secs);
    while Instant::now() < end {
        if let Ok(b) = rx.recv_timeout(Duration::from_millis(100)) {
            // ConPTY (and claude) issue CSI 6n and wait for a reply: answer it like a real terminal.
            for _ in b.windows(4).filter(|w| *w == b"[6n") {
                use std::io::Write;
                let _ = writer.write_all(b"[1;1R");
                let _ = writer.flush();
            }
            raw.extend(b);
        }
    }
    let _ = child.kill();
    let mut p = vt100::Parser::new(40, 120, 0);
    p.process(&raw);
    println!("raw bytes: {}", raw.len());
    println!(
        "---- screen (alt={}, cursor={:?}, hidden={}) ----",
        p.screen().alternate_screen(),
        p.screen().cursor_position(),
        p.screen().hide_cursor()
    );
    println!("{}", p.screen().contents());
    println!("---- sequences ----");
    let s = String::from_utf8_lossy(&raw);
    for (name, pat) in [
        ("alt screen ?1049h", "\x1b[?1049h"),
        ("sync output ?2026h", "\x1b[?2026h"),
        ("bracketed paste ?2004h", "\x1b[?2004h"),
        ("focus events ?1004h", "\x1b[?1004h"),
        ("truecolor fg 38;2", "38;2;"),
        ("256 fg 38;5", "38;5;"),
        ("kitty kbd query CSI ? u", "\x1b[?u"),
        ("kitty kbd push CSI > u", "\x1b[>1u"),
        ("modifyOtherKeys CSI > 4 ; 2 m", "\x1b[>4;2m"),
        ("DA1 query CSI c", "\x1b[c"),
        ("DA2 CSI > c", "\x1b[>c"),
        ("DSR CSI 6 n", "\x1b[6n"),
        ("XTVERSION CSI > q", "\x1b[>0q"),
        ("OSC 0/2 title", "\x1b]0;"),
        ("OSC 8 hyperlink", "\x1b]8;"),
        ("OSC 9 / 777 notify", "\x1b]9;"),
        ("OSC 10/11 colour query", "\x1b]11;?"),
        ("mouse ?1000/?1006", "\x1b[?1006h"),
        ("cursor shape DECSCUSR", " q"),
        ("hide cursor ?25l", "\x1b[?25l"),
    ] {
        println!("{:40} {}", name, s.matches(pat).count());
    }
    let mut oscs = std::collections::BTreeSet::new();
    let b = &raw;
    let mut i = 0;
    while i + 2 < b.len() {
        if b[i] == 0x1b && b[i + 1] == b']' {
            let j = (i + 2..b.len().min(i + 60))
                .find(|&k| b[k] == 7 || b[k] == 0x1b)
                .unwrap_or(b.len().min(i + 22));
            oscs.insert(
                String::from_utf8_lossy(&b[i + 2..j])
                    .chars()
                    .take(30)
                    .collect::<String>(),
            );
        }
        i += 1;
    }
    println!("OSC samples: {oscs:?}");
    let mut privs = std::collections::BTreeSet::new();
    for m in s.match_indices("\x1b[?") {
        let rest = &s[m.0 + 3..];
        let e = rest
            .find(|c: char| c.is_ascii_alphabetic())
            .map_or(rest.len().min(8), |e| e + 1);
        privs.insert(rest[..e].to_string());
    }
    println!("private modes: {privs:?}");
    // cell colour sample
    let sc = p.screen();
    let mut tc = 0;
    for r in 0..40 {
        for c in 0..120 {
            if let Some(cell) = sc.cell(r, c) {
                if matches!(cell.fgcolor(), vt100::Color::Rgb(..))
                    || matches!(cell.bgcolor(), vt100::Color::Rgb(..))
                {
                    tc += 1;
                }
            }
        }
    }
    println!("cells with rgb colours in vt100 screen: {tc}");
    std::fs::write(std::env::temp_dir().join("claude_spike_raw.bin"), &raw).ok();
}
