//! Drives a program (ks.exe) in a ConPTY from a script and dumps vt100 screens as JSON.
//!
//! Script lines: `size C R` | `env K=V` | `unenv PREFIX` | `cwd DIR` | `start EXE [ARGS..]` |
//! `wait MS` | `send TEXT` (escapes: \e \r \t \b(=DEL) \n \s ^X) | `type TEXT [MS]` |
//! `shot NAME` | `rec NAME` / `endrec` | `quit`. Lines starting with # are comments.

use std::io::{Read, Write};
use std::path::PathBuf;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use portable_pty::{native_pty_system, Child, CommandBuilder, PtySize};
use serde_json::{json, Value};

type Writer = Arc<Mutex<Box<dyn Write + Send>>>;

struct Session {
    parser: Arc<Mutex<vt100::Parser>>,
    writer: Writer,
    child: Box<dyn Child + Send + Sync>,
    version: Arc<Mutex<u64>>,
}

struct Rec {
    name: String,
    start: Instant,
    frames: Vec<Value>,
    last: u64,
}

fn color(c: vt100::Color) -> Value {
    match c {
        vt100::Color::Default => Value::Null,
        vt100::Color::Idx(i) => json!(i),
        vt100::Color::Rgb(r, g, b) => json!(format!("#{r:02x}{g:02x}{b:02x}")),
    }
}

fn frame(screen: &vt100::Screen) -> Value {
    let (rows, cols) = screen.size();
    let mut cells = Vec::new();
    for r in 0..rows {
        let mut row = Vec::new();
        for c in 0..cols {
            let cell = screen.cell(r, c).unwrap();
            let flags = (cell.bold() as u8)
                | (cell.italic() as u8) << 1
                | (cell.underline() as u8) << 2
                | (cell.inverse() as u8) << 3
                | (cell.dim() as u8) << 4
                | (cell.is_wide() as u8) << 5
                | (cell.is_wide_continuation() as u8) << 6;
            row.push(json!([
                cell.contents(),
                color(cell.fgcolor()),
                color(cell.bgcolor()),
                flags
            ]));
        }
        cells.push(Value::Array(row));
    }
    let (cr, cc) = screen.cursor_position();
    json!({"rows": rows, "cols": cols, "cursor": [cr, cc], "hide_cursor": screen.hide_cursor(), "cells": cells})
}

fn unescape(s: &str) -> Vec<u8> {
    let mut out = Vec::new();
    let mut it = s.chars().peekable();
    while let Some(ch) = it.next() {
        match ch {
            '\\' => match it.next() {
                Some('e') => out.push(0x1b),
                Some('r') => out.push(b'\r'),
                Some('n') => out.push(b'\n'),
                Some('t') => out.push(b'\t'),
                Some('b') => out.push(0x7f),
                Some('s') => out.push(b' '),
                Some('\\') => out.push(b'\\'),
                Some(o) => {
                    out.push(b'\\');
                    out.extend(o.to_string().as_bytes());
                }
                None => out.push(b'\\'),
            },
            '^' if it
                .peek()
                .is_some_and(|c| c.is_ascii_alphabetic() || *c == '\\') =>
            {
                let c = it.next().unwrap();
                out.push((c.to_ascii_uppercase() as u8) & 0x1f);
            }
            _ => {
                let mut b = [0; 4];
                out.extend(ch.encode_utf8(&mut b).as_bytes());
            }
        }
    }
    out
}

fn poll(rec: &mut Option<Rec>, s: &Option<Session>) {
    let (Some(rec), Some(s)) = (rec.as_mut(), s.as_ref()) else {
        return;
    };
    let v = *s.version.lock().unwrap();
    if v != rec.last {
        rec.last = v;
        let mut f = frame(s.parser.lock().unwrap().screen());
        f["t"] = json!(rec.start.elapsed().as_millis() as u64);
        rec.frames.push(f);
    }
}

/// Sleeps `ms`, sampling the screen every 20 ms while recording.
fn pause(ms: u64, rec: &mut Option<Rec>, s: &Option<Session>) {
    let end = Instant::now() + Duration::from_millis(ms);
    loop {
        poll(rec, s);
        let now = Instant::now();
        if now >= end {
            break;
        }
        std::thread::sleep((end - now).min(Duration::from_millis(20)));
    }
}

fn write(s: &Option<Session>, bytes: &[u8]) {
    let s = s.as_ref().expect("no session");
    let mut w = s.writer.lock().unwrap();
    w.write_all(bytes).unwrap();
    w.flush().unwrap();
}

