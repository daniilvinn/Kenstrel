//! 10b: the embedded Claude pane: `claude` in a ConPTY (`portable-pty`) rendered through `vt100`.
//!
//! Spike verdict (docs/spikes/claude-pty.md): `vt100` parses Claude Code's startup screen cleanly
//! (truecolor, synchronized output, bracketed paste, hyperlinks, hidden cursor), so we embed with
//! `vt100`.
//!
//! # Threads
//!
//! `ClaudePane::spawn` starts two `std::thread`s and returns immediately:
//! * a **reader** thread that reads PTY output, feeds the `vt100::Parser`, answers the cursor
//!   position request (`CSI 6 n`) that ConPTY issues at startup (without the answer ConPTY never
//!   emits any output), and emits a *coalesced* [`ClaudeEvent::PaneUpdated`];
//! * a **waiter** thread that polls the child for exit (and for cancellation), emits
//!   [`ClaudeEvent::PaneExited`] and then closes the PTY so the reader ends.
//!
//! The UI calls [`ClaudePane::snapshot`] on `PaneUpdated` and renders the [`ScreenSnapshot`].
//!
//! # Safety rails
//!
//! `--bare` is rejected ([`PaneError::ForbiddenArg`]); the env policy strips API credentials from
//! the child environment.

use std::io::{Read, Write};
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, Mutex, MutexGuard};
use std::time::{Duration, Instant};

use portable_pty::{native_pty_system, CommandBuilder, MasterPty, PtySize};
use tokio_util::sync::CancellationToken;

use crate::env::EnvPolicy;
use crate::events::{ClaudeEvent, EventSink};
use crate::keys::{encode_key, KeyEvent};
use crate::preflight::locate_claude;

/// Errors from pane operations.
#[derive(Debug, thiserror::Error)]
pub enum PaneError {
    /// `claude` could not be found.
    #[error("claude executable not found")]
    NotInstalled,
    /// An argument that must never be passed (currently `--bare`).
    #[error("forbidden claude argument: {0}")]
    ForbiddenArg(String),
    /// PTY / process error.
    #[error("pty error: {0}")]
    Pty(String),
    /// IO error.
    #[error(transparent)]
    Io(#[from] std::io::Error),
}

/// Pane size in terminal cells.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct PaneSize {
    /// Columns.
    pub cols: u16,
    /// Rows.
    pub rows: u16,
}

impl PaneSize {
    fn pty(self) -> PtySize {
        PtySize {
            rows: self.rows.max(1),
            cols: self.cols.max(1),
            pixel_width: 0,
            pixel_height: 0,
        }
    }
}

/// How to spawn the pane.
#[derive(Debug, Clone)]
pub struct PaneConfig {
    /// Executable; `None` = [`locate_claude`].
    pub program: Option<PathBuf>,
    /// Working directory (project root).
    pub cwd: PathBuf,
    /// Extra arguments (`--session-id`/`--resume`, `--mcp-config`, `--settings`, ...). Use
    /// `SessionMode::args` for the session part.
    pub args: Vec<String>,
    /// Additional environment (e.g. `KESTREL_IPC`).
    pub env: Vec<(String, String)>,
    /// Credential stripping.
    pub env_policy: EnvPolicy,
    /// Initial size.
    pub size: PaneSize,
    /// vt100 scrollback lines (0 = none).
    pub scrollback: usize,
}

impl PaneConfig {
    /// Defaults for `cwd` at the given size.
    pub fn new(cwd: impl Into<PathBuf>, size: PaneSize) -> Self {
        Self {
            program: None,
            cwd: cwd.into(),
            args: Vec::new(),
            env: Vec::new(),
            env_policy: EnvPolicy::default(),
            size,
            scrollback: 2000,
        }
    }
}

/// Terminal colour.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum Color {
    /// Terminal default.
    #[default]
    Default,
    /// 256-colour palette index (0..=15 are the ANSI colours).
    Indexed(u8),
    /// 24-bit colour.
    Rgb(u8, u8, u8),
}

impl From<vt100::Color> for Color {
    fn from(c: vt100::Color) -> Self {
        match c {
            vt100::Color::Default => Color::Default,
            vt100::Color::Idx(i) => Color::Indexed(i),
            vt100::Color::Rgb(r, g, b) => Color::Rgb(r, g, b),
        }
    }
}

/// One screen cell.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct Cell {
    /// Cell text (a grapheme; empty for blank cells and wide-char continuation cells).
    pub text: String,
    /// Foreground.
    pub fg: Color,
    /// Background.
    pub bg: Color,
    /// Bold.
    pub bold: bool,
    /// Dim/faint.
    pub dim: bool,
    /// Italic.
    pub italic: bool,
    /// Underline.
    pub underline: bool,
    /// Inverse video (renderer must swap fg/bg).
    pub inverse: bool,
    /// Right half of a double-width character; the renderer should skip it.
    pub wide_continuation: bool,
}

