//! 10d: IPC between the running TUI and the `ks mcp` / `ks hook` helper processes.
//!
//! Transport: `interprocess` local socket (named pipe `\\.\pipe\kestrel-<pid>` on Windows),
//! one connection may carry many requests. Framing: newline-delimited JSON.
//!
//! ```text
//! -> {"v":1,"id":7,"req":{"type":"search_files","query":"main","limit":50}}
//! <- {"v":1,"id":7,"res":{"type":"files","paths":["src/main.rs"],"truncated":false}}
//! ```
//!
//! The server runs on its own thread with a private current-thread tokio runtime, so it works
//! whether or not the host has a runtime. Requests are delivered as [`ClaudeEvent::Ipc`] with a
//! [`ReplyHandle`]; the app answers whenever it is ready (a request not answered within
//! `reply_timeout` gets an error response).

use std::time::Duration;

use interprocess::local_socket::tokio::{prelude::*, Stream};
use interprocess::local_socket::{GenericNamespaced, ListenerOptions};
use serde::{Deserialize, Serialize};
use tokio::io::{AsyncBufReadExt, AsyncReadExt, AsyncWriteExt, BufReader};
use tokio_util::sync::CancellationToken;

use crate::events::{ClaudeEvent, EventSink, ReplyHandle};

/// Protocol version carried in every message.
pub const PROTOCOL_VERSION: u32 = 1;
/// Name of the environment variable carrying the socket name to child processes.
pub const IPC_ENV: &str = "KESTREL_IPC";
/// Maximum size of one JSON line.
const MAX_LINE: u64 = 4 * 1024 * 1024;

/// Socket name for a process id: `kestrel-<pid>`.
pub fn ipc_name_for_pid(pid: u32) -> String {
    format!("kestrel-{pid}")
}

/// Requests from MCP / hook processes to the TUI.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum Request {
    /// Marked files, cursor file and cwd.
    GetSelection,
    /// Fuzzy file search.
    SearchFiles {
        /// Query.
        query: String,
        /// Optional root directory (default: project root).
        #[serde(default)]
        root: Option<String>,
        /// Max results.
        #[serde(default = "default_file_limit")]
        limit: usize,
    },
    /// Content search.
    SearchContents {
        /// Pattern (literal unless `regex`).
        pattern: String,
        /// Optional glob filter.
        #[serde(default)]
        glob: Option<String>,
        /// Treat the pattern as a regex.
        #[serde(default)]
        regex: bool,
        /// Max hits.
        #[serde(default = "default_content_limit")]
        limit: usize,
    },
    /// List a directory.
    ListDir {
        /// Directory.
        path: String,
    },
    /// Move the explorer cursor to a path.
    Reveal {
        /// Path to reveal.
        path: String,
    },
    /// PostToolUse hook attribution.
    HookEvent {
        /// Claude session id.
        session_id: String,
        /// Tool name (`Edit`, `Write`, ...).
        tool_name: String,
        /// Edited file, if the payload had one.
        #[serde(default)]
        file_path: Option<String>,
    },
}

fn default_file_limit() -> usize {
    50
}
fn default_content_limit() -> usize {
    200
}

/// A content-search hit.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ContentHit {
    /// File path.
    pub path: String,
    /// 1-based line.
    pub line: usize,
    /// Matching line text.
    pub text: String,
}

/// A directory entry.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct DirEntry {
    /// File name.
    pub name: String,
    /// Directory flag.
    pub is_dir: bool,
    /// Size in bytes for files.
    #[serde(default)]
    pub size: Option<u64>,
}

/// Responses from the TUI.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum Response {
    /// Answer to `GetSelection`.
    Selection {
        /// Marked files.
        marked: Vec<String>,
        /// File under the cursor.
        cursor: Option<String>,
        /// Current directory.
        cwd: String,
    },
    /// Answer to `SearchFiles`.
    Files {
        /// Paths.
        paths: Vec<String>,
        /// More results existed than `limit`.
        truncated: bool,
    },
    /// Answer to `SearchContents`.
    Matches {
        /// Hits.
        hits: Vec<ContentHit>,
        /// More results existed than `limit`.
        truncated: bool,
    },
    /// Answer to `ListDir`.
    Dir {
        /// Entries.
        entries: Vec<DirEntry>,
    },
    /// Success without data (`Reveal`, `HookEvent`).
    Ok,
    /// Failure.
    Error {
        /// Human readable reason.
        message: String,
    },
}

