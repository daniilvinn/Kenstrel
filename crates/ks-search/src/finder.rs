//! Fuzzy file finder (`ctrl-p`, PLAN.md section 6.2): a [`FinderSession`] streams walker
//! results into a [`nucleo::Nucleo`] matcher so results appear while the walk is still running.
//!
//! # Usage
//!
//! ```no_run
//! use std::sync::Arc;
//! use ks_search::{FinderOptions, FinderSession, WalkCache};
//!
//! let cache = Arc::new(WalkCache::new());
//! let notify: ks_search::Notify = Arc::new(|| { /* wake the UI loop: send Msg::FinderWake */ });
//! let mut s = FinderSession::start(FinderOptions::new("C:\\src"), 7, Some(cache), notify);
//! s.set_query("main rs");
//! // On every wake-up / frame:
//! let st = s.tick(10);
//! if st.changed {
//!     let rows = s.results(0, 20); // top 20 with highlight indices
//!     # let _ = rows;
//! }
//! ```
//!
//! # Threading
//!
//! The walk runs on its own `std::thread` (which itself spawns walker threads); matching runs
//! on nucleo's internal thread pool. [`FinderSession::tick`] waits at most `timeout` ms for the
//! matcher; every other method is non-blocking. Dropping the session cancels the walk (it does
//! not join).

use std::collections::{HashMap, HashSet};
use std::ffi::OsStr;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;

use nucleo::pattern::{CaseMatching, Normalization};
use nucleo::{Config, Injector, Matcher, Nucleo, Utf32String};
use parking_lot::Mutex;
use tokio_util::sync::CancellationToken;

use crate::cache::{fold_hash, CacheKey, RawNames, WalkCache, Walked};
use crate::walk::{walk, EntryKind, WalkEntry, WalkOptions, WalkSink, WalkSummary};
use crate::Notify;

/// A path known to the finder, relative to the session root (OS separators).
///
/// Stored as one reference-counted string whose first byte tags the kind (and whether the
/// name is a lossy conversion), so the walk cache and the matcher share a single allocation
/// per path (PLAN P8 memory budget).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FinderItem {
    repr: Arc<str>,
}

impl FinderItem {
    fn tag(kind: EntryKind, lossy: bool) -> char {
        let c = match kind {
            EntryKind::File => 'f',
            EntryKind::Dir => 'd',
            EntryKind::Symlink => 'l',
        };
        if lossy {
            c.to_ascii_uppercase()
        } else {
            c
        }
    }

    fn build(rel: &str, kind: EntryKind, lossy: bool) -> Self {
        let mut s = String::with_capacity(rel.len() + 1);
        s.push(Self::tag(kind, lossy));
        s.push_str(rel);
        Self { repr: s.into() }
    }

    /// An item whose name is valid Unicode.
    pub fn new(rel: impl AsRef<str>, kind: EntryKind) -> Self {
        Self::build(rel.as_ref(), kind, false)
    }

    /// An item from a relative path. A name that is not valid Unicode is stored lossily
    /// (see [`FinderItem::is_lossy`]); the session keeps the exact name on the side.
    pub fn from_rel_path(rel: &Path, kind: EntryKind) -> Self {
        Self::split_rel_path(rel, kind).0
    }

    pub(crate) fn split_rel_path(rel: &Path, kind: EntryKind) -> (Self, Option<Box<OsStr>>) {
        match rel.to_str() {
            Some(s) => (Self::new(s, kind), None),
            None => (
                Self::build(&rel.to_string_lossy(), kind, true),
                Some(rel.as_os_str().into()),
            ),
        }
    }

    /// Path relative to the root.
    pub fn rel(&self) -> &str {
        &self.repr[1..]
    }

    /// Entry kind.
    pub fn kind(&self) -> EntryKind {
        match self.repr.as_bytes()[0].to_ascii_lowercase() {
            b'd' => EntryKind::Dir,
            b'l' => EntryKind::Symlink,
            _ => EntryKind::File,
        }
    }

    /// Whether `rel` is a lossy conversion of a name that is not valid Unicode.
    pub fn is_lossy(&self) -> bool {
        self.repr.as_bytes()[0].is_ascii_uppercase()
    }

    /// The same entry under a new path. The exact-name information is dropped.
    pub fn with_rel(&self, rel: &str) -> Self {
        Self::build(rel, self.kind(), false)
    }
}

/// Session configuration.
#[derive(Debug, Clone)]
pub struct FinderOptions {
    /// Directory to index.
    pub root: PathBuf,
    /// Include hidden files (`alt-.`).
    pub show_hidden: bool,
    /// Honour gitignore (`alt-g` toggles).
    pub respect_gitignore: bool,
    /// Also index directories.
    pub include_dirs: bool,
    /// Matcher threads; `None` = one per core.
    pub match_threads: Option<usize>,
    /// Walker threads; `0` = automatic.
    pub walk_threads: usize,
    /// Do not descend into cloud-placeholder directories (see
    /// [`WalkOptions::skip_cloud_dirs`]). Off by default. Not part of the cache key.
    pub skip_cloud_dirs: bool,
    /// When the walk cache serves a session, re-walk the root in the background and merge
    /// differences into the session (default on).
    pub refresh_cached: bool,
}

