//! The `!` shell command runner: run a command string in a directory through the user's shell,
//! stream stdout/stderr lines as events, report the exit code, support cancel (kill the tree).
//!
//! Ids: [`ShellId`] is caller-chosen and separate from `JobId`; shell runs are not listed by
//! `JobManager::jobs()` nor cancelled by `cancel_all`, so wiring must keep its own table (and
//! keep ids distinct from job ids in a shared message space). Line text is sanitized (no control
//! or escape sequences) and capped at [`MAX_LINE_BYTES`]; no `Line` follows the terminal event.
//!
//! Threading: [`spawn_shell`] starts one supervisor thread plus two reader threads and returns
//! immediately. Events go to the sink from those threads. For commands that need a real TTY
//! (pagers, editors) the app should instead leave the alternate screen and use
//! [`super::editor::run_foreground`].

use std::io::{BufRead, BufReader, Read};
use std::path::PathBuf;
use std::process::{Child, Command, Stdio};
use std::sync::{Arc, Mutex};
use std::thread::JoinHandle;
use std::time::{Duration, Instant};

use tokio_util::sync::CancellationToken;

use super::editor::find_in_path;
use crate::ops::{EventSink, Generation};

/// Identifier chosen by the caller, echoed in events.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct ShellId(pub u64);

/// Which stream a line came from.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ShellStream {
    /// Standard output.
    Stdout,
    /// Standard error.
    Stderr,
}

/// Events of a shell run. Exactly one of `Exited`, `Cancelled` or `SpawnFailed` ends a run.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ShellEvent {
    /// One output line (without the line terminator, lossy UTF-8).
    Line {
        /// Run id.
        id: ShellId,
        /// Generation.
        generation: Generation,
        /// Origin.
        stream: ShellStream,
        /// Text.
        text: String,
    },
    /// The process ended. `code` is `None` if it was terminated by a signal.
    Exited {
        /// Run id.
        id: ShellId,
        /// Generation.
        generation: Generation,
        /// Exit code.
        code: Option<i32>,
    },
    /// Killed because of [`ShellHandle::cancel`].
    Cancelled {
        /// Run id.
        id: ShellId,
        /// Generation.
        generation: Generation,
    },
    /// The shell could not be started.
    SpawnFailed {
        /// Run id.
        id: ShellId,
        /// Generation.
        generation: Generation,
        /// Error text.
        error: String,
    },
}

/// The shell used to interpret command strings.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ShellProgram {
    /// Executable.
    pub program: String,
    /// Arguments placed before the command string.
    pub args: Vec<String>,
    /// Text prepended to the command (forces UTF-8 output in PowerShell).
    pub prefix: String,
}

/// Windows: `pwsh` if on `PATH`, else `powershell -NoProfile -Command`; elsewhere `sh -c`.
pub fn resolve_shell() -> ShellProgram {
    if cfg!(windows) {
        let program = find_in_path("pwsh")
            .map(|p| p.to_string_lossy().into_owned())
            .unwrap_or_else(|| "powershell.exe".to_owned());
        ShellProgram {
            program,
            args: ["-NoLogo", "-NoProfile", "-NonInteractive", "-Command"]
                .map(String::from)
                .to_vec(),
            prefix: "[Console]::OutputEncoding = [System.Text.UTF8Encoding]::new($false); ".into(),
        }
    } else {
        ShellProgram {
            program: "sh".into(),
            args: vec!["-c".into()],
            prefix: String::new(),
        }
    }
}

/// What to run.
#[derive(Debug, Clone)]
pub struct ShellRequest {
    /// Caller id.
    pub id: ShellId,
    /// Caller generation.
    pub generation: Generation,
    /// The command string, interpreted by the shell.
    pub command: String,
    /// Working directory.
    pub cwd: PathBuf,
    /// Shell override (`None` = [`resolve_shell`]).
    pub shell: Option<ShellProgram>,
}

/// Handle to a running command.
pub struct ShellHandle {
    token: CancellationToken,
    join: JoinHandle<()>,
}

impl ShellHandle {
    /// Kill the process tree. The run ends with [`ShellEvent::Cancelled`].
    pub fn cancel(&self) {
        self.token.cancel();
    }

