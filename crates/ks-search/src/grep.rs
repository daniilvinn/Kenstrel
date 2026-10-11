//! Content search (`ctrl-f`, PLAN.md section 6.3) built on the ripgrep crates.
//!
//! Pipeline: parallel walker (with gitignore/hidden/glob filtering, size and cloud-placeholder
//! pre-checks) -> bounded channel -> N worker threads, each with its own
//! [`grep_searcher::Searcher`] (binary detection on) -> collector thread that batches results
//! and flushes them every [`GrepQuery::flush_interval`] (30 ms by default).
//!
//! # Usage
//!
//! ```no_run
//! use std::sync::Arc;
//! use ks_search::{spawn_search, CancellationToken, GrepEvent, GrepQuery};
//!
//! let (tx, rx) = crossbeam_channel::unbounded();
//! let sink = Arc::new(move |e: GrepEvent| { let _ = tx.send(e); });
//! let cancel = CancellationToken::new();
//! let handle = spawn_search(GrepQuery::new("C:\\src", "TODO"), 42, cancel.clone(), sink)?;
//! while let Ok(event) = rx.recv() {
//!     if event.is_terminal() { break; }
//! }
//! # let _ = handle;
//! # Ok::<(), std::io::Error>(())
//! ```
//!
//! # Threading
//!
//! [`spawn_search`] returns immediately. It starts one orchestrator thread (which runs the
//! walker, blocking only itself), `threads` worker threads and one collector thread. The sink
//! is invoked from the collector (and, for `Error`/`Done`/`Cancelled`, the orchestrator)
//! thread, never from the caller's thread. Every event carries the caller's `generation`.
//! Exactly one terminal event ([`GrepEvent::Done`], [`GrepEvent::Cancelled`] or
//! [`GrepEvent::Error`]) is emitted per search, always last.
//!
//! The 120 ms debounce is a UI concern; see [`crate::Debouncer`].

use std::io;
use std::ops::Range;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, AtomicU64, AtomicUsize, Ordering};
use std::sync::Arc;
use std::thread::JoinHandle;
use std::time::{Duration, Instant};

use crossbeam_channel::{bounded, unbounded, RecvTimeoutError};
use grep_matcher::Matcher;
use grep_regex::{RegexMatcher, RegexMatcherBuilder};
use grep_searcher::{BinaryDetection, Searcher, SearcherBuilder, Sink, SinkMatch};
use tokio_util::sync::CancellationToken;

use crate::walk::{
    build_overrides, file_attributes, is_cloud_placeholder, walk, EntryKind, WalkEntry,
    WalkOptions, WalkSink,
};
use crate::EventSink;

/// Default maximum file size (10 MB).
pub const DEFAULT_MAX_FILE_SIZE: u64 = 10 * 1024 * 1024;
/// Default match cap.
pub const DEFAULT_MAX_MATCHES: usize = 10_000;

/// A content search request.
#[derive(Debug, Clone)]
pub struct GrepQuery {
    /// Directory to search.
    pub root: PathBuf,
    /// The text to find. Literal unless [`regex`](Self::regex) is set. Empty = no-op search.
    pub pattern: String,
    /// Interpret `pattern` as a regular expression (Rust regex syntax).
    pub regex: bool,
    /// Match whole words only.
    pub whole_word: bool,
    /// Optional glob filter field: globs separated by whitespace or commas, for example
    /// `*.rs !target/**`.
    pub glob: String,
    /// Search hidden files. (`.git` directories are always skipped.)
    pub show_hidden: bool,
    /// Honour gitignore rules.
    pub respect_gitignore: bool,
    /// Files larger than this are skipped (counted in [`GrepStats::skipped_large`]).
    pub max_file_size: u64,
    /// Stop after this many matches and report `capped`.
    pub max_matches: usize,
    /// Matched lines are truncated to about this many characters (centred on the first match).
    pub max_line_chars: usize,
    /// Worker threads; `0` = `available_parallelism`.
    pub threads: usize,
    /// How often batched results are flushed.
    pub flush_interval: Duration,
    /// Do not descend into cloud-placeholder directories (counted in
    /// [`GrepStats::skipped_cloud`]). Files with placeholder attributes are always skipped.
    pub skip_cloud_dirs: bool,
}

impl GrepQuery {
    /// A literal, smart-case search with default limits.
    pub fn new(root: impl Into<PathBuf>, pattern: impl Into<String>) -> Self {
        Self {
            root: root.into(),
            pattern: pattern.into(),
            regex: false,
            whole_word: false,
            glob: String::new(),
            show_hidden: false,
            respect_gitignore: true,
            max_file_size: DEFAULT_MAX_FILE_SIZE,
            max_matches: DEFAULT_MAX_MATCHES,
            max_line_chars: 300,
            threads: 0,
            flush_interval: Duration::from_millis(30),
            skip_cloud_dirs: false,
        }
    }
}