impl FinderOptions {
    /// Defaults: files only, hidden off, gitignore on.
    pub fn new(root: impl Into<PathBuf>) -> Self {
        Self {
            root: root.into(),
            show_hidden: false,
            respect_gitignore: true,
            include_dirs: false,
            match_threads: None,
            walk_threads: 0,
            skip_cloud_dirs: false,
            refresh_cached: true,
        }
    }

    fn walk_options(&self) -> WalkOptions {
        WalkOptions {
            root: self.root.clone(),
            show_hidden: self.show_hidden,
            respect_gitignore: self.respect_gitignore,
            include_dirs: self.include_dirs,
            threads: self.walk_threads,
            overrides: None,
            skip_cloud_dirs: self.skip_cloud_dirs,
        }
    }

    /// The cache key for these options.
    pub fn cache_key(&self) -> CacheKey {
        CacheKey {
            root: self.root.clone(),
            show_hidden: self.show_hidden,
            respect_gitignore: self.respect_gitignore,
            include_dirs: self.include_dirs,
        }
    }
}

/// Result of [`FinderSession::tick`].
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct FinderStatus {
    /// The snapshot changed: re-read [`FinderSession::results`] and the counts.
    pub changed: bool,
    /// The matcher is still working; keep ticking (for example on the next frame).
    pub matching: bool,
    /// The walk is still producing paths.
    pub walking: bool,
    /// A background re-walk of a cached root is in flight (the results shown are the cache).
    pub refreshing: bool,
}

impl FinderStatus {
    /// Whether the caller should keep ticking.
    pub fn busy(&self) -> bool {
        self.matching || self.walking || self.refreshing
    }
}

/// One visible result row.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FinderResult {
    /// Path relative to the root (OS separators).
    pub rel: String,
    /// Absolute path.
    pub path: PathBuf,
    /// Entry kind.
    pub kind: EntryKind,
    /// Sorted, deduplicated character indices into `rel` that matched (empty for an empty
    /// query), for highlighting.
    pub indices: Vec<u32>,
}

struct WalkShared {
    walking: AtomicBool,
    summary: Mutex<Option<WalkSummary>>,
}

/// What a background re-walk of a cached root found.
struct RefreshOutcome {
    /// Items that were not in the cached walk.
    added: Vec<FinderItem>,
    /// The complete fresh walk, only when something in the cache no longer exists (nucleo
    /// cannot remove items, so the matcher is rebuilt from it).
    full: Option<Arc<Walked>>,
    /// Exact names of non-Unicode entries among `added`.
    raws: RawNames,
}

#[derive(Default)]
struct RefreshShared {
    running: AtomicBool,
    pending: Mutex<Option<RefreshOutcome>>,
}

/// A running (or cached) fuzzy-find session. See the module docs.
pub struct FinderSession {
    generation: u64,
    opts: FinderOptions,
    nucleo: Nucleo<FinderItem>,
    injector: Injector<FinderItem>,
    query: String,
    matcher: Matcher,
    cancel: CancellationToken,
    shared: Arc<WalkShared>,
    refresh: Arc<RefreshShared>,
    /// Exact names of non-Unicode entries (by lossy `rel`).
    raws: Arc<Mutex<RawNames>>,
    notify: Notify,
    from_cache: bool,
}

fn fill(item: &FinderItem, cols: &mut [Utf32String]) {
    cols[0] = Utf32String::from(item.rel());
}

/// Per-walker-thread sink: injects into nucleo immediately and buffers for the cache.
struct InjectSink {
    root: PathBuf,
    injector: Injector<FinderItem>,
    local: Vec<FinderItem>,
    local_raws: Vec<(Box<str>, Box<OsStr>)>,
    collected: Arc<Mutex<Collected>>,
}

#[derive(Default)]
struct Collected {
    chunks: Vec<Vec<FinderItem>>,
    raws: RawNames,
}

impl WalkSink for InjectSink {
    fn entry(&mut self, entry: WalkEntry) -> bool {
        let (item, raw) = FinderItem::split_rel_path(entry.rel(&self.root), entry.kind);
        if let Some(raw) = raw {
            self.local_raws.push((item.rel().into(), raw));
        }
        self.injector.push(item.clone(), fill);
        self.local.push(item);
        true
    }
}

impl Drop for InjectSink {
    fn drop(&mut self) {
        let mut c = self.collected.lock();
        if !self.local.is_empty() {
            c.chunks.push(std::mem::take(&mut self.local));
        }
        c.raws.extend(self.local_raws.drain(..));
    }
}

/// What one walker thread of a background re-walk found.
#[derive(Default)]
struct RefreshPart {
    items: Vec<FinderItem>,
    added: Vec<FinderItem>,
    raws: RawNames,
    hits: usize,
}

