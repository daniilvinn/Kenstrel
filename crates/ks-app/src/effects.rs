//! The effect runner: performs [`Effect`]s on background tasks and reports back as
//! [`Msg`]s.
//!
//! Every effect runs under a [`CancellationToken`] so it can be replaced, cancelled, or
//! torn down at shutdown without leaking tasks. Effects that must not overlap share a
//! [`Slot`]: starting one cancels the previous one in the same slot.

use std::collections::HashMap;
use std::ffi::OsString;
use std::path::PathBuf;
use std::sync::Arc;

use crate::fileops::{EditorSettings, FileOpsRunner};
use crate::persist::{StateStore, StateWriter};
use ks_core::{Effect, Gen, Level, Msg, PreviewKind, PreviewMsg, PreviewRequest, TimerKind};

use crate::claude::{ClaudeHost, ClaudeOptions};
use ks_fs::DirWatcher;
use ks_preview::{Limits, Palette, PreviewEvent, PreviewOptions, Previewer, RequestKind};
use ks_search::{FinderOptions, FinderSession, GrepHandle, GrepQuery, WalkCache};
use tokio::sync::mpsc::Sender;
use tokio::time::sleep;
use tokio_util::sync::CancellationToken;

use crate::search::FinderWorker;

/// Background work of which only the latest request matters.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
enum Slot {
    /// Directory listings and drive queries.
    Listing,
    Watcher,
    Git,
    ChildCounts,
    /// Lazy size/mtime reads of a results list; apart from `ChildCounts` so a directory
    /// tab's child counts never cancel them.
    Stats,
}

/// Most parked-tab directories that are watched at once (one per tab, the active tab has its
/// own watch).
const PARKED_WATCH_LIMIT: usize = ks_core::tabs::MAX_TABS - 1;

/// Starts a directory watch that lives until `token` is cancelled; every debounced change
/// sends `make()`. A watch that cannot be set up warns the user when `warn` is set.
fn spawn_watch(
    tx: Sender<Msg>,
    token: CancellationToken,
    path: PathBuf,
    make: impl Fn() -> Msg + Send + 'static,
    warn: bool,
) {
    tokio::spawn(async move {
        let notify_tx = tx.clone();
        let watch_path = path.clone();
        // Setting up a watch touches the file system, so it stays off the loop.
        let created = tokio::task::spawn_blocking(move || {
            DirWatcher::new(&watch_path, move || {
                let _ = notify_tx.blocking_send(make());
            })
        })
        .await;
        let watcher = match created {
            Ok(Ok(watcher)) => watcher,
            Ok(Err(error)) => {
                tracing::warn!(path = %path.display(), %error, "cannot watch directory");
                if warn && !token.is_cancelled() {
                    let _ = tx
                        .send(Msg::Notify {
                            level: Level::Warning,
                            text: "This directory cannot be watched; changes will not appear on their own"
                                .to_owned(),
                        })
                        .await;
                }
                return;
            }
            Err(error) => {
                tracing::error!(%error, "watcher setup task failed");
                return;
            }
        };
        token.cancelled().await;
        // Dropping stops the OS watch; do it where blocking is allowed.
        let _ = tokio::task::spawn_blocking(move || drop(watcher)).await;
    });
}

/// Bookkeeping of the `git status` run in flight. A request for the directory that is being
/// scanned does not start a second run (or kill the first, which on a huge repository under
/// constant file churn would never finish): it only records the newest generation and that
/// the files changed again, and the running task answers with the newest generation and
/// scans once more.
struct GitRun {
    dir: PathBuf,
    gen: Gen,
    /// Files changed since the scan in flight started.
    dirty: bool,
    /// The task is still going (cleared by the task itself, under the lock).
    running: bool,
}

pub struct EffectRunner {
    tx: Sender<Msg>,
    /// Cancels every task at shutdown; per-effect tokens are children of it.
    shutdown: CancellationToken,
    timers: HashMap<TimerKind, CancellationToken>,
    slots: HashMap<Slot, CancellationToken>,
    /// Where bookmarks and frecency live; without it state effects are ignored (tests).
    store: Option<(StateStore, StateWriter)>,
    /// Jobs, shell runs and launching (P6).
    fileops: FileOpsRunner,
    /// Preview engines, created on first use (they start worker threads). The main pane and
    /// the parent column each have their own because a `Previewer` has one request slot.
    previewer: Option<Previewer>,
    parent_previewer: Option<Previewer>,
    /// Syntax colours handed to the engine, derived from the active theme.
    palette: Palette,
    /// The walk cache of find file, shared by every finder session of this run.
    walk_cache: Arc<WalkCache>,
    finder: Option<FinderWorker>,
    grep: Option<GrepHandle>,
    /// Watches of the directories of the parked tabs, by path (`Effect::WatchParked`).
    parked: Vec<(PathBuf, CancellationToken)>,
    /// The running background re-list of each parked directory: a newer one for the same
    /// path, or the tab being activated, cancels it so re-lists never pile up under churn.
    parked_lists: Vec<(PathBuf, CancellationToken)>,
    /// The `git status` run in flight, if any (see [`GitRun`]).
    git_run: Option<Arc<std::sync::Mutex<GitRun>>>,
    /// Performs the `Effect::Claude*` requests (PLAN section 10).
    claude: ClaudeHost,
}

