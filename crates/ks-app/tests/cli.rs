//! Black-box tests of the `ks` binary that never touch a real terminal.

use std::process::{Command, Stdio};

fn ks() -> Command {
    let mut cmd = Command::new(env!("CARGO_BIN_EXE_ks"));
    // Never inherit a console: these tests must not start the TUI.
    cmd.stdin(Stdio::null());
    cmd
}

#[test]
fn version_flag_prints_the_version() {
    let output = ks().arg("--version").output().unwrap();
    assert!(output.status.success());
    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(stdout.starts_with("ks "), "{stdout}");
    assert!(stdout.contains(env!("CARGO_PKG_VERSION")), "{stdout}");
}

#[test]
fn help_lists_flags_but_not_the_hidden_debug_flag() {
    let output = ks().arg("--help").output().unwrap();
    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(stdout.contains("--cwd-file"), "{stdout}");
    assert!(!stdout.contains("debug-panic"), "{stdout}");
}

#[test]
fn refuses_to_start_without_a_terminal() {
    let output = ks()
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .output()
        .unwrap();
    assert!(!output.status.success());
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(stderr.contains("must be run in a terminal"), "{stderr}");
}

#[test]
fn unknown_flags_are_rejected() {
    let output = ks().arg("--no-such-flag").output().unwrap();
    assert!(!output.status.success());
}

#[test]
fn dump_keymap_prints_the_committed_documentation() {
    let output = ks().arg("--dump-keymap").output().unwrap();
    assert!(output.status.success());
    let printed = String::from_utf8(output.stdout).unwrap();
    let committed =
        std::fs::read_to_string(concat!(env!("CARGO_MANIFEST_DIR"), "/../../docs/keymap.md"))
            .unwrap()
            .replace("\r\n", "\n");
    assert_eq!(printed, committed, "docs/keymap.md is out of date");
}