/// One matching line.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LineMatch {
    /// 1-based line number.
    pub line_number: u64,
    /// Sanitized, possibly truncated line text (no terminator, tabs expanded, control chars
    /// replaced). May start/end with `…` when truncated.
    pub text: String,
    /// Byte ranges of matches **within `text`**, ascending, non-overlapping, on char
    /// boundaries.
    pub ranges: Vec<Range<usize>>,
}

/// All matches in one file.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FileMatches {
    /// Absolute path.
    pub path: PathBuf,
    /// Path relative to the query root.
    pub rel: PathBuf,
    /// Matching lines in file order.
    pub lines: Vec<LineMatch>,
}

/// Counters describing search progress.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct GrepStats {
    /// Files fully searched (or searched until binary data stopped them).
    pub files_searched: u64,
    /// Files with at least one match.
    pub files_with_matches: u64,
    /// Matching lines reported.
    pub matches: u64,
    /// Skipped because they are cloud placeholders (would trigger a download).
    pub skipped_cloud: u64,
    /// Skipped because larger than the size limit.
    pub skipped_large: u64,
    /// Files in which binary data (NUL) was detected; searching stopped there.
    pub binary: u64,
    /// Access-denied errors (quietly skipped).
    pub denied: u64,
    /// Other IO errors (vanished files and so on).
    pub errors: u64,
}

/// Events delivered to the sink. All carry the caller's generation.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum GrepEvent {
    /// A batch of new matches (flushed at most every `flush_interval`).
    Matches {
        /// Generation id.
        generation: u64,
        /// Files with new matches. Order across files is not deterministic.
        files: Vec<FileMatches>,
    },
    /// Progress counters, sent with flushes whenever they changed.
    Progress {
        /// Generation id.
        generation: u64,
        /// Current counters.
        stats: GrepStats,
    },
    /// Finished normally. `capped` means more matches existed than `max_matches`.
    Done {
        /// Generation id.
        generation: u64,
        /// Final counters.
        stats: GrepStats,
        /// The match cap was hit ("refine your search").
        capped: bool,
    },
    /// Stopped by the caller's cancellation token.
    Cancelled {
        /// Generation id.
        generation: u64,
        /// Counters at the time of cancellation.
        stats: GrepStats,
    },
    /// The search could not start (invalid regex or glob, ...).
    Error {
        /// Generation id.
        generation: u64,
        /// Human readable message.
        message: String,
    },
}

impl GrepEvent {
    /// Generation id of the event.
    pub fn generation(&self) -> u64 {
        match self {
            GrepEvent::Matches { generation, .. }
            | GrepEvent::Progress { generation, .. }
            | GrepEvent::Done { generation, .. }
            | GrepEvent::Cancelled { generation, .. }
            | GrepEvent::Error { generation, .. } => *generation,
        }
    }

    /// Whether this is the last event of a search.
    pub fn is_terminal(&self) -> bool {
        matches!(
            self,
            GrepEvent::Done { .. } | GrepEvent::Cancelled { .. } | GrepEvent::Error { .. }
        )
    }
}

/// Handle to a running search.
///
/// **Dropping the handle cancels the search** (like `FinderSession`), so replacing a stored
/// handle with a new one stops the old scan. Call [`detach`](Self::detach) to let a search
/// run on without a handle. A cancelled search still emits its terminal
/// [`GrepEvent::Cancelled`] afterwards, possibly after the caller has moved on: the receiving
/// side must drop events whose `generation` is not the current one.
pub struct GrepHandle {
    cancel: CancellationToken,
    join: Option<JoinHandle<()>>,
    detached: bool,
}

impl Drop for GrepHandle {
    fn drop(&mut self) {
        if !self.detached {
            self.cancel.cancel();
        }
    }
}

impl GrepHandle {
    /// Requests cancellation (non-blocking). The terminal event follows shortly.
    pub fn cancel(&self) {
        self.cancel.cancel();
    }

    /// The token controlling this search.
    pub fn token(&self) -> &CancellationToken {
        &self.cancel
    }

    /// Whether the orchestrator thread has finished (terminal event already emitted).
    pub fn is_finished(&self) -> bool {
        self.join.as_ref().is_none_or(JoinHandle::is_finished)
    }

    /// Consumes the handle without cancelling the search.
    pub fn detach(mut self) {
        self.detached = true;
    }

