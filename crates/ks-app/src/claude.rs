//! Glue between the `ks-claude` engine and the app (PLAN section 10): performs the
//! `Effect::Claude*` requests of `update` and turns engine events into `Msg`s.
//!
//! Nothing here reads application state. Requests carry everything they need, and results come
//! back as messages, so the host is driven entirely by the effect runner:
//!
//! * engine threads report through an unbounded channel (the sink never blocks, which matters
//!   because the IPC server calls it from inside its own async runtime); a forwarder task turns
//!   the events into `Msg`s, rate-limiting screen snapshots to about 60 per second;
//! * keys, pastes, mentions and resizes go through one ordered writer thread per pane, so a slow
//!   pseudo console can never stall the UI loop and keystrokes can never be reordered;
//! * file-system work for IPC requests runs on the blocking pool and replies from there.

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::mpsc as std_mpsc;
use std::sync::{Arc, Mutex, MutexGuard};
use std::time::Duration;

use ks_claude::env::EnvPolicy;
use ks_claude::events::{ClaudeEvent, EventSink, ReplyHandle};
use ks_claude::gen::{
    default_launch_dir, sweep_stale_launch_dirs, write_launch_files, HookCommandStyle,
};
use ks_claude::ipc::{ipc_name_for_pid, IpcServer, IpcServerOptions, Request, Response, IPC_ENV};
use ks_claude::keys::{KeyCode as PaneKey, KeyEvent as PaneKeyEvent, Modifiers as PaneMods};
use ks_claude::pane::{ClaudePane, Color, PaneConfig, PaneError, PaneSize, ScreenSnapshot};
use ks_claude::preflight::{PreflightOptions, PreflightResult};
use ks_claude::session::{new_session_id, SessionMode, SessionStore};
use ks_claude::wt::{launch_wt_split, WtSplit};
use ks_core::claude::{
    ClaudeCell, ClaudeColor, ClaudeDirEntry, ClaudeHit, ClaudePreflight, ClaudeRequest,
    ClaudeResponse, ClaudeScreen, ClaudeSpawn,
};
use ks_core::{Effect, Key, KeyChord, Level, Modifiers, Msg, Size};
use ks_search::WalkCache;
use tokio::sync::mpsc::{self, UnboundedReceiver};
use tokio_util::sync::CancellationToken;

use crate::ipc_work;

/// Screen snapshots are taken at most this often (about 60 fps).
const SNAPSHOT_INTERVAL: Duration = Duration::from_millis(16);
/// Cached file walks older than this are dropped, so files changed outside Claude show up.
const WALK_CACHE_TTL: Duration = Duration::from_secs(5);
/// A diff request waits this long and is skipped if a newer one arrived (cursor moves).
const DIFF_DEBOUNCE: Duration = Duration::from_millis(60);
/// Launch directories of dead processes are removed once they are older than this margin.
const STALE_LAUNCH_DIR_AGE: Duration = Duration::from_secs(60);
/// A resumed session that fails this soon after launch is taken to be unresumable (`claude`
/// rejected the stored id): the id is forgotten so `g r` does not retry it forever.
const RESUME_FAILURE_WINDOW: Duration = Duration::from_secs(30);

/// Where the host keeps its files. All optional so tests never touch the real locations.
#[derive(Clone, Debug, Default)]
pub struct ClaudeOptions {
    /// JSON map of project root to session id (`SessionStore`). `None` disables resume.
    pub sessions_file: Option<PathBuf>,
    /// Directory for the generated `--mcp-config` / `--settings` files
    /// (default: `<temp>/kestrel-<pid>`).
    pub launch_dir: Option<PathBuf>,
    /// The `ks` executable the generated files point at (default: the running one).
    pub ks_exe: Option<PathBuf>,
    /// Name of the IPC socket (default: `kestrel-<pid>`).
    pub ipc_name: Option<String>,
    /// Where Claude keeps its transcripts (default: `~/.claude/projects`); decides whether a
    /// stored session can be resumed.
    pub projects_dir: Option<PathBuf>,
}

fn lock<T>(m: &Mutex<T>) -> MutexGuard<'_, T> {
    m.lock().unwrap_or_else(|p| p.into_inner())
}

/// One thing for a pane's writer thread to do. Ordered: the thread handles them one by one.
enum InputOp {
    Key(KeyChord),
    Paste(String),
    Text(String),
    Resize(PaneSize),
    Scroll(isize),
}

struct Running {
    generation: u64,
    cwd: PathBuf,
    pane: Arc<ClaudePane>,
    input: std_mpsc::Sender<InputOp>,
}

/// What the latest launch was, to judge its exit.
struct LaunchInfo {
    generation: u64,
    root: PathBuf,
    /// Directory the process was started in.
    cwd: PathBuf,
    resumed: bool,
    started: std::time::Instant,
}

/// Whether the exit of a launched pane means its stored session id is no good: a `--resume`
/// that failed at once *and* said so. A quick Ctrl+C or an unrelated crash (network, auth)
/// leaves a valid conversation alone.
fn resume_failed(info: &LaunchInfo, code: Option<u32>, output: &str) -> bool {
    info.resumed
        && code.is_some_and(|c| c != 0)
        && info.started.elapsed() < RESUME_FAILURE_WINDOW
        && output.to_lowercase().contains("no conversation found")
}

struct Inner {
    last_launch: Mutex<Option<LaunchInfo>>,
    options: ClaudeOptions,
    tx: mpsc::Sender<Msg>,
    shutdown: CancellationToken,
    sink: EventSink,
    running: Mutex<Option<Running>>,
    ipc: Mutex<Option<IpcServer>>,
    replies: Mutex<HashMap<u64, ReplyHandle>>,
    next_reply: AtomicU64,
    launch_dir: Mutex<Option<PathBuf>>,
    swept: AtomicBool,
    /// Panes launched under an older generation than this were closed before they started.
    min_generation: AtomicU64,
    walk_cache: Arc<WalkCache>,
    /// When the walk cache was last emptied (it is also emptied on hook events).
    walk_cache_epoch: Mutex<std::time::Instant>,
    /// Latest `ClaudeDiff` request; older ones that have not started git yet give up.
    diff_seq: AtomicU64,
    /// A Windows Terminal split may still use the generated files after we exit.
    split_launched: AtomicBool,
}

