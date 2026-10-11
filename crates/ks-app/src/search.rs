//! Workers behind the search effects (phases P3 + P4).
//!
//! * [`FinderWorker`] owns a `ks_search::FinderSession` on a dedicated thread. nucleo's
//!   snapshot model is pull based, so the worker ticks the session while it is walking or
//!   matching (about every 16 ms) and pushes a [`FinderSnapshot`] to the UI as a `Msg`,
//!   throttled to the frame rate. A keystroke is answered immediately (no throttling), so the
//!   keystroke-to-results latency is the matcher's own. When nothing is running the thread
//!   sleeps on its command channel.
//! * Content search needs no worker: `ks_search::spawn_search` already has its own threads
//!   and its events map 1:1 onto `Msg::Grep` (see the effect runner).

use std::path::PathBuf;
use std::sync::mpsc::{self, RecvTimeoutError, TryRecvError};
use std::sync::Arc;
use std::thread::JoinHandle;
use std::time::{Duration, Instant};

use ks_core::{FinderSnapshot, Msg};
use ks_search::{FinderSession, Notify};
use tokio::sync::mpsc::error::TrySendError;
use tokio::sync::mpsc::Sender;

/// Minimum time between two snapshots of a changing result set.
pub const SNAPSHOT_INTERVAL: Duration = Duration::from_millis(16);
/// How long one `tick` waits for the matcher.
const TICK_WAIT_MS: u64 = 10;
/// How long a snapshot may wait for the matcher to finish a new query.
const MAX_SETTLE: Duration = Duration::from_millis(250);

enum Cmd {
    Query {
        query: String,
        offset: u32,
        limit: u32,
    },
    Window {
        offset: u32,
        limit: u32,
    },
    /// New items or matches may be available (from nucleo / the walker).
    Wake,
    Stop,
}

/// Handle to a find-file worker thread. Dropping it stops the worker (and with it the walk).
pub struct FinderWorker {
    gen: u64,
    cmds: mpsc::Sender<Cmd>,
    join: Option<JoinHandle<()>>,
}

impl FinderWorker {
    /// Starts a worker. `make` builds the session on the worker thread from the wake-up
    /// callback; it also returns the root being searched (shown to the user).
    pub fn spawn(
        gen: u64,
        make: impl FnOnce(Notify) -> (FinderSession, PathBuf) + Send + 'static,
        query: String,
        offset: u32,
        limit: u32,
        out: Sender<Msg>,
    ) -> Self {
        let (cmds, rx) = mpsc::channel();
        let wake = cmds.clone();
        let (failed_out, failed_query) = (out.clone(), query.clone());
        let join = std::thread::Builder::new()
            .name("ks-finder".into())
            .spawn(move || {
                let notify: Notify = Arc::new(move || {
                    let _ = wake.send(Cmd::Wake);
                });
                let (session, root) = make(notify);
                run(gen, session, root, query, (offset, limit), &rx, &out);
            })
            .map_err(|error| {
                tracing::error!(%error, "cannot start the finder thread");
                // Stop the spinner and tell the user instead of walking forever.
                let _ = failed_out.try_send(Msg::FinderResults {
                    gen,
                    snapshot: Box::new(ks_core::search::FinderSnapshot {
                        query: failed_query,
                        root: PathBuf::new(),
                        matched: 0,
                        total: 0,
                        walking: false,
                        from_cache: false,
                        denied: 0,
                        offset: 0,
                        results: Vec::new(),
                    }),
                });
                let _ = failed_out.try_send(Msg::Notify {
                    level: ks_core::state::Level::Error,
                    text: format!("Cannot start the file search: {error}"),
                });
            })
            .ok();
        Self { gen, cmds, join }
    }

    pub fn gen(&self) -> u64 {
        self.gen
    }

    pub fn set_query(&self, query: String, offset: u32, limit: u32) {
        let _ = self.cmds.send(Cmd::Query {
            query,
            offset,
            limit,
        });
    }