#[derive(Debug, Serialize, Deserialize)]
struct RequestEnvelope {
    v: u32,
    id: u64,
    req: Request,
}

#[derive(Debug, Serialize, Deserialize)]
struct ResponseEnvelope {
    v: u32,
    id: u64,
    res: Response,
}

/// IPC errors (client side and server startup).
#[derive(Debug, thiserror::Error)]
pub enum IpcError {
    /// The TUI is not reachable (not running, wrong name).
    #[error("Kestrel is not reachable: {0}")]
    Unreachable(String),
    /// Request timed out.
    #[error("timed out waiting for Kestrel")]
    Timeout,
    /// Protocol violation.
    #[error("protocol error: {0}")]
    Protocol(String),
    /// IO error.
    #[error(transparent)]
    Io(#[from] std::io::Error),
}

fn to_name(name: &str) -> Result<interprocess::local_socket::Name<'_>, IpcError> {
    name.to_ns_name::<GenericNamespaced>()
        .map_err(|e| IpcError::Unreachable(e.to_string()))
}

// ---------------------------------------------------------------- server

/// Server options.
#[derive(Debug, Clone)]
pub struct IpcServerOptions {
    /// How long to wait for the app to answer a request.
    pub reply_timeout: Duration,
}

impl Default for IpcServerOptions {
    fn default() -> Self {
        Self {
            reply_timeout: Duration::from_secs(15),
        }
    }
}

/// Running IPC server. Dropping it (or cancelling the token) stops it.
pub struct IpcServer {
    name: String,
    cancel: CancellationToken,
}

impl IpcServer {
    /// Start listening on `name` (use [`ipc_name_for_pid`]). The listener is created before this
    /// returns, so clients may connect immediately; accepting runs on a background thread.
    /// Requests are delivered to `sink` as [`ClaudeEvent::Ipc`] tagged with `generation`.
    pub fn start(
        name: &str,
        generation: u64,
        sink: EventSink,
        cancel: CancellationToken,
        opts: IpcServerOptions,
    ) -> Result<IpcServer, IpcError> {
        let rt = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()?;
        let owned = name.to_string();
        let listener = {
            // Creating a tokio listener needs a runtime context.
            let _guard = rt.enter();
            ListenerOptions::new()
                .name(to_name(&owned)?)
                .create_tokio()?
        };
        let token = cancel.child_token();
        let loop_token = token.clone();
        std::thread::Builder::new()
            .name("ks-claude-ipc".into())
            .spawn(move || {
                rt.block_on(async move {
                    loop {
                        tokio::select! {
                            () = loop_token.cancelled() => break,
                            accepted = listener.accept() => match accepted {
                                Ok(conn) => {
                                    tokio::spawn(serve_connection(
                                        conn, generation, sink.clone(), loop_token.clone(), opts.clone(),
                                    ));
                                }
                                Err(e) => {
                                    tracing::warn!("ipc accept failed: {e}");
                                    tokio::time::sleep(Duration::from_millis(50)).await;
                                }
                            }
                        }
                    }
                });
            })?;
        Ok(IpcServer {
            name: owned,
            cancel: token,
        })
    }

    /// The socket name clients connect to (pass it as `KESTREL_IPC`).
    pub fn name(&self) -> &str {
        &self.name
    }

    /// Stop the server.
    pub fn stop(&self) {
        self.cancel.cancel();
    }
}

impl Drop for IpcServer {
    fn drop(&mut self) {
        self.cancel.cancel();
    }
}

