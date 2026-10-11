//! End-to-end tests of the Claude integration through the real effect runner, a real local
//! socket and a real ConPTY, without the real `claude` (a fake `claude.cmd` stands in) and
//! without ever sending a prompt. The one test that needs the real binary is `#[ignore]`.

// The fake `claude.cmd` needs Windows; elsewhere only the helpers compile.
#![cfg_attr(not(windows), allow(dead_code, unused_imports))]

use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

use ks_app::claude::ClaudeOptions;
use ks_app::effects::EffectRunner;
use ks_core::claude::ClaudeScreen;
use ks_core::{
    update, AppState, Config, Entry, KeyContext, Keymap, Msg, Places, Size, StartParams,
};
use tokio::sync::mpsc;

fn state(cwd: &Path, binary: Option<&Path>, size: Size) -> AppState {
    let mut config = Config::default();
    if let Some(binary) = binary {
        config.claude.binary = binary.to_string_lossy().into_owned();
    }
    AppState::new(StartParams {
        cwd: cwd.to_path_buf(),
        home: None,
        places: Places::default(),
        config,
        keymap: Keymap::defaults(),
        size,
        now: std::time::SystemTime::UNIX_EPOCH,
        utc_offset_secs: 0,
    })
}

struct World {
    state: AppState,
    runner: EffectRunner,
    rx: mpsc::Receiver<Msg>,
}

impl World {
    fn new(state: AppState, options: ClaudeOptions) -> Self {
        let (tx, rx) = mpsc::channel(256);
        Self {
            state,
            runner: EffectRunner::new(tx).with_claude(options),
            rx,
        }
    }

    fn apply(&mut self, msg: Msg) {
        let effects = update(&mut self.state, msg);
        self.runner.run(effects);
    }

    fn key(&mut self, text: &str) {
        self.apply(Msg::Key(text.parse().unwrap()));
    }

    fn type_text(&mut self, text: &str) {
        for c in text.chars() {
            self.apply(Msg::Key(ks_core::KeyChord::char(c)));
        }
    }

    /// Runs the message loop until `done` holds.
    async fn pump_until(&mut self, what: &str, done: impl Fn(&AppState) -> bool) {
        let deadline = Instant::now() + Duration::from_secs(180);
        loop {
            if done(&self.state) {
                return;
            }
            let left = deadline.saturating_duration_since(Instant::now());
            assert!(
                !left.is_zero(),
                "timed out waiting for {what}\nstatus: {:?}\nscreen:\n{}",
                self.state.status,
                self.screen()
            );
            if let Ok(Some(msg)) =
                tokio::time::timeout(left.min(Duration::from_millis(100)), self.rx.recv()).await
            {
                self.apply(msg);
            }
        }
    }

    fn screen(&self) -> String {
        self.state
            .claude
            .screen
            .as_ref()
            .map_or_else(String::new, screen_text)
    }

    async fn pump_until_screen_has(&mut self, needle: &str) {
        let needle = needle.to_owned();
        self.pump_until(&format!("`{needle}` on the screen"), |s| {
            s.claude
                .screen
                .as_ref()
                .is_some_and(|screen| screen_text(screen).contains(&needle))
        })
        .await;
    }
}

fn screen_text(screen: &ClaudeScreen) -> String {
    (0..screen.rows)
        .map(|row| {
            (0..screen.cols)
                .map(|col| {
                    screen
                        .cell(row, col)
                        .map_or(" ", |c| if c.text.is_empty() { " " } else { &c.text })
                })
                .collect::<String>()
                .trim_end()
                .to_owned()
        })
        .collect::<Vec<_>>()
        .join("\n")
}

fn options(dir: &Path, name: &str) -> ClaudeOptions {
    ClaudeOptions {
        sessions_file: Some(dir.join("sessions.json")),
        launch_dir: Some(dir.join("launch")),
        ks_exe: Some(PathBuf::from(env!("CARGO_BIN_EXE_ks"))),
        ipc_name: Some(format!("kestrel-test-{}-{name}", std::process::id())),
        projects_dir: Some(dir.join("projects")),
    }
}

