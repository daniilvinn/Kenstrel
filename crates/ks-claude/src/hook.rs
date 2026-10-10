//! 10e: `ks hook` entry point: forwards Claude Code `PostToolUse` hook events to the TUI.
//!
//! VERIFIED against https://code.claude.com/docs/en/hooks.md (2026-10): the hook receives JSON on
//! stdin with common fields `session_id`, `transcript_path`, `cwd`, `permission_mode`,
//! `hook_event_name` (`"PostToolUse"`) plus `tool_name`, `tool_input`, `tool_response`,
//! `tool_use_id`. `tool_input.file_path` is the target of `Edit`/`Write`/`MultiEdit`; `NotebookEdit`
//! uses `tool_input.notebook_path`. Exit 0 = success (stdout is only logged); exit 2 would feed
//! stderr back to Claude, so this process **always exits 0** and prints nothing.
//! The payload shape was not captured from a live hook run (that needs a real Claude turn).

use std::io::Read;
use std::time::Duration;

use serde_json::Value;

use crate::ipc::{call_blocking, Request, IPC_ENV};

/// Maximum stdin we read (a `Write` payload contains the whole file).
const MAX_STDIN: u64 = 32 * 1024 * 1024;
/// Total time budget for forwarding to the TUI.
const FORWARD_TIMEOUT: Duration = Duration::from_millis(1500);

/// Extracted hook fields.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct HookInput {
    /// Claude session id.
    pub session_id: String,
    /// Tool name.
    pub tool_name: String,
    /// `tool_input.file_path` or (for `NotebookEdit`) `tool_input.notebook_path`.
    pub file_path: Option<String>,
}

/// Parse the PostToolUse JSON. Returns `None` if it is not valid JSON or lacks `tool_name`.
pub fn parse_hook_input(json: &str) -> Option<HookInput> {
    let v: Value = serde_json::from_str(json).ok()?;
    let tool_name = v.get("tool_name")?.as_str()?.to_string();
    let session_id = v
        .get("session_id")
        .and_then(Value::as_str)
        .unwrap_or_default()
        .to_string();
    let input = v.get("tool_input");
    let file_path = ["file_path", "notebook_path"].iter().find_map(|k| {
        input
            .and_then(|i| i.get(*k))
            .and_then(Value::as_str)
            .filter(|s| !s.is_empty())
            .map(str::to_string)
    });
    Some(HookInput {
        session_id,
        tool_name,
        file_path,
    })
}

/// Entry point for `ks hook`: read stdin, forward to `$KESTREL_IPC`, return the exit code (always 0).
pub fn run_hook(stdin: impl Read) -> i32 {
    let name = std::env::var(IPC_ENV).ok();
    run_hook_with(stdin, name.as_deref())
}

/// Like [`run_hook`] with an explicit socket name (testable).
pub fn run_hook_with(stdin: impl Read, ipc_name: Option<&str>) -> i32 {
    let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
        let Some(name) = ipc_name.filter(|n| !n.is_empty()) else {
            return;
        };
        let mut raw = String::new();
        if stdin.take(MAX_STDIN).read_to_string(&mut raw).is_err() {
            return;
        }
        let Some(h) = parse_hook_input(&raw) else {
            return;
        };
        let req = Request::HookEvent {
            session_id: h.session_id,
            tool_name: h.tool_name,
            file_path: h.file_path,
        };
        if let Err(e) = call_blocking(name, &req, FORWARD_TIMEOUT) {
            tracing::warn!("hook forward failed: {e}");
        }
    }));
    let _ = result;
    0
}

#[cfg(test)]
mod tests {
    use super::*;

