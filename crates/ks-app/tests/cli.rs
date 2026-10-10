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

#[test]
fn hook_subcommand_always_exits_zero_and_prints_nothing() {
    use std::io::Write;
    for input in [
        r#"{"session_id":"s","tool_name":"Edit","tool_input":{"file_path":"a.rs"}}"#,
        r#"{"session_id":"s","tool_name":"MultiEdit","tool_input":{"file_path":"b.rs","edits":[]}}"#,
        "not json at all",
        "{",
        "",
    ] {
        let mut child = ks()
            .arg("hook")
            .env("KESTREL_IPC", "kestrel-nobody-listens-here")
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()
            .unwrap();
        child
            .stdin
            .take()
            .unwrap()
            .write_all(input.as_bytes())
            .unwrap();
        let output = child.wait_with_output().unwrap();
        assert_eq!(output.status.code(), Some(0), "input {input:?}");
        assert!(output.stdout.is_empty(), "input {input:?}");
    }
}

#[test]
fn hook_subcommand_without_a_socket_name_still_exits_zero() {
    let output = ks()
        .arg("hook")
        .env_remove("KESTREL_IPC")
        .stdin(Stdio::null())
        .output()
        .unwrap();
    assert_eq!(output.status.code(), Some(0));
    assert!(output.stdout.is_empty());
}

#[test]
fn hook_subcommand_reaches_a_listening_kestrel() {
    use std::io::Write;
    use std::sync::{Arc, Mutex};

    let name = format!("kestrel-cli-test-{}", std::process::id());
    let seen = Arc::new(Mutex::new(Vec::new()));
    let sink_seen = Arc::clone(&seen);
    let sink: ks_claude::events::EventSink = Arc::new(move |event| {
        if let ks_claude::events::ClaudeEvent::Ipc { request, reply, .. } = event {
            sink_seen.lock().unwrap().push(request);
            reply.send(ks_claude::ipc::Response::Ok);
        }
    });
    let _server = ks_claude::ipc::IpcServer::start(
        &name,
        0,
        sink,
        tokio_util::sync::CancellationToken::new(),
        ks_claude::ipc::IpcServerOptions::default(),
    )
    .unwrap();

    let mut child = ks()
        .arg("hook")
        .env("KESTREL_IPC", &name)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .spawn()
        .unwrap();
    child
        .stdin
        .take()
        .unwrap()
        .write_all(
            br#"{"session_id":"abc","tool_name":"Write","tool_input":{"file_path":"src/new.rs"}}"#,
        )
        .unwrap();
    let output = child.wait_with_output().unwrap();
    assert_eq!(output.status.code(), Some(0));
    assert_eq!(
        seen.lock().unwrap().as_slice(),
        [ks_claude::ipc::Request::HookEvent {
            session_id: "abc".into(),
            tool_name: "Write".into(),
            file_path: Some("src/new.rs".into()),
        }]
    );
}

/// Sends one JSON-RPC line and reads the reply line.
fn rpc(
    stdin: &mut std::process::ChildStdin,
    stdout: &mut std::io::BufReader<std::process::ChildStdout>,
    request: &str,
) -> serde_json::Value {
    use std::io::{BufRead, Write};
    writeln!(stdin, "{request}").unwrap();
    let mut line = String::new();
    stdout.read_line(&mut line).unwrap();
    serde_json::from_str(&line).unwrap_or_else(|e| panic!("not JSON ({e}): {line:?}"))
}

#[test]
fn mcp_subcommand_answers_initialize_and_lists_the_tools_over_stdio() {
    use std::io::{BufReader, Write};
    let mut child = ks()
        .arg("mcp")
        .env_remove("KESTREL_IPC")
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .spawn()
        .unwrap();
    let mut stdin = child.stdin.take().unwrap();
    let mut stdout = BufReader::new(child.stdout.take().unwrap());

    let init = rpc(
        &mut stdin,
        &mut stdout,
        r#"{"jsonrpc":"2.0","id":1,"method":"initialize","params":{"protocolVersion":"2025-03-26","capabilities":{},"clientInfo":{"name":"test","version":"0"}}}"#,
    );
    assert_eq!(init["id"], 1, "{init}");
    assert!(init["result"]["serverInfo"].is_object(), "{init}");
    assert!(
        init["result"]["capabilities"]["tools"].is_object(),
        "{init}"
    );

    writeln!(
        stdin,
        r#"{{"jsonrpc":"2.0","method":"notifications/initialized"}}"#
    )
    .unwrap();
    let tools = rpc(
        &mut stdin,
        &mut stdout,
        r#"{"jsonrpc":"2.0","id":2,"method":"tools/list"}"#,
    );
    let mut names: Vec<String> = tools["result"]["tools"]
        .as_array()
        .unwrap_or_else(|| panic!("{tools}"))
        .iter()
        .map(|t| t["name"].as_str().unwrap().to_owned())
        .collect();
    names.sort();
    assert_eq!(
        names,
        [
            "get_selection",
            "list_dir",
            "reveal",
            "search_contents",
            "search_files"
        ]
    );

    // Without KESTREL_IPC a tool call fails cleanly (an error result, not a crash).
    let call = rpc(
        &mut stdin,
        &mut stdout,
        r#"{"jsonrpc":"2.0","id":3,"method":"tools/call","params":{"name":"get_selection","arguments":{}}}"#,
    );
    assert_eq!(call["id"], 3, "{call}");
    assert_eq!(call["result"]["isError"], true, "{call}");

    drop(stdin); // EOF ends the server
    let status = child.wait().unwrap();
    assert!(status.code().is_some());
}
