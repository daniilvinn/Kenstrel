//! Open with the default app and the "open with..." handler list.
//!
//! The handler list is deliberately simple: well-known programs that exist on this machine, an
//! "Explorer: reveal" entry and the system "Open with" chooser. Reading the registry
//! `OpenWithList` / `OpenWithProgids` of the extension is not done (it needs registry access or
//! `reg.exe` parsing and App Paths resolution); see the wiring notes.

use std::io;
use std::path::Path;
use std::process::{Command, Stdio};

use super::editor::{find_in_path, CommandSpec};
use crate::ops::paths::display;

/// Open `path` with the associated default application. Returns once the launcher is started
/// (the `open` crate's detached mode); never waits for the application.
pub fn open_default(path: &Path) -> io::Result<()> {
    open::that_detached(path)
}

/// An entry of the "open with" menu. `args` may contain `{file}`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct OpenHandler {
    /// Menu label.
    pub name: String,
    /// Program and argument templates.
    pub command: CommandSpec,
}

/// Handlers available on this machine for any file.
pub fn open_with_handlers() -> Vec<OpenHandler> {
    let mut out = Vec::new();
    let mut add = |name: &str, program: &str, args: &[&str]| {
        out.push(OpenHandler {
            name: name.to_owned(),
            command: CommandSpec {
                program: program.to_owned(),
                args: args.iter().map(|s| (*s).to_owned()).collect(),
            },
        });
    };
    if cfg!(windows) {
        add("Notepad", "notepad.exe", &["{file}"]);
        if let Some(code) = find_in_path("code") {
            add("VS Code", &code.to_string_lossy(), &["{file}"]);
        }
        add("Explorer (reveal)", "explorer.exe", &["/select,{file}"]);
        add(
            "Choose another app...",
            "rundll32.exe",
            &["shell32.dll,OpenAs_RunDLL", "{file}"],
        );
    } else if let Some(code) = find_in_path("code") {
        add("VS Code", &code.to_string_lossy(), &["{file}"]);
    }
    out
}

/// Launch `handler` for `path` without waiting.
pub fn launch_handler(handler: &OpenHandler, path: &Path) -> io::Result<()> {
    let file = display(path);
    let mut cmd = Command::new(&handler.command.program);
    let is_select = handler
        .command
        .args
        .first()
        .is_some_and(|a| a.starts_with("/select,"));
    #[cfg(windows)]
    {
        use std::os::windows::process::CommandExt;
        if is_select {
            // explorer needs `/select,"path"` with the quotes around the path only.
            cmd.raw_arg(format!("/select,\"{file}\""));
        } else {
            cmd.args(
                handler
                    .command
                    .args
                    .iter()
                    .map(|a| a.replace("{file}", &file)),
            );
        }
        cmd.creation_flags(0x0800_0000);
    }
    #[cfg(not(windows))]
    {
        let _ = is_select;
        cmd.args(
            handler
                .command
                .args
                .iter()
                .map(|a| a.replace("{file}", &file)),
        );
    }
    cmd.stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null());
    let mut child = cmd.spawn()?;
    std::thread::spawn(move || {
        let _ = child.wait();
    });
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn handler_list_is_nonempty_on_windows() {
        let h = open_with_handlers();
        if cfg!(windows) {
            assert!(h.iter().any(|h| h.name.starts_with("Explorer")));
            assert!(h
                .iter()
                .all(|h| h.command.args.iter().any(|a| a.contains("{file}"))));
        }
    }
}