/// Owns everything Claude-related that is not plain state. Cheap to create; background tasks
/// only start with the first Claude effect.
pub struct ClaudeHost {
    inner: Arc<Inner>,
    events: Option<UnboundedReceiver<ClaudeEvent>>,
    forwarder_started: bool,
}

impl ClaudeHost {
    /// `tx` receives every message the host produces.
    pub fn new(tx: mpsc::Sender<Msg>, options: ClaudeOptions) -> Self {
        let (event_tx, event_rx) = mpsc::unbounded_channel();
        let sink: EventSink = Arc::new(move |event| {
            // The receiver is gone only during shutdown.
            let _ = event_tx.send(event);
        });
        Self {
            inner: Arc::new(Inner {
                last_launch: Mutex::new(None),
                options,
                tx,
                shutdown: CancellationToken::new(),
                sink,
                running: Mutex::new(None),
                ipc: Mutex::new(None),
                replies: Mutex::new(HashMap::new()),
                next_reply: AtomicU64::new(1),
                launch_dir: Mutex::new(None),
                swept: AtomicBool::new(false),
                min_generation: AtomicU64::new(0),
                walk_cache: Arc::new(WalkCache::new()),
                walk_cache_epoch: Mutex::new(std::time::Instant::now()),
                diff_seq: AtomicU64::new(0),
                split_launched: AtomicBool::new(false),
            }),
            events: Some(event_rx),
            forwarder_started: false,
        }
    }

    /// True while an embedded pane exists.
    pub fn has_pane(&self) -> bool {
        lock(&self.inner.running).is_some()
    }

    /// Performs one Claude effect. Never blocks: slow work runs on other threads.
    pub fn run(&mut self, effect: Effect) {
        self.ensure_forwarder();
        let inner = &self.inner;
        match effect {
            Effect::ClaudePreflight { generation, spawn } => {
                ks_claude::preflight::spawn_preflight(
                    generation,
                    PreflightOptions {
                        program: spawn.binary,
                        env: EnvPolicy::new(spawn.strip_env),
                        ..PreflightOptions::default()
                    },
                    inner.shutdown.child_token(),
                    inner.sink.clone(),
                );
            }
            Effect::ClaudeLaunch {
                generation,
                cwd,
                resume,
                size,
                spawn,
            } => {
                let inner = Arc::clone(inner);
                tokio::task::spawn_blocking(move || {
                    let msg = match inner.launch(generation, cwd, resume, size, spawn) {
                        Ok(()) => Msg::ClaudeStarted { generation },
                        Err(e) => Msg::ClaudeLaunchFailed {
                            generation,
                            message: e.to_string(),
                        },
                    };
                    let _ = inner.tx.blocking_send(msg);
                });
            }
            Effect::ClaudeWtSplit { cwd, spawn } => {
                let inner = Arc::clone(inner);
                // `wt` is spawned detached; do it off the loop in case the launch is slow.
                tokio::task::spawn_blocking(move || {
                    let tx = inner.tx.clone();
                    let mut split = WtSplit::new(cwd);
                    split.env = EnvPolicy::new(spawn.strip_env);
                    // Same MCP tools and hooks as the embedded pane. The pane's shell does not
                    // inherit our environment, so KESTREL_IPC is set in its command line.
                    if let Some(name) = inner.ensure_ipc() {
                        match inner.write_launch_files(&name) {
                            Ok(files) => {
                                split.claude_args = files.claude_args();
                                split.set_env.push((IPC_ENV.to_owned(), name));
                            }
                            Err(e) => tracing::warn!("cannot write Claude launch files: {e}"),
                        }
                    }
                    let msg = match launch_wt_split(&split) {
                        Ok(()) => {
                            // The split keeps using the generated files after we exit.
                            inner.split_launched.store(true, Ordering::Release);
                            Msg::Notify {
                                level: Level::Info,
                                text: "Opened Claude in a Windows Terminal split".to_owned(),
                            }
                        }
                        Err(e) => Msg::Notify {
                            level: Level::Error,
                            text: format!("Could not open a Windows Terminal split: {e}"),
                        },
                    };
                    let _ = tx.blocking_send(msg);
                });
            }
            Effect::ClaudeDiff { path } => {
                let tx = inner.tx.clone();
                let seq = inner.diff_seq.fetch_add(1, Ordering::AcqRel) + 1;
                let inner = Arc::clone(inner);
                tokio::task::spawn_blocking(move || {
                    // Holding `j` queues a request per row: only the newest one runs git.
                    std::thread::sleep(DIFF_DEBOUNCE);
                    if inner.diff_seq.load(Ordering::Acquire) != seq {
                        return;
                    }
                    let text =
                        git_diff_until(&path, || inner.diff_seq.load(Ordering::Acquire) != seq);
                    let _ = tx.blocking_send(Msg::ClaudeDiff { path, text });
                });
            }
            Effect::ClaudeKey(chord) => self.input(InputOp::Key(chord)),
            Effect::ClaudePaste(text) => self.input(InputOp::Paste(text)),
            Effect::ClaudeSendPaths { paths } => {
                if let Some(cwd) = self.pane_cwd() {
                    self.input(InputOp::Text(ks_claude::mention::format_mentions(
                        &paths, &cwd,
                    )));
                }
            }
            Effect::ClaudeSendRange { path, start, end } => {
                if let Some(cwd) = self.pane_cwd() {
                    self.input(InputOp::Text(ks_claude::mention::format_range_mention(
                        &path, &cwd, start, end,
                    )));
                }
            }
            Effect::ClaudeResize { size } => self.input(InputOp::Resize(pane_size(size))),
            Effect::ClaudeScroll { lines } => self.input(InputOp::Scroll(lines)),
            Effect::ClaudeClose { generation } => {
                // A launch that is still in progress finds this and kills its own pane.
                inner.min_generation.fetch_max(generation, Ordering::AcqRel);
                if let Some(running) = lock(&inner.running).take() {
                    running.pane.kill();
                }
            }
            Effect::ClaudeIpcReply { id, response } => {
                if let Some(handle) = lock(&inner.replies).remove(&id) {
                    handle.send(to_engine_response(response));
                }
            }
            Effect::ClaudeIpcWork { id, request, ctx } => {
                let Some(handle) = lock(&inner.replies).remove(&id) else {
                    return;
                };
                let inner = Arc::clone(inner);
                if matches!(request, ClaudeRequest::SearchFiles { .. }) {
                    let mut epoch = lock(&inner.walk_cache_epoch);
                    if epoch.elapsed() > WALK_CACHE_TTL {
                        inner.walk_cache.clear();
                        *epoch = std::time::Instant::now();
                    }
                }
                tokio::task::spawn_blocking(move || {
                    let outcome =
                        ipc_work::run(&request, &ctx, &inner.walk_cache, &handle.cancelled());
                    if let Some(path) = outcome.reveal {
                        let _ = inner.tx.blocking_send(Msg::ClaudeReveal { path });
                    }
                    handle.send(to_engine_response(outcome.response));
                });
            }
            // Handled by the terminal layer, not by Claude.
            Effect::SetBracketedPaste(on) => crate::terminal::set_bracketed_paste(on),
            other => tracing::warn!(?other, "not a Claude effect"),
        }
    }