impl EffectRunner {
    pub fn new(tx: Sender<Msg>) -> Self {
        let shutdown = CancellationToken::new();
        Self {
            fileops: FileOpsRunner::new(tx.clone(), shutdown.child_token()),
            claude: ClaudeHost::new(tx.clone(), ClaudeOptions::default()),
            tx,
            shutdown,
            timers: HashMap::new(),
            slots: HashMap::new(),
            store: None,
            previewer: None,
            parent_previewer: None,
            palette: Palette::default(),
            walk_cache: Arc::new(WalkCache::new()),
            finder: None,
            grep: None,
            parked: Vec::new(),
            parked_lists: Vec::new(),
            git_run: None,
        }
    }

    /// Enables loading and saving of the state files in `dir`.
    pub fn set_state_dir(&mut self, dir: PathBuf) {
        let store = StateStore::new(dir);
        let writer = StateWriter::spawn(store.clone());
        self.store = Some((store, writer));
    }

    /// Waits (at most `timeout`) for queued state writes to reach the disk.
    pub fn finish_state_writes(&mut self, timeout: std::time::Duration) {
        if let Some((_, writer)) = self.store.take() {
            writer.finish(timeout);
        }
    }

    /// Applies the parts of the config the runner needs (the `[editor]` table). Call again
    /// after a config reload.
    pub fn configure(&mut self, config: &ks_core::Config) {
        self.fileops
            .configure(EditorSettings::from_config(&config.editor));
    }

    /// Replaces the clipboard that `Y` writes to (tests).
    pub fn set_clipboard(
        &mut self,
        clipboard: impl ks_fs::launch::clipboard::Clipboard + Send + 'static,
    ) {
        self.fileops.set_clipboard(clipboard);
    }

    /// The job engine (tests, diagnostics).
    pub fn jobs(&self) -> &ks_fs::ops::JobManager {
        self.fileops.manager()
    }

    /// Sets the syntax palette used for text previews (call again after a theme change; the
    /// engine's cache key includes the palette, so old entries are simply not hit).
    pub fn set_preview_palette(&mut self, palette: Palette) {
        self.palette = palette;
    }

    /// Replaces the Claude host with one using `options` (file locations).
    #[must_use]
    pub fn with_claude(mut self, options: ClaudeOptions) -> Self {
        self.claude = ClaudeHost::new(self.tx.clone(), options);
        self
    }

    /// The Claude host, for tests.
    pub fn claude(&mut self) -> &mut ClaudeHost {
        &mut self.claude
    }

