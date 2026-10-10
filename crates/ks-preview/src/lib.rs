//! File previews: text, markdown, images, directories, archives, binary and metadata
//! (phase 2 engine).
//!
//! The crate is terminal-agnostic and independent of `ks-core`. Entry points:
//!
//! - [`Previewer`]: background service. `request(path, generation, opts)` cancels the
//!   previous request and later emits a [`PreviewEvent`] through the sink given to
//!   [`Previewer::new`].
//! - [`build_preview`]: the same work as a synchronous function (runs on the caller's
//!   thread; use it only from a thread you own).
//! - [`highlight::warm_up`]: load the syntax set on a background thread at startup.
//!
//! Output is a [`Preview`] whose [`PreviewBody`] holds [`StyledLine`]s (RGB or semantic
//! [`Role`] colours), directory/archive/hex rows or a decoded image. All text derived
//! from file content is sanitized: no control character or escape sequence ever
//! reaches the renderer.

pub mod highlight;
pub mod metadata;
pub mod model;
pub mod options;
pub mod sanitize;

mod content;
mod markdown;
mod service;
mod text;

pub use content::{build_preview, natural_cmp};
pub use metadata::{human_size, Attributes, FileMetadata};
pub use model::*;
pub use options::{Limits, Palette, PreviewOptions};
pub use service::{PreviewEvent, Previewer, CACHE_BYTE_BUDGET, CACHE_CAPACITY};