    pub fn set_window(&self, offset: u32, limit: u32) {
        let _ = self.cmds.send(Cmd::Window { offset, limit });
    }
}

impl Drop for FinderWorker {
    fn drop(&mut self) {
        let _ = self.cmds.send(Cmd::Stop);
        // Never wait for the thread: dropping the session cancels the walk on its own time.
        drop(self.join.take());
    }
}

fn run(
    gen: u64,
    mut session: FinderSession,
    root: PathBuf,
    mut query: String,
    mut window: (u32, u32),
    rx: &mpsc::Receiver<Cmd>,
    out: &Sender<Msg>,
) {
    session.set_query(&query);
    // Results are only sent once the matcher has caught up with the latest query, so a
    // snapshot labelled with a query always answers that query.
    let mut settling_since = Some(Instant::now());
    let from_cache = session.from_cache();
    let mut need_tick = true;
    let mut need_send = true;
    let mut urgent = true;
    let mut busy = true;
    let mut last_walking = true;
    let mut last_send: Option<Instant> = None;

    loop {
        // Wait for something to do: a command, or the next frame while work is going on.
        let wait = if need_tick || (need_send && urgent) {
            Some(Duration::ZERO)
        } else if need_send {
            let since = last_send.map_or(SNAPSHOT_INTERVAL, |t| t.elapsed());
            Some(SNAPSHOT_INTERVAL.saturating_sub(since))
        } else if busy {
            Some(SNAPSHOT_INTERVAL)
        } else {
            None
        };
        let first = match wait {
            Some(d) => rx.recv_timeout(d),
            None => rx.recv().map_err(|_| RecvTimeoutError::Disconnected),
        };
        let mut cmd = match first {
            Ok(cmd) => Some(cmd),
            Err(RecvTimeoutError::Timeout) => None,
            Err(RecvTimeoutError::Disconnected) => return,
        };
        loop {
            match cmd.take() {
                Some(Cmd::Stop) => return,
                Some(Cmd::Wake) => need_tick = true,
                Some(Cmd::Query {
                    query: q,
                    offset,
                    limit,
                }) => {
                    if q != query {
                        settling_since = Some(Instant::now());
                        session.set_query(&q);
                        query = q;
                    }
                    window = (offset, limit);
                    need_tick = true;
                    need_send = true;
                    urgent = true;
                }
                Some(Cmd::Window { offset, limit }) => {
                    window = (offset, limit);
                    need_send = true;
                    urgent = true;
                }
                None => {}
            }
            match rx.try_recv() {
                Ok(next) => cmd = Some(next),
                Err(TryRecvError::Empty) => break,
                Err(TryRecvError::Disconnected) => return,
            }
        }

        if need_tick || busy {
            let status = session.tick(TICK_WAIT_MS);
            need_tick = false;
            busy = status.busy();
            if status.changed || status.walking != last_walking {
                need_send = true;
            }
            last_walking = status.walking;
            if !status.matching {
                settling_since = None;
            }
        }

        let due = urgent || last_send.is_none_or(|t| t.elapsed() >= SNAPSHOT_INTERVAL);
        // A huge list that takes long to match still shows partial results after a while.
        let settled = settling_since.is_none_or(|t| t.elapsed() > MAX_SETTLE);
        if need_send && due && settled {
            let snapshot = FinderSnapshot {
                query: query.clone(),
                root: root.clone(),
                matched: session.matched_count(),
                total: session.total_count(),
                walking: session.walking(),
                from_cache,
                denied: session.denied_count(),
                offset: window.0,
                results: session.results(window.0, window.1),
            };
            match out.try_send(Msg::FinderResults {
                gen,
                snapshot: Box::new(snapshot),
            }) {
                Ok(()) => {
                    need_send = false;
                    urgent = false;
                    last_send = Some(Instant::now());
                }
                // The UI is behind: try again with fresher data on the next frame.
                Err(TrySendError::Full(_)) => {
                    urgent = false;
                    last_send = Some(Instant::now());
                }
                Err(TrySendError::Closed(_)) => return,
            }
        }
    }
}
