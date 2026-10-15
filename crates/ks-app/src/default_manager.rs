//! `ks --set-default` / `ks --unset-default`: make Kestrel the default file manager, or give
//! the job back to File Explorer. The registry work lives in
//! [`ks_fs::launch::default_manager`]; this module decides what the registered commands run.
//!
//! Every way in ends in the same PowerShell session ([`session_script`]): the user's profile
//! loads, `ks` is aliased to this `ks.exe`, the `k` function of `--print-shell-init` is
//! defined and `k` runs in the folder. Quitting with `Q` leaves a PowerShell prompt in the
//! directory Kestrel ended in.
//!
//! - With Windows Terminal: `wt -w new -p <PowerShell profile> -d "<folder>\." powershell
//!   -NoExit -EncodedCommand ...`. The profile gives the window the user's font, colours and
//!   icon. Terminal has no console of its own, so nothing flashes first.
//! - Without it: `ks --via-powershell "<folder>"` ([`launch_powershell`]), which runs the same
//!   session in the console window Windows opens for `ks.exe`.

use std::path::{Path, PathBuf};
use std::process::{Command, ExitCode};

use anyhow::{Context, Result};
use base64::Engine as _;
use ks_fs::launch::default_manager::{set_default, unset_default, Outcome, Target};
use ks_fs::launch::find_in_path;

/// Windows Terminal's profile ids for the PowerShell it generates profiles for; they are the
/// same on every machine.
const WINDOWS_POWERSHELL_PROFILE: &str = "{61c54bbd-c2c6-5271-96e7-009a87ff44bf}";
const POWERSHELL_7_PROFILE: &str = "{574e775e-4f2a-5b96-ac1e-a2962a402336}";

/// PowerShell 7 when installed, Windows PowerShell otherwise.
fn find_powershell() -> PathBuf {
    find_in_path("pwsh.exe")
        .or_else(|| find_in_path("powershell.exe"))
        .unwrap_or_else(|| PathBuf::from("powershell.exe"))
}

fn is_pwsh(shell: &Path) -> bool {
    shell
        .file_stem()
        .is_some_and(|s| s.eq_ignore_ascii_case("pwsh"))
}

/// The PowerShell session: `ks` is this `exe`, `k` is defined, and `k` runs in the process's
/// working directory. That is the folder being opened even when the profile changes the
/// location, because PowerShell never moves the process directory.
pub fn session_script(exe: &Path) -> String {
    format!(
        "Set-Alias ks {}\n\
         ks --print-shell-init pwsh | Out-String | Invoke-Expression\n\
         Set-Location -LiteralPath ([Environment]::CurrentDirectory)\n\
         k\n",
        quote(&exe.display().to_string())
    )
}

/// Arguments that start PowerShell with the session.
fn session_args(exe: &Path) -> [String; 4] {
    [
        "-NoLogo".into(),
        "-NoExit".into(),
        "-EncodedCommand".into(),
        encode_command(&session_script(exe)),
    ]
}

/// `ks --via-powershell [path]`: run the session in `path` (the current directory without
/// one, the home directory when it cannot be opened) and wait for it to end.
pub fn launch_powershell(path: Option<&Path>) -> Result<ExitCode> {
    let exe = current_exe()?;
    let home = home();
    let cwd = std::env::current_dir().unwrap_or_else(|_| home.clone());
    let dir = crate::startup::resolve_start_dir(path, &cwd).unwrap_or(home);
    let shell = find_powershell();
    let status = Command::new(&shell)
        .args(session_args(&exe))
        .current_dir(&dir)
        .status()
        .with_context(|| format!("cannot start {}", shell.display()))?;
    Ok(match status.code() {
        Some(0) => ExitCode::SUCCESS,
        _ => ExitCode::FAILURE,
    })
}

/// The registered commands. `terminal` is `wt.exe`, `shell` the PowerShell to run.
pub fn target(exe: &Path, home: &Path, terminal: Option<&Path>, shell: &Path) -> Target {
    let command = |dir: &str| match terminal {
        Some(wt) => {
            let profile = if is_pwsh(shell) {
                POWERSHELL_7_PROFILE
            } else {
                WINDOWS_POWERSHELL_PROFILE
            };
            format!(
                "\"{}\" -w new -p \"{profile}\" -d \"{dir}\" \"{}\" {}",
                wt.display(),
                shell.display(),
                session_args(exe).join(" ")
            )
        }
        None => format!("\"{}\" --via-powershell \"{dir}\"", exe.display()),
    };
    Target {
        icon: exe.to_owned(),
        // `"%1"` of a drive root is `"C:\"`, whose `\"` reads as an escaped quote; `\.` keeps
        // the quote intact and names the same directory. `ks` repairs it on its own.
        folder_command: command(if terminal.is_some() { r"%1\." } else { "%1" }),
        home_command: command(&home.display().to_string()),
    }
}

/// A PowerShell single-quoted string. PowerShell also treats the typographic single quotes
/// as quote characters, so those are doubled too.
fn quote(text: &str) -> String {
    let mut out = String::from("'");
    for ch in text.chars() {
        if matches!(ch, '\'' | '\u{2018}' | '\u{2019}' | '\u{201A}' | '\u{201B}') {
            out.push(ch);
        }
        out.push(ch);
    }
    out.push('\'');
    out
}

