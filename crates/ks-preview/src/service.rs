//! The [`Previewer`] service: background workers, cancellation, LRU cache.

use std::panic::{catch_unwind, AssertUnwindSafe};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Condvar, Mutex, MutexGuard};
use std::thread;

use lru::LruCache;
use tokio_util::sync::CancellationToken;

use crate::content::{build_with_meta, classify_io};
use crate::metadata::FileMetadata;
use crate::model::{mtime_nanos, Preview, PreviewBody, UnavailableReason};
use crate::options::PreviewOptions;

/// Number of previews kept in the LRU cache (PLAN.md section 8).
pub const CACHE_CAPACITY: usize = 64;
/// Approximate byte budget of the cache (decoded images dominate). The oldest entries
/// are evicted when the sum of [`Preview::approx_bytes`] exceeds it; a single preview
/// larger than the budget is delivered but not cached.
pub const CACHE_BYTE_BUDGET: usize = 96 * 1024 * 1024;
const WORKERS: usize = 3;

/// Result of one request, delivered through the sink.
#[derive(Debug, Clone)]
pub struct PreviewEvent {
    /// The generation id given to [`Previewer::request`]; the UI drops events whose
    /// generation is not the current one.
    pub generation: u64,
    pub path: PathBuf,
    pub result: Arc<Preview>,
    /// Served from the LRU cache.
    pub from_cache: bool,
}

#[derive(Debug, Clone, PartialEq, Eq, Hash)]
struct CacheKey {
    path: PathBuf,
    mtime: Option<i128>,
    size: u64,
    attrs: u32,
    opts: u64,
}

struct Job {
    path: PathBuf,
    generation: u64,
    opts: PreviewOptions,
    cancel: CancellationToken,
}

#[derive(Default)]
struct Slot {
    /// Latest queued request (older queued requests are replaced, never run).
    pending: Option<Job>,
    /// Token of the most recent request, queued or running.
    current: Option<CancellationToken>,
}

/// LRU cache with an entry cap and a byte budget.
struct Cache {
    lru: LruCache<CacheKey, Arc<Preview>>,
    bytes: usize,
}

impl Cache {
    fn new() -> Self {
        Self {
            lru: LruCache::new(
                std::num::NonZeroUsize::new(CACHE_CAPACITY).unwrap_or(std::num::NonZeroUsize::MIN),
            ),
            bytes: 0,
        }
    }

    fn put(&mut self, key: CacheKey, value: Arc<Preview>) {
        let size = value.approx_bytes();
        if size > CACHE_BYTE_BUDGET {
            return;
        }
        // `push` returns the replaced or evicted entry in both cases.
        if let Some((_, old)) = self.lru.push(key, value) {
            self.bytes = self.bytes.saturating_sub(old.approx_bytes());
        }
        self.bytes += size;
        while self.bytes > CACHE_BYTE_BUDGET {
            let Some((_, old)) = self.lru.pop_lru() else {
                break;
            };
            self.bytes = self.bytes.saturating_sub(old.approx_bytes());
        }
    }

    fn remove(&mut self, key: &CacheKey) {
        if let Some(old) = self.lru.pop(key) {
            self.bytes = self.bytes.saturating_sub(old.approx_bytes());
        }
    }

    fn clear(&mut self) {
        self.lru.clear();
        self.bytes = 0;
    }
}

type Sink = dyn Fn(PreviewEvent) + Send + Sync + 'static;

struct Inner {
    slot: Mutex<Slot>,
    wake: Condvar,
    shutdown: AtomicBool,
    cache: Mutex<Cache>,
    sink: Box<Sink>,
}

fn lock<T>(m: &Mutex<T>) -> MutexGuard<'_, T> {
    // A poisoned lock only means a worker panicked; the data is still usable.
    m.lock().unwrap_or_else(|p| p.into_inner())
}