/// Per-walker-thread sink of a background re-walk: reuses the cached allocation of every path
/// that is still there, so the transient cost of a refresh is small.
struct RefreshSink {
    root: PathBuf,
    known: Arc<HashMap<u64, u32>>,
    cached: Arc<Walked>,
    scratch: String,
    local: RefreshPart,
    out: Arc<Mutex<Vec<RefreshPart>>>,
}

impl WalkSink for RefreshSink {
    fn entry(&mut self, entry: WalkEntry) -> bool {
        let rel = entry.rel(&self.root);
        let text = rel.to_string_lossy();
        let hash = fold_hash(&mut self.scratch, &text);
        if let Some(&idx) = self.known.get(&hash) {
            let old = &self.cached.items[idx as usize];
            if old.kind() == entry.kind {
                self.local.items.push(old.clone());
                self.local.hits += 1;
                return true;
            }
        }
        let (item, raw) = FinderItem::split_rel_path(rel, entry.kind);
        if let Some(raw) = raw {
            self.local.raws.insert(item.rel().into(), raw);
        }
        self.local.items.push(item.clone());
        self.local.added.push(item);
        true
    }
}

impl Drop for RefreshSink {
    fn drop(&mut self) {
        self.out.lock().push(std::mem::take(&mut self.local));
    }
}

/// Feeds `walked` into `injector` from a helper thread, clearing `shared.walking` at the end.
fn spawn_feed(
    walked: Arc<Walked>,
    injector: Injector<FinderItem>,
    cancel: CancellationToken,
    shared: Arc<WalkShared>,
    notify: Notify,
) {
    let (feed_walked, feed_injector) = (walked.clone(), injector.clone());
    let (feed_cancel, feed_shared, feed_notify) = (cancel, shared.clone(), notify.clone());
    let feed = move || {
        for chunk in feed_walked.items.chunks(8192) {
            if feed_cancel.is_cancelled() {
                break;
            }
            for e in chunk {
                feed_injector.push(e.clone(), fill);
            }
            feed_notify();
        }
        feed_shared.walking.store(false, Ordering::Release);
        feed_notify();
    };
    if let Err(e) = std::thread::Builder::new()
        .name("ks-finder-feed".into())
        .spawn(feed)
    {
        // Without a helper thread inject inline: slower, but never loses results.
        tracing::error!(error = %e, "failed to spawn finder feed thread; injecting inline");
        for it in &walked.items {
            injector.push(it.clone(), fill);
        }
        shared.walking.store(false, Ordering::Release);
        notify();
    }
}

/// Re-walks a cached root in the background and reports what changed since the cache was
/// filled. The result is stored in the cache and handed to the session through `refresh`.
fn spawn_refresh(
    opts: &FinderOptions,
    cached: Arc<Walked>,
    cache: Arc<WalkCache>,
    cancel: CancellationToken,
    refresh: Arc<RefreshShared>,
    notify: Notify,
) {
    let walk_opts = opts.walk_options();
    let key = opts.cache_key();
    refresh.running.store(true, Ordering::Release);
    let thread_refresh = refresh.clone();
    let body = move || {
        let refresh = thread_refresh;
        let finish = |outcome: Option<RefreshOutcome>| {
            *refresh.pending.lock() = outcome;
            refresh.running.store(false, Ordering::Release);
            notify();
        };
        if !walk_opts.root.is_dir() {
            // Do not mistake an unreadable or vanished root for an emptied one.
            return finish(None);
        }
        let mut scratch = String::new();
        let mut known: HashMap<u64, u32> = HashMap::with_capacity(cached.items.len());
        for (i, item) in cached.items.iter().enumerate() {
            known.insert(fold_hash(&mut scratch, item.rel()), i as u32);
        }
        let known = Arc::new(known);
        let out: Arc<Mutex<Vec<RefreshPart>>> = Arc::default();
        let summary = walk(&walk_opts, &cancel, || RefreshSink {
            root: walk_opts.root.clone(),
            known: known.clone(),
            cached: cached.clone(),
            scratch: String::new(),
            local: RefreshPart::default(),
            out: out.clone(),
        });
        if summary.cancelled {
            return finish(None);
        }
        let mut items = Vec::new();
        let mut added = Vec::new();
        let mut raws = RawNames::new();
        let mut hits = 0;
        for part in out.lock().drain(..) {
            items.extend(part.items);
            added.extend(part.added);
            raws.extend(part.raws);
            hits += part.hits;
        }
        drop(known);
        let removed = hits < cached.items.len();
        if added.is_empty() && !removed {
            return finish(None);
        }
        let mut all_raws = cached.raws.clone();
        if removed && !all_raws.is_empty() {
            // Keep the exact names of the non-Unicode entries that survived.
            let alive: HashSet<&str> = items.iter().map(|i| i.rel()).collect();
            all_raws.retain(|k, _| alive.contains(&**k));
        }
        all_raws.extend(raws.iter().map(|(k, v)| (k.clone(), v.clone())));
        let fresh = Arc::new(Walked {
            items,
            raws: all_raws,
        });
        cache.put_walked_if_unchanged(key, &cached, fresh.clone());
        finish(Some(RefreshOutcome {
            added,
            full: removed.then_some(fresh),
            raws,
        }));
    };
    if let Err(e) = std::thread::Builder::new()
        .name("ks-finder-refresh".into())
        .spawn(body)
    {
        tracing::error!(error = %e, "failed to spawn finder refresh thread");
        refresh.running.store(false, Ordering::Release);
    }
}