    /// Starts the given effects. Must be called from within a tokio runtime.
    pub fn run(&mut self, effects: impl IntoIterator<Item = Effect>) {
        for effect in effects {
            match effect {
                Effect::StartTimer { kind, gen, delay } => {
                    let token = self.shutdown.child_token();
                    if let Some(previous) = self.timers.insert(kind, token.clone()) {
                        previous.cancel();
                    }
                    let tx = self.tx.clone();
                    tokio::spawn(async move {
                        tokio::select! {
                            () = token.cancelled() => {}
                            () = sleep(delay) => {
                                // A closed channel means the app is exiting.
                                let _ = tx.send(Msg::Timer { kind, gen }).await;
                            }
                        }
                    });
                }
                Effect::CancelTimer(kind) => {
                    if let Some(token) = self.timers.remove(&kind) {
                        token.cancel();
                    }
                }
                Effect::ListDir { gen, path } => self.list_dir(gen, path),
                Effect::ListParked { gen, path } => self.list_parked(gen, path),
                Effect::ListDrives { gen } => self.list_drives(gen),
                Effect::WatchDir { gen, path } => self.watch_dir(gen, path),
                Effect::StopWatching => {
                    if let Some(token) = self.slots.remove(&Slot::Watcher) {
                        token.cancel();
                    }
                }
                Effect::WatchParked { paths } => self.watch_parked(paths),
                Effect::GitStatus { gen, dir } => self.git_status(gen, dir),
                Effect::CountChildren { gen, dir, names } => self.count_children(gen, dir, names),
                Effect::StatEntries { gen, dir, names } => self.stat_entries(gen, dir, names),
                // These change the loop's own theme and config; `event_loop::dispatch`
                // performs them before effects reach the runner.
                Effect::SetTheme { .. } | Effect::ReloadConfig => {
                    tracing::debug!("loop-level effect reached the runner; ignored");
                }
                Effect::LoadNavData => self.load_nav_data(),
                Effect::WriteState { file, contents } => {
                    if let Some((_, writer)) = &self.store {
                        writer.send(file, contents);
                    }
                }
                // The event loop owns the terminal and runs foreground programs itself.
                Effect::RunForeground { .. } => {
                    tracing::warn!("RunForeground reached the effect runner");
                }
                Effect::RequestPreview { gen, request } => {
                    self.request_preview(gen, &request, false)
                }
                Effect::RequestParent { gen, request } => self.request_preview(gen, &request, true),
                Effect::CancelPreview => {
                    if let Some(previewer) = &self.previewer {
                        previewer.cancel();
                    }
                }
                Effect::InvalidatePreview { paths } => {
                    for previewer in [&self.previewer, &self.parent_previewer]
                        .into_iter()
                        .flatten()
                    {
                        for path in &paths {
                            previewer.invalidate(path);
                        }
                    }
                }
                Effect::CancelParent => {
                    if let Some(previewer) = &self.parent_previewer {
                        previewer.cancel();
                    }
                }
                Effect::FinderStart {
                    gen,
                    scope,
                    cwd,
                    show_hidden,
                    respect_gitignore,
                    query,
                    offset,
                    limit,
                } => self.finder_start(
                    gen,
                    scope,
                    cwd,
                    (show_hidden, respect_gitignore),
                    (query, offset, limit),
                ),
                Effect::FinderQuery {
                    gen,
                    query,
                    offset,
                    limit,
                } => {
                    if let Some(worker) = self.finder.as_ref().filter(|w| w.gen() == gen) {
                        worker.set_query(query, offset, limit);
                    }
                }
                Effect::FinderWindow { gen, offset, limit } => {
                    if let Some(worker) = self.finder.as_ref().filter(|w| w.gen() == gen) {
                        worker.set_window(offset, limit);
                    }
                }
                // Dropping the worker stops its thread and cancels the walk.
                Effect::FinderStop => self.finder = None,
                Effect::GrepStart {
                    gen,
                    root,
                    pattern,
                    regex,
                    whole_word,
                    glob,
                    show_hidden,
                    respect_gitignore,
                } => {
                    let mut query = GrepQuery::new(root, pattern);
                    query.regex = regex;
                    query.whole_word = whole_word;
                    query.glob = glob;
                    query.show_hidden = show_hidden;
                    query.respect_gitignore = respect_gitignore;
                    self.grep_start(gen, query);
                }
                // Dropping the handle cancels the search.
                Effect::GrepCancel => self.grep = None,
                Effect::WalkCacheChanges(changes) => {
                    let cache = self.walk_cache.clone();
                    tokio::task::spawn_blocking(move || cache.apply_changes(&changes));
                }
                claude @ (Effect::SetBracketedPaste(_)
                | Effect::ClaudePreflight { .. }
                | Effect::ClaudeLaunch { .. }
                | Effect::ClaudeWtSplit { .. }
                | Effect::ClaudeKey(_)
                | Effect::ClaudePaste(_)
                | Effect::ClaudeSendPaths { .. }
                | Effect::ClaudeSendRange { .. }
                | Effect::ClaudeResize { .. }
                | Effect::ClaudeScroll { .. }
                | Effect::ClaudeClose { .. }
                | Effect::ClaudeIpcReply { .. }
                | Effect::ClaudeDiff { .. }
                | Effect::ClaudeIpcWork { .. }) => self.claude.run(claude),
                // File operations, shell runs and launching (P6).
                other => self.fileops.handle(other),
            }
        }
    }

    /// Cancels everything still running.
    pub fn shutdown(&mut self) {
        self.fileops.shutdown();
        self.claude.shutdown();
        self.shutdown.cancel();
        self.timers.clear();
        self.slots.clear();
        self.parked.clear();
        self.parked_lists.clear();
        // Dropping a previewer cancels its work and stops its threads.
        self.previewer = None;
        self.parent_previewer = None;
        self.finder = None;
        self.grep = None;
    }

    /// A fresh token for `slot`; whatever ran there before is cancelled.
    fn claim(&mut self, slot: Slot) -> CancellationToken {
        let token = self.shutdown.child_token();
        if let Some(previous) = self.slots.insert(slot, token.clone()) {
            previous.cancel();
        }
        token
    }

    fn list_dir(&mut self, gen: Gen, path: PathBuf) {
        let token = self.claim(Slot::Listing);
        let tx = self.tx.clone();
        tokio::task::spawn_blocking(move || {
            // `blocking_send` gives backpressure: a slow UI slows the listing down instead
            // of letting chunks pile up. A closed channel means the app is exiting.
            let result = ks_fs::list_dir(&path, &token, |entries| {
                tx.blocking_send(Msg::ListingChunk { gen, entries }).is_ok()
            });
            if let Err(error) = &result {
                tracing::warn!(path = %path.display(), %error, "listing failed");
            }
            // A cancelled listing has been superseded; its end must not be reported.
            if !token.is_cancelled() {
                let _ = tx.blocking_send(Msg::ListingFinished {
                    gen,
                    error: result.err(),
                });
            }
        });
    }