    fn pane_cwd(&self) -> Option<PathBuf> {
        lock(&self.inner.running).as_ref().map(|r| r.cwd.clone())
    }

    fn input(&self, op: InputOp) {
        if let Some(running) = lock(&self.inner.running).as_ref() {
            let _ = running.input.send(op);
        }
    }

    /// Starts the IPC server if it is not running and returns its socket name; `None` if it
    /// cannot start (Claude then runs without the Kestrel MCP tools and hooks). A launch does
    /// this lazily; tests and tools may do it explicitly.
    pub fn ensure_ipc(&mut self) -> Option<String> {
        self.ensure_forwarder();
        self.inner.ensure_ipc()
    }

    fn ensure_forwarder(&mut self) {
        if self.forwarder_started {
            return;
        }
        let Some(rx) = self.events.take() else {
            return;
        };
        self.forwarder_started = true;
        tokio::spawn(forward(Arc::clone(&self.inner), rx));
    }

    /// Stops everything: kills the pane, closes the IPC server and removes generated files.
    pub fn shutdown(&mut self) {
        let inner = &self.inner;
        inner.shutdown.cancel();
        if let Some(running) = lock(&inner.running).take() {
            running.pane.kill();
        }
        *lock(&inner.ipc) = None;
        lock(&inner.replies).clear();
        let dir = lock(&inner.launch_dir).take();
        // Keep the files when a Windows Terminal split still uses them; the next launch sweeps
        // them once they are a day old.
        if let Some(dir) = dir.filter(|_| !inner.split_launched.load(Ordering::Acquire)) {
            let _ = std::fs::remove_dir_all(dir);
        }
    }
}

impl Drop for ClaudeHost {
    fn drop(&mut self) {
        self.shutdown();
    }
}

impl Inner {
    fn ensure_ipc(&self) -> Option<String> {
        let mut slot = lock(&self.ipc);
        if let Some(server) = slot.as_ref() {
            return Some(server.name().to_owned());
        }
        let name = self
            .options
            .ipc_name
            .clone()
            .unwrap_or_else(|| ipc_name_for_pid(std::process::id()));
        match IpcServer::start(
            &name,
            0,
            self.sink.clone(),
            self.shutdown.child_token(),
            IpcServerOptions::default(),
        ) {
            Ok(server) => {
                *slot = Some(server);
                Some(name)
            }
            Err(e) => {
                tracing::warn!("cannot start the IPC server: {e}");
                None
            }
        }
    }

    fn write_launch_files(&self, ipc_name: &str) -> std::io::Result<ks_claude::gen::LaunchFiles> {
        let ks_exe = match &self.options.ks_exe {
            Some(exe) => exe.clone(),
            None => std::env::current_exe()?,
        };
        let dir = self
            .options
            .launch_dir
            .clone()
            .unwrap_or_else(default_launch_dir);
        let files = write_launch_files(&dir, &ks_exe, ipc_name, HookCommandStyle::default())?;
        *lock(&self.launch_dir) = Some(dir);
        Ok(files)
    }

    /// Spawns the pane. Runs on the blocking pool: it creates files and a pseudo console.
    fn launch(
        self: &Arc<Self>,
        generation: u64,
        cwd: PathBuf,
        resume: bool,
        size: Size,
        spawn: ClaudeSpawn,
    ) -> Result<(), LaunchError> {
        if let Some(previous) = lock(&self.running).take() {
            previous.pane.kill();
        }
        if !self.swept.swap(true, Ordering::AcqRel) && self.options.launch_dir.is_none() {
            sweep_stale_launch_dirs(
                &std::env::temp_dir(),
                std::process::id(),
                STALE_LAUNCH_DIR_AGE,
                ks_fs::pid_alive,
            );
        }
        let ipc_name = self.ensure_ipc();

        let root = project_root(&cwd);
        let (mode, cwd) = match &self.options.sessions_file {
            // Sessions belong to the project (the enclosing git repository), not to the
            // directory Kestrel happened to be in.
            Some(file) => {
                let mut store = SessionStore::new(file);
                if self.options.projects_dir.is_some() {
                    store = store.with_projects_dir(self.options.projects_dir.clone());
                }
                // The process starts where the stored session was created: Claude only
                // finds its transcript from that directory.
                let launch = store.launch_for(&root, &cwd, resume)?;
                (launch.mode, launch.cwd)
            }
            None => (SessionMode::New(new_session_id()), cwd),
        };
        *lock(&self.last_launch) = Some(LaunchInfo {
            generation,
            root,
            cwd: cwd.clone(),
            resumed: matches!(mode, SessionMode::Resume(_)),
            started: std::time::Instant::now(),
        });
        let mut args = mode.args();
        let mut env = Vec::new();
        if let Some(name) = &ipc_name {
            match self.write_launch_files(name) {
                Ok(files) => {
                    args.extend(files.claude_args());
                    env.push((IPC_ENV.to_owned(), name.clone()));
                }
                Err(e) => tracing::warn!("cannot write Claude launch files: {e}"),
            }
        }

        let mut config = PaneConfig::new(cwd.clone(), pane_size(size));
        config.program = spawn.binary;
        config.env_policy = EnvPolicy::new(spawn.strip_env);
        config.args = args;
        config.env = env;
        let pane = Arc::new(ClaudePane::spawn(
            config,
            generation,
            self.sink.clone(),
            self.shutdown.child_token(),
        )?);
        let (input, input_rx) = std_mpsc::channel();
        let writer = Writer {
            pane: Arc::clone(&pane),
            generation,
            tx: self.tx.clone(),
        };
        std::thread::Builder::new()
            .name("ks-claude-input".into())
            .spawn(move || writer.run(&input_rx))?;
        let mut running = lock(&self.running);
        if generation < self.min_generation.load(Ordering::Acquire) {
            // Closed while it was starting.
            pane.kill();
            return Err(LaunchError::Closed);
        }
        if pane.has_exited() {
            // Died while starting: its exit event found nothing registered. `update` resets
            // the phase from that event, so just do not keep the dead pane.
            return Ok(());
        }
        *running = Some(Running {
            generation,
            cwd,
            pane,
            input,
        });
        Ok(())
    }
}

