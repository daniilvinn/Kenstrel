#![allow(clippy::print_stderr)]

//! `ks --print-shell-init` and the shell function it prints, end to end.
//!
//! The TUI cannot run in a test, so the real `ks` is replaced by a stand-in script that does
//! what `ks --cwd-file <file>` does after `Q`: it writes the final directory to `<file>`
//! (UTF-8, no BOM, no trailing newline). The function itself is the exact text the real
//! binary prints.

use std::process::{Command, Stdio};

fn ks() -> Command {
    let mut cmd = Command::new(env!("CARGO_BIN_EXE_ks"));
    cmd.stdin(Stdio::null());
    cmd
}

fn printed(shell: &str) -> String {
    let output = ks().args(["--print-shell-init", shell]).output().unwrap();
    assert!(output.status.success(), "{shell}");
    String::from_utf8(output.stdout).unwrap()
}

#[test]
fn the_binary_prints_a_function_per_shell() {
    let ps = printed("powershell");
    assert!(ps.contains("function k {"), "{ps}");
    assert!(ps.contains("-Encoding UTF8"), "{ps}");
    assert!(ps.contains("Set-Location"), "{ps}");
    assert_eq!(printed("pwsh"), ps, "pwsh is an alias");

    let bash = printed("bash");
    assert!(bash.contains("k() {"), "{bash}");
    assert!(bash.contains("cd -- \"$dir\""), "{bash}");
    assert_eq!(printed("zsh"), bash);
}

#[test]
fn unknown_shells_are_rejected() {
    let output = ks().args(["--print-shell-init", "cmd"]).output().unwrap();
    assert!(!output.status.success());
}

#[cfg(windows)]
mod powershell {
    use std::fs;
    use std::path::Path;

    use super::*;

    const FAKE_KS: &str = r#"param([string]$flag, [string]$file)
if ($flag -ne '--cwd-file') { throw "unexpected argument $flag" }
if ($env:KS_TEST_TARGET) {
    [IO.File]::WriteAllBytes($file, [Text.UTF8Encoding]::new($false).GetBytes($env:KS_TEST_TARGET))
}
"#;

    /// Runs `k` in the PowerShell `exe` starting in `start`, after the fake `ks` wrote
    /// `target` (empty: wrote nothing, like a plain `q`). Returns where the shell ended up.
    fn run_k(exe: &str, start: &Path, target: &str) -> Option<String> {
        let dir = tempfile::tempdir().unwrap();
        fs::write(dir.path().join("ks.ps1"), FAKE_KS).unwrap();
        fs::write(dir.path().join("init.ps1"), printed("powershell")).unwrap();
        let out = dir.path().join("where.txt");
        let path = format!(
            "{};{}",
            dir.path().display(),
            std::env::var("PATH").unwrap_or_default()
        );
        let script = format!(
            ". '{}'; k; [IO.File]::WriteAllText($env:KS_OUT, (Get-Location).ProviderPath, [Text.UTF8Encoding]::new($false))",
            dir.path().join("init.ps1").display()
        );
        let status = Command::new(exe)
            .args([
                "-NoProfile",
                "-NonInteractive",
                "-ExecutionPolicy",
                "Bypass",
            ])
            .args(["-Command", &script])
            .current_dir(start)
            .env("PATH", path)
            .env("KS_TEST_TARGET", target)
            .env("KS_OUT", &out)
            .stdin(Stdio::null())
            .status()
            .ok()?;
        assert!(status.success(), "{exe} failed");
        Some(fs::read_to_string(out).unwrap())
    }

    fn available(exe: &str) -> bool {
        Command::new(exe)
            .args(["-NoProfile", "-Command", "exit 0"])
            .stdin(Stdio::null())
            .status()
            .is_ok_and(|s| s.success())
    }

    fn check(exe: &str) {
        if !available(exe) {
            eprintln!("{exe} not available, skipping");
            return;
        }
        let root = tempfile::tempdir().unwrap();
        let start = root.path().join("start");
        // A space and non-ASCII characters: the cwd file must be read as UTF-8, not ANSI.
        let target = root.path().join("Zażółć gęślą jaźń é");
        fs::create_dir(&start).unwrap();
        fs::create_dir(&target).unwrap();
        let canon = |p: &Path| dunce::canonicalize(p).unwrap().display().to_string();

        // `Q`: the shell changes to the directory in the file.
        let ended = run_k(exe, &start, target.to_str().unwrap()).unwrap();
        assert_eq!(ended.to_lowercase(), canon(&target).to_lowercase(), "{exe}");

        // Plain `q` writes nothing: the shell stays where it was.
        let ended = run_k(exe, &start, "").unwrap();
        assert_eq!(ended.to_lowercase(), canon(&start).to_lowercase(), "{exe}");

        // A directory that vanished meanwhile is not a reason to fail.
        let gone = root.path().join("gone");
        let ended = run_k(exe, &start, gone.to_str().unwrap()).unwrap();
        assert_eq!(ended.to_lowercase(), canon(&start).to_lowercase(), "{exe}");
    }

    #[test]
    fn windows_powershell_5_changes_directory() {
        check("powershell.exe");
    }

    #[test]
    fn powershell_7_changes_directory_when_installed() {
        check("pwsh.exe");
    }

    #[test]
    fn the_function_leaves_no_temporary_file_behind() {
        if !available("powershell.exe") {
            return;
        }
        let temp = tempfile::tempdir().unwrap();
        let start = tempfile::tempdir().unwrap();
        let dir = tempfile::tempdir().unwrap();
        fs::write(dir.path().join("ks.ps1"), FAKE_KS).unwrap();
        fs::write(dir.path().join("init.ps1"), printed("powershell")).unwrap();
        let path = format!(
            "{};{}",
            dir.path().display(),
            std::env::var("PATH").unwrap_or_default()
        );
        let script = format!(". '{}'; k", dir.path().join("init.ps1").display());
        let status = Command::new("powershell.exe")
            .args([
                "-NoProfile",
                "-NonInteractive",
                "-ExecutionPolicy",
                "Bypass",
            ])
            .args(["-Command", &script])
            .current_dir(start.path())
            .env("PATH", path)
            .env("TEMP", temp.path())
            .env("TMP", temp.path())
            .env("KS_TEST_TARGET", start.path())
            .stdin(Stdio::null())
            .status()
            .unwrap();
        assert!(status.success());
        assert_eq!(fs::read_dir(temp.path()).unwrap().count(), 0);
    }
}
