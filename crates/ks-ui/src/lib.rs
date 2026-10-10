//! Kestrel UI: theme tokens and the ratatui rendering of an [`AppState`](ks_core::AppState).

pub mod format;
pub mod icons;
pub mod list;
pub mod navaids;
pub mod render;
pub mod text;
pub mod theme;

#[cfg(test)]
mod testutil;

pub use render::{render, HINT_MIN_HEIGHT};
pub use theme::{ColorDepth, Theme, ThemeError, ThemeLoader};