    /// Lists the directory of a parked tab on its own task: it neither replaces nor is
    /// replaced by the listing of the active tab.
    fn list_parked(&mut self, gen: Gen, path: PathBuf) {
        let token = self.shutdown.child_token();
        self.parked_lists.retain(|(p, t)| {
            let same = *p == path;
            if same {
                t.cancel();
            }
            !same && !t.is_cancelled()
        });
        self.parked_lists.push((path.clone(), token.clone()));
        let tx = self.tx.clone();
        tokio::task::spawn_blocking(move || {
            let result = ks_fs::list_dir(&path, &token, |entries| {
                tx.blocking_send(Msg::ListingChunk { gen, entries }).is_ok()
            });
            if !token.is_cancelled() {
                let _ = tx.blocking_send(Msg::ListingFinished {
                    gen,
                    error: result.err(),
                });
            }
        });
    }

    fn list_drives(&mut self, gen: Gen) {
        let token = self.claim(Slot::Listing);
        let tx = self.tx.clone();
        tokio::task::spawn_blocking(move || {
            let drives = ks_fs::list_drives();
            if !token.is_cancelled() {
                let _ = tx.blocking_send(Msg::DrivesLoaded { gen, drives });
            }
        });
    }

    fn watch_dir(&mut self, gen: Gen, path: PathBuf) {
        let token = self.claim(Slot::Watcher);
        spawn_watch(
            self.tx.clone(),
            token,
            path,
            move || Msg::DirChanged { gen },
            true,
        );
    }

    /// Makes the set of watched parked-tab directories exactly `paths`: new ones get a
    /// watcher, ones that stay keep theirs, the rest stop.
    fn watch_parked(&mut self, paths: Vec<PathBuf>) {
        // At most one watch per tab, whatever the caller sends.
        let paths: Vec<PathBuf> = paths.into_iter().take(PARKED_WATCH_LIMIT).collect();
        self.parked.retain(|(path, token)| {
            let keep = paths.contains(path);
            if !keep {
                token.cancel();
            }
            keep
        });
        // A tab that is no longer parked (activated, closed or moved on) needs no re-list.
        self.parked_lists.retain(|(path, token)| {
            let keep = paths.contains(path);
            if !keep {
                token.cancel();
            }
            keep
        });
        for path in paths {
            if self.parked.iter().any(|(p, _)| *p == path) {
                continue;
            }
            let token = self.shutdown.child_token();
            let changed = path.clone();
            spawn_watch(
                self.tx.clone(),
                token.clone(),
                path.clone(),
                move || Msg::ParkedDirChanged {
                    path: changed.clone(),
                },
                false,
            );
            self.parked.push((path, token));
        }
    }

