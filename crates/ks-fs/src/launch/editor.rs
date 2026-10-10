//! Editor resolution and open-at-line command construction. Nothing here goes through a shell:
//! commands are tokenised once and arguments are passed as separate argv entries, so file names
//! with spaces or metacharacters are safe.

use std::io;
use std::path::{Path, PathBuf};
use std::process::{Command, ExitStatus, Stdio};

use crate::ops::paths::display;

/// A program and its arguments.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CommandSpec {
    /// Program (resolved to a full path when found on `PATH`).
    pub program: String,
    /// Arguments, one argv entry each.
    pub args: Vec<String>,
}

impl CommandSpec {
    /// Build a [`Command`] (stdio untouched).
    pub fn to_command(&self) -> Command {
        let mut c = Command::new(&self.program);
        c.args(&self.args);
        c
    }
}

/// Environment access, abstracted for tests.
pub trait Env {
    /// Environment variable.
    fn var(&self, name: &str) -> Option<String>;
    /// Resolve a program on `PATH` (honouring `PATHEXT` on Windows).
    fn which(&self, name: &str) -> Option<PathBuf>;
}

/// The real environment.
#[derive(Debug, Default, Clone, Copy)]
pub struct SystemEnv;

impl Env for SystemEnv {
    fn var(&self, name: &str) -> Option<String> {
        std::env::var(name).ok().filter(|v| !v.trim().is_empty())
    }
    fn which(&self, name: &str) -> Option<PathBuf> {
        find_in_path(name)
    }
}

/// Look `name` up on `PATH`. Names containing a path separator are checked directly.
pub fn find_in_path(name: &str) -> Option<PathBuf> {
    let exts: Vec<String> = if cfg!(windows) {
        std::env::var("PATHEXT")
            .unwrap_or_else(|_| ".COM;.EXE;.BAT;.CMD".into())
            .split(';')
            .filter(|e| !e.is_empty())
            .map(|e| e.to_lowercase())
            .collect()
    } else {
        Vec::new()
    };
    let try_dir = |dir: &Path| -> Option<PathBuf> {
        let base = dir.join(name);
        if base.is_file() && (!cfg!(windows) || Path::new(name).extension().is_some()) {
            return Some(base);
        }
        for e in &exts {
            let mut s = base.clone().into_os_string();
            s.push(e);
            let p = PathBuf::from(s);
            if p.is_file() {
                return Some(p);
            }
        }
        None
    };
    if name.contains('/') || name.contains('\\') {
        return try_dir(Path::new(""));
    }
    let path = std::env::var_os("PATH")?;
    std::env::split_paths(&path).find_map(|d| try_dir(&d))
}

/// Split a command line into arguments: whitespace separates, `"..."` and `'...'` group.
/// Backslashes are literal (Windows paths), so there is no escaping; use the other quote kind to
/// embed a quote character.
pub fn split_command_line(s: &str) -> Vec<String> {
    let mut out = Vec::new();
    let mut cur = String::new();
    let mut has = false;
    let mut quote: Option<char> = None;
    for c in s.chars() {
        match (quote, c) {
            (Some(q), c) if c == q => quote = None,
            (Some(_), c) => cur.push(c),
            (None, '"' | '\'') => {
                quote = Some(c);
                has = true;
            }
            (None, c) if c.is_whitespace() => {
                if has || !cur.is_empty() {
                    out.push(std::mem::take(&mut cur));
                    has = false;
                }
            }
            (None, c) => cur.push(c),
        }
    }
    if has || !cur.is_empty() {
        out.push(cur);
    }
    out
}

/// A resolved editor.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Editor {
    /// Program and the arguments given in `$VISUAL` / `$EDITOR` (e.g. `-w`).
    pub command: CommandSpec,
    /// Runs inside the terminal (vim, nvim, helix, nano, ...): the TUI must be suspended and
    /// [`run_foreground`] used. GUI editors use [`spawn_detached`].
    pub terminal: bool,
}

fn stem_of(program: &str) -> String {
    let name = program
        .rsplit(['/', '\\'])
        .next()
        .unwrap_or(program)
        .to_lowercase();
    for ext in [".exe", ".cmd", ".bat", ".com"] {
        if let Some(s) = name.strip_suffix(ext) {
            return s.to_owned();
        }
    }
    name
}

const TERMINAL_EDITORS: &[&str] = &[
    "vim", "nvim", "vi", "nvi", "hx", "helix", "nano", "micro", "kak", "pico", "ed", "joe",
    "mcedit",
];

/// Whether a program name is a known terminal editor.
pub fn is_terminal_editor(program: &str) -> bool {
    TERMINAL_EDITORS.contains(&stem_of(program).as_str())
}