/// Immutable copy of the visible screen.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ScreenSnapshot {
    /// Rows.
    pub rows: u16,
    /// Columns.
    pub cols: u16,
    /// Row-major cells, `rows * cols` long.
    pub cells: Vec<Cell>,
    /// Cursor `(row, col)` when visible (hidden cursor or scrolled-back view gives `None`).
    pub cursor: Option<(u16, u16)>,
    /// Whether the app is on the alternate screen.
    pub alternate_screen: bool,
    /// Whether the app enabled bracketed paste (`?2004h`).
    pub bracketed_paste: bool,
}

impl ScreenSnapshot {
    /// Cell at `(row, col)`.
    pub fn cell(&self, row: u16, col: u16) -> Option<&Cell> {
        if row < self.rows && col < self.cols {
            self.cells
                .get(usize::from(row) * usize::from(self.cols) + usize::from(col))
        } else {
            None
        }
    }

    /// Plain text of one row (trailing spaces trimmed).
    pub fn row_text(&self, row: u16) -> String {
        let mut s = String::new();
        for c in 0..self.cols {
            if let Some(cell) = self.cell(row, c) {
                if cell.wide_continuation {
                    continue;
                }
                if cell.text.is_empty() {
                    s.push(' ');
                } else {
                    s.push_str(&cell.text);
                }
            }
        }
        s.trim_end().to_string()
    }

    /// Plain text of the whole screen.
    pub fn text(&self) -> String {
        (0..self.rows)
            .map(|r| self.row_text(r))
            .collect::<Vec<_>>()
            .join("\n")
    }
}

struct Shared {
    parser: Mutex<vt100::Parser>,
    dirty: AtomicBool,
    exited: Mutex<Option<Option<u32>>>,
    /// Set when the reader thread ended (EOF or error).
    reader_done: AtomicBool,
    /// Milliseconds since `started` at which output last arrived.
    last_output_ms: AtomicU64,
    started: Instant,
}

impl Shared {
    fn now_ms(&self) -> u64 {
        u64::try_from(self.started.elapsed().as_millis()).unwrap_or(u64::MAX)
    }
}

fn lock<T>(m: &Mutex<T>) -> MutexGuard<'_, T> {
    m.lock().unwrap_or_else(|p| p.into_inner())
}

/// A running embedded Claude Code session.
pub struct ClaudePane {
    shared: Arc<Shared>,
    writer: Arc<Mutex<Box<dyn Write + Send>>>,
    master: Arc<Mutex<Option<Box<dyn MasterPty + Send>>>>,
    killer: Mutex<Box<dyn portable_pty::ChildKiller + Send + Sync>>,
    cancel: CancellationToken,
    sink: EventSink,
    generation: u64,
}

impl std::fmt::Debug for ClaudePane {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("ClaudePane")
    }
}

fn contains_forbidden(args: &[String]) -> Option<String> {
    args.iter()
        .find(|a| a.as_str() == "--bare" || a.starts_with("--bare="))
        .cloned()
}