impl FinderSession {
    /// Starts a session. If `cache` holds a completed walk for the same root and toggles the
    /// items are injected immediately and, unless [`FinderOptions::refresh_cached`] is off, a
    /// background re-walk reconciles the session (and the cache) with the disk; otherwise a
    /// walk thread is spawned and, when it completes uncancelled, stored in the cache.
    ///
    /// `notify` is called (from any thread) whenever new results may be available; it should
    /// only schedule a [`tick`](Self::tick), never do work. `generation` is returned by
    /// [`generation`](Self::generation) so the wiring can discard stale sessions.
    pub fn start(
        opts: FinderOptions,
        generation: u64,
        cache: Option<Arc<WalkCache>>,
        notify: Notify,
    ) -> Self {
        Self::start_with_token(opts, generation, cache, notify, CancellationToken::new())
    }

    /// Like [`start`](Self::start) but with a caller-supplied cancellation token (cancelling
    /// it stops the walk and the cache feed; the session also cancels it on drop).
    ///
    /// On a cache hit the cached items are injected from a helper thread, so this call never
    /// does O(n) work; [`walking`](Self::walking) stays `true` until the feed has finished.
    pub fn start_with_token(
        opts: FinderOptions,
        generation: u64,
        cache: Option<Arc<WalkCache>>,
        notify: Notify,
        cancel: CancellationToken,
    ) -> Self {
        let nucleo_notify = notify.clone();
        let nucleo = Nucleo::new(
            Config::DEFAULT.match_paths(),
            Arc::new(move || nucleo_notify()),
            opts.match_threads,
            1,
        );
        let injector = nucleo.injector();
        let shared = Arc::new(WalkShared {
            walking: AtomicBool::new(true),
            summary: Mutex::new(None),
        });
        let refresh = Arc::new(RefreshShared::default());
        let raws: Arc<Mutex<RawNames>> = Arc::default();
        let key = opts.cache_key();

        let cached = cache.as_ref().and_then(|c| c.get(&key));
        let from_cache = cached.is_some();
        if let Some(entries) = cached {
            raws.lock().extend(entries.raws.clone());
            spawn_feed(
                entries.clone(),
                injector.clone(),
                cancel.clone(),
                shared.clone(),
                notify.clone(),
            );
            if let (true, Some(cache)) = (opts.refresh_cached, cache) {
                spawn_refresh(
                    &opts,
                    entries,
                    cache,
                    cancel.clone(),
                    refresh.clone(),
                    notify.clone(),
                );
            }
        } else {
            let walk_opts = opts.walk_options();
            let thread_cancel = cancel.clone();
            let thread_shared = shared.clone();
            let thread_injector = injector.clone();
            let thread_raws = raws.clone();
            let thread_notify = notify.clone();
            let spawned = std::thread::Builder::new()
                .name("ks-finder-walk".into())
                .spawn(move || {
                    let notify = thread_notify;
                    let collected: Arc<Mutex<Collected>> = Arc::default();
                    let summary = walk(&walk_opts, &thread_cancel, || InjectSink {
                        root: walk_opts.root.clone(),
                        injector: thread_injector.clone(),
                        local: Vec::with_capacity(1024),
                        local_raws: Vec::new(),
                        collected: collected.clone(),
                    });
                    if !summary.cancelled {
                        let Collected { chunks, raws } = std::mem::take(&mut *collected.lock());
                        thread_raws
                            .lock()
                            .extend(raws.iter().map(|(k, v)| (k.clone(), v.clone())));
                        if let Some(cache) = cache {
                            let all: Vec<FinderItem> = chunks.into_iter().flatten().collect();
                            cache.put_walked(key, Arc::new(Walked { items: all, raws }));
                        }
                    }
                    tracing::debug!(?summary, "finder walk finished");
                    *thread_shared.summary.lock() = Some(summary);
                    thread_shared.walking.store(false, Ordering::Release);
                    notify();
                });
            if let Err(e) = spawned {
                tracing::error!(error = %e, "failed to spawn finder walk thread");
                shared.walking.store(false, Ordering::Release);
            }
        }

        Self {
            generation,
            opts,
            nucleo,
            injector,
            query: String::new(),
            matcher: Matcher::new(Config::DEFAULT.match_paths()),
            cancel,
            shared,
            refresh,
            raws,
            notify,
            from_cache,
        }
    }