/// Background preview service.
///
/// Threading: `new` starts three plain `std::thread` workers (no async runtime needed).
/// `request` never blocks and never touches the filesystem on the caller's thread: it
/// cancels the previous request, queues the new one (replacing any request still
/// waiting in the queue) and returns. The worker stats the path, consults the cache,
/// builds the preview and calls the sink from the worker thread. Superseded or
/// cancelled requests emit **nothing**: the UI must clear its own "loading" state when
/// it cancels (or on the next generation), and must always compare `generation`, since
/// a result can still be in flight between the last cancellation check and the sink
/// call. A `Previewer` has a single request slot, so a `Metadata` request (the `i`
/// toggle) cancels a pending `Auto` request and vice versa; use two instances if both
/// must be live. A panic while building a preview (or inside the sink) is caught and
/// logged; the request then yields `Unavailable { Io }` so the UI never waits forever.
///
/// Failures that may be transient (`Locked`, `PermissionDenied`, `Io`) are never cached.
///
/// Wiring: give the sink a closure that forwards to the app's message channel, e.g.
/// `move |ev| { let _ = tx.send(Msg::Preview(ev)); }`, and compare `ev.generation`
/// with the current one in `update()`.
pub struct Previewer {
    inner: Arc<Inner>,
}

impl Previewer {
    /// Create a service that delivers events to `sink` (called on worker threads).
    pub fn new(sink: impl Fn(PreviewEvent) + Send + Sync + 'static) -> Self {
        Self::with_workers(sink, WORKERS)
    }

    fn with_workers(sink: impl Fn(PreviewEvent) + Send + Sync + 'static, workers: usize) -> Self {
        let inner = Arc::new(Inner {
            slot: Mutex::new(Slot::default()),
            wake: Condvar::new(),
            shutdown: AtomicBool::new(false),
            cache: Mutex::new(Cache::new()),
            sink: Box::new(sink),
        });
        for n in 0..workers {
            let inner = Arc::clone(&inner);
            let spawned = thread::Builder::new()
                .name(format!("ks-preview-{n}"))
                .spawn(move || worker(inner));
            if let Err(e) = spawned {
                tracing::error!(error = %e, "failed to spawn preview worker");
            }
        }
        Self { inner }
    }

    /// Convenience: create a service plus a `std::sync::mpsc` receiver for its events.
    pub fn with_channel() -> (Self, std::sync::mpsc::Receiver<PreviewEvent>) {
        let (tx, rx) = std::sync::mpsc::channel();
        (
            Self::new(move |ev| {
                let _ = tx.send(ev);
            }),
            rx,
        )
    }

    /// Request a preview of `path`. Cancels any earlier in-flight request.
    pub fn request(&self, path: impl Into<PathBuf>, generation: u64, opts: PreviewOptions) {
        let cancel = CancellationToken::new();
        let job = Job {
            path: path.into(),
            generation,
            opts,
            cancel: cancel.clone(),
        };
        {
            let mut slot = lock(&self.inner.slot);
            if let Some(prev) = slot.current.replace(cancel) {
                prev.cancel();
            }
            slot.pending = Some(job);
        }
        self.inner.wake.notify_one();
    }

    /// Cancel the in-flight request without starting a new one (for example when the
    /// preview pane is hidden).
    pub fn cancel(&self) {
        let mut slot = lock(&self.inner.slot);
        if let Some(t) = slot.current.take() {
            t.cancel();
        }
        slot.pending = None;
    }

    /// Drop every cached preview.
    pub fn clear_cache(&self) {
        lock(&self.inner.cache).clear();
    }

    /// Drop cached previews of `path` (call on watcher events if mtime granularity is
    /// a concern).
    pub fn invalidate(&self, path: &Path) {
        let mut cache = lock(&self.inner.cache);
        let stale: Vec<CacheKey> = cache
            .lru
            .iter()
            .filter(|(k, _)| k.path == path)
            .map(|(k, _)| k.clone())
            .collect();
        for k in stale {
            cache.remove(&k);
        }
    }