impl ClaudePane {
    /// Spawn `claude` in a PTY and start the reader/waiter threads. Returns quickly.
    ///
    /// `cancel` kills the process when cancelled. Events carry `generation`.
    pub fn spawn(
        cfg: PaneConfig,
        generation: u64,
        sink: EventSink,
        cancel: CancellationToken,
    ) -> Result<ClaudePane, PaneError> {
        if let Some(bad) = contains_forbidden(&cfg.args) {
            return Err(PaneError::ForbiddenArg(bad));
        }
        let program = cfg
            .program
            .clone()
            .or_else(locate_claude)
            .ok_or(PaneError::NotInstalled)?;
        let pair = native_pty_system()
            .openpty(cfg.size.pty())
            .map_err(|e| PaneError::Pty(e.to_string()))?;
        let mut cmd = CommandBuilder::new(&program);
        cmd.args(&cfg.args);
        cmd.cwd(&cfg.cwd);
        cmd.env("COLORTERM", "truecolor");
        for (k, v) in &cfg.env {
            cmd.env(k, v);
        }
        cfg.env_policy.apply_pty(&mut cmd);
        let mut child = pair
            .slave
            .spawn_command(cmd)
            .map_err(|e| PaneError::Pty(e.to_string()))?;
        drop(pair.slave);
        let mut reader = pair
            .master
            .try_clone_reader()
            .map_err(|e| PaneError::Pty(e.to_string()))?;
        let writer: Box<dyn Write + Send> = pair
            .master
            .take_writer()
            .map_err(|e| PaneError::Pty(e.to_string()))?;
        let writer = Arc::new(Mutex::new(writer));
        let killer = child.clone_killer();
        let master = Arc::new(Mutex::new(Some(pair.master)));
        let shared = Arc::new(Shared {
            parser: Mutex::new(vt100::Parser::new(
                cfg.size.rows.max(1),
                cfg.size.cols.max(1),
                cfg.scrollback,
            )),
            dirty: AtomicBool::new(false),
            exited: Mutex::new(None),
            reader_done: AtomicBool::new(false),
            last_output_ms: AtomicU64::new(0),
            started: Instant::now(),
        });

        // Reader thread.
        {
            let shared = shared.clone();
            let writer = writer.clone();
            let sink = sink.clone();
            std::thread::Builder::new()
                .name("ks-claude-pty-read".into())
                .spawn(move || {
                    let mut buf = [0u8; 16 * 1024];
                    let mut scanner = QueryScanner::default();
                    loop {
                        let n = match reader.read(&mut buf) {
                            Ok(0) | Err(_) => break,
                            Ok(n) => n,
                        };
                        shared
                            .last_output_ms
                            .store(shared.now_ms(), Ordering::Release);
                        let chunk = &buf[..n];
                        // Process the chunk in segments that end at each terminal query so the
                        // reply reflects the screen state at the point of the request.
                        let mut done = 0;
                        for (end, query) in scanner.feed(chunk) {
                            let cursor = {
                                let mut p = lock(&shared.parser);
                                p.process(&chunk[done..end]);
                                p.screen().cursor_position()
                            };
                            done = end;
                            if let Some(reply) = query_reply(query, cursor) {
                                let mut w = lock(&writer);
                                let _ = w.write_all(&reply);
                                let _ = w.flush();
                            }
                        }
                        lock(&shared.parser).process(&chunk[done..]);
                        if !shared.dirty.swap(true, Ordering::AcqRel) {
                            sink(ClaudeEvent::PaneUpdated { generation });
                        }
                    }
                    shared.reader_done.store(true, Ordering::Release);
                })?;
        }

        // Waiter thread: exit + cancellation.
        {
            let shared = shared.clone();
            let master = master.clone();
            let cancel = cancel.clone();
            let sink = sink.clone();
            std::thread::Builder::new()
                .name("ks-claude-pty-wait".into())
                .spawn(move || {
                    let code = loop {
                        match child.try_wait() {
                            Ok(Some(st)) => break Some(st.exit_code()),
                            Ok(None) => {}
                            Err(_) => break None,
                        }
                        if cancel.is_cancelled() {
                            let _ = child.kill();
                            let st = child.wait().ok().map(|s| s.exit_code());
                            break st;
                        }
                        std::thread::sleep(Duration::from_millis(30));
                    };
                    // Let the reader drain the last output (ConPTY keeps the pipe open, so EOF
                    // only comes once we close the PTY): wait until the reader saw EOF or no
                    // output arrived for DRAIN_IDLE after the exit, bounded by DRAIN_MAX.
                    let exit_ms = shared.now_ms();
                    let begin = Instant::now();
                    while !shared.reader_done.load(Ordering::Acquire) && begin.elapsed() < DRAIN_MAX
                    {
                        let last = shared.last_output_ms.load(Ordering::Acquire).max(exit_ms);
                        if shared.now_ms().saturating_sub(last) >= DRAIN_IDLE.as_millis() as u64 {
                            break;
                        }
                        std::thread::sleep(Duration::from_millis(10));
                    }
                    *lock(&master) = None;
                    *lock(&shared.exited) = Some(code);
                    if !shared.dirty.swap(true, Ordering::AcqRel) {
                        sink(ClaudeEvent::PaneUpdated { generation });
                    }
                    sink(ClaudeEvent::PaneExited { generation, code });
                })?;
        }

        Ok(ClaudePane {
            shared,
            writer,
            master,
            killer: Mutex::new(killer),
            cancel,
            sink,
            generation,
        })
    }

    /// Resize the PTY and the emulator.
    pub fn resize(&self, size: PaneSize) {
        if let Some(m) = lock(&self.master).as_ref() {
            if let Err(e) = m.resize(size.pty()) {
                tracing::debug!("pty resize failed: {e}");
            }
        }
        lock(&self.shared.parser)
            .screen_mut()
            .set_size(size.rows.max(1), size.cols.max(1));
        // The visible snapshot changed shape even if Claude prints nothing: tell the UI (same
        // coalescing contract as output).
        if !self.shared.dirty.swap(true, Ordering::AcqRel) {
            (self.sink)(ClaudeEvent::PaneUpdated {
                generation: self.generation,
            });
        }
    }

    /// Send raw bytes to the child's stdin.
    pub fn write_input(&self, bytes: &[u8]) -> std::io::Result<()> {
        let mut w = lock(&self.writer);
        w.write_all(bytes)?;
        w.flush()
    }