async fn serve_connection(
    conn: Stream,
    generation: u64,
    sink: EventSink,
    cancel: CancellationToken,
    opts: IpcServerOptions,
) {
    let (recv, mut send) = conn.split();
    let mut reader = BufReader::new(recv);
    loop {
        let mut line = Vec::new();
        let mut limited = (&mut reader).take(MAX_LINE);
        let read = tokio::select! {
            () = cancel.cancelled() => return,
            r = limited.read_until(b'\n', &mut line) => r,
        };
        match read {
            Ok(0) | Err(_) => return,
            Ok(_) => {}
        }
        if !line.ends_with(b"\n") {
            tracing::warn!("ipc line too long or truncated; closing connection");
            return;
        }
        let (id, res) = match serde_json::from_slice::<RequestEnvelope>(&line) {
            Ok(env) if env.v == PROTOCOL_VERSION => {
                let (tx, rx) = tokio::sync::oneshot::channel();
                // Cancelled when the reply timeout expires, the server stops (child of
                // `cancel`) or this connection task ends (guard), so work done for the request
                // can stop instead of running to its own budget.
                let request_cancel = cancel.child_token();
                let _guard = request_cancel.clone().drop_guard();
                sink(ClaudeEvent::Ipc {
                    generation,
                    request: env.req,
                    reply: ReplyHandle {
                        tx,
                        cancel: request_cancel.clone(),
                    },
                });
                let res = match tokio::time::timeout(opts.reply_timeout, rx).await {
                    Ok(Ok(r)) => r,
                    Ok(Err(_)) => Response::Error {
                        message: "request dropped by Kestrel".into(),
                    },
                    Err(_) => {
                        request_cancel.cancel();
                        Response::Error {
                            message: "Kestrel did not answer in time".into(),
                        }
                    }
                };
                (env.id, res)
            }
            Ok(env) => (
                env.id,
                Response::Error {
                    message: format!(
                        "unsupported protocol version {} (server speaks {PROTOCOL_VERSION})",
                        env.v
                    ),
                },
            ),
            Err(e) => (
                0,
                Response::Error {
                    message: format!("bad request: {e}"),
                },
            ),
        };
        let out = serde_json::to_vec(&ResponseEnvelope {
            v: PROTOCOL_VERSION,
            id,
            res,
        });
        let Ok(mut out) = out else { return };
        out.push(b'\n');
        if send.write_all(&out).await.is_err() || send.flush().await.is_err() {
            return;
        }
    }
}

// ---------------------------------------------------------------- client

/// Send one request and wait for the response (async; used by the MCP server).
pub async fn call(name: &str, request: &Request, timeout: Duration) -> Result<Response, IpcError> {
    tokio::time::timeout(timeout, call_inner(name, request))
        .await
        .map_err(|_| IpcError::Timeout)?
}

async fn call_inner(name: &str, request: &Request) -> Result<Response, IpcError> {
    let conn = Stream::connect(to_name(name)?)
        .await
        .map_err(|e| IpcError::Unreachable(e.to_string()))?;
    let (recv, mut send) = conn.split();
    let mut line = serde_json::to_vec(&RequestEnvelope {
        v: PROTOCOL_VERSION,
        id: 1,
        req: request.clone(),
    })
    .map_err(|e| IpcError::Protocol(e.to_string()))?;
    line.push(b'\n');
    send.write_all(&line).await?;
    send.flush().await?;
    let mut reader = BufReader::new(recv);
    let mut buf = Vec::new();
    let n = (&mut reader)
        .take(MAX_LINE)
        .read_until(b'\n', &mut buf)
        .await?;
    if n == 0 {
        return Err(IpcError::Protocol(
            "connection closed without a response".into(),
        ));
    }
    let env: ResponseEnvelope =
        serde_json::from_slice(&buf).map_err(|e| IpcError::Protocol(e.to_string()))?;
    if env.v != PROTOCOL_VERSION {
        return Err(IpcError::Protocol(format!(
            "server speaks protocol {}",
            env.v
        )));
    }
    Ok(env.res)
}