    /// Creates a session over an already known list of relative paths (no walking, no cache).
    /// Useful for tests, benchmarks and for feeding results of an external index.
    pub fn from_items(
        opts: FinderOptions,
        generation: u64,
        items: impl IntoIterator<Item = FinderItem>,
        notify: Notify,
    ) -> Self {
        let nucleo_notify = notify.clone();
        let nucleo = Nucleo::new(
            Config::DEFAULT.match_paths(),
            Arc::new(move || nucleo_notify()),
            opts.match_threads,
            1,
        );
        let injector = nucleo.injector();
        for item in items {
            injector.push(item, fill);
        }
        notify();
        Self {
            generation,
            opts,
            nucleo,
            injector,
            query: String::new(),
            matcher: Matcher::new(Config::DEFAULT.match_paths()),
            cancel: CancellationToken::new(),
            shared: Arc::new(WalkShared {
                walking: AtomicBool::new(false),
                summary: Mutex::new(None),
            }),
            refresh: Arc::default(),
            raws: Arc::default(),
            notify,
            from_cache: false,
        }
    }

    /// Caller-supplied generation id.
    pub fn generation(&self) -> u64 {
        self.generation
    }

    /// Options this session was started with.
    pub fn options(&self) -> &FinderOptions {
        &self.opts
    }

    /// Session root.
    pub fn root(&self) -> &Path {
        &self.opts.root
    }

    /// Whether the items came from the walk cache (no walk performed).
    pub fn from_cache(&self) -> bool {
        self.from_cache
    }

    /// Current query.
    pub fn query(&self) -> &str {
        &self.query
    }

    /// Updates the query (nucleo pattern syntax, smart case). Cheap: when the new query only
    /// appends to the old one, nucleo re-scores only the previous matches. Call
    /// [`tick`](Self::tick) afterwards.
    pub fn set_query(&mut self, query: &str) {
        let append = query.starts_with(self.query.as_str()) && !self.query.is_empty();
        self.nucleo
            .pattern
            .reparse(0, query, CaseMatching::Smart, Normalization::Smart, append);
        self.query.clear();
        self.query.push_str(query);
    }

    /// Drives the matcher, waiting at most `timeout_ms` (10 is a good value) for it.
    pub fn tick(&mut self, timeout_ms: u64) -> FinderStatus {
        let applied = self.apply_refresh();
        let st = self.nucleo.tick(timeout_ms);
        FinderStatus {
            changed: st.changed || applied,
            matching: st.running,
            walking: self.walking(),
            refreshing: self.refreshing(),
        }
    }

    /// Applies the result of the background re-walk once the initial feed is complete.
    fn apply_refresh(&mut self) -> bool {
        if self.walking() {
            return false;
        }
        let Some(outcome) = self.refresh.pending.lock().take() else {
            return false;
        };
        self.raws.lock().extend(outcome.raws);
        match outcome.full {
            Some(full) => {
                // Something disappeared: rebuild the matcher from the fresh walk. The old
                // results stay on screen until the new items are matched.
                self.nucleo.restart(false);
                self.injector = self.nucleo.injector();
                self.shared.walking.store(true, Ordering::Release);
                spawn_feed(
                    full,
                    self.injector.clone(),
                    self.cancel.clone(),
                    self.shared.clone(),
                    self.notify.clone(),
                );
            }
            None => {
                for item in outcome.added {
                    self.injector.push(item, fill);
                }
            }
        }
        true
    }

    /// Whether a background re-walk of a cached root is still in flight (or its result is not
    /// applied yet).
    pub fn refreshing(&self) -> bool {
        self.refresh.running.load(Ordering::Acquire) || self.refresh.pending.lock().is_some()
    }

    /// Whether the walker is still running.
    pub fn walking(&self) -> bool {
        self.shared.walking.load(Ordering::Acquire)
    }

    /// Walk summary (denied counts etc.) once the walk is done and was not served from cache.
    pub fn walk_summary(&self) -> Option<WalkSummary> {
        *self.shared.summary.lock()
    }

    /// Number of items matching the query in the current snapshot.
    pub fn matched_count(&self) -> u32 {
        self.nucleo.snapshot().matched_item_count()
    }

    /// Number of items considered in the current snapshot.
    pub fn total_count(&self) -> u32 {
        self.nucleo.snapshot().item_count()
    }

    /// Rows `offset..offset + limit` of the ranked matches in the current snapshot, with
    /// highlight indices. Pass `(0, n)` for the top n. Cost is proportional to `limit`.
    pub fn results(&mut self, offset: u32, limit: u32) -> Vec<FinderResult> {
        let snap = self.nucleo.snapshot();
        let total = snap.matched_item_count();
        let start = offset.min(total);
        let end = start.saturating_add(limit).min(total);
        let pattern = snap.pattern().column_pattern(0);
        let mut out = Vec::with_capacity((end - start) as usize);
        for item in snap.matched_items(start..end) {
            let mut indices = Vec::new();
            if !pattern.atoms.is_empty() {
                pattern.indices(
                    item.matcher_columns[0].slice(..),
                    &mut self.matcher,
                    &mut indices,
                );
                indices.sort_unstable();
                indices.dedup();
            }
            let rel = item.data.rel();
            let exact = if item.data.is_lossy() {
                self.raws.lock().get(rel).cloned()
            } else {
                None
            };
            out.push(FinderResult {
                rel: rel.to_owned(),
                path: match exact {
                    Some(raw) => self.opts.root.join(&*raw),
                    None => self.opts.root.join(rel),
                },
                kind: item.data.kind(),
                indices,
            });
        }
        out
    }