/// A stand-in for `claude`: answers `auth status`, prints its arguments and environment, then
/// echoes lines until it reads `exit`.
#[cfg(windows)]
fn fake_claude(dir: &Path) -> PathBuf {
    let script = r#"@echo off
if "%~1"=="auth" (
  echo {"loggedIn":true,"authMethod":"claude.ai","apiProvider":"firstParty"}
  exit /b 0
)
echo FAKE-CLAUDE-READY
for %%a in (%*) do echo ARG %%a
echo IPC=%KESTREL_IPC%
echo KEY=[%ANTHROPIC_API_KEY%]
:loop
set LINE=
set /p LINE=prompt^>
if /i "%LINE%"=="exit" exit /b 7
echo you said: %LINE%
goto loop
"#;
    let path = dir.join("claude.cmd");
    std::fs::write(&path, script.replace('\n', "\r\n")).unwrap();
    path
}

// Everything below that launches `claude` is Windows-only (a `.cmd` stand-in on a ConPTY); on
// other platforms only the sync test at the end of the file runs.
fn arg_after(screen: &str, flag: &str) -> String {
    let args: Vec<&str> = screen
        .lines()
        .filter_map(|l| l.trim().strip_prefix("ARG "))
        .collect();
    let at = args
        .iter()
        .position(|a| *a == flag)
        .unwrap_or_else(|| panic!("no {flag} in {args:?}"));
    args[at + 1].to_owned()
}