    fn git_status(&mut self, gen: Gen, dir: PathBuf) {
        // Coalesce with the run in flight for the same directory.
        if let Some(run) = &self.git_run {
            let mut run = run
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner);
            let live = self
                .slots
                .get(&Slot::Git)
                .is_some_and(|t| !t.is_cancelled());
            if run.running && live && run.dir == dir {
                run.gen = gen;
                run.dirty = true;
                return;
            }
        }
        let token = self.claim(Slot::Git);
        let run = Arc::new(std::sync::Mutex::new(GitRun {
            dir: dir.clone(),
            gen,
            dirty: false,
            running: true,
        }));
        self.git_run = Some(run.clone());
        let tx = self.tx.clone();
        tokio::spawn(async move {
            loop {
                // Dropping the status future (on cancellation) kills the git processes.
                let snapshot = tokio::select! {
                    () = token.cancelled() => return,
                    snapshot = ks_fs::git_status(&dir) => snapshot,
                };
                let (gen, again) = {
                    let mut state = run
                        .lock()
                        .unwrap_or_else(std::sync::PoisonError::into_inner);
                    let again = std::mem::take(&mut state.dirty);
                    if !again {
                        state.running = false;
                    }
                    (state.gen, again)
                };
                // The answer of a scan that something overtook is still shown (newest
                // generation) while the next scan runs.
                let _ = tx
                    .send(Msg::GitStatus {
                        gen,
                        snapshot: snapshot.map(Box::new),
                    })
                    .await;
                if !again {
                    return;
                }
            }
        });
    }

    fn load_nav_data(&mut self) {
        let Some((store, _)) = &self.store else {
            return;
        };
        let store = store.clone();
        let tx = self.tx.clone();
        // Reading touches the disk, so it stays off the loop; the result arrives as a message.
        tokio::task::spawn_blocking(move || {
            let loaded = store.load();
            let _ = tx.blocking_send(Msg::NavDataLoaded(Box::new(loaded)));
        });
    }

    /// Hands a preview request to the engine. Never blocks and never touches the file
    /// system on this thread; the engine answers through the sink, which forwards the event
    /// 1:1 as a message.
    fn request_preview(&mut self, gen: Gen, request: &PreviewRequest, parent: bool) {
        let options = self.preview_options(request);
        let tx = self.tx.clone();
        let slot = if parent {
            &mut self.parent_previewer
        } else {
            &mut self.previewer
        };
        let previewer = slot.get_or_insert_with(|| {
            // The sink runs on the engine's own worker threads (plain `std` threads), where
            // `blocking_send` is allowed.
            Previewer::new(move |event: PreviewEvent| {
                let msg = PreviewMsg(event);
                let _ = tx.blocking_send(if parent {
                    Msg::ParentPreview(msg)
                } else {
                    Msg::Preview(msg)
                });
            })
        });
        previewer.request(request.path.clone(), gen.raw(), options);
    }

    fn preview_options(&self, request: &PreviewRequest) -> PreviewOptions {
        let limits = Limits {
            max_text_bytes: usize::try_from(request.max_bytes).unwrap_or(usize::MAX),
            max_text_lines: request.max_lines,
            ..Limits::default()
        };
        PreviewOptions {
            kind: match request.kind {
                PreviewKind::Auto => RequestKind::Auto,
                PreviewKind::Metadata => RequestKind::Metadata,
            },
            palette: self.palette,
            limits,
            show_hidden: request.show_hidden,
            owner: Some(ks_preview::OwnerResolver(ks_fs::file_owner)),
            ..PreviewOptions::default()
        }
    }

    fn finder_start(
        &mut self,
        gen: u64,
        scope: ks_search::Scope,
        cwd: PathBuf,
        (show_hidden, respect_gitignore): (bool, bool),
        (query, offset, limit): (String, u32, u32),
    ) {
        // Replacing the worker drops (stops) the previous one.
        self.finder = None;
        let cache = self.walk_cache.clone();
        let make = move |notify: ks_search::Notify| {
            // Resolving the scope touches the file system, so it happens on the worker.
            let root = ks_search::resolve_scope(scope, &cwd);
            let mut options = FinderOptions::new(root.clone());
            options.show_hidden = show_hidden;
            options.respect_gitignore = respect_gitignore;
            (
                FinderSession::start(options, gen, Some(cache), notify),
                root,
            )
        };
        self.finder = Some(FinderWorker::spawn(
            gen,
            make,
            query,
            offset,
            limit,
            self.tx.clone(),
        ));
    }

    fn grep_start(&mut self, gen: u64, query: GrepQuery) {
        self.grep = None;
        let tx = self.tx.clone();
        // Runs on the engine's threads (plain std threads), where blocking is fine and
        // gives the engine backpressure when the UI is behind.
        let sink: ks_search::EventSink<ks_search::GrepEvent> = Arc::new(move |event| {
            let _ = tx.blocking_send(Msg::Grep(event));
        });
        match ks_search::spawn_search(query, gen, self.shutdown.child_token(), sink) {
            Ok(handle) => self.grep = Some(handle),
            Err(error) => {
                tracing::error!(%error, "cannot start the content search");
                let _ = self.tx.try_send(Msg::Notify {
                    level: Level::Error,
                    text: format!("Cannot start the search: {error}"),
                });
            }
        }
    }

    fn stat_entries(&mut self, gen: Gen, dir: PathBuf, names: Vec<OsString>) {
        let token = self.claim(Slot::Stats);
        let tx = self.tx.clone();
        tokio::task::spawn_blocking(move || {
            ks_fs::stat_entries(&dir, names, &token, |batch| {
                let stats = batch
                    .into_iter()
                    .map(|(name, stat)| (name, stat.size, stat.mtime))
                    .collect();
                tx.blocking_send(Msg::EntryStats { gen, stats }).is_ok()
            });
        });
    }

    fn count_children(&mut self, gen: Gen, dir: PathBuf, names: Vec<OsString>) {
        let token = self.claim(Slot::ChildCounts);
        let tx = self.tx.clone();
        tokio::task::spawn_blocking(move || {
            ks_fs::count_children(&dir, names, &token, |counts| {
                tx.blocking_send(Msg::ChildCounts { gen, counts }).is_ok()
            });
        });
    }
}

#[cfg(test)]
mod tests {
    use std::time::Duration;

    use ks_core::GenCounter;
    use tokio::sync::mpsc;

    use super::*;

    fn timer(kind: TimerKind, gens: &mut GenCounter, ms: u64) -> Effect {
        Effect::StartTimer {
            kind,
            gen: gens.bump(),
            delay: Duration::from_millis(ms),
        }
    }

    #[tokio::test(start_paused = true)]
    async fn timer_fires_after_its_delay() {
        let (tx, mut rx) = mpsc::channel(8);
        let mut runner = EffectRunner::new(tx);
        let mut gens = GenCounter::default();
        let effect = timer(TimerKind::WhichKey, &mut gens, 300);
        let Effect::StartTimer { gen, .. } = effect else {
            unreachable!()
        };
        runner.run([effect]);

        tokio::time::sleep(Duration::from_millis(299)).await;
        assert!(rx.try_recv().is_err(), "must not fire early");
        tokio::time::sleep(Duration::from_millis(2)).await;
        assert_eq!(
            rx.recv().await,
            Some(Msg::Timer {
                kind: TimerKind::WhichKey,
                gen
            })
        );
    }

    #[tokio::test(start_paused = true)]
    async fn cancelled_timer_never_fires() {
        let (tx, mut rx) = mpsc::channel(8);
        let mut runner = EffectRunner::new(tx);
        let mut gens = GenCounter::default();
        runner.run([
            timer(TimerKind::WhichKey, &mut gens, 300),
            Effect::CancelTimer(TimerKind::WhichKey),
        ]);
        tokio::time::sleep(Duration::from_secs(1)).await;
        assert!(rx.try_recv().is_err());
    }

