//! Engine events. The app wires an [`EventSink`] that converts these to its own `Msg` values.
//!
//! Failure reporting: every long-running piece reports its end through an event of its own
//! (`Preflight` always yields exactly one event, `PaneExited` ends a pane). `IpcServer::start`
//! returns its startup error synchronously; later accept errors are logged with `tracing` and
//! retried, so there is deliberately no "server failed" event. The sink has no "closed" signal:
//! it is a plain callback, and a sink whose receiver is gone should simply drop events.

use std::sync::{Arc, Mutex};

use tokio_util::sync::CancellationToken;

use crate::ipc::{Request, Response};
use crate::preflight::PreflightResult;

/// Callback invoked from engine threads. Must be cheap and non-blocking (typically a channel send).
pub type EventSink = Arc<dyn Fn(ClaudeEvent) + Send + Sync>;

/// One-shot reply handle for an IPC request. The app answers by calling [`ReplyHandle::send`].
///
/// It is neither `Clone` nor `Debug`-transparent: a `Msg` variant that must be `Clone` should
/// carry [`SharedReply`] (see [`ReplyHandle::into_shared`]).
pub struct ReplyHandle {
    pub(crate) tx: tokio::sync::oneshot::Sender<Response>,
    pub(crate) cancel: CancellationToken,
}

impl ReplyHandle {
    /// Send the response back to the connected MCP/hook process. Ignored if the peer is gone.
    pub fn send(self, response: Response) {
        let _ = self.tx.send(response);
    }

    /// Token cancelled when nobody is waiting for the answer any more: the server's reply
    /// timeout expired, the server stopped, or the connection ended. Long-running work done on
    /// behalf of the request (directory walks, searches) should poll it and stop.
    pub fn cancelled(&self) -> CancellationToken {
        self.cancel.clone()
    }

    /// Wrap into a cloneable handle for use inside a `Clone` message type.
    pub fn into_shared(self) -> SharedReply {
        SharedReply(Arc::new(Mutex::new(Some(self))))
    }
}

impl std::fmt::Debug for ReplyHandle {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("ReplyHandle")
    }
}

/// Cloneable, take-once wrapper around a [`ReplyHandle`] (`Arc<Mutex<Option<..>>>`).
#[derive(Clone)]
pub struct SharedReply(Arc<Mutex<Option<ReplyHandle>>>);

impl SharedReply {
    /// Take the handle (first caller wins; later calls get `None`).
    pub fn take(&self) -> Option<ReplyHandle> {
        self.0.lock().unwrap_or_else(|p| p.into_inner()).take()
    }

    /// Answer if nobody has yet; returns whether this call delivered the response.
    pub fn send(&self, response: Response) -> bool {
        match self.take() {
            Some(h) => {
                h.send(response);
                true
            }
            None => false,
        }
    }
}

impl std::fmt::Debug for SharedReply {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("SharedReply")
    }
}

/// Everything the engine can report.
#[derive(Debug)]
pub enum ClaudeEvent {
    /// `spawn_preflight` finished.
    Preflight {
        /// Caller supplied generation.
        generation: u64,
        /// Outcome.
        result: PreflightResult,
    },
    /// The embedded pane's screen changed; call `ClaudePane::snapshot`. Coalesced: at most one
    /// pending event until the next `snapshot()`.
    PaneUpdated {
        /// Generation passed to `ClaudePane::spawn`.
        generation: u64,
    },
    /// The embedded `claude` process exited.
    PaneExited {
        /// Generation passed to `ClaudePane::spawn`.
        generation: u64,
        /// Exit code if known.
        code: Option<u32>,
    },
    /// An MCP/hook process sent a request over IPC. Answer via `reply` (a hook event can be
    /// answered with `Response::Ok`). If the app never replies, the client times out.
    Ipc {
        /// Generation passed to `IpcServer::start`.
        generation: u64,
        /// The request.
        request: Request,
        /// Reply handle.
        reply: ReplyHandle,
    },
}
