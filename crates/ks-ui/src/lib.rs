//! Kestrel UI: theme tokens and the ratatui rendering of an [`AppState`](ks_core::AppState).

pub mod format;
pub mod icons;
pub mod images;
pub mod list;
pub mod preview;
pub mod render;
pub mod syntax;
pub mod text;
pub mod theme;

#[cfg(test)]
mod preview_tests;
#[cfg(test)]
mod testutil;
#[cfg(test)]
mod testutil_preview;

pub use render::{render, HINT_MIN_HEIGHT};
pub use theme::{ColorDepth, Theme, ThemeError, ThemeLoader};