    /// Cancels the walk (matching state is kept; results found so far stay available).
    pub fn cancel(&self) {
        self.cancel.cancel();
    }
}

impl Drop for FinderSession {
    fn drop(&mut self) {
        self.cancel.cancel();
    }
}

#[cfg(test)]
#[allow(clippy::print_stderr)]
mod tests {
    use super::*;
    use std::fs;
    use std::time::{Duration, Instant};

    fn touch(root: &Path, rel: &str) {
        let p = root.join(rel);
        fs::create_dir_all(p.parent().unwrap()).unwrap();
        fs::write(p, "x").unwrap();
    }

    fn noop() -> Notify {
        Arc::new(|| {})
    }

    /// Ticks until the walk and the matcher are idle.
    fn settle(s: &mut FinderSession) {
        let t = Instant::now();
        loop {
            let st = s.tick(10);
            if !st.busy() {
                // One more tick so the final snapshot is taken.
                s.tick(10);
                return;
            }
            assert!(
                t.elapsed() < Duration::from_secs(20),
                "finder did not settle"
            );
        }
    }

    fn tree() -> tempfile::TempDir {
        let t = tempfile::tempdir().unwrap();
        touch(t.path(), "src/main.rs");
        touch(t.path(), "src/lib.rs");
        touch(t.path(), "docs/readme.md");
        touch(t.path(), "main_test.txt");
        t
    }

    #[test]
    fn streams_and_matches_with_indices() {
        let t = tree();
        let mut s = FinderSession::start(FinderOptions::new(t.path()), 3, None, noop());
        assert_eq!(s.generation(), 3);
        s.set_query("main");
        settle(&mut s);
        assert_eq!(s.total_count(), 4);
        let r = s.results(0, 10);
        assert_eq!(r.len(), 2);
        // File-name match in src/main.rs ranks above main_test.txt? Both match; just check
        // indices cover "main".
        for row in &r {
            let chars: Vec<char> = row.rel.chars().collect();
            let m: String = row.indices.iter().map(|&i| chars[i as usize]).collect();
            assert_eq!(m, "main");
            assert!(row.path.starts_with(t.path()));
        }
        assert!(s.walk_summary().is_some());
    }

    #[test]
    fn empty_query_lists_everything_without_indices() {
        let t = tree();
        let mut s = FinderSession::start(FinderOptions::new(t.path()), 1, None, noop());
        settle(&mut s);
        assert_eq!(s.matched_count(), 4);
        assert!(s.results(0, 100).iter().all(|r| r.indices.is_empty()));
        assert_eq!(s.results(2, 100).len(), 2);
        assert!(s.results(50, 10).is_empty());
    }

    #[test]
    fn filename_weighted_higher() {
        let t = tempfile::tempdir().unwrap();
        touch(t.path(), "config/other/stuff.txt");
        touch(t.path(), "a/config.toml");
        let mut s = FinderSession::start(FinderOptions::new(t.path()), 1, None, noop());
        s.set_query("config");
        settle(&mut s);
        let r = s.results(0, 5);
        assert_eq!(r[0].rel.replace('\\', "/"), "a/config.toml");
    }

    #[test]
    fn append_then_backspace() {
        let t = tree();
        let mut s = FinderSession::start(FinderOptions::new(t.path()), 1, None, noop());
        settle(&mut s);
        s.set_query("m");
        settle(&mut s);
        let a = s.matched_count();
        s.set_query("ma");
        settle(&mut s);
        s.set_query("mai");
        settle(&mut s);
        s.set_query("m");
        settle(&mut s);
        assert_eq!(s.matched_count(), a);
        s.set_query("zzzz");
        settle(&mut s);
        assert_eq!(s.matched_count(), 0);
    }

    #[test]
    fn second_session_served_from_cache_and_updates_apply() {
        let t = tree();
        let cache = Arc::new(WalkCache::new());
        let opts = FinderOptions::new(t.path());
        let mut s1 = FinderSession::start(opts.clone(), 1, Some(cache.clone()), noop());
        assert!(!s1.from_cache());
        settle(&mut s1);
        assert!(cache.contains(&opts.cache_key()));

        let mut s2 = FinderSession::start(opts.clone(), 2, Some(cache.clone()), noop());
        assert!(s2.from_cache());
        settle(&mut s2);
        assert!(!s2.walking());
        assert_eq!(s2.total_count(), 4);

        cache.apply_changes(&[
            crate::PathChange::Added(t.path().join("new.rs"), EntryKind::File),
            crate::PathChange::Removed(t.path().join("src")),
        ]);
        // The changes above are synthetic (nothing happened on disk): serve the cache only.
        let mut cache_only = opts;
        cache_only.refresh_cached = false;
        let mut s3 = FinderSession::start(cache_only, 3, Some(cache), noop());
        settle(&mut s3);
        assert_eq!(s3.total_count(), 3);
        s3.set_query("new");
        settle(&mut s3);
        assert_eq!(s3.results(0, 5)[0].rel, "new.rs");
    }

