//! 10a: launch Claude in a Windows Terminal split pane.
//!
//! Produces exactly (plan 10.2 10a):
//!
//! ```text
//! wt -w 0 sp -V -d "<cwd>" pwsh -NoProfile -Command "Remove-Item Env:A,Env:B -ErrorAction SilentlyContinue; claude"
//! ```
//!
//! A WT pane does not inherit Kestrel's environment, so the stripping is part of the pane's own
//! command. WT treats an unescaped `;` anywhere on its command line as a command separator, so
//! every argument has `;` rewritten to `\;` (WT un-escapes it before the child sees it). That
//! includes the `; claude` inside the PowerShell command text. Process-level quoting of spaces and
//! quotes is done by `std::process::Command` (standard Windows argv rules).

use std::path::{Path, PathBuf};

use crate::env::EnvPolicy;

/// Which PowerShell to run in the pane.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Shell {
    /// PowerShell 7+ (`pwsh`).
    Pwsh,
    /// Windows PowerShell 5.1 (`powershell`), the fallback.
    WindowsPowerShell,
}

impl Shell {
    /// Executable name passed to WT.
    pub fn exe(self) -> &'static str {
        match self {
            Shell::Pwsh => "pwsh",
            Shell::WindowsPowerShell => "powershell",
        }
    }

    /// `pwsh` if it is on PATH, otherwise `powershell`.
    pub fn detect() -> Shell {
        if which::which("pwsh").is_ok() {
            Shell::Pwsh
        } else {
            Shell::WindowsPowerShell
        }
    }
}

/// Split direction (`-V` = vertical divider, panes side by side; `-H` = stacked).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SplitDirection {
    /// `-V`
    Vertical,
    /// `-H`
    Horizontal,
}

/// Parameters of the split launch.
#[derive(Debug, Clone)]
pub struct WtSplit {
    /// Directory the pane starts in (`-d`).
    pub cwd: PathBuf,
    /// Extra arguments for `claude` (e.g. `--resume <id>`, `--mcp-config <path>`).
    pub claude_args: Vec<String>,
    /// Variables to remove inside the pane.
    pub env: EnvPolicy,
    /// Shell to use.
    pub shell: Shell,
    /// Split direction.
    pub direction: SplitDirection,
    /// Variables to set inside the pane before `claude` starts (e.g. `KESTREL_IPC`, which the
    /// hook command needs; a WT pane does not inherit the caller's environment).
    pub set_env: Vec<(String, String)>,
}

impl WtSplit {
    /// Defaults: vertical split, detected shell, default env policy, no extra args.
    pub fn new(cwd: impl Into<PathBuf>) -> Self {
        Self {
            cwd: cwd.into(),
            claude_args: Vec::new(),
            env: EnvPolicy::default(),
            shell: Shell::detect(),
            direction: SplitDirection::Vertical,
            set_env: Vec::new(),
        }
    }
}

/// Escape for WT's own command-line parser: `;` becomes `\;`.
pub fn escape_wt_arg(arg: &str) -> String {
    arg.replace(';', "\\;")
}

/// Quote a string as a PowerShell single-quoted literal.
pub fn ps_single_quote(s: &str) -> String {
    format!("'{}'", s.replace('\'', "''"))
}

fn is_ps_bare_word(s: &str) -> bool {
    !s.is_empty()
        && s.chars()
            .all(|c| c.is_ascii_alphanumeric() || matches!(c, '-' | '_' | '.' | ':' | '/' | '\\'))
}

/// The PowerShell `-Command` text run in the pane (before WT escaping).
pub fn powershell_command(env: &EnvPolicy, claude_args: &[String]) -> String {
    powershell_command_with_env(env, &[], claude_args)
}

/// Like [`powershell_command`], additionally setting `set_env` (`$env:NAME='value'; `) after the
/// stripping, so a variable that is also on the strip list ends up set.
pub fn powershell_command_with_env(
    env: &EnvPolicy,
    set_env: &[(String, String)],
    claude_args: &[String],
) -> String {
    let mut out = String::new();
    let names = env.shell_safe_names();
    if !names.is_empty() {
        let list: Vec<String> = names.iter().map(|n| format!("Env:{n}")).collect();
        out.push_str(&format!(
            "Remove-Item {} -ErrorAction SilentlyContinue; ",
            list.join(",")
        ));
    }
    for (name, value) in set_env {
        out.push_str(&format!("$env:{name}={}; ", ps_single_quote(value)));
    }
    out.push_str("claude");
    for a in claude_args {
        out.push(' ');
        if is_ps_bare_word(a) {
            out.push_str(a);
        } else {
            out.push_str(&ps_single_quote(a));
        }
    }
    out
}

fn wt_cwd(cwd: &Path) -> String {
    let s = cwd.to_string_lossy().into_owned();
    // A trailing backslash would escape the closing quote in some parsers; keep roots (`C:\`).
    let trimmed = s.trim_end_matches(['\\', '/']);
    if trimmed.is_empty() || (trimmed.len() == 2 && trimmed.ends_with(':')) {
        s
    } else {
        trimmed.to_string()
    }
}