    /// Blocks until the search thread has ended. Do not call from the UI thread unless the
    /// search was cancelled and you accept a short wait.
    pub fn join(mut self) {
        // Joining waits for completion; never cancel as a side effect of the consuming drop.
        self.detached = true;
        if let Some(j) = self.join.take() {
            let _ = j.join();
        }
    }
}

#[derive(Default)]
struct AtomicStats {
    files_searched: AtomicU64,
    files_with_matches: AtomicU64,
    matches: AtomicU64,
    skipped_cloud: AtomicU64,
    skipped_large: AtomicU64,
    binary: AtomicU64,
    denied: AtomicU64,
    errors: AtomicU64,
}

impl AtomicStats {
    fn snapshot(&self) -> GrepStats {
        let l = |a: &AtomicU64| a.load(Ordering::Relaxed);
        GrepStats {
            files_searched: l(&self.files_searched),
            files_with_matches: l(&self.files_with_matches),
            matches: l(&self.matches),
            skipped_cloud: l(&self.skipped_cloud),
            skipped_large: l(&self.skipped_large),
            binary: l(&self.binary),
            denied: l(&self.denied),
            errors: l(&self.errors),
        }
    }

    fn io_error(&self, e: &io::Error) {
        if e.kind() == io::ErrorKind::PermissionDenied {
            self.denied.fetch_add(1, Ordering::Relaxed);
        } else {
            self.errors.fetch_add(1, Ordering::Relaxed);
        }
    }
}

/// Starts a content search on background threads and returns immediately.
///
/// Errors only if threads cannot be spawned; query problems are reported as
/// [`GrepEvent::Error`] through the sink.
pub fn spawn_search(
    query: GrepQuery,
    generation: u64,
    cancel: CancellationToken,
    sink: EventSink<GrepEvent>,
) -> io::Result<GrepHandle> {
    let token = cancel.clone();
    let join = std::thread::Builder::new()
        .name("ks-grep".into())
        .spawn(move || run_search(query, generation, token, sink))?;
    Ok(GrepHandle {
        cancel,
        join: Some(join),
        detached: false,
    })
}

fn build_matcher(q: &GrepQuery) -> Result<RegexMatcher, String> {
    let pat = if q.regex {
        q.pattern.clone()
    } else {
        regex_syntax::escape(&q.pattern)
    };
    RegexMatcherBuilder::new()
        .case_smart(true)
        .word(q.whole_word)
        .build(&pat)
        .map_err(|e| format!("invalid pattern: {}", regex_error_summary(&e.to_string())))
}

/// The regex crate's errors are multi-line (an echo of the pattern with a caret, then
/// `error: <reason>`). Only the reason fits a one-row footer and says what is wrong.
fn regex_error_summary(message: &str) -> String {
    let last = message
        .lines()
        .rev()
        .find(|l| !l.trim().is_empty())
        .unwrap_or(message)
        .trim();
    last.strip_prefix("error: ").unwrap_or(last).to_owned()
}