    /// Encode and send a key.
    pub fn send_key(&self, key: KeyEvent) -> std::io::Result<()> {
        let bytes = encode_key(key, self.application_cursor());
        if bytes.is_empty() {
            return Ok(());
        }
        self.write_input(&bytes)
    }

    /// Paste text: wrapped in bracketed-paste markers only if the app enabled `?2004`, otherwise
    /// newlines become `CR`. Any embedded paste-end marker is stripped so pasted text cannot
    /// terminate the paste early.
    pub fn paste(&self, text: &str) -> std::io::Result<()> {
        let bracketed = lock(&self.shared.parser).screen().bracketed_paste();
        self.write_input(&paste_bytes(text, bracketed))
    }

    /// Whether the app enabled bracketed paste.
    pub fn bracketed_paste(&self) -> bool {
        lock(&self.shared.parser).screen().bracketed_paste()
    }

    /// Whether the app enabled application cursor keys (DECCKM).
    pub fn application_cursor(&self) -> bool {
        lock(&self.shared.parser).screen().application_cursor()
    }

    /// Scroll the view back by `lines` (0 = live screen).
    pub fn set_scrollback(&self, lines: usize) {
        lock(&self.shared.parser).screen_mut().set_scrollback(lines);
        self.shared.dirty.store(false, Ordering::Release);
    }

    /// Scrolls the view by `delta` lines (positive = back into history, negative = towards the
    /// live screen) and returns the new offset. vt100 clamps the offset to the history size.
    pub fn scroll_by(&self, delta: isize) -> usize {
        let mut p = lock(&self.shared.parser);
        let cur = p.screen().scrollback();
        let next = cur.saturating_add_signed(delta);
        p.screen_mut().set_scrollback(next);
        p.screen().scrollback()
    }

    /// Copy of the visible screen. Clears the coalescing flag so the next change emits a new
    /// `PaneUpdated`.
    pub fn snapshot(&self) -> ScreenSnapshot {
        self.shared.dirty.store(false, Ordering::Release);
        let p = lock(&self.shared.parser);
        snapshot_screen(p.screen())
    }

    /// `Some(exit code)` once the process has exited (inner `None` = code unknown).
    pub fn exit_status(&self) -> Option<Option<u32>> {
        *lock(&self.shared.exited)
    }

    /// True once the process exited.
    pub fn has_exited(&self) -> bool {
        self.exit_status().is_some()
    }

    /// Kill the process (also happens on drop and on cancellation).
    pub fn kill(&self) {
        let _ = lock(&self.killer).kill();
    }
}

impl Drop for ClaudePane {
    fn drop(&mut self) {
        self.cancel.cancel();
        self.kill();
    }
}

/// After the child exits, output is considered drained once none arrived for this long...
const DRAIN_IDLE: Duration = Duration::from_millis(120);
/// ...or after this long in any case.
const DRAIN_MAX: Duration = Duration::from_millis(1500);

/// Copy the visible part of a `vt100` screen into a [`ScreenSnapshot`].
pub fn snapshot_screen(screen: &vt100::Screen) -> ScreenSnapshot {
    let (rows, cols) = screen.size();
    let mut cells = Vec::with_capacity(usize::from(rows) * usize::from(cols));
    for r in 0..rows {
        for c in 0..cols {
            match screen.cell(r, c) {
                Some(cell) => cells.push(Cell {
                    text: cell.contents().to_string(),
                    fg: cell.fgcolor().into(),
                    bg: cell.bgcolor().into(),
                    bold: cell.bold(),
                    dim: cell.dim(),
                    italic: cell.italic(),
                    underline: cell.underline(),
                    inverse: cell.inverse(),
                    wide_continuation: cell.is_wide_continuation(),
                }),
                None => cells.push(Cell::default()),
            }
        }
    }
    let cursor = if screen.hide_cursor() || screen.scrollback() > 0 {
        None
    } else {
        Some(screen.cursor_position())
    };
    ScreenSnapshot {
        rows,
        cols,
        cells,
        cursor,
        alternate_screen: screen.alternate_screen(),
        bracketed_paste: screen.bracketed_paste(),
    }
}

/// Terminal queries the pane answers itself (the emulator has no output channel).
///
/// Deliberately **not** answered: DA1 (ConPTY answers it itself, a second reply would be typed
/// into Claude) and the kitty keyboard query `CSI ? u` (an answer would make Claude switch to
/// kitty key encoding, which [`crate::keys`] does not produce; unanswered = legacy keys).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TerminalQuery {
    /// `CSI 6 n`: report cursor position (ConPTY emits no output until answered).
    CursorPosition,
    /// `CSI > q` / `CSI > 0 q`: XTVERSION.
    XtVersion,
    /// `OSC 10 ; ?` (10 = foreground) or `OSC 11 ; ?` (11 = background) colour query.
    OscColor(u8),
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
enum ScanState {
    #[default]
    Ground,
    Esc,
    Csi,
    Osc,
    OscEsc,
}