/// The enclosing git repository of `cwd` (a `.git` directory or file), else `cwd` itself.
pub fn project_root(cwd: &Path) -> PathBuf {
    cwd.ancestors()
        .find(|dir| dir.join(".git").exists())
        .unwrap_or(cwd)
        .to_path_buf()
}

/// Cap for the diff text shown in the changes view.
const DIFF_MAX_LINES: usize = 400;

/// `git diff HEAD` of one file (staged and unstaged); a new untracked file is shown as all-added.
/// Always returns something displayable: git errors become a one-line explanation.
#[cfg(test)]
fn git_diff(path: &Path) -> String {
    git_diff_until(path, || false)
}

/// Most bytes of git output that are read (the display is capped at [`DIFF_MAX_LINES`] anyway).
const DIFF_MAX_BYTES: u64 = 512 * 1024;

/// Runs git and reads at most [`DIFF_MAX_BYTES`] of its output, killing it after that.
fn run_git(dir: &Path, args: &[&std::ffi::OsStr]) -> std::io::Result<(bool, Vec<u8>)> {
    use std::io::Read;
    let mut child = ks_fs::git::git_command(dir)
        .args(["-c", "core.quotepath=false", "--no-pager"])
        .args(args)
        .stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::null())
        .spawn()?;
    let mut out = Vec::new();
    if let Some(stdout) = child.stdout.take() {
        stdout.take(DIFF_MAX_BYTES).read_to_end(&mut out)?;
    }
    // Over the cap the process may still be writing: do not wait for it.
    let capped = out.len() as u64 >= DIFF_MAX_BYTES;
    if capped {
        let _ = child.kill();
    }
    let status = child.wait()?;
    Ok((capped || status.success(), out))
}

/// The empty file `git diff --no-index` compares an untracked file against.
const NULL_DEVICE: &str = if cfg!(windows) { "NUL" } else { "/dev/null" };

/// [`git_diff`] that gives up (with a placeholder text nobody shows) once `stale()` is true.
fn git_diff_until(path: &Path, stale: impl Fn() -> bool) -> String {
    let (Some(dir), Some(name)) = (path.parent(), path.file_name()) else {
        return "No diff available".to_owned();
    };
    use std::ffi::OsStr;
    let git = |args: &[&OsStr]| {
        if stale() {
            return Err(std::io::Error::other("superseded"));
        }
        run_git(dir, args)
    };
    let name: &OsStr = name;
    let tracked = git(&[
        OsStr::new("diff"),
        OsStr::new("--no-color"),
        OsStr::new("--no-ext-diff"),
        OsStr::new("--no-textconv"),
        OsStr::new("HEAD"),
        OsStr::new("--"),
        name,
    ]);
    let text = match tracked {
        Err(e) => return format!("git is not available: {e}"),
        Ok((false, _)) => return "Not in a git repository (no diff)".to_owned(),
        Ok((true, out)) => String::from_utf8_lossy(&out).into_owned(),
    };
    let is_tracked = || {
        git(&[
            OsStr::new("ls-files"),
            OsStr::new("--error-unmatch"),
            OsStr::new("--"),
            name,
        ])
        .is_ok_and(|(ok, _)| ok)
    };
    let text = if text.trim().is_empty() && path.is_file() && !is_tracked() {
        // Untracked (new) file: compare against nothing. Exit status 1 only means "differs".
        match git(&[
            OsStr::new("diff"),
            OsStr::new("--no-color"),
            OsStr::new("--no-ext-diff"),
            OsStr::new("--no-textconv"),
            OsStr::new("--no-index"),
            OsStr::new("--"),
            OsStr::new(NULL_DEVICE),
            name,
        ]) {
            Ok((_, out)) => String::from_utf8_lossy(&out).into_owned(),
            Err(_) => String::new(),
        }
    } else {
        text
    };
    if text.trim().is_empty() {
        return "No changes against HEAD".to_owned();
    }
    let mut lines: Vec<&str> = text.lines().take(DIFF_MAX_LINES + 1).collect();
    let truncated = lines.len() > DIFF_MAX_LINES;
    lines.truncate(DIFF_MAX_LINES);
    let mut out = lines.join("\n");
    if truncated {
        out.push_str("\n... (diff truncated)");
    }
    out
}