fn main() {
    let args: Vec<String> = std::env::args().collect();
    // `{ks}` and `{demo}` stand for $KS_EXE and $KS_DEMO, so the scripts are machine-neutral.
    let script = std::fs::read_to_string(&args[1])
        .expect("script")
        .replace(
            "{ks}",
            &std::env::var("KS_EXE").expect("KS_EXE: path of ks.exe"),
        )
        .replace(
            "{demo}",
            &std::env::var("KS_DEMO").expect("KS_DEMO: demo root"),
        );
    let out_dir = PathBuf::from(&args[2]);
    std::fs::create_dir_all(&out_dir).unwrap();

    let (mut cols, mut rows) = (120u16, 32u16);
    let mut env: Vec<(String, String)> = Vec::new();
    let mut unenv: Vec<String> = Vec::new();
    let mut cwd = std::env::current_dir().unwrap();
    let mut session: Option<Session> = None;
    let mut rec: Option<Rec> = None;

    for line in script.lines() {
        let line = line.trim_end();
        if line.trim().is_empty() || line.trim_start().starts_with('#') {
            continue;
        }
        let (cmd, rest) = line.split_once(' ').unwrap_or((line, ""));
        match cmd {
            "size" => {
                let v: Vec<u16> = rest
                    .split_whitespace()
                    .map(|x| x.parse().unwrap())
                    .collect();
                cols = v[0];
                rows = v[1];
            }
            "env" => {
                let (k, v) = rest.split_once('=').unwrap();
                env.push((k.into(), v.into()));
            }
            "unenv" => unenv.push(rest.into()),
            "cwd" => cwd = PathBuf::from(rest),
            "start" => {
                let mut parts = rest.split_whitespace();
                let exe = parts.next().unwrap();
                let pair = native_pty_system()
                    .openpty(PtySize {
                        rows,
                        cols,
                        pixel_width: 0,
                        pixel_height: 0,
                    })
                    .unwrap();
                let mut cmd = CommandBuilder::new(exe);
                for a in parts {
                    cmd.arg(a);
                }
                cmd.cwd(&cwd);
                for (k, _) in std::env::vars() {
                    if unenv.iter().any(|p| k.starts_with(p.as_str())) {
                        cmd.env_remove(&k);
                    }
                }
                for (k, v) in &env {
                    cmd.env(k, v);
                }
                let child = pair.slave.spawn_command(cmd).unwrap();
                drop(pair.slave);
                let mut reader = pair.master.try_clone_reader().unwrap();
                let writer: Writer = Arc::new(Mutex::new(pair.master.take_writer().unwrap()));
                std::mem::forget(pair.master);
                let parser = Arc::new(Mutex::new(vt100::Parser::new(rows, cols, 0)));
                let version = Arc::new(Mutex::new(0u64));
                {
                    let (parser, writer, version) =
                        (parser.clone(), writer.clone(), version.clone());
                    std::thread::spawn(move || {
                        let mut buf = [0u8; 65536];
                        while let Ok(n) = reader.read(&mut buf) {
                            if n == 0 {
                                break;
                            }
                            let chunk = &buf[..n];
                            let mut p = parser.lock().unwrap();
                            p.process(chunk);
                            // Answer the queries a real terminal answers, so nothing waits.
                            let text = String::from_utf8_lossy(chunk);
                            let reply = |b: &[u8]| {
                                let _ = writer.lock().unwrap().write_all(b);
                            };
                            if text.contains("\x1b[6n") {
                                let (r, c) = p.screen().cursor_position();
                                reply(format!("\x1b[{};{}R", r + 1, c + 1).as_bytes());
                            }
                            if text.contains("\x1b]10;?") {
                                reply(b"\x1b]10;rgb:e8e8/e3e3/dcdc\x1b\\");
                            }
                            if text.contains("\x1b]11;?") {
                                reply(b"\x1b]11;rgb:1717/1515/1414\x1b\\");
                            }
                            *version.lock().unwrap() += 1;
                        }
                    });
                }
                session = Some(Session {
                    parser,
                    writer,
                    child,
                    version,
                });
            }
            "wait" => pause(rest.trim().parse().unwrap(), &mut rec, &session),
            "send" => write(&session, &unescape(rest)),
            "type" => {
                let (text, ms) = match rest.rsplit_once(' ') {
                    Some((t, m)) if m.parse::<u64>().is_ok() => (t.to_string(), m.parse().unwrap()),
                    _ => (rest.to_string(), 90),
                };
                for ch in text.chars() {
                    let mut b = [0; 4];
                    write(&session, ch.encode_utf8(&mut b).as_bytes());
                    pause(ms, &mut rec, &session);
                }
            }
            "shot" => {
                let s = session.as_ref().unwrap();
                let f = frame(s.parser.lock().unwrap().screen());
                std::fs::write(out_dir.join(format!("{}.json", rest.trim())), f.to_string())
                    .unwrap();
                eprintln!("shot {}", rest.trim());
            }
            "rec" => {
                rec = Some(Rec {
                    name: rest.trim().into(),
                    start: Instant::now(),
                    frames: Vec::new(),
                    last: u64::MAX,
                });
                poll(&mut rec, &session);
            }
            "endrec" => {
                if let Some(mut r) = rec.take() {
                    let s = session.as_ref().unwrap();
                    let mut f = frame(s.parser.lock().unwrap().screen());
                    f["t"] = json!(r.start.elapsed().as_millis() as u64);
                    r.frames.push(f);
                    eprintln!("rec {}: {} frames", r.name, r.frames.len());
                    std::fs::write(
                        out_dir.join(format!("{}.rec.json", r.name)),
                        Value::Array(r.frames).to_string(),
                    )
                    .unwrap();
                }
            }
            "quit" => {
                if let Some(mut s) = session.take() {
                    let _ = s.child.kill();
                }
            }
            other => panic!("unknown command {other}"),
        }
    }
    if let Some(mut s) = session.take() {
        let _ = s.child.kill();
    }
}
