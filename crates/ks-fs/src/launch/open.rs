//! Open with the default app and the "open with..." handler list.
//!
//! The handler list starts with the programs the registry lists for the file's extension
//! (see [`super::registry`]), followed by the fixed fallbacks: well-known programs that exist
//! on this machine, an "Explorer: reveal" entry and the system "Open with" chooser.

use std::io;
use std::path::Path;
use std::process::{Command, Stdio};

use super::editor::{find_in_path, CommandSpec};
use super::registry::{dotted_extension, push_unique, registry_handlers};
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

/// Handlers for `path`: the registry's list for its extension, then the fallbacks of
/// [`open_with_handlers`]. Reads the registry; call it off the UI thread.
pub fn open_with_handlers_for(path: Option<&Path>) -> Vec<OpenHandler> {
    let mut out = path
        .and_then(dotted_extension)
        .map(|ext| registry_handlers(&ext))
        .unwrap_or_default();
    for handler in open_with_handlers() {
        push_unique(&mut out, handler);
    }
    out
}

/// Handlers that apply to every path of a selection: the handlers of the first path that
/// every other extension offers too (matched by label). Registry reads happen once per
/// distinct extension; call it off the UI thread.
pub fn open_with_handlers_for_all(paths: &[std::path::PathBuf]) -> Vec<OpenHandler> {
    let mut seen: Vec<(Option<String>, Vec<OpenHandler>)> = Vec::new();
    for path in paths {
        let ext = dotted_extension(path);
        if !seen.iter().any(|(e, _)| *e == ext) {
            let list = open_with_handlers_for(Some(path));
            seen.push((ext, list));
        }
    }
    common_handlers(seen.into_iter().map(|(_, list)| list).collect())
}

/// The handlers of the first list that all other lists contain (by label), in its order.
pub fn common_handlers(mut lists: Vec<Vec<OpenHandler>>) -> Vec<OpenHandler> {
    if lists.is_empty() {
        return Vec::new();
    }
    let mut first = lists.remove(0);
    first.retain(|h| lists.iter().all(|l| l.iter().any(|o| o.name == h.name)));
    first
}

/// The fixed handlers available on this machine for any file.
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

    fn handler(name: &str) -> OpenHandler {
        OpenHandler {
            name: name.into(),
            command: CommandSpec {
                program: format!("{name}.exe"),
                args: vec!["{file}".into()],
            },
        }
    }

    #[test]
    fn mixed_selections_only_offer_common_handlers() {
        let txt = vec![handler("Notepad"), handler("Code"), handler("Reveal")];
        let png = vec![handler("Photos"), handler("Reveal"), handler("Code")];
        let names = |l: Vec<OpenHandler>| l.into_iter().map(|h| h.name).collect::<Vec<_>>();
        assert_eq!(
            names(common_handlers(vec![txt.clone(), png])),
            ["Code", "Reveal"]
        );
        assert_eq!(names(common_handlers(vec![txt.clone()])), names(txt));
        assert!(common_handlers(Vec::new()).is_empty());
    }

    #[test]
    fn fallback_handlers_remain_when_the_registry_knows_nothing() {
        // an extension nobody registers
        let list = open_with_handlers_for(Some(Path::new("x.kestrel-no-such-ext")));
        assert_eq!(
            list.iter().map(|h| &h.name).collect::<Vec<_>>(),
            open_with_handlers()
                .iter()
                .map(|h| &h.name)
                .collect::<Vec<_>>()
        );
    }

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