#[cfg(windows)]
#[tokio::test(flavor = "multi_thread")]
async fn a_fake_claude_runs_through_preflight_launch_keys_mentions_hooks_and_exit() {
    let dir = tempfile::tempdir().unwrap();
    let project = dir.path().join("proj");
    std::fs::create_dir_all(&project).unwrap();
    let fake = fake_claude(dir.path());
    // A key that must never reach Claude.
    std::env::set_var("ANTHROPIC_API_KEY", "sk-test-must-not-leak");

    let mut world = World::new(
        state(&project, Some(&fake), Size::new(120, 40)),
        options(dir.path(), "flow"),
    );
    // Two files in the (pretend) listing so `C` has something to send.
    let height = world.state.viewport_height();
    world.state.view.push_chunk(
        vec![Entry::file("alpha.rs", 1), Entry::file("beta.rs", 2)],
        height,
    );
    world.state.view.finish_loading();

    // c: preflight (the fake answers `auth status` with a subscription login), then launch.
    world.key("c");
    world
        .pump_until("the pane to run", |s| s.claude.is_running())
        .await;
    world.pump_until_screen_has("FAKE-CLAUDE-READY").await;
    // The fake prints its ARG/IPC/KEY lines after READY and only then asks for input: wait
    // for the prompt, the last thing it prints, before reading any of them.
    world.pump_until_screen_has("prompt>").await;
    assert_eq!(world.state.key_context(), KeyContext::Claude);

    let screen = world.screen();
    // Per-spawn injection: session id, MCP config and settings, never --bare.
    let session = arg_after(&screen, "--session-id");
    assert_eq!(session.len(), 36, "{session}");
    let mcp = PathBuf::from(arg_after(&screen, "--mcp-config"));
    let settings = PathBuf::from(arg_after(&screen, "--settings"));
    assert!(!screen.contains("--bare"));
    let mcp_json: serde_json::Value =
        serde_json::from_str(&std::fs::read_to_string(&mcp).unwrap()).unwrap();
    assert_eq!(mcp_json["mcpServers"]["kestrel"]["args"][0], "mcp");
    let settings_json: serde_json::Value =
        serde_json::from_str(&std::fs::read_to_string(&settings).unwrap()).unwrap();
    assert!(
        settings_json["hooks"]["PostToolUse"][0]["hooks"][0]["command"]
            .as_str()
            .unwrap()
            .ends_with("hook")
    );
    // The IPC name is passed, the API key is stripped from the child environment.
    let ipc_name = format!("kestrel-test-{}-flow", std::process::id());
    assert!(screen.contains(&format!("IPC={ipc_name}")), "{screen}");
    assert!(screen.contains("KEY=[]"), "{screen}");
    assert!(!screen.contains("sk-test-must-not-leak"));

    // Keys reach Claude verbatim while it has focus.
    world.type_text("hello");
    world.key("enter");
    world.pump_until_screen_has("you said: hello").await;
    assert_eq!(world.state.quit, None);

    // The focus toggle returns to the explorer; its keys work again and nothing is forwarded.
    world.key("ctrl-\\");
    assert_eq!(world.state.key_context(), KeyContext::Normal);
    world.key("alt-=");
    let grown = world.state.claude.pty.unwrap();
    world
        .pump_until("the grown pane screen", |s| {
            s.claude
                .screen
                .as_ref()
                .is_some_and(|sc| sc.rows == grown.height && sc.cols == grown.width)
        })
        .await;

    // C: the cursor file is typed as an @ mention and Claude gets focus.
    world.key("C");
    assert_eq!(world.state.key_context(), KeyContext::Claude);
    world.pump_until_screen_has("@alpha.rs").await;
    world.key("enter");
    world.pump_until_screen_has("you said: @alpha.rs").await;

    // A hook event from "Claude" (the hook process talks to the pipe) marks the file.
    let hook_name = ipc_name.clone();
    let session_for_hook = session.clone();
    let reply = tokio::task::spawn_blocking(move || {
        ks_claude::ipc::call_blocking(
            &hook_name,
            &ks_claude::ipc::Request::HookEvent {
                session_id: session_for_hook,
                tool_name: "Edit".into(),
                file_path: Some("alpha.rs".to_owned()),
            },
            Duration::from_secs(10),
        )
    });
    let project_alpha = world.state.cwd.join("alpha.rs");
    world
        .pump_until("the edit to be recorded", |s| {
            s.claude.changes.contains(&project_alpha)
        })
        .await;
    assert_eq!(reply.await.unwrap().unwrap(), ks_claude::ipc::Response::Ok);

    // Typing exit ends the process: the pane closes with its exit code in the status line.
    world.type_text("exit");
    world.key("enter");
    world
        .pump_until("the pane to close", |s| !s.claude.is_running())
        .await;
    assert!(
        world
            .state
            .status
            .as_ref()
            .is_some_and(|st| st.text.contains("code 7")),
        "{:?}",
        world.state.status
    );
    assert_eq!(world.state.key_context(), KeyContext::Normal);

    // g r resumes the remembered session. Claude writes a transcript after the first message;
    // stand in for it so the store offers `--resume <id>`.
    let transcripts = dir
        .path()
        .join("projects")
        .join(ks_claude::session::project_dir_name(&project));
    std::fs::create_dir_all(&transcripts).unwrap();
    std::fs::write(transcripts.join(format!("{session}.jsonl")), "{}").unwrap();
    world.key("g");
    world.key("r");
    world
        .pump_until("the resumed pane", |s| s.claude.is_running())
        .await;
    world.pump_until_screen_has("FAKE-CLAUDE-READY").await;
    world.pump_until_screen_has("prompt>").await;
    assert_eq!(arg_after(&world.screen(), "--resume"), session);
    assert!(!world.screen().contains("--session-id"));
    world.runner.shutdown();
}

/// A stand-in whose `--resume` fails the way `claude` does for an unknown conversation.
#[cfg(windows)]
fn fake_claude_rejecting_resume(dir: &Path) -> PathBuf {
    let script = r#"@echo off
if "%~1"=="auth" (
  echo {"loggedIn":true,"authMethod":"claude.ai","apiProvider":"firstParty"}
  exit /b 0
)
if "%~1"=="--resume" (
  echo No conversation found with session ID: %~2
  exit /b 1
)
echo FAKE-CLAUDE-READY
:loop
set LINE=
set /p LINE=prompt^>
if /i "%LINE%"=="exit" exit /b 0
goto loop
"#;
    let path = dir.join("claude.cmd");
    std::fs::write(&path, script.replace('\n', "\r\n")).unwrap();
    path
}