    #[test]
    fn reopen_serves_cache_then_merges_background_changes() {
        let t = tree();
        let cache = Arc::new(WalkCache::new());
        let opts = FinderOptions::new(t.path());
        let mut s1 = FinderSession::start(opts.clone(), 1, Some(cache.clone()), noop());
        settle(&mut s1);
        assert_eq!(s1.total_count(), 4);
        drop(s1);

        // Deep changes the displayed-directory diffs never see.
        touch(t.path(), "docs/deep/er/new_file.md");
        fs::remove_file(t.path().join("src/lib.rs")).unwrap();

        let mut s2 = FinderSession::start(opts.clone(), 2, Some(cache.clone()), noop());
        assert!(s2.from_cache());
        settle(&mut s2);
        assert!(!s2.refreshing());
        s2.set_query("");
        settle(&mut s2);
        let rels: Vec<String> = s2
            .results(0, 50)
            .into_iter()
            .map(|r| r.rel.replace('\\', "/"))
            .collect();
        assert!(
            rels.contains(&"docs/deep/er/new_file.md".to_string()),
            "{rels:?}"
        );
        assert!(!rels.contains(&"src/lib.rs".to_string()), "{rels:?}");
        assert_eq!(rels.len(), 4);
        // The cache itself was updated for the next opening.
        assert_eq!(cache.len_of(&opts.cache_key()), Some(4));
    }

    #[test]
    fn background_refresh_only_adds_when_nothing_vanished() {
        let t = tree();
        let cache = Arc::new(WalkCache::new());
        let opts = FinderOptions::new(t.path());
        let mut s1 = FinderSession::start(opts.clone(), 1, Some(cache.clone()), noop());
        settle(&mut s1);
        drop(s1);
        touch(t.path(), "extra/one.txt");
        let mut s2 = FinderSession::start(opts, 2, Some(cache), noop());
        s2.set_query("one");
        settle(&mut s2);
        assert_eq!(s2.total_count(), 5);
        assert_eq!(s2.matched_count(), 1);
    }

    #[test]
    fn vanished_root_does_not_empty_a_cached_session() {
        let t = tempfile::tempdir().unwrap();
        let root = t.path().join("gone");
        let opts = FinderOptions::new(&root);
        let cache = Arc::new(WalkCache::new());
        cache.put(
            opts.cache_key(),
            vec![FinderItem::new("a.txt", EntryKind::File)],
        );
        let mut s = FinderSession::start(opts, 1, Some(cache), noop());
        settle(&mut s);
        assert_eq!(s.total_count(), 1);
    }

    #[test]
    fn cancelled_walk_not_cached() {
        let t = tree();
        let cache = Arc::new(WalkCache::new());
        let opts = FinderOptions::new(t.path());
        // A pre-cancelled token makes the walk stop before it delivers anything.
        let token = CancellationToken::new();
        token.cancel();
        let mut s =
            FinderSession::start_with_token(opts.clone(), 1, Some(cache.clone()), noop(), token);
        settle(&mut s);
        assert!(s.walk_summary().is_some_and(|w| w.cancelled));
        assert_eq!(s.total_count(), 0);
        assert!(cache.len_of(&opts.cache_key()).is_none());
    }

    #[test]
    fn toggles_hidden_and_gitignore_at_session_level() {
        let t = tempfile::tempdir().unwrap();
        touch(t.path(), "a.txt");
        touch(t.path(), ".hidden.txt");
        touch(t.path(), "ignored/x.txt");
        fs::write(t.path().join(".gitignore"), "ignored/\n").unwrap();
        let list = |hidden: bool, gi: bool| -> Vec<String> {
            let mut o = FinderOptions::new(t.path());
            o.show_hidden = hidden;
            o.respect_gitignore = gi;
            let mut s = FinderSession::start(o, 1, None, noop());
            settle(&mut s);
            s.results(0, 100)
                .into_iter()
                .map(|r| r.rel.replace('\\', "/"))
                .collect()
        };
        let base = list(false, true);
        assert!(base.contains(&"a.txt".to_string()));
        assert!(!base
            .iter()
            .any(|r| r == ".hidden.txt" || r.starts_with("ignored")));
        let hidden = list(true, true);
        assert!(hidden.contains(&".hidden.txt".to_string()));
        assert!(!hidden.iter().any(|r| r.starts_with("ignored")));
        let all = list(true, false);
        assert!(all.contains(&"ignored/x.txt".to_string()));
    }