/// Blocking variant (used by `ks hook`, which has no runtime). Builds a throwaway runtime.
pub fn call_blocking(
    name: &str,
    request: &Request,
    timeout: Duration,
) -> Result<Response, IpcError> {
    let rt = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()?;
    rt.block_on(call(name, request, timeout))
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Arc;

    fn unique_name(tag: &str) -> String {
        format!(
            "kestrel-test-{tag}-{}-{}",
            std::process::id(),
            uuid::Uuid::new_v4().simple()
        )
    }

    /// Sink that answers requests on a helper thread with a canned handler.
    fn answering_sink(
        handler: impl Fn(Request) -> Response + Send + Sync + 'static,
        seen: Arc<std::sync::Mutex<Vec<Request>>>,
    ) -> EventSink {
        let handler = Arc::new(handler);
        Arc::new(move |ev| {
            if let ClaudeEvent::Ipc { request, reply, .. } = ev {
                seen.lock().unwrap().push(request.clone());
                let h = handler.clone();
                std::thread::spawn(move || reply.send(h(request)));
            }
        })
    }

    #[test]
    fn round_trip_all_requests() {
        let name = unique_name("rt");
        let seen = Arc::new(std::sync::Mutex::new(Vec::new()));
        let sink = answering_sink(
            |req| match req {
                Request::GetSelection => Response::Selection {
                    marked: vec!["a.rs".into()],
                    cursor: Some("b.rs".into()),
                    cwd: "C:\\p".into(),
                },
                Request::SearchFiles { query, limit, .. } => Response::Files {
                    paths: vec![format!("{query}:{limit}")],
                    truncated: false,
                },
                Request::SearchContents { pattern, regex, .. } => Response::Matches {
                    hits: vec![ContentHit {
                        path: "x".into(),
                        line: 3,
                        text: format!("{pattern}{regex}"),
                    }],
                    truncated: true,
                },
                Request::ListDir { path } => Response::Dir {
                    entries: vec![DirEntry {
                        name: path,
                        is_dir: true,
                        size: None,
                    }],
                },
                Request::Reveal { .. } | Request::HookEvent { .. } => Response::Ok,
            },
            seen.clone(),
        );
        let cancel = CancellationToken::new();
        let server =
            IpcServer::start(&name, 3, sink, cancel.clone(), IpcServerOptions::default()).unwrap();
        let t = Duration::from_secs(5);

        let r = call_blocking(server.name(), &Request::GetSelection, t).unwrap();
        assert_eq!(
            r,
            Response::Selection {
                marked: vec!["a.rs".into()],
                cursor: Some("b.rs".into()),
                cwd: "C:\\p".into()
            }
        );
        let r = call_blocking(
            &name,
            &Request::SearchFiles {
                query: "q".into(),
                root: None,
                limit: 9,
            },
            t,
        )
        .unwrap();
        assert_eq!(
            r,
            Response::Files {
                paths: vec!["q:9".into()],
                truncated: false
            }
        );
        let r = call_blocking(
            &name,
            &Request::SearchContents {
                pattern: "p".into(),
                glob: Some("*.rs".into()),
                regex: true,
                limit: 5,
            },
            t,
        )
        .unwrap();
        assert!(matches!(
            r,
            Response::Matches {
                truncated: true,
                ..
            }
        ));
        let r = call_blocking(&name, &Request::ListDir { path: "d".into() }, t).unwrap();
        assert!(matches!(r, Response::Dir { .. }));
        assert_eq!(
            call_blocking(&name, &Request::Reveal { path: "x".into() }, t).unwrap(),
            Response::Ok
        );
        let hook = Request::HookEvent {
            session_id: "s".into(),
            tool_name: "Edit".into(),
            file_path: Some("f".into()),
        };
        assert_eq!(call_blocking(&name, &hook, t).unwrap(), Response::Ok);
        assert_eq!(seen.lock().unwrap().len(), 6);
        server.stop();
    }

    #[test]
    fn multiple_requests_on_one_connection_and_version_check() {
        let name = unique_name("raw");
        let seen = Arc::new(std::sync::Mutex::new(Vec::new()));
        let sink = answering_sink(|_| Response::Ok, seen);
        let _server = IpcServer::start(
            &name,
            1,
            sink,
            CancellationToken::new(),
            IpcServerOptions::default(),
        )
        .unwrap();
        let rt = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .unwrap();
        rt.block_on(async {
            let conn = Stream::connect(to_name(&name).unwrap()).await.unwrap();
            let (recv, mut send) = conn.split();
            let mut reader = BufReader::new(recv);
            for (i, v) in [(1u64, 1u32), (2, 99), (3, 1)] {
                let msg = format!(
                    "{{\"v\":{v},\"id\":{i},\"req\":{{\"type\":\"reveal\",\"path\":\"p\"}}}}\n"
                );
                send.write_all(msg.as_bytes()).await.unwrap();
                let mut line = String::new();
                reader.read_line(&mut line).await.unwrap();
                let env: ResponseEnvelope = serde_json::from_str(&line).unwrap();
                assert_eq!(env.id, i);
                if v == 1 {
                    assert_eq!(env.res, Response::Ok);
                } else {
                    assert!(matches!(env.res, Response::Error { .. }));
                }
            }
            // garbage line yields an error response, connection stays usable
            send.write_all(b"not json\n").await.unwrap();
            let mut line = String::new();
            reader.read_line(&mut line).await.unwrap();
            assert!(line.contains("bad request"));
        });
    }

    #[test]
    fn unanswered_request_times_out_with_error() {
        let name = unique_name("to");
        let sink: EventSink = Arc::new(|_| {}); // drops the reply handle
        let _s = IpcServer::start(
            &name,
            1,
            sink,
            CancellationToken::new(),
            IpcServerOptions::default(),
        )
        .unwrap();
        let r = call_blocking(&name, &Request::GetSelection, Duration::from_secs(5)).unwrap();
        assert!(matches!(r, Response::Error { .. }));
    }

    #[test]
    fn unreachable_server_is_graceful_error() {
        let r = call_blocking(
            &unique_name("none"),
            &Request::GetSelection,
            Duration::from_secs(2),
        );
        assert!(
            matches!(r, Err(IpcError::Unreachable(_) | IpcError::Io(_))),
            "{r:?}"
        );
    }

    #[test]
    fn defaults_apply_when_fields_missing() {
        let r: Request = serde_json::from_str(r#"{"type":"search_files","query":"x"}"#).unwrap();
        assert_eq!(
            r,
            Request::SearchFiles {
                query: "x".into(),
                root: None,
                limit: 50
            }
        );
        let r: Request =
            serde_json::from_str(r#"{"type":"search_contents","pattern":"x"}"#).unwrap();
        assert_eq!(
            r,
            Request::SearchContents {
                pattern: "x".into(),
                glob: None,
                regex: false,
                limit: 200
            }
        );
    }

    fn start(name: &str, gen: u64, sink: EventSink, reply_timeout: Duration) -> IpcServer {
        IpcServer::start(
            name,
            gen,
            sink,
            CancellationToken::new(),
            IpcServerOptions { reply_timeout },
        )
        .unwrap()
    }

    #[test]
    fn reply_timeout_yields_error_and_cancels_the_request_token() {
        let name = unique_name("rto");
        let held = Arc::new(std::sync::Mutex::new(Vec::<ReplyHandle>::new()));
        let h2 = held.clone();
        let sink: EventSink = Arc::new(move |ev| {
            if let ClaudeEvent::Ipc { reply, .. } = ev {
                h2.lock().unwrap().push(reply); // never answers, keeps the handle alive
            }
        });
        let _s = start(&name, 1, sink, Duration::from_millis(200));
        let r = call_blocking(&name, &Request::GetSelection, Duration::from_secs(5)).unwrap();
        assert_eq!(
            r,
            Response::Error {
                message: "Kestrel did not answer in time".into()
            }
        );
        let handle = held.lock().unwrap().pop().unwrap();
        assert!(handle.cancelled().is_cancelled());
    }

    #[test]
    fn dropped_reply_handle_gives_request_dropped() {
        let name = unique_name("drop");
        let sink: EventSink = Arc::new(|_| {});
        let _s = start(&name, 1, sink, Duration::from_secs(30));
        let r = call_blocking(&name, &Request::GetSelection, Duration::from_secs(5)).unwrap();
        assert_eq!(
            r,
            Response::Error {
                message: "request dropped by Kestrel".into()
            }
        );
    }

    #[test]
    fn oversized_line_closes_the_connection() {
        let name = unique_name("big");
        let sink = answering_sink(
            |_| Response::Ok,
            Arc::new(std::sync::Mutex::new(Vec::new())),
        );
        let _s = start(&name, 1, sink, Duration::from_secs(5));
        let rt = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .unwrap();
        rt.block_on(async {
            let conn = Stream::connect(to_name(&name).unwrap()).await.unwrap();
            let (recv, mut send) = conn.split();
            let chunk = vec![b'a'; 1 << 20];
            for _ in 0..5 {
                if send.write_all(&chunk).await.is_err() {
                    break; // the server already hung up
                }
            }
            let mut reader = BufReader::new(recv);
            let mut line = Vec::new();
            let n =
                tokio::time::timeout(Duration::from_secs(5), reader.read_until(b'\n', &mut line))
                    .await
                    .expect("server must close, not hang");
            assert!(matches!(n, Ok(0) | Err(_)), "{n:?}");
        });
    }

    #[test]
    fn stop_makes_new_connections_fail() {
        let name = unique_name("stop");
        let sink = answering_sink(
            |_| Response::Ok,
            Arc::new(std::sync::Mutex::new(Vec::new())),
        );
        let server = start(&name, 1, sink, Duration::from_secs(5));
        assert_eq!(
            call_blocking(&name, &Request::GetSelection, Duration::from_secs(5)).unwrap(),
            Response::Ok
        );
        server.stop();
        let end = std::time::Instant::now() + Duration::from_secs(5);
        loop {
            match call_blocking(&name, &Request::GetSelection, Duration::from_secs(1)) {
                Err(_) => break,
                Ok(_) if std::time::Instant::now() < end => {
                    std::thread::sleep(Duration::from_millis(50));
                }
                Ok(r) => panic!("server still answering after stop: {r:?}"),
            }
        }
    }

    #[test]
    fn dropping_the_server_stops_it() {
        let name = unique_name("dropsrv");
        let sink = answering_sink(
            |_| Response::Ok,
            Arc::new(std::sync::Mutex::new(Vec::new())),
        );
        let server = start(&name, 1, sink, Duration::from_secs(5));
        drop(server);
        let end = std::time::Instant::now() + Duration::from_secs(5);
        while call_blocking(&name, &Request::GetSelection, Duration::from_secs(1)).is_ok() {
            assert!(std::time::Instant::now() < end, "still serving after drop");
            std::thread::sleep(Duration::from_millis(50));
        }
    }

    #[test]
    fn generation_is_propagated() {
        let name = unique_name("gen");
        let gens = Arc::new(std::sync::Mutex::new(Vec::new()));
        let g2 = gens.clone();
        let sink: EventSink = Arc::new(move |ev| {
            if let ClaudeEvent::Ipc {
                generation, reply, ..
            } = ev
            {
                g2.lock().unwrap().push(generation);
                reply.send(Response::Ok);
            }
        });
        let _s = start(&name, 77, sink, Duration::from_secs(5));
        call_blocking(&name, &Request::GetSelection, Duration::from_secs(5)).unwrap();
        assert_eq!(*gens.lock().unwrap(), vec![77]);
    }

    #[test]
    fn concurrent_clients_get_their_own_answers() {
        let name = unique_name("conc");
        let sink = answering_sink(
            |req| match req {
                Request::ListDir { path } => Response::Dir {
                    entries: vec![DirEntry {
                        name: path,
                        is_dir: false,
                        size: None,
                    }],
                },
                _ => Response::Ok,
            },
            Arc::new(std::sync::Mutex::new(Vec::new())),
        );
        let _s = start(&name, 1, sink, Duration::from_secs(10));
        let handles: Vec<_> = (0..16)
            .map(|i| {
                let name = name.clone();
                std::thread::spawn(move || {
                    let r = call_blocking(
                        &name,
                        &Request::ListDir {
                            path: format!("dir{i}"),
                        },
                        Duration::from_secs(10),
                    )
                    .unwrap();
                    match r {
                        Response::Dir { entries } => assert_eq!(entries[0].name, format!("dir{i}")),
                        other => panic!("{other:?}"),
                    }
                })
            })
            .collect();
        for h in handles {
            h.join().unwrap();
        }
    }

    /// Timing for the progress notes: `cargo test -p ks-claude --release ipc_latency -- --ignored --nocapture`.
    #[test]
    #[ignore = "timing measurement"]
    #[allow(clippy::print_stderr)]
    fn ipc_latency() {
        let name = unique_name("lat");
        let sink = answering_sink(
            |_| Response::Ok,
            Arc::new(std::sync::Mutex::new(Vec::new())),
        );
        let _s = start(&name, 1, sink, Duration::from_secs(5));
        let n = 200;
        let t = std::time::Instant::now();
        for _ in 0..n {
            call_blocking(&name, &Request::GetSelection, Duration::from_secs(5)).unwrap();
        }
        let per = t.elapsed() / n;
        eprintln!("ipc round trip incl. connect + throwaway runtime: {per:?}");
    }

    #[test]
    fn shared_reply_answers_once() {
        let (tx, rx) = tokio::sync::oneshot::channel();
        let shared = ReplyHandle {
            tx,
            cancel: CancellationToken::new(),
        }
        .into_shared();
        let clone = shared.clone();
        assert!(shared.send(Response::Ok));
        assert!(!clone.send(Response::Ok));
        assert_eq!(rx.blocking_recv().unwrap(), Response::Ok);
    }
}