    /// Number of cached previews.
    pub fn cache_len(&self) -> usize {
        lock(&self.inner.cache).lru.len()
    }

    /// Approximate bytes held by the cache.
    pub fn cache_bytes(&self) -> usize {
        lock(&self.inner.cache).bytes
    }
}

impl Drop for Previewer {
    fn drop(&mut self) {
        self.inner.shutdown.store(true, Ordering::SeqCst);
        self.cancel();
        self.inner.wake.notify_all();
    }
}

fn worker(inner: Arc<Inner>) {
    loop {
        let job = {
            let mut slot = lock(&inner.slot);
            loop {
                if inner.shutdown.load(Ordering::SeqCst) {
                    return;
                }
                if let Some(job) = slot.pending.take() {
                    break job;
                }
                slot = inner.wake.wait(slot).unwrap_or_else(|p| p.into_inner());
            }
        };
        if job.cancel.is_cancelled() {
            continue;
        }
        let result = match catch_unwind(AssertUnwindSafe(|| run(&inner, &job))) {
            Ok(r) => r,
            Err(_) => {
                tracing::error!(path = %job.path.display(), "preview build panicked");
                Some((
                    Arc::new(Preview {
                        path: job.path.clone(),
                        meta: None,
                        body: PreviewBody::Unavailable {
                            reason: UnavailableReason::Io,
                            message: "Preview failed unexpectedly".into(),
                        },
                    }),
                    false,
                ))
            }
        };
        if let Some((preview, from_cache)) = result {
            if !job.cancel.is_cancelled() {
                let ev = PreviewEvent {
                    generation: job.generation,
                    path: job.path,
                    result: preview,
                    from_cache,
                };
                if catch_unwind(AssertUnwindSafe(|| (inner.sink)(ev))).is_err() {
                    tracing::error!("preview sink panicked");
                }
            }
        }
    }
}