fn run_search(
    query: GrepQuery,
    generation: u64,
    caller_cancel: CancellationToken,
    sink: EventSink<GrepEvent>,
) {
    let stats = Arc::new(AtomicStats::default());
    if query.pattern.is_empty() {
        sink(GrepEvent::Done {
            generation,
            stats: GrepStats::default(),
            capped: false,
        });
        return;
    }
    let matcher = match build_matcher(&query) {
        Ok(m) => m,
        Err(message) => {
            sink(GrepEvent::Error {
                generation,
                message,
            });
            return;
        }
    };
    let overrides = match build_overrides(&query.root, &query.glob) {
        Ok(o) => o,
        Err(e) => {
            sink(GrepEvent::Error {
                generation,
                message: format!("invalid glob: {e}"),
            });
            return;
        }
    };

    // Internal token so hitting the cap stops the pipeline without looking like a user cancel.
    let cancel = caller_cancel.child_token();
    let capped = Arc::new(AtomicBool::new(false));
    let total_matches = Arc::new(AtomicUsize::new(0));

    let threads = if query.threads == 0 {
        std::thread::available_parallelism().map_or(4, |n| n.get())
    } else {
        query.threads
    };
    let (path_tx, path_rx) = bounded::<PathBuf>(threads * 64);
    let (res_tx, res_rx) = unbounded::<FileMatches>();

    let mut workers = Vec::with_capacity(threads);
    for i in 0..threads {
        let ctx = WorkerCtx {
            rx: path_rx.clone(),
            tx: res_tx.clone(),
            matcher: matcher.clone(),
            stats: stats.clone(),
            cancel: cancel.clone(),
            capped: capped.clone(),
            total: total_matches.clone(),
            max_matches: query.max_matches,
            max_line_chars: query.max_line_chars,
            root: query.root.clone(),
        };
        match std::thread::Builder::new()
            .name(format!("ks-grep-{i}"))
            .spawn(move || ctx.run())
        {
            Ok(h) => workers.push(h),
            Err(e) => tracing::error!(error = %e, "failed to spawn grep worker"),
        }
    }
    drop(path_rx);
    drop(res_tx);
    if workers.is_empty() {
        sink(GrepEvent::Error {
            generation,
            message: "could not start search worker threads".into(),
        });
        return;
    }

    let collector: io::Result<JoinHandle<()>> = {
        let sink = sink.clone();
        let stats = stats.clone();
        let caller_cancel = caller_cancel.clone();
        let interval = query.flush_interval;
        std::thread::Builder::new()
            .name("ks-grep-collect".into())
            .spawn(move || collect(res_rx, sink, stats, generation, interval, caller_cancel))
    };

    let walk_opts = WalkOptions {
        root: query.root.clone(),
        show_hidden: query.show_hidden,
        respect_gitignore: query.respect_gitignore,
        include_dirs: false,
        threads: 0,
        overrides,
        skip_cloud_dirs: query.skip_cloud_dirs,
    };
    let max_size = query.max_file_size;
    let summary = walk(&walk_opts, &cancel, || GrepWalkSink {
        tx: path_tx.clone(),
        stats: stats.clone(),
        max_size,
    });
    stats
        .skipped_cloud
        .fetch_add(summary.skipped_cloud, Ordering::Relaxed);
    stats.denied.fetch_add(summary.denied, Ordering::Relaxed);
    stats.errors.fetch_add(summary.errors, Ordering::Relaxed);
    drop(path_tx);

    let mut crashed = false;
    for w in workers {
        crashed |= w.join().is_err();
    }
    match collector {
        Ok(c) => {
            let _ = c.join();
        }
        Err(e) => {
            tracing::error!(error = %e, "failed to spawn grep collector");
            sink(GrepEvent::Error {
                generation,
                message: format!("could not start result collector: {e}"),
            });
            return;
        }
    }

    if crashed && !caller_cancel.is_cancelled() {
        sink(GrepEvent::Error {
            generation,
            message: "a search worker crashed; results may be incomplete".into(),
        });
        return;
    }
    let stats = stats.snapshot();
    if caller_cancel.is_cancelled() {
        sink(GrepEvent::Cancelled { generation, stats });
    } else {
        sink(GrepEvent::Done {
            generation,
            stats,
            capped: capped.load(Ordering::Relaxed),
        });
    }
}

fn collect(
    rx: crossbeam_channel::Receiver<FileMatches>,
    sink: EventSink<GrepEvent>,
    stats: Arc<AtomicStats>,
    generation: u64,
    interval: Duration,
    caller_cancel: CancellationToken,
) {
    let mut batch: Vec<FileMatches> = Vec::new();
    let mut last_flush = Instant::now();
    let mut last_stats = GrepStats::default();
    let flush = |batch: &mut Vec<FileMatches>, last_stats: &mut GrepStats| {
        if !batch.is_empty() {
            sink(GrepEvent::Matches {
                generation,
                files: std::mem::take(batch),
            });
        }
        let now = stats.snapshot();
        if now != *last_stats {
            *last_stats = now;
            sink(GrepEvent::Progress {
                generation,
                stats: now,
            });
        }
    };
    loop {
        let wait = interval.saturating_sub(last_flush.elapsed());
        match rx.recv_timeout(wait) {
            Ok(fm) => batch.push(fm),
            Err(RecvTimeoutError::Timeout) => {}
            Err(RecvTimeoutError::Disconnected) => break,
        }
        if last_flush.elapsed() >= interval {
            if caller_cancel.is_cancelled() {
                batch.clear();
            } else {
                flush(&mut batch, &mut last_stats);
            }
            last_flush = Instant::now();
        }
    }
    if caller_cancel.is_cancelled() {
        return;
    }
    flush(&mut batch, &mut last_stats);
}

/// Walker-side sink: cheap pre-filters, then hands the path to the workers.
struct GrepWalkSink {
    tx: crossbeam_channel::Sender<PathBuf>,
    stats: Arc<AtomicStats>,
    max_size: u64,
}