/// Incremental scanner that finds [`TerminalQuery`] sequences in a byte stream, correctly across
/// arbitrary read boundaries.
#[derive(Debug, Default)]
pub struct QueryScanner {
    state: ScanState,
    buf: Vec<u8>,
}

impl QueryScanner {
    /// Feed the next chunk. Returns `(end, query)` pairs where `end` is the index in `chunk`
    /// just past the final byte of the query sequence.
    pub fn feed(&mut self, chunk: &[u8]) -> Vec<(usize, TerminalQuery)> {
        let mut out = Vec::new();
        for (i, &b) in chunk.iter().enumerate() {
            match self.state {
                ScanState::Ground => {
                    if b == 0x1b {
                        self.state = ScanState::Esc;
                    }
                }
                ScanState::Esc => {
                    self.buf.clear();
                    self.state = match b {
                        b'[' => ScanState::Csi,
                        b']' => ScanState::Osc,
                        0x1b => ScanState::Esc,
                        _ => ScanState::Ground,
                    };
                }
                ScanState::Csi => match b {
                    0x20..=0x3f if self.buf.len() < 32 => self.buf.push(b),
                    0x40..=0x7e => {
                        if let Some(q) = csi_query(&self.buf, b) {
                            out.push((i + 1, q));
                        }
                        self.state = ScanState::Ground;
                    }
                    0x1b => self.state = ScanState::Esc,
                    _ => self.state = ScanState::Ground,
                },
                ScanState::Osc => match b {
                    0x07 => {
                        if let Some(q) = osc_query(&self.buf) {
                            out.push((i + 1, q));
                        }
                        self.state = ScanState::Ground;
                    }
                    0x1b => self.state = ScanState::OscEsc,
                    _ if self.buf.len() < 64 => self.buf.push(b),
                    _ => self.state = ScanState::Ground,
                },
                ScanState::OscEsc => {
                    // ST = ESC \ (0x5c).
                    if b == 0x5c {
                        if let Some(q) = osc_query(&self.buf) {
                            out.push((i + 1, q));
                        }
                    }
                    self.state = ScanState::Ground;
                }
            }
        }
        out
    }
}

fn csi_query(params: &[u8], final_byte: u8) -> Option<TerminalQuery> {
    match (params, final_byte) {
        (b"6", b'n') => Some(TerminalQuery::CursorPosition),
        (b">" | b">0", b'q') => Some(TerminalQuery::XtVersion),
        _ => None,
    }
}

fn osc_query(body: &[u8]) -> Option<TerminalQuery> {
    match body {
        b"10;?" => Some(TerminalQuery::OscColor(10)),
        b"11;?" => Some(TerminalQuery::OscColor(11)),
        _ => None,
    }
}

/// Reply bytes for a query; `cursor` is the 0-based `(row, col)` at the point of the request.
pub fn query_reply(query: TerminalQuery, cursor: (u16, u16)) -> Option<Vec<u8>> {
    Some(match query {
        TerminalQuery::CursorPosition => {
            format!("\x1b[{};{}R", cursor.0 + 1, cursor.1 + 1).into_bytes()
        }
        TerminalQuery::XtVersion => b"\x1bP>|kestrel\x1b\\".to_vec(),
        // Light grey on black: Claude then picks its dark theme.
        TerminalQuery::OscColor(10) => b"\x1b]10;rgb:cccc/cccc/cccc\x1b\\".to_vec(),
        TerminalQuery::OscColor(11) => b"\x1b]11;rgb:0000/0000/0000\x1b\\".to_vec(),
        TerminalQuery::OscColor(_) => return None,
    })
}

