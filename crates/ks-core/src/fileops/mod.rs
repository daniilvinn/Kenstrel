//! File operations: yank register, paste, trash, delete, rename, create,
//! bulk rename, undo, conflict prompts, the jobs table, copy-path/open-with menus, the `!`
//! pager, and the prompt / confirm state behind them.
//!
//! State lives in [`FileOps`] (`AppState::fileops`); the engine (`ks-fs`) is reached only
//! through [`Effect`](crate::effect::Effect)s and [`Msg`](crate::msg::Msg)s, so this module is
//! testable without touching the disk.

mod input;
mod logic;
mod types;

pub use input::PromptInput;
pub use logic::{
    confirmation_token, job_summary, pager_rows, pager_width, rename_cursor, same_path, targets,
};
pub use types::*;

pub(crate) use logic::{
    discard_reedit_if_any, handle_action, intercept_key, on_arm_timer, on_bulk_planned,
    on_foreground_done, on_foreground_request, on_job, on_open_handlers, on_shell, request_quit,
};