    /// Whether the supervisor thread has finished (the terminal event was sent).
    pub fn is_finished(&self) -> bool {
        self.join.is_finished()
    }

    /// Wait for the supervisor thread. For tests and shutdown.
    pub fn join(self) {
        let _ = self.join.join();
    }
}

/// Longest line (in bytes) delivered in one [`ShellEvent::Line`]; the rest of a longer line is
/// dropped and the text ends with an ellipsis.
pub const MAX_LINE_BYTES: usize = 64 * 1024;

/// Strip terminal control sequences and control characters from one line of child output so it
/// is safe to render or print. CSI and OSC/DCS-style sequences are removed whole; tabs become a
/// single space; every other control character (including C1 and lone `\r`) is dropped.
pub fn sanitize_line(input: &str) -> String {
    let mut out = String::with_capacity(input.len());
    let mut it = input.chars();
    while let Some(c) = it.next() {
        match c {
            '\u{1b}' => match it.next() {
                Some('[') => {
                    // CSI: parameters/intermediates, then a final byte 0x40..=0x7e.
                    for n in it.by_ref() {
                        if ('\u{40}'..='\u{7e}').contains(&n) {
                            break;
                        }
                    }
                }
                Some(']' | 'P' | 'X' | '^' | '_') => {
                    // OSC/DCS/SOS/PM/APC: until BEL or ST (ESC \).
                    while let Some(n) = it.next() {
                        if n == '\u{7}' {
                            break;
                        }
                        if n == '\u{1b}' {
                            it.next();
                            break;
                        }
                    }
                }
                _ => {} // two-character escape
            },
            '\t' => out.push(' '),
            c if c.is_control() => {}
            c => out.push(c),
        }
    }
    out
}

/// Read one line of at most `MAX_LINE_BYTES` into `buf`, discarding the excess. Returns false
/// at end of stream with nothing read.
fn read_bounded_line<R: BufRead>(r: &mut R, buf: &mut Vec<u8>) -> std::io::Result<bool> {
    buf.clear();
    let mut any = false;
    let mut truncated = false;
    loop {
        let chunk = r.fill_buf()?;
        if chunk.is_empty() {
            break;
        }
        any = true;
        let (take, found) = match chunk.iter().position(|b| *b == b'\n') {
            Some(i) => (i + 1, true),
            None => (chunk.len(), false),
        };
        let content = if found { take - 1 } else { take };
        let room = MAX_LINE_BYTES.saturating_sub(buf.len());
        let keep = content.min(room);
        buf.extend_from_slice(&chunk[..keep]);
        if keep < content {
            truncated = true;
        }
        r.consume(take);
        if found {
            break;
        }
    }
    if truncated {
        buf.extend_from_slice("\u{2026}".as_bytes());
    }
    Ok(any)
}

fn read_lines<R: Read + Send + 'static>(
    reader: R,
    stream: ShellStream,
    id: ShellId,
    generation: Generation,
    sink: Arc<dyn EventSink<ShellEvent>>,
    closed: Arc<Mutex<bool>>,
) -> JoinHandle<()> {
    std::thread::spawn(move || {
        let mut r = BufReader::new(reader);
        let mut buf = Vec::new();
        loop {
            match read_bounded_line(&mut r, &mut buf) {
                Ok(false) | Err(_) => break,
                Ok(true) => {
                    while matches!(buf.last(), Some(b'\n' | b'\r')) {
                        buf.pop();
                    }
                    let text = sanitize_line(&String::from_utf8_lossy(&buf));
                    let guard = closed.lock().unwrap_or_else(|p| p.into_inner());
                    if *guard {
                        break; // the run already ended; never emit after the terminal event
                    }
                    sink.send(ShellEvent::Line {
                        id,
                        generation,
                        stream,
                        text,
                    });
                    drop(guard);
                }
            }
        }
    })
}

fn kill_tree(child: &mut Child) {
    #[cfg(windows)]
    {
        use std::os::windows::process::CommandExt;
        let _ = Command::new("taskkill")
            .args(["/PID", &child.id().to_string(), "/T", "/F"])
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .creation_flags(0x0800_0000)
            .status();
    }
    let _ = child.kill();
}

