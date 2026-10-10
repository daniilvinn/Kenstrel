//! Kestrel file system layer: directory listing, drives, watching, git status, file operations
//! ([`ops`]: background jobs, conflicts, undo) and launching ([`launch`]: clipboard, open, editor, shell).
//!
//! Everything here is blocking or async work that runs off the UI thread; `ks-app`'s effect
//! runner calls it and reports results to the core as messages.

pub mod attrs;
pub mod children;
pub mod git;
pub mod launch;
pub mod listing;
pub mod ops;
mod platform;
pub mod watcher;

pub use children::count_children;
pub use git::read_status as git_status;
pub use listing::{link_target, list_dir};
pub use platform::{file_owner, list_drives, utc_offset_secs};
pub use watcher::DirWatcher;