impl WalkSink for GrepWalkSink {
    fn entry(&mut self, entry: WalkEntry) -> bool {
        if entry.kind != EntryKind::File {
            return true;
        }
        match entry.inner.metadata() {
            Ok(md) => {
                if is_cloud_placeholder(file_attributes(&md)) {
                    self.stats.skipped_cloud.fetch_add(1, Ordering::Relaxed);
                    return true;
                }
                if md.len() > self.max_size {
                    self.stats.skipped_large.fetch_add(1, Ordering::Relaxed);
                    return true;
                }
            }
            Err(e) => {
                match e.io_error() {
                    Some(io) => self.stats.io_error(io),
                    None => {
                        self.stats.errors.fetch_add(1, Ordering::Relaxed);
                    }
                }
                return true;
            }
        }
        // A send error means all workers are gone (cancelled or capped): stop walking.
        self.tx.send(entry.into_path()).is_ok()
    }
}

struct WorkerCtx {
    rx: crossbeam_channel::Receiver<PathBuf>,
    tx: crossbeam_channel::Sender<FileMatches>,
    matcher: RegexMatcher,
    stats: Arc<AtomicStats>,
    cancel: CancellationToken,
    capped: Arc<AtomicBool>,
    total: Arc<AtomicUsize>,
    max_matches: usize,
    max_line_chars: usize,
    root: PathBuf,
}

impl WorkerCtx {
    fn run(self) {
        let mut searcher: Searcher = SearcherBuilder::new()
            .binary_detection(BinaryDetection::quit(0))
            .line_number(true)
            .build();
        while let Ok(path) = self.rx.recv() {
            if self.cancel.is_cancelled() {
                break;
            }
            #[cfg(test)]
            assert!(
                path.file_name() != Some(std::ffi::OsStr::new("__worker_crash__")),
                "forced worker crash"
            );
            let mut sink = LineSink {
                matcher: &self.matcher,
                cancel: &self.cancel,
                capped: &self.capped,
                total: &self.total,
                max_matches: self.max_matches,
                max_line_chars: self.max_line_chars,
                lines: Vec::new(),
                binary: false,
            };
            // A bug while handling one file must cost that file, not the worker (and with it
            // every file it would have searched afterwards).
            let res = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
                #[cfg(test)]
                assert!(
                    path.file_name() != Some(std::ffi::OsStr::new("__file_panic__")),
                    "forced file panic"
                );
                searcher.search_path(&self.matcher, &path, &mut sink)
            }));
            let Ok(res) = res else {
                tracing::error!(path = %path.display(), "grep panicked on a file");
                self.stats.errors.fetch_add(1, Ordering::Relaxed);
                continue;
            };
            match res {
                Ok(()) => {
                    self.stats.files_searched.fetch_add(1, Ordering::Relaxed);
                }
                Err(e) => {
                    self.stats.io_error(&e);
                    tracing::debug!(path = %path.display(), error = %e, "grep file error");
                }
            }
            if sink.binary {
                self.stats.binary.fetch_add(1, Ordering::Relaxed);
            }
            if !sink.lines.is_empty() {
                self.stats
                    .matches
                    .fetch_add(sink.lines.len() as u64, Ordering::Relaxed);
                self.stats
                    .files_with_matches
                    .fetch_add(1, Ordering::Relaxed);
                let rel = path.strip_prefix(&self.root).unwrap_or(&path).to_path_buf();
                let fm = FileMatches {
                    path,
                    rel,
                    lines: sink.lines,
                };
                if self.tx.send(fm).is_err() {
                    break;
                }
            }
        }
    }
}

struct LineSink<'a> {
    matcher: &'a RegexMatcher,
    cancel: &'a CancellationToken,
    capped: &'a AtomicBool,
    total: &'a AtomicUsize,
    max_matches: usize,
    max_line_chars: usize,
    lines: Vec<LineMatch>,
    binary: bool,
}

impl Sink for LineSink<'_> {
    type Error = io::Error;

    fn matched(&mut self, _: &Searcher, mat: &SinkMatch<'_>) -> Result<bool, io::Error> {
        if self.cancel.is_cancelled() {
            return Ok(false);
        }
        let prev = self.total.fetch_add(1, Ordering::Relaxed);
        if prev >= self.max_matches {
            self.capped.store(true, Ordering::Relaxed);
            self.cancel.cancel();
            return Ok(false);
        }
        let bytes = trim_eol(mat.bytes());
        // Match on the original bytes (what the searcher matched), then map the ranges onto
        // the lossy UTF-8 text that is displayed.
        let mut byte_ranges = Vec::new();
        if let Err(e) = self.matcher.find_iter(bytes, |m| {
            if m.start() < m.end() {
                byte_ranges.push(m.start()..m.end());
            }
            true
        }) {
            // The line matched already; keep it, just without highlights.
            tracing::debug!(error = %e, "re-match for highlights failed");
            byte_ranges.clear();
        }
        let (text, ranges) = lossy_with_ranges(bytes, &byte_ranges);
        let (text, ranges) = sanitize_line(&text, &ranges, self.max_line_chars);
        self.lines.push(LineMatch {
            line_number: mat.line_number().unwrap_or(0),
            text,
            ranges,
        });
        Ok(true)
    }

    fn binary_data(&mut self, _: &Searcher, _: u64) -> Result<bool, io::Error> {
        self.binary = true;
        Ok(true)
    }
}