    #[test]
    fn different_toggles_do_not_share_cache_entries() {
        let t = tempfile::tempdir().unwrap();
        touch(t.path(), "a.txt");
        touch(t.path(), ".h.txt");
        let cache = Arc::new(WalkCache::new());
        let plain = FinderOptions::new(t.path());
        let mut hidden = plain.clone();
        hidden.show_hidden = true;
        let mut s1 = FinderSession::start(plain.clone(), 1, Some(cache.clone()), noop());
        settle(&mut s1);
        assert_eq!(s1.total_count(), 1);
        let mut s2 = FinderSession::start(hidden.clone(), 2, Some(cache.clone()), noop());
        assert!(
            !s2.from_cache(),
            "hidden=true must not reuse the hidden=false walk"
        );
        settle(&mut s2);
        assert!(s2.total_count() > s1.total_count());
        assert_ne!(plain.cache_key(), hidden.cache_key());
        // The same root spelled with different case hits the same entry on Windows.
        if cfg!(windows) {
            let mut again = plain.clone();
            again.root = PathBuf::from(plain.root.to_string_lossy().to_uppercase());
            assert!(cache.contains(&again.cache_key()));
        }
    }

    #[test]
    fn cache_and_matcher_share_one_allocation_per_path() {
        // A clone (what the cache hands to the matcher) is a pointer copy, not a string copy,
        // and the item stays two words wide: this is what keeps 1M paths under budget.
        let a = FinderItem::new("src/some/long/path/file.rs", EntryKind::File);
        let b = a.clone();
        assert!(Arc::ptr_eq(&a.repr, &b.repr));
        assert_eq!(
            std::mem::size_of::<FinderItem>(),
            2 * std::mem::size_of::<usize>()
        );
    }

    #[cfg(windows)]
    #[test]
    fn a_removal_refresh_keeps_the_exact_names_of_surviving_non_unicode_entries() {
        use std::os::windows::ffi::OsStringExt;
        let t = tempfile::tempdir().unwrap();
        let wide: Vec<u16> = "bad"
            .encode_utf16()
            .chain([0xD800])
            .chain("name.txt".encode_utf16())
            .collect();
        let name = std::ffi::OsString::from_wide(&wide);
        if fs::write(t.path().join(&name), "x").is_err() {
            eprintln!("SKIPPED: filesystem rejects lone surrogates");
            return;
        }
        touch(t.path(), "gone.txt");
        let cache = Arc::new(WalkCache::new());
        let opts = FinderOptions::new(t.path());
        let mut s1 = FinderSession::start(opts.clone(), 1, Some(cache.clone()), noop());
        settle(&mut s1);
        drop(s1);
        fs::remove_file(t.path().join("gone.txt")).unwrap();
        let mut s2 = FinderSession::start(opts.clone(), 2, Some(cache.clone()), noop());
        settle(&mut s2);
        drop(s2);
        // The cache now holds the post-removal walk; a third session must still resolve the
        // lossy entry to the real file.
        let mut s3 = FinderSession::start(opts, 3, Some(cache), noop());
        settle(&mut s3);
        let r = s3.results(0, 5);
        assert_eq!(r.len(), 1);
        assert!(r[0].path.exists(), "{:?}", r[0].path);
    }

    #[cfg(windows)]
    #[test]
    fn non_unicode_names_keep_an_openable_path() {
        use std::os::windows::ffi::OsStringExt;
        let t = tempfile::tempdir().unwrap();
        let wide: Vec<u16> = "bad"
            .encode_utf16()
            .chain([0xD800])
            .chain("name.txt".encode_utf16())
            .collect();
        let name = std::ffi::OsString::from_wide(&wide);
        if fs::write(t.path().join(&name), "x").is_err() {
            eprintln!("SKIPPED non_unicode_names: filesystem rejects lone surrogates");
            return;
        }
        let mut s = FinderSession::start(FinderOptions::new(t.path()), 1, None, noop());
        settle(&mut s);
        let r = s.results(0, 5);
        assert_eq!(r.len(), 1);
        assert!(r[0].rel.contains('\u{FFFD}'));
        assert!(r[0].path.exists(), "{:?}", r[0].path);
    }

    /// Keystroke-to-results latency on 500k paths (spec target: under 25 ms). Run with
    /// `cargo test -p ks-search --release -- --ignored finder_keystroke_latency`.
    #[test]
    #[ignore = "performance guard; run in release mode"]
    fn finder_keystroke_latency() {
        let items = crate::testgen::synthetic_paths(500_000, 42)
            .into_iter()
            .map(|p| FinderItem::new(p, EntryKind::File));
        let mut s = FinderSession::from_items(FinderOptions::new("C:\\bench"), 1, items, noop());
        settle(&mut s);
        let mut worst = Duration::ZERO;
        let mut q = String::new();
        for c in "src main".chars() {
            q.push(c);
            let t0 = Instant::now();
            s.set_query(&q);
            while s.tick(5).matching {}
            let _ = s.results(0, 30);
            worst = worst.max(t0.elapsed());
        }
        eprintln!("worst keystroke latency: {worst:?}");
        assert!(worst < Duration::from_millis(25), "{worst:?}");
    }

    #[test]
    fn notify_is_called() {
        let t = tree();
        let hits = Arc::new(std::sync::atomic::AtomicUsize::new(0));
        let h = hits.clone();
        let mut s = FinderSession::start(
            FinderOptions::new(t.path()),
            1,
            None,
            Arc::new(move || {
                h.fetch_add(1, Ordering::Relaxed);
            }),
        );
        settle(&mut s);
        assert!(hits.load(Ordering::Relaxed) > 0);
    }
}