#[derive(Debug, thiserror::Error)]
enum LaunchError {
    #[error(transparent)]
    Pane(#[from] PaneError),
    #[error("cannot store the session id: {0}")]
    Io(#[from] std::io::Error),
    #[error("closed before it started")]
    Closed,
}

/// Applies the ordered operations of one pane.
struct Writer {
    pane: Arc<ClaudePane>,
    generation: u64,
    tx: mpsc::Sender<Msg>,
}

impl Writer {
    fn run(self, ops: &std_mpsc::Receiver<InputOp>) {
        while let Ok(op) = ops.recv() {
            match op {
                InputOp::Key(chord) => {
                    self.leave_history();
                    if let Some(key) = to_pane_key(chord) {
                        if let Err(e) = self.pane.send_key(key) {
                            tracing::warn!("cannot send a key to Claude: {e}");
                        }
                    }
                }
                InputOp::Paste(text) | InputOp::Text(text) => {
                    self.leave_history();
                    if let Err(e) = self.pane.paste(&text) {
                        tracing::warn!("cannot paste into Claude: {e}");
                    }
                }
                InputOp::Resize(size) => self.pane.resize(size),
                InputOp::Scroll(lines) => {
                    self.pane.scroll_by(lines);
                    self.post_screen();
                }
            }
        }
    }

    /// Typing snaps the view back to the live screen.
    fn leave_history(&self) {
        if self.pane.scroll_by(0) > 0 {
            self.pane.set_scrollback(0);
            self.post_screen();
        }
    }

    fn post_screen(&self) {
        let screen = convert_screen(&self.pane.snapshot());
        let _ = self.tx.blocking_send(Msg::ClaudeScreen {
            generation: self.generation,
            screen: Box::new(screen),
        });
    }
}

/// Snapshot of the pane of `generation` as a message; `None` if that pane is not registered.
fn screen_msg(inner: &Inner, generation: u64) -> Option<Msg> {
    lock(&inner.running)
        .as_ref()
        .filter(|r| r.generation == generation)
        .map(|r| Msg::ClaudeScreen {
            generation,
            screen: Box::new(convert_screen(&r.pane.snapshot())),
        })
}

/// Turns engine events into messages. Screen snapshots are rate limited without ever delaying
/// other events: an update that comes too early is remembered and served by a timer.
async fn forward(inner: Arc<Inner>, mut events: UnboundedReceiver<ClaudeEvent>) {
    let mut last_snapshot = tokio::time::Instant::now() - SNAPSHOT_INTERVAL;
    // Generation whose snapshot is waiting for the rate limit.
    let mut pending: Option<u64> = None;
    loop {
        let msg = tokio::select! {
            event = events.recv() => {
                let Some(event) = event else { break };
                match event {
                    ClaudeEvent::Preflight { generation, result } => Some(Msg::ClaudePreflight {
                        generation,
                        result: convert_preflight(result),
                    }),
                    ClaudeEvent::PaneUpdated { generation } => {
                        if last_snapshot.elapsed() >= SNAPSHOT_INTERVAL {
                            last_snapshot = tokio::time::Instant::now();
                            screen_msg(&inner, generation)
                        } else {
                            pending = Some(generation);
                            None
                        }
                    }
                    ClaudeEvent::PaneExited { generation, code } => {
                        // The last output of a failing process is the reason it failed: make
                        // sure the state has seen it before the pane is reset.
                        if code != Some(0) {
                            if let Some(screen) = screen_msg(&inner, generation) {
                                if inner.tx.send(screen).await.is_err() {
                                    break;
                                }
                            }
                        }
                        forget_failed_resume(&inner, generation, code);
                        let mut running = lock(&inner.running);
                        if running.as_ref().is_some_and(|r| r.generation == generation) {
                            *running = None;
                        }
                        drop(running);
                        if pending == Some(generation) {
                            pending = None;
                        }
                        Some(Msg::ClaudePaneExited { generation, code })
                    }
                    ClaudeEvent::Ipc { request, reply, .. } => {
                        if matches!(request, Request::HookEvent { .. }) {
                            // Claude is changing files: file searches must not serve old walks.
                            inner.walk_cache.clear();
                        }
                        let id = inner.next_reply.fetch_add(1, Ordering::Relaxed);
                        let mut replies = lock(&inner.replies);
                        // Requests nobody waits for any more (timed out, connection gone) are dropped.
                        replies.retain(|_, handle| !handle.cancelled().is_cancelled());
                        replies.insert(id, reply);
                        drop(replies);
                        Some(Msg::ClaudeIpc {
                            id,
                            request: to_core_request(request),
                        })
                    }
                }
            }
            () = tokio::time::sleep_until(last_snapshot + SNAPSHOT_INTERVAL), if pending.is_some() => {
                last_snapshot = tokio::time::Instant::now();
                pending.take().and_then(|generation| screen_msg(&inner, generation))
            }
        };
        if let Some(msg) = msg {
            if inner.tx.send(msg).await.is_err() {
                break;
            }
        }
    }
}

/// A `--resume` that died at once: drop the stored session id (off the async threads).
fn forget_failed_resume(inner: &Arc<Inner>, generation: u64, code: Option<u32>) {
    let info = lock(&inner.last_launch).take_if(|i| i.generation == generation);
    let (Some(info), Some(file)) = (info, inner.options.sessions_file.clone()) else {
        return;
    };
    let output = lock(&inner.running)
        .as_ref()
        .filter(|r| r.generation == generation)
        .map(|r| ks_core::claude::screen_text(&convert_screen(&r.pane.snapshot())))
        .unwrap_or_default();
    if resume_failed(&info, code, &output) {
        tracing::warn!(root = %info.root.display(), "resume failed; forgetting the stored session");
        tokio::task::spawn_blocking(move || {
            if let Err(error) = SessionStore::new(file).forget_launched_in(&info.root, &info.cwd) {
                tracing::warn!(%error, "cannot forget the session");
            }
        });
    }
}

fn pane_size(size: Size) -> PaneSize {
    PaneSize {
        cols: size.width.max(1),
        rows: size.height.max(1),
    }
}

// ----- conversions -----

/// Maps the engine's preflight outcome to the core type.
pub fn convert_preflight(result: PreflightResult) -> ClaudePreflight {
    match result {
        PreflightResult::Ok(_) => ClaudePreflight::Ok,
        PreflightResult::BillingWarning(status) => ClaudePreflight::BillingWarning {
            method: status.auth_method.unwrap_or_else(|| "unknown".to_owned()),
            provider: status.api_provider.unwrap_or_else(|| "unknown".to_owned()),
        },
        PreflightResult::NotLoggedIn => ClaudePreflight::NotLoggedIn,
        PreflightResult::NotInstalled { instructions } => {
            ClaudePreflight::NotInstalled(instructions)
        }
        PreflightResult::Failed(message) => ClaudePreflight::Failed(message),
    }
}

fn convert_color(color: Color) -> ClaudeColor {
    match color {
        Color::Default => ClaudeColor::Default,
        Color::Indexed(i) => ClaudeColor::Indexed(i),
        Color::Rgb(r, g, b) => ClaudeColor::Rgb(r, g, b),
    }
}

/// Converts the engine snapshot to the core screen type.
pub fn convert_screen(snapshot: &ScreenSnapshot) -> ClaudeScreen {
    ClaudeScreen {
        cols: snapshot.cols,
        rows: snapshot.rows,
        cells: snapshot
            .cells
            .iter()
            .map(|c| ClaudeCell {
                text: if c.wide_continuation {
                    String::new()
                } else {
                    c.text.clone()
                },
                fg: convert_color(c.fg),
                bg: convert_color(c.bg),
                bold: c.bold,
                dim: c.dim,
                italic: c.italic,
                underline: c.underline,
                inverse: c.inverse,
            })
            .collect(),
        cursor: snapshot.cursor,
    }
}

/// Converts a key chord to the engine's key type; `None` for keys a terminal cannot send.
pub fn to_pane_key(chord: KeyChord) -> Option<PaneKeyEvent> {
    let code = match chord.key {
        Key::Char(c) => PaneKey::Char(c),
        Key::Enter => PaneKey::Enter,
        Key::Esc => PaneKey::Esc,
        Key::Tab => PaneKey::Tab,
        Key::BackTab => PaneKey::BackTab,
        Key::Backspace => PaneKey::Backspace,
        Key::Delete => PaneKey::Delete,
        Key::Insert => PaneKey::Insert,
        Key::Up => PaneKey::Up,
        Key::Down => PaneKey::Down,
        Key::Left => PaneKey::Left,
        Key::Right => PaneKey::Right,
        Key::Home => PaneKey::Home,
        Key::End => PaneKey::End,
        Key::PageUp => PaneKey::PageUp,
        Key::PageDown => PaneKey::PageDown,
        Key::F(n) => PaneKey::F(n),
    };
    Some(PaneKeyEvent::with(
        code,
        PaneMods {
            shift: chord.mods.contains(Modifiers::SHIFT),
            alt: chord.mods.contains(Modifiers::ALT),
            ctrl: chord.mods.contains(Modifiers::CTRL),
        },
    ))
}

fn to_core_request(request: Request) -> ClaudeRequest {
    match request {
        Request::GetSelection => ClaudeRequest::GetSelection,
        Request::SearchFiles { query, root, limit } => {
            ClaudeRequest::SearchFiles { query, root, limit }
        }
        Request::SearchContents {
            pattern,
            glob,
            regex,
            limit,
        } => ClaudeRequest::SearchContents {
            pattern,
            glob,
            regex,
            limit,
        },
        Request::ListDir { path } => ClaudeRequest::ListDir { path },
        Request::Reveal { path } => ClaudeRequest::Reveal { path },
        Request::HookEvent {
            session_id,
            tool_name,
            file_path,
        } => ClaudeRequest::HookEvent {
            session_id,
            tool_name,
            file_path,
        },
    }
}

fn to_engine_response(response: ClaudeResponse) -> Response {
    match response {
        ClaudeResponse::Selection {
            marked,
            cursor,
            cwd,
        } => Response::Selection {
            marked,
            cursor,
            cwd,
        },
        ClaudeResponse::Files { paths, truncated } => Response::Files { paths, truncated },
        ClaudeResponse::Matches { hits, truncated } => Response::Matches {
            hits: hits
                .into_iter()
                .map(
                    |ClaudeHit { path, line, text }| ks_claude::ipc::ContentHit {
                        path,
                        line,
                        text,
                    },
                )
                .collect(),
            truncated,
        },
        ClaudeResponse::Dir { entries } => Response::Dir {
            entries: entries
                .into_iter()
                .map(
                    |ClaudeDirEntry { name, is_dir, size }| ks_claude::ipc::DirEntry {
                        name,
                        is_dir,
                        size,
                    },
                )
                .collect(),
        },
        ClaudeResponse::Ok => Response::Ok,
        ClaudeResponse::Error { message } => Response::Error { message },
    }
}

/// Path of the sessions file under the state directory.
pub fn sessions_file(state_dir: &Path) -> PathBuf {
    state_dir.join("claude-sessions.json")
}

#[cfg(test)]
mod tests {
    use ks_claude::pane::Cell;
    use ks_claude::preflight::{classify, parse_auth_status};
    use ks_core::{update, AppState, Config, Keymap, Places, StartParams};

    use super::*;

    fn chord(text: &str) -> KeyChord {
        text.parse().unwrap()
    }

    #[test]
    fn chords_convert_to_engine_keys_with_modifiers() {
        assert_eq!(
            to_pane_key(chord("ctrl-c")),
            Some(PaneKeyEvent::with(PaneKey::Char('c'), PaneMods::CTRL))
        );
        assert_eq!(
            to_pane_key(chord("A")),
            Some(PaneKeyEvent::new(PaneKey::Char('A')))
        );
        assert_eq!(
            to_pane_key(chord("shift-tab")).unwrap().code,
            PaneKey::BackTab
        );
        assert_eq!(
            to_pane_key(chord("alt-left")),
            Some(PaneKeyEvent::with(PaneKey::Left, PaneMods::ALT))
        );
        assert_eq!(
            to_pane_key(chord("shift-pageup")),
            Some(PaneKeyEvent::with(PaneKey::PageUp, PaneMods::SHIFT))
        );
        assert_eq!(to_pane_key(chord("f5")).unwrap().code, PaneKey::F(5));
        assert_eq!(to_pane_key(chord("esc")).unwrap().code, PaneKey::Esc);
    }

    #[test]
    fn engine_bytes_for_forwarded_keys_match_what_claude_expects() {
        // The forwarded keys must reach the pane exactly as a terminal would send them.
        let encode =
            |text: &str| ks_claude::keys::encode_key(to_pane_key(chord(text)).unwrap(), false);
        assert_eq!(encode("esc"), b"\x1b");
        assert_eq!(encode("ctrl-c"), b"\x03");
        assert_eq!(encode("shift-tab"), b"\x1b[Z");
        assert_eq!(encode("enter"), b"\r");
        assert_eq!(encode("a"), b"a");
    }

    #[test]
    fn snapshots_convert_cell_by_cell() {
        let snapshot = ScreenSnapshot {
            rows: 1,
            cols: 2,
            cells: vec![
                Cell {
                    text: "?".into(),
                    fg: Color::Rgb(1, 2, 3),
                    bold: true,
                    ..Cell::default()
                },
                Cell {
                    text: "x".into(),
                    wide_continuation: true,
                    bg: Color::Indexed(4),
                    inverse: true,
                    ..Cell::default()
                },
            ],
            cursor: Some((0, 1)),
            alternate_screen: true,
            bracketed_paste: false,
        };
        let screen = convert_screen(&snapshot);
        assert_eq!(
            (screen.rows, screen.cols, screen.cursor),
            (1, 2, Some((0, 1)))
        );
        assert_eq!(screen.cells[0].text, "?");
        assert_eq!(screen.cells[0].fg, ClaudeColor::Rgb(1, 2, 3));
        assert!(screen.cells[0].bold);
        assert_eq!(screen.cells[1].text, "");
        assert_eq!(screen.cells[1].bg, ClaudeColor::Indexed(4));
        assert!(screen.cells[1].inverse);
    }

    // ----- preflight decision flow with `claude auth status` fixtures -----

    const SUBSCRIPTION: &str = r#"{"loggedIn":true,"authMethod":"claude.ai","apiProvider":"firstParty","subscriptionType":"max"}"#;
    const API_KEY: &str = r#"{"loggedIn":true,"authMethod":"api_key","apiProvider":"firstParty"}"#;
    const API_KEY_HELPER: &str =
        r#"{"loggedIn":true,"authMethod":"api_key_helper","apiProvider":"firstParty"}"#;
    const BEDROCK: &str = r#"{"loggedIn":true,"authMethod":"claude.ai","apiProvider":"bedrock"}"#;
    const LOGGED_OUT: &str = r#"{"loggedIn":false,"authMethod":"none"}"#;

    fn fixture_state() -> AppState {
        AppState::new(StartParams {
            cwd: PathBuf::from("/work/proj"),
            home: None,
            places: Places::default(),
            config: Config::default(),
            keymap: Keymap::defaults(),
            size: Size::new(120, 32),
            now: std::time::SystemTime::UNIX_EPOCH,
            utc_offset_secs: 0,
        })
    }

    /// Runs the real classifier on `json`, converts like the host does and feeds `c` + result
    /// into a fresh state. Returns the effects of the result message and the state.
    fn decide(json: &str) -> (Vec<Effect>, AppState) {
        let result = classify(parse_auth_status(json).unwrap());
        decide_result(result)
    }

    fn decide_result(result: PreflightResult) -> (Vec<Effect>, AppState) {
        let mut state = fixture_state();
        update(&mut state, Msg::Key(chord("c")));
        let generation = state.claude.generation;
        let effects = update(
            &mut state,
            Msg::ClaudePreflight {
                generation,
                result: convert_preflight(result),
            },
        );
        (effects, state)
    }

    fn launches(effects: &[Effect]) -> bool {
        effects
            .iter()
            .any(|e| matches!(e, Effect::ClaudeLaunch { .. }))
    }

    #[test]
    fn subscription_login_launches_without_asking() {
        let (effects, state) = decide(SUBSCRIPTION);
        assert!(launches(&effects));
        assert!(state.claude.dialog.is_none());
    }

    #[test]
    fn api_key_billing_never_launches_without_the_explicit_y() {
        for json in [API_KEY, API_KEY_HELPER, BEDROCK] {
            let (effects, mut state) = decide(json);
            assert!(!launches(&effects), "{json}");
            assert!(state.claude.dialog.is_some(), "{json}");
            // enter does not confirm, esc aborts without launching anything.
            let effects = update(&mut state, Msg::Key(chord("enter")));
            assert!(!launches(&effects), "{json}");
            let effects = update(&mut state, Msg::Key(chord("esc")));
            assert!(!launches(&effects), "{json}");
            assert!(state.claude.dialog.is_none() && state.overlays.is_empty());
        }
        let (_, mut state) = decide(API_KEY);
        let effects = update(&mut state, Msg::Key(chord("y")));
        assert!(launches(&effects));
    }

    #[test]
    fn logged_out_and_missing_claude_show_instructions() {
        let (effects, state) = decide(LOGGED_OUT);
        assert!(!launches(&effects));
        assert!(matches!(
            state.claude.dialog,
            Some(ks_core::ClaudeDialog::Info { ref body, .. }) if body.contains("/login")
        ));

        let (effects, state) = decide_result(PreflightResult::NotInstalled {
            instructions: ks_claude::preflight::INSTALL_INSTRUCTIONS.to_owned(),
        });
        assert!(!launches(&effects));
        assert!(matches!(
            state.claude.dialog,
            Some(ks_core::ClaudeDialog::Info { ref body, .. }) if body.contains("install.ps1")
        ));

        let (effects, state) = decide_result(PreflightResult::Failed("timed out".into()));
        assert!(!launches(&effects));
        assert!(state.claude.dialog.is_some());
    }

    #[test]
    fn billing_conversion_keeps_method_and_provider_only() {
        let converted = convert_preflight(classify(parse_auth_status(API_KEY).unwrap()));
        assert_eq!(
            converted,
            ClaudePreflight::BillingWarning {
                method: "api_key".into(),
                provider: "firstParty".into()
            }
        );
    }

    #[test]
    fn core_default_strip_list_matches_the_engine() {
        let core: Vec<&str> = ks_core::config::DEFAULT_CLAUDE_STRIP_ENV.to_vec();
        let engine: Vec<&str> = ks_claude::DEFAULT_STRIP_ENV.to_vec();
        assert_eq!(core, engine);
        // And the launch effect carries the configured list.
        let (_, mut state) = decide_result(PreflightResult::Failed("x".into()));
        state.claude.dialog = None;
        let spawn = ClaudeSpawn::from_config(&Config::default().claude);
        assert_eq!(spawn.strip_env, engine);
    }

    /// Cost of one pane frame at 200x60 with a fully coloured screen: snapshot conversion plus
    /// the ratatui draw. Run: `cargo test -p ks-app --release --lib -- --ignored --nocapture perf`.
    #[test]
    #[ignore = "timing measurement"]
    #[allow(clippy::print_stderr)]
    fn perf_convert_and_render_a_full_pane() {
        use std::time::Instant;

        use ks_ui::{ColorDepth, Theme};
        use ratatui::backend::TestBackend;
        use ratatui::Terminal;

        let (rows, cols) = (58u16, 198u16);
        let cells = (0..usize::from(rows) * usize::from(cols))
            .map(|i| Cell {
                text: char::from(b'a' + u8::try_from(i % 26).unwrap_or(0)).to_string(),
                fg: Color::Rgb(u8::try_from(i % 251).unwrap_or(0), 120, 200),
                bg: Color::Indexed(u8::try_from(16 + i % 200).unwrap_or(16)),
                bold: i % 7 == 0,
                ..Cell::default()
            })
            .collect();
        let snapshot = ScreenSnapshot {
            rows,
            cols,
            cells,
            cursor: Some((3, 4)),
            alternate_screen: false,
            bracketed_paste: true,
        };
        let iterations = 300;

        let start = Instant::now();
        let mut screen = None;
        for _ in 0..iterations {
            screen = Some(convert_screen(&snapshot));
        }
        let convert = start.elapsed() / iterations;

        let mut state = fixture_state();
        state.size = Size::new(200, 60);
        state.claude.phase = ks_core::ClaudePhase::Running;
        state.claude.screen = screen;
        let theme = Theme::builtin("ember", ColorDepth::TrueColor).unwrap();
        let mut terminal = Terminal::new(TestBackend::new(200, 60)).unwrap();
        let start = Instant::now();
        for _ in 0..iterations {
            terminal.draw(|f| ks_ui::render(f, &state, &theme)).unwrap();
        }
        let draw = start.elapsed() / iterations;
        eprintln!("convert_screen 200x60: {convert:?}; full frame draw with pane: {draw:?}");
        assert!(convert.as_millis() < 5 && draw.as_millis() < 16);
    }

    #[test]
    fn sessions_file_lives_in_the_state_dir() {
        assert_eq!(
            sessions_file(Path::new("/s")),
            PathBuf::from("/s/claude-sessions.json")
        );
    }
}

#[cfg(test)]
mod resume_failed_tests {
    use super::*;

    fn info(resumed: bool, age: Duration) -> LaunchInfo {
        LaunchInfo {
            generation: 1,
            root: PathBuf::from("r"),
            cwd: PathBuf::from("r"),
            resumed,
            started: std::time::Instant::now() - age,
        }
    }

    #[test]
    fn only_a_quick_rejected_resume_forgets_the_session() {
        let said = "No conversation found with session ID: x";
        let quick = Duration::from_secs(1);
        assert!(resume_failed(&info(true, quick), Some(1), said));
        // Not a resume, a clean exit, an unknown code, or too late.
        assert!(!resume_failed(&info(false, quick), Some(1), said));
        assert!(!resume_failed(&info(true, quick), Some(0), said));
        assert!(!resume_failed(&info(true, quick), None, said));
        assert!(!resume_failed(
            &info(true, Duration::from_secs(60)),
            Some(1),
            said
        ));
        // A quick non-zero exit for another reason keeps the conversation.
        assert!(!resume_failed(&info(true, quick), Some(130), "^C"));
    }
}

#[cfg(test)]
mod project_root_tests {
    use super::project_root;

    #[test]
    fn sessions_are_keyed_by_the_enclosing_git_repository() {
        let dir = tempfile::tempdir().unwrap();
        let repo = dir.path().join("repo");
        let deep = repo.join("src").join("inner");
        std::fs::create_dir_all(&deep).unwrap();
        std::fs::create_dir_all(repo.join(".git")).unwrap();
        assert_eq!(project_root(&deep), repo);
        assert_eq!(project_root(&repo), repo);
        // Outside any repository the directory itself is the project.
        let loose = dir.path().join("loose");
        std::fs::create_dir_all(&loose).unwrap();
        assert_eq!(project_root(&loose), loose);
    }
}

#[cfg(test)]
mod git_diff_tests {
    use super::git_diff;

    fn git(dir: &std::path::Path, args: &[&str]) -> bool {
        std::process::Command::new("git")
            .arg("-C")
            .arg(dir)
            .args(["-c", "user.name=t", "-c", "user.email=t@t"])
            .args(args)
            .output()
            .map(|o| o.status.success())
            .unwrap_or(false)
    }

    #[test]
    fn diff_of_modified_untracked_and_unchanged_files() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path();
        if !git(root, &["init", "-q"]) {
            return; // no git on this machine
        }
        std::fs::write(root.join("a.txt"), "one\ntwo\n").unwrap();
        assert!(git(root, &["add", "a.txt"]));
        assert!(git(root, &["commit", "-q", "-m", "init"]));
        assert_eq!(git_diff(&root.join("a.txt")), "No changes against HEAD");

        std::fs::write(root.join("a.txt"), "one\nthree\n").unwrap();
        let d = git_diff(&root.join("a.txt"));
        assert!(d.contains("-two") && d.contains("+three"), "{d}");

        std::fs::write(root.join("new.txt"), "fresh\n").unwrap();
        let d = git_diff(&root.join("new.txt"));
        assert!(d.contains("+fresh"), "{d}");
    }

    /// A repository from an archive can carry `core.fsmonitor` and `diff.<x>.textconv` programs;
    /// opening the Changes view must not run either.
    #[test]
    fn booby_trapped_repo_never_executes_hooks_on_diff() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path().join("repo");
        std::fs::create_dir(&root).unwrap();
        if !git(&root, &["init", "-q"]) {
            return; // no git on this machine
        }
        std::fs::write(
            root.join("a.txt"),
            "one
",
        )
        .unwrap();
        assert!(git(&root, &["add", "a.txt"]));
        assert!(git(&root, &["commit", "-q", "-m", "init"]));
        std::fs::write(
            root.join(".gitattributes"),
            "* diff=x
",
        )
        .unwrap();
        let fsm = dir.path().join("fsmonitor-ran");
        let tc = dir.path().join("textconv-ran");
        let q = |p: &std::path::Path| p.to_string_lossy().replace(char::from(92), "/");
        let fsm_hook = format!("echo ran > \"{}\"; echo", q(&fsm));
        let tc_hook = format!("echo ran > \"{}\"; cat", q(&tc));
        assert!(git(&root, &["config", "core.fsmonitor", &fsm_hook]));
        assert!(git(&root, &["config", "diff.x.textconv", &tc_hook]));
        std::fs::write(
            root.join("a.txt"),
            "two
",
        )
        .unwrap();

        // Control: plain git fires both hooks, so the assertions below mean something.
        let _ = std::process::Command::new("git")
            .arg("-C")
            .arg(&root)
            .args(["diff", "HEAD", "--", "a.txt"])
            .output();
        let fired = (fsm.exists(), tc.exists());
        let _ = std::fs::remove_file(&fsm);
        let _ = std::fs::remove_file(&tc);
        if !(fired.0 && fired.1) {
            return; // this machine's git has no sh to run the hook strings: nothing to prove
        }

        let d = git_diff(&root.join("a.txt"));
        assert!(d.contains("+two"), "{d}");
        std::fs::write(
            root.join("new.txt"),
            "fresh
",
        )
        .unwrap();
        let _ = git_diff(&root.join("new.txt"));
        assert!(!fsm.exists(), "fsmonitor ran");
        assert!(!tc.exists(), "textconv ran");
    }

    #[test]
    fn outside_a_repository_is_explained() {
        let dir = tempfile::tempdir().unwrap();
        let file = dir.path().join("x.txt");
        std::fs::write(&file, "x").unwrap();
        let d = git_diff(&file);
        // Either "not in a repo" or (if the temp dir sits inside one) a normal answer.
        assert!(!d.is_empty());
    }
}