    const EDIT: &str = r#"{"session_id":"abc123","transcript_path":"/t.jsonl","cwd":"C:\\p",
      "permission_mode":"default","hook_event_name":"PostToolUse","tool_name":"Edit",
      "tool_input":{"file_path":"C:\\p\\src\\app.ts","old_string":"a","new_string":"b"},
      "tool_response":{"success":true},"tool_use_id":"toolu_1"}"#;

    #[test]
    fn edit_payload() {
        let h = parse_hook_input(EDIT).unwrap();
        assert_eq!(h.session_id, "abc123");
        assert_eq!(h.tool_name, "Edit");
        assert_eq!(h.file_path.as_deref(), Some("C:\\p\\src\\app.ts"));
    }

    #[test]
    fn notebook_payload_uses_notebook_path() {
        let j = r#"{"session_id":"s","tool_name":"NotebookEdit","tool_input":{"notebook_path":"n.ipynb","new_source":"x"}}"#;
        assert_eq!(
            parse_hook_input(j).unwrap().file_path.as_deref(),
            Some("n.ipynb")
        );
    }

    #[test]
    fn missing_pieces() {
        assert_eq!(
            parse_hook_input(r#"{"tool_name":"Bash","tool_input":{"command":"ls"}}"#)
                .unwrap()
                .file_path,
            None
        );
        assert!(parse_hook_input(r#"{"session_id":"s"}"#).is_none());
        assert!(parse_hook_input("garbage").is_none());
        assert!(parse_hook_input("").is_none());
    }

    #[test]
    fn always_exit_zero() {
        assert_eq!(run_hook_with(EDIT.as_bytes(), None), 0);
        assert_eq!(
            run_hook_with("garbage".as_bytes(), Some("kestrel-nonexistent-zzz")),
            0
        );
        assert_eq!(
            run_hook_with(EDIT.as_bytes(), Some("kestrel-nonexistent-zzz")),
            0
        );
        assert_eq!(run_hook_with(std::io::empty(), Some("")), 0);
    }

    #[test]
    fn forwards_to_server() {
        use crate::events::{ClaudeEvent, EventSink};
        use crate::ipc::{IpcServer, IpcServerOptions, Response};
        use std::sync::{mpsc, Arc, Mutex};
        let name = format!("kestrel-test-hook-{}", std::process::id());
        let (tx, rx) = mpsc::channel();
        let tx = Mutex::new(tx);
        let sink: EventSink = Arc::new(move |ev| {
            if let ClaudeEvent::Ipc { request, reply, .. } = ev {
                let _ = tx.lock().unwrap().send(request);
                reply.send(Response::Ok);
            }
        });
        let _server = IpcServer::start(
            &name,
            1,
            sink,
            tokio_util::sync::CancellationToken::new(),
            IpcServerOptions::default(),
        )
        .unwrap();
        assert_eq!(run_hook_with(EDIT.as_bytes(), Some(&name)), 0);
        let got = rx.recv_timeout(Duration::from_secs(3)).unwrap();
        assert_eq!(
            got,
            Request::HookEvent {
                session_id: "abc123".into(),
                tool_name: "Edit".into(),
                file_path: Some("C:\\p\\src\\app.ts".into())
            }
        );
    }

    #[test]
    fn multi_edit_payload() {
        let j = r#"{"session_id":"s9","hook_event_name":"PostToolUse","tool_name":"MultiEdit",
          "tool_input":{"file_path":"C:\\p\\a.rs","edits":[{"old_string":"a","new_string":"b"},
          {"old_string":"c","new_string":"d"}]},"tool_response":{"success":true}}"#;
        let h = parse_hook_input(j).unwrap();
        assert_eq!(h.tool_name, "MultiEdit");
        assert_eq!(h.session_id, "s9");
        assert_eq!(h.file_path.as_deref(), Some("C:\\p\\a.rs"));
    }

    #[test]
    fn stdin_is_size_capped() {
        struct Counting(u64);
        impl Read for Counting {
            fn read(&mut self, buf: &mut [u8]) -> std::io::Result<usize> {
                buf.fill(b'a');
                self.0 += buf.len() as u64;
                Ok(buf.len())
            }
        }
        let mut src = Counting(0);
        let t = std::time::Instant::now();
        assert_eq!(run_hook_with(&mut src, Some("kestrel-nonexistent-zzz")), 0);
        assert!(src.0 <= MAX_STDIN + 64 * 1024, "read {} bytes", src.0);
        assert!(t.elapsed() < Duration::from_secs(10));
    }

    #[test]
    fn unresponsive_server_does_not_hold_the_hook_past_the_forward_timeout() {
        use crate::events::{ClaudeEvent, EventSink};
        use crate::ipc::{IpcServer, IpcServerOptions};
        use std::sync::{Arc, Mutex};
        let name = format!("kestrel-test-hook-hang-{}", std::process::id());
        let held = Arc::new(Mutex::new(Vec::new()));
        let h2 = held.clone();
        let sink: EventSink = Arc::new(move |ev| {
            if let ClaudeEvent::Ipc { reply, .. } = ev {
                h2.lock().unwrap().push(reply); // never answered
            }
        });
        let _server = IpcServer::start(
            &name,
            1,
            sink,
            tokio_util::sync::CancellationToken::new(),
            IpcServerOptions {
                reply_timeout: Duration::from_secs(30),
            },
        )
        .unwrap();
        let t = std::time::Instant::now();
        assert_eq!(run_hook_with(EDIT.as_bytes(), Some(&name)), 0);
        let took = t.elapsed();
        assert!(took >= Duration::from_millis(1000), "{took:?}");
        assert!(
            took < FORWARD_TIMEOUT + Duration::from_millis(1500),
            "{took:?}"
        );
    }

    #[test]
    fn unreachable_server_returns_immediately() {
        let t = std::time::Instant::now();
        assert_eq!(
            run_hook_with(EDIT.as_bytes(), Some("kestrel-nonexistent-yyy")),
            0
        );
        assert!(
            t.elapsed() < Duration::from_millis(1000),
            "{:?}",
            t.elapsed()
        );
    }
}