/// Lossy UTF-8 conversion that also maps byte `ranges` of the input onto the output. A range
/// edge inside an invalid sequence snaps outward to the replacement character.
fn lossy_with_ranges(bytes: &[u8], ranges: &[Range<usize>]) -> (String, Vec<Range<usize>>) {
    if let Ok(s) = std::str::from_utf8(bytes) {
        return (s.to_owned(), snap_ranges(s, ranges));
    }
    let mut out = String::with_capacity(bytes.len() + 8);
    // start_map[i]: new offset of input offset i (inside an invalid run: the run start);
    // end_map[i]: same but inside an invalid run: the run end.
    let mut start_map = vec![0usize; bytes.len() + 1];
    let mut end_map = vec![0usize; bytes.len() + 1];
    let mut pos = 0usize;
    for chunk in bytes.utf8_chunks() {
        let valid = chunk.valid();
        for i in 0..valid.len() {
            // Continuation bytes (never real range edges) stay monotonic.
            let n = if valid.is_char_boundary(i) {
                out.len() + i
            } else {
                start_map[pos + i - 1]
            };
            start_map[pos + i] = n;
            end_map[pos + i] = n;
        }
        out.push_str(valid);
        pos += valid.len();
        let inv = chunk.invalid();
        if !inv.is_empty() {
            let (ns, ne) = (out.len(), out.len() + '\u{FFFD}'.len_utf8());
            for i in 0..inv.len() {
                start_map[pos + i] = ns;
                end_map[pos + i] = ne;
            }
            out.push('\u{FFFD}');
            pos += inv.len();
        }
    }
    start_map[bytes.len()] = out.len();
    end_map[bytes.len()] = out.len();
    let mapped = ranges
        .iter()
        .map(|r| start_map[r.start]..end_map[r.end])
        .filter(|r| r.start < r.end)
        .collect();
    (out, mapped)
}

/// `ranges` clamped to `text` and moved outward to char boundaries; empty ones are dropped.
/// A byte-level regex (`(?-u:©)`) can match in the middle of a character.
fn snap_ranges(text: &str, ranges: &[Range<usize>]) -> Vec<Range<usize>> {
    ranges
        .iter()
        .filter_map(|r| {
            let mut start = r.start.min(text.len());
            let mut end = r.end.min(text.len());
            while !text.is_char_boundary(start) {
                start -= 1;
            }
            while !text.is_char_boundary(end) {
                end += 1;
            }
            (start < end).then_some(start..end)
        })
        .collect()
}

fn trim_eol(mut b: &[u8]) -> &[u8] {
    if let [rest @ .., b'\n'] = b {
        b = rest;
    }
    if let [rest @ .., b'\r'] = b {
        b = rest;
    }
    b
}

/// Characters replaced by U+FFFD in displayed lines: control characters (C0, DEL, C1) and
/// invisible formatting characters that can spoof the displayed text (bidirectional
/// overrides/isolates/marks, line and paragraph separators, zero-width space, word joiner and
/// invisible operators, BOM). Zero-width (non-)joiners are kept for emoji and Indic scripts.
pub fn is_unsafe_char(c: char) -> bool {
    c.is_control()
        || matches!(c,
            '\u{061C}' | '\u{200B}' | '\u{200E}' | '\u{200F}'
            | '\u{2028}'..='\u{202E}'
            | '\u{2060}'..='\u{2064}'
            | '\u{2066}'..='\u{206F}'
            | '\u{FEFF}')
}

