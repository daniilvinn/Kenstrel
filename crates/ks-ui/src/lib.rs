//! Kestrel UI: theme tokens and the ratatui rendering of an [`AppState`](ks_core::AppState).

pub mod format;
pub mod help;
pub mod icons;
pub mod list;
pub mod palette;
pub mod popup;
pub mod render;
pub mod text;
pub mod theme;
pub mod which_key;

#[cfg(test)]
mod testutil;

pub use render::{render, HINT_MIN_HEIGHT};
pub use theme::{ColorDepth, Theme, ThemeError, ThemeLoader};