/// Start `request`. Returns immediately; events arrive on `sink`.
pub fn spawn_shell(request: ShellRequest, sink: impl EventSink<ShellEvent>) -> ShellHandle {
    let sink: Arc<dyn EventSink<ShellEvent>> = Arc::new(sink);
    let token = CancellationToken::new();
    let tok = token.clone();
    let join = std::thread::spawn(move || supervise(request, sink, tok));
    ShellHandle { token, join }
}

fn supervise(req: ShellRequest, sink: Arc<dyn EventSink<ShellEvent>>, token: CancellationToken) {
    let (id, generation) = (req.id, req.generation);
    let shell = req.shell.clone().unwrap_or_else(resolve_shell);
    let mut cmd = Command::new(&shell.program);
    cmd.args(&shell.args)
        .arg(format!("{}{}", shell.prefix, req.command))
        .current_dir(&req.cwd)
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    #[cfg(windows)]
    {
        use std::os::windows::process::CommandExt;
        cmd.creation_flags(0x0800_0000); // CREATE_NO_WINDOW
    }
    let mut child = match cmd.spawn() {
        Ok(c) => c,
        Err(e) => {
            tracing::warn!(error = %e, "shell spawn failed");
            sink.send(ShellEvent::SpawnFailed {
                id,
                generation,
                error: e.to_string(),
            });
            return;
        }
    };
    let closed = Arc::new(Mutex::new(false));
    let mut readers = Vec::new();
    if let Some(out) = child.stdout.take() {
        readers.push(read_lines(
            out,
            ShellStream::Stdout,
            id,
            generation,
            Arc::clone(&sink),
            Arc::clone(&closed),
        ));
    }
    if let Some(err) = child.stderr.take() {
        readers.push(read_lines(
            err,
            ShellStream::Stderr,
            id,
            generation,
            Arc::clone(&sink),
            Arc::clone(&closed),
        ));
    }
    let mut cancelled = false;
    let code = loop {
        match child.try_wait() {
            Ok(Some(status)) => break status.code(),
            Ok(None) => {
                if token.is_cancelled() {
                    kill_tree(&mut child);
                    let _ = child.wait();
                    cancelled = true;
                    break None;
                }
                std::thread::sleep(Duration::from_millis(10));
            }
            Err(e) => {
                tracing::warn!(error = %e, "waiting for shell failed");
                break None;
            }
        }
    };
    // Let the readers drain, but do not hang on grandchildren that keep the pipes open.
    let deadline = Instant::now() + Duration::from_millis(1500);
    for r in readers {
        while !r.is_finished() && Instant::now() < deadline {
            std::thread::sleep(Duration::from_millis(5));
        }
        if r.is_finished() {
            let _ = r.join();
        }
    }
    // Close the gate and send the terminal event atomically: no Line can follow it.
    let mut gate = closed.lock().unwrap_or_else(|p| p.into_inner());
    *gate = true;
    sink.send(if cancelled {
        ShellEvent::Cancelled { id, generation }
    } else {
        ShellEvent::Exited {
            id,
            generation,
            code,
        }
    });
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::mpsc::channel;

    fn run(cmd: &str) -> Vec<ShellEvent> {
        let (tx, rx) = channel();
        let h = spawn_shell(
            ShellRequest {
                id: ShellId(1),
                generation: Generation(7),
                command: cmd.to_owned(),
                cwd: std::env::temp_dir(),
                shell: None,
            },
            tx,
        );
        h.join();
        rx.try_iter().collect()
    }

    #[test]
    fn captures_both_streams_and_exit_code() {
        let cmd = if cfg!(windows) {
            "Write-Output one; [Console]::Error.WriteLine('two'); Write-Output 'ünï'; exit 3"
        } else {
            "echo one; echo two >&2; echo ünï; exit 3"
        };
        let ev = run(cmd);
        let lines: Vec<(ShellStream, String)> = ev
            .iter()
            .filter_map(|e| match e {
                ShellEvent::Line { stream, text, .. } => Some((*stream, text.clone())),
                _ => None,
            })
            .collect();
        assert!(
            lines.contains(&(ShellStream::Stdout, "one".into())),
            "{lines:?}"
        );
        assert!(
            lines.contains(&(ShellStream::Stderr, "two".into())),
            "{lines:?}"
        );
        assert!(
            lines.contains(&(ShellStream::Stdout, "ünï".into())),
            "{lines:?}"
        );
        assert_eq!(
            ev.last(),
            Some(&ShellEvent::Exited {
                id: ShellId(1),
                generation: Generation(7),
                code: Some(3)
            })
        );
    }

    #[test]
    fn runs_in_requested_dir() {
        let cmd = if cfg!(windows) {
            "(Get-Location).Path"
        } else {
            "pwd"
        };
        let ev = run(cmd);
        let cwd = dunce::canonicalize(std::env::temp_dir()).unwrap();
        let got = ev.iter().find_map(|e| match e {
            ShellEvent::Line { text, .. } => Some(text.clone()),
            _ => None,
        });
        let got = dunce::canonicalize(got.unwrap()).unwrap();
        assert_eq!(got, cwd);
    }

    #[test]
    fn cancel_kills() {
        let (tx, rx) = channel();
        let cmd = if cfg!(windows) {
            "Start-Sleep -Seconds 30"
        } else {
            "sleep 30"
        };
        let started = Instant::now();
        let h = spawn_shell(
            ShellRequest {
                id: ShellId(2),
                generation: Generation(0),
                command: cmd.into(),
                cwd: std::env::temp_dir(),
                shell: None,
            },
            tx,
        );
        h.cancel();
        h.join();
        let ev: Vec<_> = rx.try_iter().collect();
        assert_eq!(
            ev.last(),
            Some(&ShellEvent::Cancelled {
                id: ShellId(2),
                generation: Generation(0)
            })
        );
        assert!(started.elapsed() < Duration::from_secs(10));
    }

    #[test]
    fn spawn_failure_is_an_event() {
        let (tx, rx) = channel();
        let h = spawn_shell(
            ShellRequest {
                id: ShellId(3),
                generation: Generation(0),
                command: "x".into(),
                cwd: std::env::temp_dir(),
                shell: Some(ShellProgram {
                    program: "definitely-not-a-shell-xyz".into(),
                    args: vec![],
                    prefix: String::new(),
                }),
            },
            tx,
        );
        h.join();
        assert!(matches!(
            rx.try_iter().last(),
            Some(ShellEvent::SpawnFailed { .. })
        ));
    }

    #[test]
    fn sanitize_strips_escapes() {
        assert_eq!(sanitize_line("a\u{1b}[31mred\u{1b}[0m b"), "ared b");
        assert_eq!(sanitize_line("x\u{1b}]0;title\u{7}y"), "xy");
        assert_eq!(sanitize_line("x\u{1b}]52;c;AAAA\u{1b}\\y"), "xy");
        assert_eq!(sanitize_line("a\tb\r\u{8}c\u{9b}d"), "a bcd");
        assert_eq!(sanitize_line("ünï ok"), "ünï ok");
    }

    #[test]
    fn bounded_line_truncates_and_continues() {
        let mut data = vec![b'a'; MAX_LINE_BYTES * 3];
        data.extend_from_slice(b"\nnext\n");
        let mut r = std::io::Cursor::new(data);
        let mut buf = Vec::new();
        assert!(read_bounded_line(&mut r, &mut buf).unwrap());
        assert_eq!(buf.len(), MAX_LINE_BYTES + 3); // + ellipsis
        assert!(buf.ends_with("\u{2026}".as_bytes()));
        assert!(read_bounded_line(&mut r, &mut buf).unwrap());
        assert_eq!(buf, b"next");
        assert!(!read_bounded_line(&mut r, &mut buf).unwrap());
    }

    #[test]
    fn no_line_after_terminal_event() {
        let cmd = if cfg!(windows) {
            "1..200 | ForEach-Object { Write-Output $_ }"
        } else {
            "seq 1 200"
        };
        let ev = run(cmd);
        let term = ev
            .iter()
            .position(|e| !matches!(e, ShellEvent::Line { .. }))
            .unwrap();
        assert_eq!(ev.len() - 1, term);
    }
}