    #[tokio::test(start_paused = true)]
    async fn restarting_a_timer_replaces_the_previous_one() {
        let (tx, mut rx) = mpsc::channel(8);
        let mut runner = EffectRunner::new(tx);
        let mut gens = GenCounter::default();
        runner.run([timer(TimerKind::StatusExpiry, &mut gens, 100)]);
        let second = timer(TimerKind::StatusExpiry, &mut gens, 100);
        let Effect::StartTimer { gen, .. } = second else {
            unreachable!()
        };
        runner.run([second]);

        tokio::time::sleep(Duration::from_millis(200)).await;
        assert_eq!(
            rx.recv().await,
            Some(Msg::Timer {
                kind: TimerKind::StatusExpiry,
                gen
            })
        );
        assert!(
            rx.try_recv().is_err(),
            "the replaced timer must stay silent"
        );
    }

    #[tokio::test(start_paused = true)]
    async fn different_kinds_run_independently_and_shutdown_cancels_all() {
        let (tx, mut rx) = mpsc::channel(8);
        let mut runner = EffectRunner::new(tx);
        let mut gens = GenCounter::default();
        runner.run([
            timer(TimerKind::WhichKey, &mut gens, 100),
            timer(TimerKind::StatusExpiry, &mut gens, 100),
        ]);
        runner.shutdown();
        tokio::time::sleep(Duration::from_secs(1)).await;
        assert!(rx.try_recv().is_err());
    }
}

#[cfg(test)]
mod fs_effect_tests {
    use std::time::Duration;

    use ks_core::{FsErrorKind, GenCounter};
    use tokio::sync::mpsc::{self, Receiver};
    use tokio::time::timeout;

    use super::*;

    const WAIT: Duration = Duration::from_secs(5);

    fn gen() -> Gen {
        GenCounter::default().bump()
    }

