//! Kestrel core: state, messages, effects, the keymap model and the config model.
//!
//! This crate has no terminal, filesystem or async dependencies, so everything in it can be
//! tested by feeding [`Msg`]s to [`update`] and asserting on the resulting [`AppState`].

pub mod action;
pub mod config;
pub mod effect;
pub mod generation;
pub mod key;
pub mod keymap;
pub mod mode;
pub mod msg;
pub mod state;
pub mod update;

pub use action::Action;
pub use config::Config;
pub use effect::Effect;
pub use generation::{Gen, GenCounter};
pub use key::{Key, KeyChord, Modifiers};
pub use keymap::Keymap;
pub use mode::{KeyContext, Mode, Overlay};
pub use msg::{Msg, TimerKind};
pub use state::{AppState, JobId, Level, QuitKind, Size, StartParams, Status};
pub use update::update;