/// Expands tabs, replaces control characters, truncates to about `max_chars` characters
/// (windowed around the first match, with `…` markers) and remaps the match ranges.
pub fn sanitize_line(
    text: &str,
    ranges: &[Range<usize>],
    max_chars: usize,
) -> (String, Vec<Range<usize>>) {
    let max_chars = max_chars.max(8);
    // Any caller-supplied range is made safe to slice with.
    let ranges = &snap_ranges(text, ranges)[..];
    let total_chars = text.chars().count();
    // Window in bytes [ws, we).
    let (mut ws, mut we) = (0, text.len());
    let (mut lead, mut trail) = (false, false);
    if total_chars > max_chars {
        let first = ranges.first().map_or(0, |r| r.start);
        let first_char = text[..first.min(text.len())].chars().count();
        let skip = if first_char > max_chars / 2 {
            first_char - max_chars / 4
        } else {
            0
        };
        let skip = skip.min(total_chars.saturating_sub(max_chars));
        let mut it = text.char_indices().skip(skip);
        ws = it.next().map_or(text.len(), |(i, _)| i);
        we = text
            .char_indices()
            .nth(skip + max_chars)
            .map_or(text.len(), |(i, _)| i);
        lead = skip > 0;
        trail = we < text.len();
    }
    let slice = &text[ws..we];
    let mut out = String::with_capacity(slice.len() + 8);
    if lead {
        out.push('…');
    }
    // map[old_offset_in_slice] = new offset in out (valid at char starts and at the end).
    let mut map = vec![0usize; slice.len() + 1];
    for (i, c) in slice.char_indices() {
        map[i] = out.len();
        match c {
            '\t' => out.push_str("    "),
            c if is_unsafe_char(c) => out.push('\u{FFFD}'),
            c => out.push(c),
        }
    }
    map[slice.len()] = out.len();
    if trail {
        out.push('…');
    }
    let mut new_ranges = Vec::with_capacity(ranges.len());
    for r in ranges {
        let s = r.start.max(ws);
        let e = r.end.min(we);
        if s >= e {
            continue;
        }
        new_ranges.push(map[s - ws]..map[e - ws]);
    }
    (out, new_ranges)
}

/// Convenience: whether `path` lies under `root` (used by wiring to ignore stale events).
pub fn is_under(root: &Path, path: &Path) -> bool {
    path.starts_with(root)
}

#[cfg(test)]
#[allow(clippy::single_range_in_vec_init)]
mod tests {
    use super::*;

    /// Runs a search over `root` and returns the terminal event.
    fn finish(root: &std::path::Path, pattern: &str) -> GrepEvent {
        let (tx, rx) = crossbeam_channel::unbounded();
        let sink = Arc::new(move |e: GrepEvent| {
            let _ = tx.send(e);
        });
        let handle = spawn_search(
            GrepQuery::new(root, pattern),
            1,
            CancellationToken::new(),
            sink,
        )
        .unwrap();
        let mut last = None;
        while let Ok(e) = rx.recv_timeout(std::time::Duration::from_secs(30)) {
            let end = matches!(
                e,
                GrepEvent::Done { .. } | GrepEvent::Cancelled { .. } | GrepEvent::Error { .. }
            );
            last = Some(e);
            if end {
                break;
            }
        }
        handle.join();
        last.expect("search must terminate")
    }

    #[test]
    fn a_panic_on_one_file_costs_that_file_and_counts_one_error() {
        let t = tempfile::tempdir().unwrap();
        std::fs::write(
            t.path().join("__file_panic__"),
            "needle
",
        )
        .unwrap();
        std::fs::write(
            t.path().join("ok.txt"),
            "needle
",
        )
        .unwrap();
        let GrepEvent::Done { stats, .. } = finish(t.path(), "needle") else {
            panic!("the search must still finish normally");
        };
        assert_eq!(stats.errors, 1);
        assert_eq!(stats.files_searched, 1, "the other file is still searched");
    }

    #[test]
    fn a_crashed_worker_is_reported_as_an_error_event() {
        let t = tempfile::tempdir().unwrap();
        std::fs::write(
            t.path().join("__worker_crash__"),
            "needle
",
        )
        .unwrap();
        std::fs::write(
            t.path().join("ok.txt"),
            "needle
",
        )
        .unwrap();
        let GrepEvent::Error { message, .. } = finish(t.path(), "needle") else {
            panic!("a worker crash must not look like a clean Done");
        };
        assert!(message.contains("crashed"), "{message}");
    }

    #[test]
    fn sanitize_never_panics_on_any_range_over_multibyte_text() {
        let samples = [
            "é".repeat(400),
            format!("{}{}", "日本語".repeat(120), "x"),
            format!("a{}b", "😀é\u{202E}".repeat(150)),
            "\t\u{0}é".repeat(200),
            "é".to_owned(),
            String::new(),
        ];
        for text in &samples {
            let n = text.len();
            let mut starts: Vec<usize> = (0..=n.min(40)).collect();
            starts.extend([
                n / 2,
                n / 2 + 1,
                n.saturating_sub(2),
                n.saturating_sub(1),
                n,
                n + 3,
            ]);
            for &a in &starts {
                for &b in &starts {
                    for max in [0, 8, 50, 300] {
                        let r = [a..b];
                        let (t, ranges) = sanitize_line(text, &r, max);
                        for rg in &ranges {
                            assert!(rg.start < rg.end && rg.end <= t.len());
                            assert!(t.is_char_boundary(rg.start) && t.is_char_boundary(rg.end));
                        }
                        let (lossy, ranges) =
                            lossy_with_ranges(text.as_bytes(), &[a.min(n)..b.min(n)]);
                        let _ = sanitize_line(&lossy, &ranges, max);
                    }
                }
            }
        }
    }

