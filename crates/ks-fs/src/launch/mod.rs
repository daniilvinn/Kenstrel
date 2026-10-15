//! Launching things: clipboard, default apps, editors and the `!` shell runner.
//!
//! - [`clipboard`]: format paths (absolute / relative / name) and put them on the clipboard
//!   behind the [`Clipboard`] trait.
//! - [`open`]: open with the default app (non-blocking) and the "open with..." handler list.
//! - [`editor`]: `$VISUAL` / `$EDITOR` resolution, open-at-line templates, never via a shell.
//!   Terminal editors need the TUI to be suspended: the app calls
//!   [`editor::run_foreground`] after leaving the alternate screen / raw mode, and re-enters
//!   afterwards. GUI editors use [`editor::spawn_detached`].
//! - [`default_manager`]: register Kestrel as the default file manager and undo it.
//! - [`shell`]: run a command string through the user's shell, streaming output as events,
//!   cancellable.

pub mod clipboard;
pub mod default_manager;
pub mod editor;
pub mod open;
pub mod registry;
pub mod shell;

pub use clipboard::{
    format_paths, Clipboard, ClipboardError, MemoryClipboard, PathFormat, SystemClipboard,
};
pub use editor::{
    build_open_command, find_in_path, resolve_editor, run_foreground, spawn_detached,
    split_command_line, CommandSpec, Editor, Env, SystemEnv,
};
pub use open::{
    launch_handler, open_default, open_with_handlers, open_with_handlers_for,
    open_with_handlers_for_all, OpenHandler,
};
pub use shell::{
    resolve_shell, spawn_shell, ShellEvent, ShellHandle, ShellId, ShellProgram, ShellRequest,
    ShellStream,
};