/// `$VISUAL`, then `$EDITOR`, then `code`, then `notepad`. An entry is used only if its program
/// can be found (absolute path that exists, or on `PATH`).
pub fn resolve_editor(env: &dyn Env) -> Editor {
    let mut candidates: Vec<String> = Vec::new();
    for var in ["VISUAL", "EDITOR"] {
        if let Some(v) = env.var(var) {
            candidates.push(v);
        }
    }
    candidates.push("code".into());
    for cand in candidates {
        // An unquoted path with spaces ("C:\Program Files\X\x.exe") is the whole value.
        let whole = cand.trim();
        let mut tokens = if (whole.contains('/') || whole.contains('\\'))
            && whole.contains(' ')
            && Path::new(whole).is_file()
        {
            vec![whole.to_owned()]
        } else {
            split_command_line(&cand)
        };
        if tokens.is_empty() {
            continue;
        }
        let prog = tokens.remove(0);
        let resolved = if (prog.contains('/') || prog.contains('\\')) && Path::new(&prog).is_file()
        {
            Some(PathBuf::from(&prog))
        } else {
            env.which(&prog)
        };
        if let Some(path) = resolved {
            let program = path.to_string_lossy().into_owned();
            let terminal = is_terminal_editor(&prog)
                || (stem_of(&prog) == "emacs"
                    && tokens
                        .iter()
                        .any(|t| matches!(t.as_str(), "-nw" | "-t" | "--no-window-system")));
            return Editor {
                command: CommandSpec {
                    program,
                    args: tokens,
                },
                terminal,
            };
        }
    }
    Editor {
        command: CommandSpec {
            program: env
                .which("notepad")
                .map_or_else(|| "notepad".into(), |p| p.to_string_lossy().into_owned()),
            args: Vec::new(),
        },
        terminal: false,
    }
}

/// Default open-at-line template for a known editor, with `{file}`, `{line}` and `{col}`.
pub fn default_template(program: &str) -> &'static str {
    match stem_of(program).as_str() {
        "code" | "code-insiders" | "codium" | "cursor" | "windsurf" => "-g {file}:{line}:{col}",
        "nvim" | "vim" | "vi" | "nvi" => "+{line} {file}",
        "hx" | "helix" => "{file}:{line}:{col}",
        "nano" => "+{line},{col} {file}",
        "micro" | "subl" | "sublime_text" => "{file}:{line}:{col}",
        "notepad++" => "-n{line} {file}",
        _ => "{file}",
    }
}

/// Build the command that opens `file` in `editor`, at `line`/`col` when given.
///
/// `template` overrides the per-editor default (config `[editor] template`); it is tokenised
/// with [`split_command_line`] and the placeholders `{file}`, `{line}`, `{col}` (default 1) are
/// substituted inside each token, so a file path with spaces stays one argument. Without a line
/// the file is simply appended. A template without `{file}` gets the file appended.
pub fn build_open_command(
    editor: &Editor,
    file: &Path,
    line: Option<u32>,
    col: Option<u32>,
    template: Option<&str>,
) -> CommandSpec {
    let file_s = display(file);
    let mut args = editor.command.args.clone();
    match line {
        None => args.push(file_s),
        Some(line) => {
            let tpl = template.unwrap_or_else(|| default_template(&editor.command.program));
            let uses_file = tpl.contains("{file}");
            for tok in split_command_line(tpl) {
                args.push(
                    tok.replace("{file}", &file_s)
                        .replace("{line}", &line.to_string())
                        .replace("{col}", &col.unwrap_or(1).to_string()),
                );
            }
            if !uses_file {
                args.push(file_s);
            }
        }
    }
    CommandSpec {
        program: editor.command.program.clone(),
        args,
    }
}

#[cfg(windows)]
fn no_window(cmd: &mut Command) {
    use std::os::windows::process::CommandExt;
    cmd.creation_flags(0x0800_0000); // CREATE_NO_WINDOW
}

#[cfg(not(windows))]
fn no_window(_cmd: &mut Command) {}

/// Start a GUI program without waiting (stdio detached, no console window). A reaper thread
/// collects the exit status so no zombie is left behind.
pub fn spawn_detached(spec: &CommandSpec) -> io::Result<()> {
    let mut cmd = spec.to_command();
    cmd.stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null());
    no_window(&mut cmd);
    let mut child = cmd.spawn()?;
    std::thread::spawn(move || {
        let _ = child.wait();
    });
    Ok(())
}