/// `-EncodedCommand` takes base64 UTF-16LE, which sidesteps command-line quoting entirely.
fn encode_command(script: &str) -> String {
    let bytes: Vec<u8> = script.encode_utf16().flat_map(u16::to_le_bytes).collect();
    base64::engine::general_purpose::STANDARD.encode(bytes)
}

fn current_exe() -> Result<PathBuf> {
    let exe = std::env::current_exe().context("cannot locate ks.exe")?;
    Ok(dunce::simplified(&exe).to_owned())
}

fn home() -> PathBuf {
    crate::paths::home_dir().unwrap_or_else(|| PathBuf::from(r"C:\"))
}

/// `wt.exe` on `PATH`. It is usually an App Execution Alias: a zero-byte reparse point that
/// `is_file` does not accept, so this checks for the entry without following it.
fn find_windows_terminal() -> Option<PathBuf> {
    let path = std::env::var_os("PATH")?;
    std::env::split_paths(&path)
        .map(|dir| dir.join("wt.exe"))
        .find(|wt| std::fs::symlink_metadata(wt).is_ok())
}

/// Registers (`set`) or unregisters this `ks.exe`; returns the message for the user.
pub fn run(set: bool) -> Result<String> {
    let exe = current_exe()?;
    let terminal = find_windows_terminal();
    let shell = find_powershell();
    let target = target(&exe, &home(), terminal.as_deref(), &shell);
    if set {
        let outcome =
            set_default(&target).context("cannot register Kestrel as the default file manager")?;
        let host = match terminal {
            Some(_) => "a new Windows Terminal window with your PowerShell profile",
            None => "a new console window (install Windows Terminal for a nicer one)",
        };
        Ok(match outcome {
            Outcome::Changed => format!(
                "Kestrel is now the default file manager: opening a folder or drive, and Win+E, \
                 start {} in {}, in {host}.\nExplorer stays available as \"Open\" in the context \
                 menu. If you move ks.exe, or install or remove Windows Terminal or PowerShell 7, \
                 run `ks --set-default` again. Undo with `ks --unset-default`.",
                exe.display(),
                shell.display()
            ),
            Outcome::Unchanged => "Kestrel already is the default file manager.".to_owned(),
        })
    } else {
        let outcome = unset_default(&target)
            .context("cannot restore File Explorer as the default file manager")?;
        Ok(match outcome {
            Outcome::Changed => "File Explorer is the default file manager again.".to_owned(),
            Outcome::Unchanged => {
                "Kestrel was not the default file manager; nothing to undo.".to_owned()
            }
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const EXE: &str = r"C:\Tools\ks.exe";
    const WT: &str = r"C:\Apps\wt.exe";
    const WINPS: &str = r"C:\Windows\System32\WindowsPowerShell\v1.0\powershell.exe";

    #[test]
    fn quotes_double_every_single_quote_kind() {
        assert_eq!(quote(r"C:\Bob's ‘x’"), r"'C:\Bob''s ‘‘x’’'");
        assert_eq!(quote("$env:x `n"), "'$env:x `n'");
    }

    #[test]
    fn session_aliases_this_exe_and_runs_k_in_the_process_directory() {
        let text = session_script(Path::new(r"C:\It's\ks.exe"));
        assert_eq!(
            text,
            "Set-Alias ks 'C:\\It''s\\ks.exe'\n\
             ks --print-shell-init pwsh | Out-String | Invoke-Expression\n\
             Set-Location -LiteralPath ([Environment]::CurrentDirectory)\n\
             k\n"
        );
    }

    #[test]
    fn encoded_command_is_utf16le_base64() {
        // [Convert]::ToBase64String([Text.Encoding]::Unicode.GetBytes('k'))
        assert_eq!(encode_command("k"), "awA=");
    }

    #[test]
    fn terminal_commands_use_the_matching_powershell_profile() {
        let encoded = encode_command(&session_script(Path::new(EXE)));
        let t = target(
            Path::new(EXE),
            Path::new(r"C:\Users\me"),
            Some(Path::new(WT)),
            Path::new(WINPS),
        );
        assert_eq!(
            t.folder_command,
            format!(
                r#""{WT}" -w new -p "{WINDOWS_POWERSHELL_PROFILE}" -d "%1\." "{WINPS}" -NoLogo -NoExit -EncodedCommand {encoded}"#
            )
        );
        assert!(
            t.home_command.contains(r#" -d "C:\Users\me" "#),
            "{}",
            t.home_command
        );
        // No `;`: Windows Terminal would split the command there.
        assert!(!t.folder_command.contains(';'));

        let pwsh = r"C:\Program Files\PowerShell\7\pwsh.exe";
        let t = target(
            Path::new(EXE),
            Path::new(r"C:\Users\me"),
            Some(Path::new(WT)),
            Path::new(pwsh),
        );
        assert!(t.folder_command.contains(POWERSHELL_7_PROFILE));
        assert!(t.folder_command.contains(&format!("\"{pwsh}\"")));
    }

    #[test]
    fn without_terminal_ks_launches_the_session_itself() {
        let t = target(
            Path::new(EXE),
            Path::new(r"C:\Users\me"),
            None,
            Path::new(WINPS),
        );
        assert_eq!(
            t.folder_command,
            format!(r#""{EXE}" --via-powershell "%1""#)
        );
        assert_eq!(
            t.home_command,
            format!(r#""{EXE}" --via-powershell "C:\Users\me""#)
        );
    }
}
