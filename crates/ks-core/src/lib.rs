//! Kestrel core: state, messages, effects, the keymap model and the config model.
//!
//! This crate has no terminal, filesystem or async dependencies, so everything in it can be
//! tested by feeding [`Msg`]s to [`update`] and asserting on the resulting [`AppState`].

pub mod action;
pub mod aids;
pub mod bookmarks;
pub mod command;
pub mod config;
pub mod cursor;
pub mod docs;
pub mod drives;
pub mod effect;
pub mod entry;
pub mod error;
pub mod fileops;
pub mod frecency;
pub mod generation;
pub mod git;
pub mod help;
pub mod key;
pub mod keymap;
pub mod layout;
pub mod mode;
pub mod msg;
mod nav;
pub mod palette;
pub mod state;
pub mod tabs;
#[cfg(test)]
mod testworld;
pub mod text_input;
pub mod update;
pub mod view;

pub use action::Action;
pub use aids::{pending_writes, JumpHit, JumpState, LoadedNavData, NavAids, StateFile};
pub use bookmarks::Bookmarks;
pub use config::Config;
pub use drives::{DriveInfo, DriveKind, DrivesView};
pub use effect::Effect;
pub use entry::{Attrs, Entry, EntryKind};
pub use error::{FsError, FsErrorKind};
pub use frecency::{Frecency, FrecencyEntry};
pub use generation::{Gen, GenCounter};
pub use git::{GitBranch, GitCounts, GitRecord, GitSnapshot, GitState};
pub use key::{Key, KeyChord, Modifiers};
pub use keymap::Keymap;
pub use mode::{KeyContext, Mode, Overlay};
pub use msg::{Msg, TimerKind};
pub use state::{AppState, JobId, Level, Places, QuitKind, Size, StartParams, Status};
pub use tabs::Tabs;
pub use update::update;
pub use view::DirView;