/// Run a program attached to the terminal and **block** until it exits.
///
/// The app must call this only after leaving the alternate screen and disabling raw mode
/// (terminal editors such as nvim, `!` commands that want a TTY), and restore the TUI and force
/// a full redraw afterwards. Call it from the thread that owns the terminal.
pub fn run_foreground(spec: &CommandSpec) -> io::Result<ExitStatus> {
    spec.to_command()
        .stdin(Stdio::inherit())
        .stdout(Stdio::inherit())
        .stderr(Stdio::inherit())
        .status()
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::HashMap;

    struct FakeEnv {
        vars: HashMap<&'static str, &'static str>,
        found: Vec<&'static str>,
    }
    impl Env for FakeEnv {
        fn var(&self, name: &str) -> Option<String> {
            self.vars.get(name).map(|s| s.to_string())
        }
        fn which(&self, name: &str) -> Option<PathBuf> {
            self.found
                .contains(&name)
                .then(|| PathBuf::from(format!("/bin/{name}")))
        }
    }

    #[test]
    fn splitting() {
        assert_eq!(
            split_command_line(r#"code -g "a b.txt":1"#),
            vec!["code", "-g", "a b.txt:1"]
        );
        assert_eq!(
            split_command_line(r"C:\Program Files\x.exe"),
            vec![r"C:\Program", r"Files\x.exe"]
        );
        assert_eq!(
            split_command_line(r#""C:\Program Files\x.exe" -w"#),
            vec![r"C:\Program Files\x.exe", "-w"]
        );
        assert_eq!(split_command_line("  a   'b c' "), vec!["a", "b c"]);
        assert_eq!(split_command_line(r#"a "" b"#), vec!["a", "", "b"]);
        assert!(split_command_line("   ").is_empty());
    }

    #[test]
    fn resolution_order() {
        let env = FakeEnv {
            vars: HashMap::from([("VISUAL", "nvim"), ("EDITOR", "nano")]),
            found: vec!["nvim", "nano", "code"],
        };
        let e = resolve_editor(&env);
        assert!(e.terminal);
        assert!(e.command.program.ends_with("nvim"));

        let env = FakeEnv {
            vars: HashMap::from([("VISUAL", "ghost"), ("EDITOR", "nano -w")]),
            found: vec!["nano", "code"],
        };
        let e = resolve_editor(&env);
        assert!(e.command.program.ends_with("nano"));
        assert_eq!(e.command.args, vec!["-w"]);

        let env = FakeEnv {
            vars: HashMap::new(),
            found: vec!["code"],
        };
        let e = resolve_editor(&env);
        assert!(e.command.program.ends_with("code"));
        assert!(!e.terminal);

        let env = FakeEnv {
            vars: HashMap::new(),
            found: vec![],
        };
        assert_eq!(resolve_editor(&env).command.program, "notepad");
    }

    #[test]
    fn unquoted_path_with_spaces_and_emacs_nw() {
        let td = tempfile::tempdir().unwrap();
        let dir = td.path().join("Program Files");
        std::fs::create_dir_all(&dir).unwrap();
        let exe = dir.join("my editor.exe");
        std::fs::write(&exe, b"").unwrap();
        let val: &'static str = Box::leak(exe.to_string_lossy().into_owned().into_boxed_str());
        let env = FakeEnv {
            vars: HashMap::from([("EDITOR", val)]),
            found: vec!["code"],
        };
        let e = resolve_editor(&env);
        assert_eq!(PathBuf::from(&e.command.program), exe);
        assert!(e.command.args.is_empty());

        let env = FakeEnv {
            vars: HashMap::from([("EDITOR", "emacs -nw")]),
            found: vec!["emacs"],
        };
        assert!(resolve_editor(&env).terminal);
    }

    fn ed(p: &str) -> Editor {
        Editor {
            command: CommandSpec {
                program: p.into(),
                args: vec![],
            },
            terminal: is_terminal_editor(p),
        }
    }

    #[test]
    fn open_at_line_templates() {
        let f = Path::new("my dir/a b.rs");
        let fs = display(f);
        let c = build_open_command(&ed("code"), f, Some(12), Some(3), None);
        assert_eq!(c.args, vec!["-g".to_string(), format!("{fs}:12:3")]);
        let c = build_open_command(&ed("nvim"), f, Some(12), None, None);
        assert_eq!(c.args, vec!["+12".to_string(), fs.clone()]);
        let c = build_open_command(&ed("hx"), f, Some(7), None, None);
        assert_eq!(c.args, vec![format!("{fs}:7:1")]);
        let c = build_open_command(&ed("nvim"), f, None, None, None);
        assert_eq!(c.args, vec![fs.clone()]);
        let c = build_open_command(
            &ed("zed"),
            f,
            Some(2),
            None,
            Some("--line {line} --col {col}"),
        );
        assert_eq!(c.args, vec!["--line", "2", "--col", "1", &fs]);
        let c = build_open_command(&ed("weird"), f, Some(2), None, Some("-x {file}#{line}"));
        assert_eq!(c.args, vec!["-x".to_string(), format!("{fs}#2")]);
    }

    #[test]
    fn terminal_detection() {
        assert!(is_terminal_editor(r"C:\tools\nvim.exe"));
        assert!(!is_terminal_editor("code.cmd"));
    }

    #[cfg(windows)]
    #[test]
    fn finds_cmd_exe_on_path() {
        let p = find_in_path("cmd").unwrap();
        assert!(p.to_string_lossy().to_lowercase().ends_with("cmd.exe"));
        assert!(find_in_path("definitely-not-a-program-xyz").is_none());
    }
}
