//! Search engines for Kestrel: directory walker, fuzzy file finder,
//! in-directory filter and streaming content search.
//!
//! This crate is deliberately self-contained: it defines its own types and does not depend on
//! `ks-core`. UI wiring maps the engine events 1:1 onto application messages.
//!
//! # Threading model
//!
//! * Everything runs on **plain `std::thread`s** (no tokio runtime required). No function in
//!   this crate blocks the caller except the explicitly synchronous ones documented as such
//!   ([`walk::walk`], and [`FinderSession::tick`] with a small timeout).
//! * Long-running operations take a [`CancellationToken`] and a caller supplied `generation`
//!   id which is echoed on every event so stale results can be dropped.
//! * Events are delivered through an [`EventSink`]: a cheap `Fn(E) + Send + Sync` closure that
//!   must not block (typically it forwards into an unbounded channel).
//!
//! # Modules
//!
//! * [`walk`]: parallel, cancellable directory walker; [`scope`]: scope root helpers.
//! * [`finder`]: fuzzy file finder session over `nucleo`, with [`cache`] for instant reopen.
//! * [`filter`]: in-memory list filter for the `/` mode.
//! * [`grep`]: content search pipeline (walker, bounded channel, workers, batching).
//! * [`debounce`]: a tiny pure debouncer.

pub mod cache;
pub mod debounce;
pub mod filter;
pub mod finder;
pub mod grep;
pub mod scope;
pub mod testgen;
pub mod walk;

use std::sync::Arc;

pub use cache::{CacheKey, PathChange, WalkCache};
pub use debounce::Debouncer;
pub use filter::{filter_list, FilterMatch, ListFilter};
pub use finder::{FinderOptions, FinderResult, FinderSession, FinderStatus};
pub use grep::{
    is_unsafe_char, spawn_search, FileMatches, GrepEvent, GrepHandle, GrepQuery, GrepStats,
    LineMatch,
};
pub use scope::{drive_root, find_git_root, home_dir, resolve_scope, Scope};
pub use tokio_util::sync::CancellationToken;
pub use walk::{walk, EntryKind, WalkEntry, WalkOptions, WalkSink, WalkSummary};

/// Callback through which engines deliver events. Must be fast and must not block.
pub type EventSink<E> = Arc<dyn Fn(E) + Send + Sync>;

/// Callback used to wake the UI loop (for example to schedule a `tick`).
pub type Notify = Arc<dyn Fn() + Send + Sync>;