    #[test]
    fn byte_level_match_inside_a_char_is_snapped_outward() {
        let (text, ranges) = lossy_with_ranges("aéb".as_bytes(), &[2..3]);
        assert_eq!(&text[ranges[0].clone()], "é");
    }

    #[test]
    fn regex_errors_are_reduced_to_their_reason() {
        let mut q = GrepQuery::new(".", "(");
        q.regex = true;
        let msg = build_matcher(&q).unwrap_err();
        assert!(!msg.contains('\n'), "{msg:?}");
        assert!(msg.starts_with("invalid pattern: "), "{msg:?}");
        assert!(msg.contains("unclosed group"), "{msg:?}");
        assert!(!msg.contains("(?:"), "{msg:?}");
    }

    #[test]
    fn trim_eol_variants() {
        assert_eq!(trim_eol(b"abc\r\n"), b"abc");
        assert_eq!(trim_eol(b"abc\n"), b"abc");
        assert_eq!(trim_eol(b"abc"), b"abc");
    }

    #[test]
    fn sanitize_tabs_and_ranges() {
        let (t, r) = sanitize_line("\tfoo\tbar", &[1..4, 5..8], 100);
        assert_eq!(t, "    foo    bar");
        assert_eq!(&t[r[0].clone()], "foo");
        assert_eq!(&t[r[1].clone()], "bar");
    }

    #[test]
    fn sanitize_truncates_around_match() {
        let line = format!("{}NEEDLE{}", "a".repeat(500), "b".repeat(500));
        let (t, r) = sanitize_line(&line, &[500..506], 100);
        assert!(t.chars().count() <= 102, "{}", t.chars().count());
        assert!(t.starts_with('…') && t.ends_with('…'));
        assert_eq!(&t[r[0].clone()], "NEEDLE");
    }

    #[test]
    fn sanitize_match_at_start_truncates_end_only() {
        let line = format!("NEEDLE{}", "b".repeat(500));
        let (t, r) = sanitize_line(&line, &[0..6], 50);
        assert!(!t.starts_with('…') && t.ends_with('…'));
        assert_eq!(&t[r[0].clone()], "NEEDLE");
    }

    #[test]
    fn sanitize_unicode_safe() {
        let line = format!("{}щит{}", "ы".repeat(300), "ю".repeat(300));
        let start = "ы".len() * 300;
        let (t, r) = sanitize_line(&line, &[start..start + "щит".len()], 40);
        assert_eq!(&t[r[0].clone()], "щит");
    }

    #[test]
    fn bidi_and_invisible_chars_replaced() {
        let src = "a\u{202E}b\u{2066}c\u{200B}d\u{FEFF}\u{2028}e";
        let (t, r) = sanitize_line(src, &[0..1], 100);
        for c in ['\u{202E}', '\u{2066}', '\u{200B}', '\u{FEFF}', '\u{2028}'] {
            assert!(!t.contains(c), "{c:?} survived");
        }
        assert_eq!(&t[r[0].clone()], "a");
        // Emoji joiners survive.
        let (t, _) = sanitize_line("\u{1F468}\u{200D}\u{1F469}", &[], 100);
        assert!(t.contains('\u{200D}'));
    }

    #[test]
    fn lossy_ranges_map_over_invalid_bytes() {
        let bytes = b"caf\xE9 needle \xFF\xFEend";
        let (t, r) = lossy_with_ranges(bytes, &[5..11, 12..14]);
        assert_eq!(&t[r[0].clone()], "needle");
        assert_eq!(&t[r[1].clone()], "\u{FFFD}\u{FFFD}");
        let (t, r) = lossy_with_ranges("h\u{e9}llo".as_bytes(), &[0..3]);
        assert_eq!(&t[r[0].clone()], "h\u{e9}");
    }

    #[test]
    fn control_chars_replaced() {
        let (t, _) = sanitize_line("a\u{1b}[31mb", &[], 100);
        assert!(!t.contains('\u{1b}'));
    }
}