/// Bytes for a paste. Exposed for tests and for callers that write through other paths.
///
/// Every control character except tab and line breaks is removed (ESC, ^C, DEL, C1 ...), so
/// pasted text can neither terminate a bracketed paste early (no `ESC` survives, whatever the
/// input looks like after removal) nor inject key presses. Line breaks become `CR` outside
/// bracketed paste.
pub fn paste_bytes(text: &str, bracketed: bool) -> Vec<u8> {
    let clean: String = text
        .chars()
        .filter(|&c| matches!(c, '\t' | '\n' | '\r') || !c.is_control())
        .collect();
    if bracketed {
        let mut v = b"\x1b[200~".to_vec();
        v.extend_from_slice(clean.as_bytes());
        v.extend_from_slice(b"\x1b[201~");
        v
    } else {
        clean.replace("\r\n", "\r").replace('\n', "\r").into_bytes()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::mpsc;

    fn channel_sink() -> (EventSink, mpsc::Receiver<ClaudeEvent>) {
        let (tx, rx) = mpsc::channel();
        let tx = Mutex::new(tx);
        let sink: EventSink = Arc::new(move |e| {
            let _ = lock(&tx).send(e);
        });
        (sink, rx)
    }

    fn powershell() -> Option<PathBuf> {
        which::which("powershell").ok()
    }

    fn ps_config(script: &str) -> Option<PaneConfig> {
        let mut cfg = PaneConfig::new(
            std::env::temp_dir(),
            PaneSize {
                cols: 100,
                rows: 30,
            },
        );
        cfg.program = Some(powershell()?);
        cfg.args = vec!["-NoProfile".into(), "-Command".into(), script.into()];
        Some(cfg)
    }

    /// Collect events until `PaneExited` (or `max` elapses).
    fn collect_until_exit(rx: &mpsc::Receiver<ClaudeEvent>, max: Duration) -> Vec<ClaudeEvent> {
        let end = Instant::now() + max;
        let mut v = Vec::new();
        while let Some(left) = end.checked_duration_since(Instant::now()) {
            match rx.recv_timeout(left) {
                Ok(e) => {
                    let done = matches!(e, ClaudeEvent::PaneExited { .. });
                    v.push(e);
                    if done {
                        break;
                    }
                }
                Err(_) => break,
            }
        }
        v
    }

    #[test]
    fn paste_bracketed_and_plain() {
        assert_eq!(
            paste_bytes("@a.rs @b.rs ", true),
            b"\x1b[200~@a.rs @b.rs \x1b[201~"
        );
        assert_eq!(paste_bytes("a\nb\r\nc", false), b"a\rb\rc");
    }

    #[test]
    fn paste_strips_end_marker() {
        let v = paste_bytes("x\x1b[201~rm -rf", true);
        assert_eq!(v, b"\x1b[200~x[201~rm -rf\x1b[201~");
        // The only ESC bytes are the framing ones.
        assert_eq!(v.iter().filter(|&&b| b == 0x1b).count(), 2);
    }

    #[test]
    fn paste_cannot_be_bypassed_by_nesting() {
        // Single-pass removal of the marker would collapse this to a real end marker.
        let v = paste_bytes("\x1b[2\x1b[201~01~evil\r", true);
        assert_eq!(v.iter().filter(|&&b| b == 0x1b).count(), 2);
        assert!(v.ends_with(b"\x1b[201~"));
        assert!(!v[..v.len() - 6].windows(6).any(|w| w == b"\x1b[201~"));
    }

    #[test]
    fn paste_drops_control_bytes_in_both_modes() {
        for bracketed in [true, false] {
            let v = paste_bytes("a\x03b\x1bc\x7fd\u{9b}e\tf\n", bracketed);
            let body: Vec<u8> = if bracketed {
                v[6..v.len() - 6].to_vec()
            } else {
                v
            };
            let text = String::from_utf8(body).unwrap();
            assert!(
                text.starts_with("ab[") || text.starts_with("abc"),
                "{text:?}"
            );
            assert!(!text.contains('\x03') && !text.contains('\x1b') && !text.contains('\x7f'));
            assert!(text.contains('\t'));
        }
    }

    #[test]
    fn scanner_whole_sequences_and_two_in_one_chunk() {
        let mut s = QueryScanner::default();
        let hits = s.feed(b"ab\x1b[6ncd\x1b[>0qe");
        assert_eq!(
            hits,
            vec![
                (6, TerminalQuery::CursorPosition),
                (13, TerminalQuery::XtVersion)
            ]
        );
        let mut s = QueryScanner::default();
        assert_eq!(
            s.feed(b"\x1b[6n\x1b[6n"),
            vec![
                (4, TerminalQuery::CursorPosition),
                (8, TerminalQuery::CursorPosition)
            ]
        );
    }

    #[test]
    fn scanner_split_at_every_position_counts_once() {
        let seq = b"xx\x1b[6nyy";
        for split in 0..=seq.len() {
            let mut s = QueryScanner::default();
            let mut total = s.feed(&seq[..split]).len();
            total += s.feed(&seq[split..]).len();
            assert_eq!(total, 1, "split at {split}");
        }
        // OSC with both terminators, split mid-way.
        for term in [&b"\x07"[..], &b"\x1b\\"[..]] {
            let mut full = b"\x1b]11;?".to_vec();
            full.extend_from_slice(term);
            for split in 0..=full.len() {
                let mut s = QueryScanner::default();
                let mut hits = s.feed(&full[..split]);
                hits.extend(s.feed(&full[split..]));
                assert_eq!(
                    hits.iter().map(|h| h.1).collect::<Vec<_>>(),
                    vec![TerminalQuery::OscColor(11)],
                    "split at {split}"
                );
            }
        }
    }

    #[test]
    fn scanner_ignores_other_sequences() {
        let mut s = QueryScanner::default();
        // DA1, kitty query, SGR and a colour *set* are not answered.
        assert!(s
            .feed(b"\x1b[c\x1b[?u\x1b[38;2;1;2;3m\x1b]11;rgb:00/00/00\x07\x1b[5n")
            .is_empty());
    }

    #[test]
    fn replies_use_cursor_at_request_point() {
        assert_eq!(
            query_reply(TerminalQuery::CursorPosition, (0, 0)).unwrap(),
            b"\x1b[1;1R"
        );
        // The reply reflects the screen at the end of the sequence, not at the end of the chunk.
        let chunk = b"AB\x1b[6nCDEF";
        let mut scanner = QueryScanner::default();
        let mut parser = vt100::Parser::new(5, 20, 0);
        let mut done = 0;
        let mut replies = Vec::new();
        for (end, q) in scanner.feed(chunk) {
            parser.process(&chunk[done..end]);
            done = end;
            replies.push(query_reply(q, parser.screen().cursor_position()).unwrap());
        }
        parser.process(&chunk[done..]);
        assert_eq!(replies, vec![b"\x1b[1;3R".to_vec()]);
        assert_eq!(parser.screen().cursor_position(), (0, 6));
    }

    #[test]
    fn snapshot_maps_cells() {
        let mut p = vt100::Parser::new(4, 10, 0);
        p.process(
            b"\x1b[38;2;10;20;30;1mA\x1b[0m\x1b[7;3;4mB\x1b[0m\x1b[44m \x1b[0m\xe7\x95\x8c\x1b[?2004h\x1b[2;1H",
        );
        let snap = snapshot_screen(p.screen());
        assert_eq!((snap.rows, snap.cols), (4, 10));
        assert_eq!(snap.cells.len(), 40);
        let a = snap.cell(0, 0).unwrap();
        assert_eq!(a.text, "A");
        assert_eq!(a.fg, Color::Rgb(10, 20, 30));
        assert!(a.bold && !a.inverse);
        let b = snap.cell(0, 1).unwrap();
        assert!(b.inverse && b.italic && b.underline);
        assert_eq!(snap.cell(0, 2).unwrap().bg, Color::Indexed(4));
        let wide = snap.cell(0, 3).unwrap();
        assert_eq!(wide.text, "\u{754c}");
        assert!(!wide.wide_continuation);
        assert!(snap.cell(0, 4).unwrap().wide_continuation);
        assert_eq!(snap.row_text(0), "AB \u{754c}");
        assert_eq!(snap.cursor, Some((1, 0)));
        assert!(snap.bracketed_paste);
        assert!(!snap.alternate_screen);
        assert!(snap.cell(4, 0).is_none());
        let mut p2 = vt100::Parser::new(2, 5, 0);
        p2.process(b"\x1b[?25l\x1b[?1049h");
        let s2 = snapshot_screen(p2.screen());
        assert_eq!(s2.cursor, None);
        assert!(s2.alternate_screen);
    }

    #[test]
    fn bare_is_rejected() {
        let mut cfg = PaneConfig::new(".", PaneSize { cols: 80, rows: 24 });
        cfg.args = vec!["--bare".into()];
        let sink: EventSink = Arc::new(|_| {});
        let err = ClaudePane::spawn(cfg, 1, sink, CancellationToken::new()).unwrap_err();
        assert!(matches!(err, PaneError::ForbiddenArg(_)));
    }

    #[test]
    fn missing_program_errors() {
        let mut cfg = PaneConfig::new(".", PaneSize { cols: 80, rows: 24 });
        cfg.program = Some(PathBuf::from("Z:\\nope\\claude-nope.exe"));
        let sink: EventSink = Arc::new(|_| {});
        assert!(ClaudePane::spawn(cfg, 1, sink, CancellationToken::new()).is_err());
    }

    #[cfg(windows)]
    #[test]
    fn child_output_colour_paste_mode_coalescing_and_exit() {
        let script = "[Console]::Out.Write([string][char]27+'[38;2;1;2;3mHELLO'+[string][char]27+'[0m'+[string][char]27+'[?2004h'); Start-Sleep -Milliseconds 400; exit 3";
        let Some(cfg) = ps_config(script) else { return };
        let (sink, rx) = channel_sink();
        let pane = ClaudePane::spawn(cfg, 42, sink, CancellationToken::new()).unwrap();
        let events = collect_until_exit(&rx, Duration::from_secs(30));
        let updates = events
            .iter()
            .filter(|e| matches!(e, ClaudeEvent::PaneUpdated { generation: 42 }))
            .count();
        assert_eq!(updates, 1, "coalesced until snapshot(): {events:?}");
        assert!(matches!(
            events.last(),
            Some(ClaudeEvent::PaneExited {
                generation: 42,
                code: Some(3)
            })
        ));
        assert_eq!(pane.exit_status(), Some(Some(3)));
        assert!(pane.has_exited());
        assert!(pane.bracketed_paste());
        let snap = pane.snapshot();
        assert!(snap.text().contains("HELLO"), "{}", snap.text());
        let h = (0..snap.rows)
            .flat_map(|r| (0..snap.cols).map(move |c| (r, c)))
            .find_map(|(r, c)| snap.cell(r, c).filter(|x| x.text == "H"))
            .expect("H cell");
        assert_eq!(h.fg, Color::Rgb(1, 2, 3));
        // After snapshot() the next change emits a new event; resize is such a change and must
        // also be reported (and the snapshot must have the new size).
        pane.resize(PaneSize { cols: 60, rows: 12 });
        assert!(matches!(
            rx.recv_timeout(Duration::from_secs(2)),
            Ok(ClaudeEvent::PaneUpdated { generation: 42 })
        ));
        let snap = pane.snapshot();
        assert_eq!((snap.cols, snap.rows), (60, 12));
    }

    #[cfg(windows)]
    #[test]
    fn cancel_kills_sleeping_child() {
        let Some(cfg) = ps_config("Start-Sleep -Seconds 60") else {
            return;
        };
        let (sink, rx) = channel_sink();
        let cancel = CancellationToken::new();
        let _pane = ClaudePane::spawn(cfg, 9, sink, cancel.clone()).unwrap();
        std::thread::sleep(Duration::from_millis(800));
        let t = Instant::now();
        cancel.cancel();
        let events = collect_until_exit(&rx, Duration::from_secs(15));
        assert!(
            matches!(
                events.last(),
                Some(ClaudeEvent::PaneExited { generation: 9, .. })
            ),
            "{events:?}"
        );
        assert!(t.elapsed() < Duration::from_secs(10));
    }

    /// Rough measurement for the progress notes: `cargo test -p ks-claude perf -- --ignored --nocapture`.
    #[test]
    #[ignore = "timing measurement"]
    #[allow(clippy::print_stderr)]
    fn perf_snapshot_and_reader_throughput() {
        let mut p = vt100::Parser::new(40, 120, 0);
        let mut burst = Vec::new();
        for i in 0..40 {
            burst.extend_from_slice(
                format!("\x1b[38;2;{};100;200m{:0>118}\x1b[0m\r\n", i * 5, i).as_bytes(),
            );
        }
        let t = Instant::now();
        for _ in 0..200 {
            p.process(&burst);
        }
        let process = t.elapsed();
        let t = Instant::now();
        let n = 200;
        for _ in 0..n {
            std::hint::black_box(snapshot_screen(p.screen()));
        }
        let snap = t.elapsed() / n;
        let mut big = vt100::Parser::new(60, 200, 0);
        let t = Instant::now();
        for _ in 0..n {
            std::hint::black_box(snapshot_screen(big.screen()));
        }
        let _ = &mut big;
        let snap_blank = t.elapsed() / n;
        eprintln!(
            "vt100 process {} KiB: {process:?}; snapshot 120x40 full: {snap:?}; 200x60 blank: {snap_blank:?}",
            burst.len() * 200 / 1024
        );
    }

    /// Needs the real `claude` on the machine; never sends a prompt. Run with `--ignored`.
    #[test]
    #[ignore = "launches the real claude CLI in a PTY"]
    fn real_claude_renders_startup_screen() {
        let dir = tempfile::tempdir().unwrap();
        let (tx, rx) = std::sync::mpsc::channel();
        let sink: EventSink = Arc::new(move |e| {
            let _ = tx.send(e);
        });
        let cancel = CancellationToken::new();
        let pane = ClaudePane::spawn(
            PaneConfig::new(
                dir.path(),
                PaneSize {
                    cols: 120,
                    rows: 40,
                },
            ),
            7,
            sink,
            cancel.clone(),
        )
        .unwrap();
        let _ = rx.recv_timeout(Duration::from_secs(10)).unwrap();
        std::thread::sleep(Duration::from_secs(4));
        let snap = pane.snapshot();
        assert!(!snap.text().trim().is_empty());
        assert!(pane.bracketed_paste());
        pane.resize(PaneSize {
            cols: 100,
            rows: 30,
        });
        assert_eq!(pane.snapshot().rows, 30);
        cancel.cancel();
        let mut exited = false;
        while let Ok(e) = rx.recv_timeout(Duration::from_secs(5)) {
            if matches!(e, ClaudeEvent::PaneExited { generation: 7, .. }) {
                exited = true;
                break;
            }
        }
        assert!(exited);
    }
}