/// Returns `None` when cancelled.
fn run(inner: &Inner, job: &Job) -> Option<(Arc<Preview>, bool)> {
    #[cfg(test)]
    if job.path.file_name().is_some_and(|n| n == "ks-panic-test") {
        panic!("injected panic");
    }
    let meta = match FileMetadata::read(&job.path) {
        Ok(m) => m,
        Err(e) => {
            let (reason, message) = classify_io(&e);
            return Some((
                Arc::new(Preview {
                    path: job.path.clone(),
                    meta: None,
                    body: PreviewBody::Unavailable { reason, message },
                }),
                false,
            ));
        }
    };
    let key = CacheKey {
        path: job.path.clone(),
        mtime: mtime_nanos(meta.modified),
        size: meta.size,
        attrs: meta.attributes.raw,
        opts: job.opts.fingerprint(),
    };
    if let Some(hit) = lock(&inner.cache).lru.get(&key) {
        return Some((Arc::clone(hit), true));
    }
    let preview = Arc::new(build_with_meta(&job.path, meta, &job.opts, &job.cancel)?);
    // Locks, ACL problems and I/O errors can clear without changing mtime/size, so they
    // are never cached; everything else is keyed on mtime/size/attrs/options.
    let transient = matches!(
        preview.body,
        PreviewBody::Unavailable {
            reason: UnavailableReason::Locked
                | UnavailableReason::PermissionDenied
                | UnavailableReason::Io,
            ..
        }
    );
    if !transient {
        lock(&inner.cache).put(key, Arc::clone(&preview));
    }
    Some((preview, false))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn new_request_cancels_previous_and_replaces_queue() {
        // No workers: requests just sit in the slot so the state is deterministic.
        let pv = Previewer::with_workers(|_| {}, 0);
        pv.request("a", 1, PreviewOptions::default());
        let first = lock(&pv.inner.slot).current.clone().expect("current token");
        assert!(!first.is_cancelled());
        pv.request("b", 2, PreviewOptions::default());
        assert!(first.is_cancelled(), "previous request is cancelled");
        {
            let slot = lock(&pv.inner.slot);
            let pending = slot.pending.as_ref().expect("pending");
            assert_eq!(pending.generation, 2);
            assert!(!pending.cancel.is_cancelled());
        }
        pv.cancel();
        let slot = lock(&pv.inner.slot);
        assert!(slot.pending.is_none());
    }

    #[test]
    fn worker_panic_yields_unavailable_and_worker_survives() {
        let (pv, rx) = Previewer::with_channel();
        let dir = tempfile::tempdir().expect("tempdir");
        let ok = dir.path().join("ok.txt");
        std::fs::write(&ok, "hello").expect("write");
        // More panics than workers: the pool must still answer afterwards.
        for g in 0..6u64 {
            pv.request(
                dir.path().join("ks-panic-test"),
                g,
                PreviewOptions::default(),
            );
            let ev = rx
                .recv_timeout(std::time::Duration::from_secs(10))
                .expect("panic still yields an event");
            assert_eq!(ev.generation, g);
            assert!(matches!(
                ev.result.body,
                PreviewBody::Unavailable {
                    reason: UnavailableReason::Io,
                    ..
                }
            ));
        }
        pv.request(&ok, 100, PreviewOptions::default());
        let ev = rx
            .recv_timeout(std::time::Duration::from_secs(10))
            .expect("service alive");
        assert!(matches!(ev.result.body, PreviewBody::Text(_)));
    }

    #[test]
    fn panicking_sink_does_not_kill_workers() {
        let calls = Arc::new(std::sync::atomic::AtomicUsize::new(0));
        let (tx, rx) = std::sync::mpsc::channel();
        let c = Arc::clone(&calls);
        let pv = Previewer::new(move |ev| {
            if c.fetch_add(1, Ordering::SeqCst) < 4 {
                panic!("sink boom");
            }
            let _ = tx.send(ev);
        });
        let dir = tempfile::tempdir().expect("tempdir");
        let p = dir.path().join("a.txt");
        std::fs::write(&p, "x").expect("write");
        for g in 0..6u64 {
            pv.request(&p, g, PreviewOptions::default());
            // Wait until the sink has been called for this request.
            let deadline = std::time::Instant::now() + std::time::Duration::from_secs(10);
            while calls.load(Ordering::SeqCst) <= g as usize {
                assert!(std::time::Instant::now() < deadline, "worker died");
                thread::sleep(std::time::Duration::from_millis(5));
            }
        }
        assert!(rx.recv_timeout(std::time::Duration::from_secs(5)).is_ok());
    }

    #[test]
    fn byte_budget_evicts_oldest() {
        let mut c = Cache::new();
        let big = |n: usize| {
            Arc::new(Preview {
                path: PathBuf::new(),
                meta: None,
                body: PreviewBody::Unavailable {
                    reason: UnavailableReason::TooLarge,
                    message: "x".repeat(n),
                },
            })
        };
        let key = |i: u64| CacheKey {
            path: PathBuf::from(format!("{i}")),
            mtime: None,
            size: 0,
            attrs: 0,
            opts: 0,
        };
        let third = CACHE_BYTE_BUDGET / 3;
        for i in 0..5 {
            c.put(key(i), big(third));
        }
        assert!(c.bytes <= CACHE_BYTE_BUDGET);
        assert!(c.lru.len() < 5 && c.lru.len() >= 2);
        assert!(c.lru.peek(&key(4)).is_some(), "newest is kept");
        assert!(c.lru.peek(&key(0)).is_none(), "oldest is evicted");
        // Oversized entries are not cached at all.
        c.put(key(9), big(CACHE_BYTE_BUDGET + 1));
        assert!(c.lru.peek(&key(9)).is_none());
        c.clear();
        assert_eq!(c.bytes, 0);
    }
}