#[cfg(windows)]
#[tokio::test(flavor = "multi_thread")]
async fn a_rejected_resume_shows_the_reason_and_forgets_the_stale_session() {
    let dir = tempfile::tempdir().unwrap();
    let project = dir.path().join("proj");
    std::fs::create_dir_all(&project).unwrap();
    let fake = fake_claude_rejecting_resume(dir.path());
    let opts = options(dir.path(), "badresume");
    // A stored id whose transcript exists somewhere (so `g r` resumes it).
    let id = ks_claude::session::new_session_id();
    ks_claude::session::SessionStore::new(opts.sessions_file.clone().unwrap())
        .set(&ks_app::claude::project_root(&project), &id)
        .unwrap();
    let transcripts = dir
        .path()
        .join("projects")
        .join(ks_claude::session::project_dir_name(&project));
    std::fs::create_dir_all(&transcripts).unwrap();
    std::fs::write(transcripts.join(format!("{id}.jsonl")), "{}").unwrap();

    let mut world = World::new(
        state(&project, Some(&fake), Size::new(120, 40)),
        opts.clone(),
    );
    world.key("g");
    world.key("r");
    world
        .pump_until("the failure to be reported", |s| {
            s.status
                .as_ref()
                .is_some_and(|st| st.text.contains("code 1"))
        })
        .await;
    let status = world.state.status.clone().unwrap();
    assert!(
        status.text.contains("No conversation found"),
        "the reason must be visible: {status:?}"
    );
    assert!(!world.state.claude.is_running());
    // The stale id is forgotten (asynchronously), so the next `g r` starts a new session.
    let store = ks_claude::session::SessionStore::new(opts.sessions_file.clone().unwrap());
    let root = ks_app::claude::project_root(&project);
    let deadline = Instant::now() + Duration::from_secs(20);
    while store.get(&root).as_deref() == Some(id.as_str()) {
        assert!(
            Instant::now() < deadline,
            "the stale session was not forgotten"
        );
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
    world.runner.shutdown();
}

#[tokio::test(flavor = "multi_thread")]
async fn ipc_requests_flow_through_update_the_runner_and_a_real_socket() {
    use ks_claude::ipc::{call, ContentHit, Request, Response};

    let dir = tempfile::tempdir().unwrap();
    let root = dir.path().join("proj");
    std::fs::create_dir_all(root.join("src")).unwrap();
    std::fs::write(root.join("a.txt"), "needle in a haystack\n").unwrap();
    std::fs::write(root.join("src/b.rs"), "fn needle() {}\n").unwrap();
    std::fs::write(root.join("src/c.rs"), "nothing\n").unwrap();
    std::fs::write(root.join("z.txt"), "z").unwrap();

    let mut world = World::new(
        state(&root, None, Size::new(100, 30)),
        options(dir.path(), "ipc"),
    );
    let height = world.state.viewport_height();
    world.state.view.push_chunk(
        vec![
            Entry::dir("src"),
            Entry::file("a.txt", 21),
            Entry::file("z.txt", 1),
        ],
        height,
    );
    world.state.view.finish_loading();
    // Mark src (sorted first), then leave the cursor on a.txt.
    world.key("space");
    world.state.claude.root = Some(root.clone());

    let name = world
        .runner
        .claude()
        .ensure_ipc()
        .expect("the ipc server starts");
    let requests = vec![
        Request::GetSelection,
        Request::ListDir { path: ".".into() },
        Request::SearchFiles {
            query: "b.rs".into(),
            root: None,
            limit: 5,
        },
        Request::SearchContents {
            pattern: "needle".into(),
            glob: Some("*.rs".into()),
            regex: false,
            limit: 10,
        },
        Request::Reveal {
            path: "z.txt".into(),
        },
        Request::Reveal {
            path: "does-not-exist".into(),
        },
        Request::HookEvent {
            session_id: "s".into(),
            tool_name: "Write".into(),
            file_path: Some("src/c.rs".into()),
        },
    ];
    let client = tokio::spawn(async move {
        let mut out = Vec::new();
        for request in requests {
            out.push(
                call(&name, &request, Duration::from_secs(20))
                    .await
                    .unwrap(),
            );
        }
        out
    });
    let mut client = client;
    let responses = loop {
        tokio::select! {
            done = &mut client => break done.unwrap(),
            Some(msg) = world.rx.recv() => world.apply(msg),
        }
    };

    let path = |rel: &str| root.join(rel).to_string_lossy().into_owned();
    match &responses[0] {
        Response::Selection {
            marked,
            cursor,
            cwd,
        } => {
            assert_eq!(marked, &[path("src")]);
            assert_eq!(cursor.as_deref(), Some(path("a.txt").as_str()));
            assert_eq!(cwd, &root.to_string_lossy());
        }
        other => panic!("{other:?}"),
    }
    match &responses[1] {
        Response::Dir { entries } => {
            let names: Vec<&str> = entries.iter().map(|e| e.name.as_str()).collect();
            assert_eq!(names, ["src", "a.txt", "z.txt"]);
        }
        other => panic!("{other:?}"),
    }
    match &responses[2] {
        Response::Files { paths, truncated } => {
            assert!(!truncated);
            assert_eq!(paths.len(), 1, "{paths:?}");
            assert!(paths[0].ends_with("b.rs"));
        }
        other => panic!("{other:?}"),
    }
    match &responses[3] {
        Response::Matches { hits, truncated } => {
            assert!(!truncated);
            assert_eq!(hits.len(), 1, "{hits:?}");
            let ContentHit { path, line, text } = &hits[0];
            assert!(path.ends_with("b.rs"));
            assert_eq!((*line, text.as_str()), (1, "fn needle() {}"));
        }
        other => panic!("{other:?}"),
    }
    assert_eq!(responses[4], Response::Ok);
    assert!(
        matches!(&responses[5], Response::Error { message } if message.contains("does not exist"))
    );
    assert_eq!(responses[6], Response::Ok);

    // The Reveal landed on the state: the cursor is on z.txt, and the hook marked the file.
    // (Late messages from the worker may still be queued.)
    world
        .pump_until("the reveal to move the cursor", |s| {
            s.view.current().is_some_and(|e| e.name == "z.txt")
        })
        .await;
    assert!(world
        .state
        .claude
        .changes
        .contains(&root.join("src").join("c.rs")));
    world.runner.shutdown();
}

#[tokio::test(flavor = "multi_thread")]
async fn a_missing_claude_binary_shows_install_instructions_and_launches_nothing() {
    let dir = tempfile::tempdir().unwrap();
    let missing = dir.path().join("no-such-claude.exe");
    let mut world = World::new(
        state(dir.path(), Some(&missing), Size::new(100, 30)),
        options(dir.path(), "missing"),
    );
    world.key("c");
    world
        .pump_until("a dialog", |s| s.claude.dialog.is_some())
        .await;
    assert!(!world.state.claude.is_running());
    assert!(!world.runner.claude().has_pane());
    world.key("esc");
    assert!(world.state.claude.dialog.is_none());
    world.runner.shutdown();
}

/// Needs the real `claude`: starts it in a temp dir (nothing is ever typed, so no prompt is
/// sent and no plan usage is consumed) and checks that its first screen reaches the state.
/// Run with `cargo test -p ks-app --test claude_flow -- --ignored`.
#[tokio::test(flavor = "multi_thread")]
#[ignore = "launches the real claude CLI"]
async fn real_claude_first_screen_reaches_the_state() {
    let dir = tempfile::tempdir().unwrap();
    let mut world = World::new(
        state(dir.path(), None, Size::new(140, 40)),
        options(dir.path(), "real"),
    );
    world.key("c");
    let generation = world.state.claude.generation;
    // Skip the preflight: it would run `claude auth status` (harmless, but not needed here).
    world.apply(Msg::ClaudePreflight {
        generation,
        result: ks_core::ClaudePreflight::Ok,
    });
    world
        .pump_until("the pane to run", |s| s.claude.is_running())
        .await;
    world
        .pump_until("Claude's first screen", |s| {
            s.claude
                .screen
                .as_ref()
                .is_some_and(|sc| !screen_text(sc).trim().is_empty())
        })
        .await;
    world.runner.shutdown();
}

#[test]
fn arg_after_reads_the_echoed_arguments() {
    let screen = "FAKE-CLAUDE-READY
ARG --resume
ARG abc
IPC=x";
    assert_eq!(arg_after(screen, "--resume"), "abc");
}
