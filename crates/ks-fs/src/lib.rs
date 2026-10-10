//! Filesystem access: listing, metadata, sorting, file operations and watching (phase 1 and 6).
//!
//! - [`ops`]: background job engine for copy/move/trash/delete/create/rename/bulk rename, conflict
//!   handling and the session undo log.
//! - [`launch`]: clipboard, open-with, editor resolution and the `!` shell command runner.

pub mod launch;
pub mod ops;
