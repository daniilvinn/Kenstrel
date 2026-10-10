//! `ks-claude`: the Claude Code integration engine for Kestrel (plan section 10).
//!
//! This crate is a self-contained library (it does **not** depend on `ks-core`). It never reads,
//! copies or stores credentials: it only spawns the user's installed, unmodified `claude` CLI,
//! with API-billing environment variables stripped so the subscription login is used, and it
//! never passes `--bare`.
//!
//! # Threading model
//!
//! * Nothing here blocks the caller's thread for long. Long-running operations (`spawn_preflight`,
//!   [`pane::ClaudePane`], [`ipc::IpcServer`]) run on **dedicated `std::thread`s** (the IPC server
//!   additionally owns a private current-thread tokio runtime), so the host does not need a tokio
//!   runtime to use them.
//! * Every long-running operation carries a caller supplied *generation id* (`u64`) and reports
//!   through an [`events::EventSink`] using [`events::ClaudeEvent`], which the app maps 1:1 to
//!   its `Msg` variants. Stale events are dropped by the app by comparing generations.
//! * Cancellation uses [`tokio_util::sync::CancellationToken`].
//! * Logging uses `tracing` only; nothing is written to stdout/stderr (the MCP stdio server owns
//!   stdout for the protocol and writes nothing else to it).
//!
//! # Modules
//!
//! | module | plan item | purpose |
//! |---|---|---|
//! | [`env`] | 10.1 | credential env stripping policy |
//! | [`preflight`] | 10.1 | `claude auth status` parsing and classification |
//! | [`wt`] | 10a | Windows Terminal split-pane launch command builder |
//! | [`keys`] | 10b | key event to terminal bytes encoder |
//! | [`pane`] | 10b | embedded ConPTY pane + `vt100` screen snapshots |
//! | [`session`] | 10b | per-project session id persistence |
//! | [`mention`] | 10c | `@path` / `@path#La-b` formatting |
//! | [`ipc`] | 10d | local-socket protocol, server and client |
//! | [`mcp`] | 10d | `ks mcp` stdio MCP server (`rmcp`) |
//! | [`hook`] | 10e | `ks hook` PostToolUse hook entry point |
//! | [`gen`] | 10d/10e | `--mcp-config` / `--settings` JSON generators |

pub mod env;
pub mod events;
pub mod gen;
pub mod hook;
pub mod ipc;
pub mod keys;
pub mod mcp;
pub mod mention;
pub mod pane;
pub mod preflight;
pub mod session;
pub mod wt;

pub use env::{EnvPolicy, DEFAULT_STRIP_ENV};
pub use events::{ClaudeEvent, EventSink};