/// Build the argv (without the leading `wt`) for the split launch. Unit-testable; never runs `wt`.
pub fn build_wt_args(split: &WtSplit) -> Vec<String> {
    let dir = match split.direction {
        SplitDirection::Vertical => "-V",
        SplitDirection::Horizontal => "-H",
    };
    let raw: Vec<String> = vec![
        "-w".into(),
        "0".into(),
        "sp".into(),
        dir.into(),
        "-d".into(),
        wt_cwd(&split.cwd),
        split.shell.exe().into(),
        "-NoProfile".into(),
        "-Command".into(),
        powershell_command_with_env(&split.env, &split.set_env, &split.claude_args),
    ];
    raw.iter().map(|a| escape_wt_arg(a)).collect()
}

/// Build the `wt` command (stripped of credential vars itself as well, harmless but consistent).
pub fn build_wt_command(split: &WtSplit) -> std::process::Command {
    let mut cmd = std::process::Command::new("wt");
    cmd.args(build_wt_args(split));
    split.env.apply_std(&mut cmd);
    cmd
}

/// Fire-and-forget launch. Spawning `wt` returns immediately; the child is not waited on.
pub fn launch_wt_split(split: &WtSplit) -> std::io::Result<()> {
    let mut cmd = build_wt_command(split);
    cmd.stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null());
    let child = cmd.spawn()?;
    // wt.exe exits right away after handing off to the running instance; reap it off-thread.
    std::thread::spawn(move || {
        let mut child = child;
        let _ = child.wait();
    });
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn split(cwd: &str) -> WtSplit {
        let mut s = WtSplit::new(cwd);
        s.shell = Shell::Pwsh;
        s
    }

    #[test]
    fn matches_plan_shape() {
        let a = build_wt_args(&split("C:\\work\\proj"));
        assert_eq!(
            &a[..9],
            [
                "-w",
                "0",
                "sp",
                "-V",
                "-d",
                "C:\\work\\proj",
                "pwsh",
                "-NoProfile",
                "-Command"
            ]
        );
        assert_eq!(
            a[9],
            "Remove-Item Env:ANTHROPIC_API_KEY,Env:ANTHROPIC_AUTH_TOKEN,Env:CLAUDE_CODE_OAUTH_TOKEN,Env:CLAUDE_CODE_USE_BEDROCK,Env:CLAUDE_CODE_USE_VERTEX,Env:CLAUDE_CODE_USE_FOUNDRY -ErrorAction SilentlyContinue\\; claude"
        );
    }

    #[test]
    fn semicolons_in_cwd_are_escaped() {
        let a = build_wt_args(&split("C:\\a;b\\c d"));
        assert_eq!(a[5], "C:\\a\\;b\\c d");
    }

    #[test]
    fn no_unescaped_semicolon_anywhere() {
        let mut s = split("C:\\x;y");
        s.claude_args = vec!["--name".into(), "a;b".into()];
        for arg in build_wt_args(&s) {
            let bytes = arg.as_bytes();
            for (i, &b) in bytes.iter().enumerate() {
                if b == b';' {
                    assert!(i > 0 && bytes[i - 1] == b'\\', "unescaped ; in {arg}");
                }
            }
        }
    }

    #[test]
    fn trailing_backslash_trimmed_but_root_kept() {
        assert_eq!(build_wt_args(&split("C:\\work\\"))[5], "C:\\work");
        assert_eq!(build_wt_args(&split("C:\\"))[5], "C:\\");
    }

    #[test]
    fn quotes_and_spaces_survive_in_argv() {
        let s = split("C:\\My \"Docs\"");
        let a = build_wt_args(&s);
        assert_eq!(a[5], "C:\\My \"Docs\"");
        // Command-level quoting is done by std (CreateProcess rules); just check it is one arg.
        let cmd = build_wt_command(&s);
        assert_eq!(cmd.get_args().count(), a.len());
    }

    #[test]
    fn claude_args_are_powershell_quoted() {
        let mut s = split("C:\\p");
        s.claude_args = vec![
            "--resume".into(),
            "11111111-2222-3333-4444-555555555555".into(),
            "--append-system-prompt".into(),
            "it's fine".into(),
        ];
        let cmd = powershell_command(&s.env, &s.claude_args);
        assert!(cmd.ends_with("claude --resume 11111111-2222-3333-4444-555555555555 --append-system-prompt 'it''s fine'"));
    }

    #[test]
    fn set_env_comes_after_stripping_and_before_claude() {
        let mut s = split("C:\\p");
        s.set_env = vec![("KESTREL_IPC".into(), "kestrel-42".into())];
        let cmd = build_wt_args(&s)[9].clone();
        let strip = cmd.find("Remove-Item").unwrap();
        let set = cmd.find("$env:KESTREL_IPC='kestrel-42'").unwrap();
        assert!(strip < set && set < cmd.find("claude").unwrap());
    }

    #[test]
    fn fallback_shell_and_horizontal() {
        let mut s = split("C:\\p");
        s.shell = Shell::WindowsPowerShell;
        s.direction = SplitDirection::Horizontal;
        let a = build_wt_args(&s);
        assert_eq!(a[3], "-H");
        assert_eq!(a[6], "powershell");
    }

    #[test]
    fn empty_env_policy_omits_remove_item() {
        let mut s = split("C:\\p");
        s.env = EnvPolicy::new(Vec::<String>::new());
        assert_eq!(build_wt_args(&s)[9], "claude");
    }

    #[test]
    fn wt_command_strips_env_too() {
        let cmd = build_wt_command(&split("C:\\p"));
        assert!(cmd
            .get_envs()
            .any(|(k, v)| k == "ANTHROPIC_API_KEY" && v.is_none()));
    }
}