    async fn next(rx: &mut Receiver<Msg>) -> Msg {
        timeout(WAIT, rx.recv())
            .await
            .expect("a message in time")
            .expect("an open channel")
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn listing_streams_chunks_then_reports_the_end() {
        let dir = tempfile::tempdir().unwrap();
        for name in ["a.txt", "b.txt", "c.txt"] {
            std::fs::write(dir.path().join(name), "x").unwrap();
        }
        let (tx, mut rx) = mpsc::channel(16);
        let mut runner = EffectRunner::new(tx);
        let g = gen();
        runner.run([Effect::ListDir {
            gen: g,
            path: dir.path().to_owned(),
        }]);

        let mut names = Vec::new();
        loop {
            match next(&mut rx).await {
                Msg::ListingChunk { gen, entries } => {
                    assert_eq!(gen, g);
                    names.extend(entries.iter().map(|e| e.display_name().into_owned()));
                }
                Msg::ListingFinished { gen, error } => {
                    assert_eq!(gen, g);
                    assert!(error.is_none());
                    break;
                }
                other => panic!("unexpected {other:?}"),
            }
        }
        names.sort();
        assert_eq!(names, ["a.txt", "b.txt", "c.txt"]);
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn a_parked_listing_does_not_cancel_the_active_one() {
        let a = tempfile::tempdir().unwrap();
        let b = tempfile::tempdir().unwrap();
        std::fs::write(a.path().join("one"), "x").unwrap();
        std::fs::write(b.path().join("two"), "x").unwrap();
        let (tx, mut rx) = mpsc::channel(16);
        let mut runner = EffectRunner::new(tx);
        let (ga, gb) = (gen(), gen());
        runner.run([
            Effect::ListDir {
                gen: ga,
                path: a.path().to_owned(),
            },
            Effect::ListParked {
                gen: gb,
                path: b.path().to_owned(),
            },
        ]);
        let mut finished = Vec::new();
        let mut names = Vec::new();
        while finished.len() < 2 {
            match next(&mut rx).await {
                Msg::ListingChunk { entries, .. } => {
                    names.extend(entries.iter().map(|e| e.display_name().into_owned()));
                }
                Msg::ListingFinished { gen, error } => {
                    assert!(error.is_none());
                    finished.push(gen);
                }
                other => panic!("unexpected {other:?}"),
            }
        }
        assert!(finished.contains(&ga) && finished.contains(&gb));
        names.sort();
        assert_eq!(names, ["one", "two"]);
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn re_lists_of_one_parked_directory_do_not_pile_up() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join("f"), "x").unwrap();
        let (tx, mut rx) = mpsc::channel(64);
        let mut runner = EffectRunner::new(tx);
        let last = gen();
        for _ in 0..5 {
            runner.run([Effect::ListParked {
                gen: gen(),
                path: dir.path().to_owned(),
            }]);
        }
        runner.run([Effect::ListParked {
            gen: last,
            path: dir.path().to_owned(),
        }]);
        assert_eq!(runner.parked_lists.len(), 1, "one slot per parked path");
        loop {
            if let Msg::ListingFinished { gen, .. } = next(&mut rx).await {
                if gen == last {
                    break;
                }
            }
        }
        // Activating the tab (it leaves the parked set) cancels its re-list.
        runner.run([Effect::WatchParked { paths: Vec::new() }]);
        assert!(runner.parked_lists.is_empty());
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn a_new_palette_re_highlights_cached_previews() {
        let dir = tempfile::tempdir().unwrap();
        let file = dir.path().join("a.rs");
        std::fs::write(
            &file,
            "fn main() { let x = 1; }
",
        )
        .unwrap();
        let (tx, mut rx) = mpsc::channel(16);
        let mut runner = EffectRunner::new(tx);
        let request = || PreviewRequest {
            path: file.clone(),
            kind: PreviewKind::Auto,
            show_hidden: false,
            max_bytes: 1 << 20,
            max_lines: 1000,
        };
        let ask = |runner: &mut EffectRunner| {
            runner.run([Effect::RequestPreview {
                gen: gen(),
                request: request(),
            }]);
        };

        ask(&mut runner);
        let Msg::Preview(first) = next(&mut rx).await else {
            panic!("a preview")
        };
        assert!(!first.0.from_cache);
        ask(&mut runner);
        let Msg::Preview(again) = next(&mut rx).await else {
            panic!("a preview")
        };
        assert!(again.0.from_cache, "same palette: served from the cache");

        runner.set_preview_palette(Palette::light());
        ask(&mut runner);
        let Msg::Preview(relit) = next(&mut rx).await else {
            panic!("a preview")
        };
        assert!(!relit.0.from_cache, "new palette: highlighted again");
        assert!(!std::sync::Arc::ptr_eq(&first.0.result, &relit.0.result));
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn listing_a_missing_directory_reports_the_error() {
        let dir = tempfile::tempdir().unwrap();
        let (tx, mut rx) = mpsc::channel(16);
        let mut runner = EffectRunner::new(tx);
        let g = gen();
        runner.run([Effect::ListDir {
            gen: g,
            path: dir.path().join("missing"),
        }]);
        match next(&mut rx).await {
            Msg::ListingFinished { gen, error } => {
                assert_eq!(gen, g);
                assert_eq!(error.unwrap().kind, FsErrorKind::NotFound);
            }
            other => panic!("unexpected {other:?}"),
        }
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn drives_are_listed() {
        let (tx, mut rx) = mpsc::channel(16);
        let mut runner = EffectRunner::new(tx);
        runner.run([Effect::ListDrives { gen: gen() }]);
        match next(&mut rx).await {
            Msg::DrivesLoaded { drives, .. } => assert!(!drives.is_empty()),
            other => panic!("unexpected {other:?}"),
        }
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn a_watched_directory_reports_changes_until_watching_stops() {
        let dir = tempfile::tempdir().unwrap();
        let (tx, mut rx) = mpsc::channel(16);
        let mut runner = EffectRunner::new(tx);
        let g = gen();
        runner.run([Effect::WatchDir {
            gen: g,
            path: dir.path().to_owned(),
        }]);
        // Give the watch time to be established.
        tokio::time::sleep(Duration::from_millis(300)).await;
        std::fs::write(dir.path().join("new.txt"), "x").unwrap();
        assert_eq!(next(&mut rx).await, Msg::DirChanged { gen: g });

        runner.run([Effect::StopWatching]);
        tokio::time::sleep(Duration::from_millis(300)).await;
        while rx.try_recv().is_ok() {}
        std::fs::write(dir.path().join("later.txt"), "x").unwrap();
        assert!(timeout(Duration::from_millis(700), rx.recv())
            .await
            .is_err());
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn parked_directories_are_watched_as_a_set() {
        let a = tempfile::tempdir().unwrap();
        let b = tempfile::tempdir().unwrap();
        let (tx, mut rx) = mpsc::channel(16);
        let mut runner = EffectRunner::new(tx);
        runner.run([Effect::WatchParked {
            paths: vec![a.path().to_owned(), b.path().to_owned()],
        }]);
        tokio::time::sleep(Duration::from_millis(400)).await;
        std::fs::write(b.path().join("new.txt"), "x").unwrap();
        assert_eq!(
            next(&mut rx).await,
            Msg::ParkedDirChanged {
                path: b.path().to_owned()
            }
        );
        std::fs::write(a.path().join("new.txt"), "x").unwrap();
        assert_eq!(
            next(&mut rx).await,
            Msg::ParkedDirChanged {
                path: a.path().to_owned()
            }
        );

        // Dropping `a` from the set stops its watch; `b` keeps working.
        runner.run([Effect::WatchParked {
            paths: vec![b.path().to_owned()],
        }]);
        tokio::time::sleep(Duration::from_millis(400)).await;
        while rx.try_recv().is_ok() {}
        std::fs::write(a.path().join("later.txt"), "x").unwrap();
        assert!(timeout(Duration::from_millis(700), rx.recv())
            .await
            .is_err());
        std::fs::write(b.path().join("later.txt"), "x").unwrap();
        assert_eq!(
            next(&mut rx).await,
            Msg::ParkedDirChanged {
                path: b.path().to_owned()
            }
        );
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn a_missing_parked_directory_is_skipped_quietly() {
        let dir = tempfile::tempdir().unwrap();
        let (tx, mut rx) = mpsc::channel(16);
        let mut runner = EffectRunner::new(tx);
        runner.run([Effect::WatchParked {
            paths: vec![dir.path().join("missing")],
        }]);
        assert!(timeout(Duration::from_millis(500), rx.recv())
            .await
            .is_err());
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn watching_a_missing_directory_warns() {
        let dir = tempfile::tempdir().unwrap();
        let (tx, mut rx) = mpsc::channel(16);
        let mut runner = EffectRunner::new(tx);
        runner.run([Effect::WatchDir {
            gen: gen(),
            path: dir.path().join("missing"),
        }]);
        match next(&mut rx).await {
            Msg::Notify { level, text } => {
                assert_eq!(level, Level::Warning);
                assert!(text.contains("cannot be watched"), "{text}");
            }
            other => panic!("unexpected {other:?}"),
        }
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn git_status_outside_a_repository_is_none() {
        let dir = tempfile::tempdir().unwrap();
        let (tx, mut rx) = mpsc::channel(16);
        let mut runner = EffectRunner::new(tx);
        let g = gen();
        runner.run([Effect::GitStatus {
            gen: g,
            dir: dir.path().to_owned(),
        }]);
        assert_eq!(
            next(&mut rx).await,
            Msg::GitStatus {
                gen: g,
                snapshot: None
            }
        );
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn git_status_requests_for_the_same_directory_coalesce() {
        let dir = tempfile::tempdir().unwrap();
        let (tx, mut rx) = mpsc::channel(16);
        let mut runner = EffectRunner::new(tx);
        let (g1, g2, g3) = (gen(), gen(), gen());
        let request = |g| Effect::GitStatus {
            gen: g,
            dir: dir.path().to_owned(),
        };
        runner.run([request(g1), request(g2), request(g3)]);
        // Only the newest generation is ever answered, and with at most one extra scan.
        let mut answers = vec![next(&mut rx).await];
        while let Ok(Some(msg)) = timeout(Duration::from_millis(800), rx.recv()).await {
            answers.push(msg);
        }
        assert!(
            (1..=2).contains(&answers.len()),
            "{} answers: {answers:?}",
            answers.len()
        );
        for answer in &answers {
            match answer {
                Msg::GitStatus { gen, .. } => assert_eq!(*gen, g3, "{answers:?}"),
                other => panic!("unexpected {other:?}"),
            }
        }

        // A different directory replaces the run instead of joining it.
        let other = tempfile::tempdir().unwrap();
        let g4 = gen();
        runner.run([Effect::GitStatus {
            gen: g4,
            dir: other.path().to_owned(),
        }]);
        match next(&mut rx).await {
            Msg::GitStatus { gen, .. } => assert_eq!(gen, g4),
            other => panic!("unexpected {other:?}"),
        }
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn child_counts_are_reported() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::create_dir(dir.path().join("sub")).unwrap();
        std::fs::write(dir.path().join("sub").join("f"), "x").unwrap();
        let (tx, mut rx) = mpsc::channel(16);
        let mut runner = EffectRunner::new(tx);
        let g = gen();
        runner.run([Effect::CountChildren {
            gen: g,
            dir: dir.path().to_owned(),
            names: vec!["sub".into()],
        }]);
        assert_eq!(
            next(&mut rx).await,
            Msg::ChildCounts {
                gen: g,
                counts: vec![("sub".into(), 1)]
            }
        );
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn entry_stats_are_reported_for_relative_paths() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::create_dir(dir.path().join("sub")).unwrap();
        std::fs::write(dir.path().join("sub").join("f.txt"), "abc").unwrap();
        let (tx, mut rx) = mpsc::channel(16);
        let mut runner = EffectRunner::new(tx);
        let g = gen();
        let rel: OsString = std::path::Path::new("sub").join("f.txt").into_os_string();
        runner.run([Effect::StatEntries {
            gen: g,
            dir: dir.path().to_owned(),
            names: vec![rel.clone()],
        }]);
        match next(&mut rx).await {
            Msg::EntryStats { gen, stats } => {
                assert_eq!(gen, g);
                assert_eq!(stats.len(), 1);
                assert_eq!(stats[0].0, rel);
                assert_eq!(stats[0].1, 3);
                assert!(stats[0].2.is_some());
            }
            other => panic!("unexpected {other:?}"),
        }
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn shutdown_stops_watching() {
        let dir = tempfile::tempdir().unwrap();
        let (tx, mut rx) = mpsc::channel(16);
        let mut runner = EffectRunner::new(tx);
        runner.run([Effect::WatchDir {
            gen: gen(),
            path: dir.path().to_owned(),
        }]);
        tokio::time::sleep(Duration::from_millis(300)).await;
        runner.shutdown();
        tokio::time::sleep(Duration::from_millis(300)).await;
        std::fs::write(dir.path().join("x"), "x").unwrap();
        assert!(timeout(Duration::from_millis(700), rx.recv())
            .await
            .is_err());
    }
}
