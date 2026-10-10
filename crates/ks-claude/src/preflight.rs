//! Preflight: locate `claude`, run `claude auth status`, classify the login (plan 10.1).
//!
//! VERIFIED against Claude Code 2.1.296: `claude auth status` (alias for `--json`) prints a JSON
//! object and exits 0 when logged in. Fields seen: `loggedIn` (bool), `authMethod` (`"claude.ai"`
//! for a subscription login), `apiProvider` (`"firstParty"`), `analyticsDisabled`,
//! `projectsDirectory`, `configDirectory`, `email`, `orgId`, `orgName`, `subscriptionType`
//! (`"max"`...). `--text` prints a human readable version. The personal fields (`email`, `orgId`,
//! `orgName`) are deliberately **not** modelled here so they can never be stored or logged.
//! VERIFIED later with `CLAUDE_CONFIG_DIR` pointing at an empty directory: logged out prints
//! `{"loggedIn": false, "authMethod": "none", ...}` and exits 1. The parser keys off `loggedIn`,
//! not the exit code; a non-zero exit with output that is not JSON (crash, broken install,
//! wrong CLI version) is `Failed`, not `NotLoggedIn`.

use std::io::Read;
use std::path::{Path, PathBuf};
use std::process::Stdio;
use std::time::{Duration, Instant};

use serde::Deserialize;
use tokio_util::sync::CancellationToken;

use crate::env::EnvPolicy;
use crate::events::{ClaudeEvent, EventSink};

/// Upper bound on the bytes read from `claude auth status`.
const MAX_OUTPUT: u64 = 1 << 20;

/// Install instructions shown when `claude` is not found.
pub const INSTALL_INSTRUCTIONS: &str = "Claude Code was not found on PATH. Install it with:\n  \
irm https://claude.ai/install.ps1 | iex\n(or `winget install Anthropic.ClaudeCode`), then run `claude` once to log in with your Pro/Max account.";

/// Parsed `claude auth status` JSON. Personal identifiers are intentionally not parsed.
#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct AuthStatus {
    /// Whether any login is active.
    #[serde(default)]
    pub logged_in: bool,
    /// `"claude.ai"` (subscription), `"api_key"`, `"api_key_helper"`, ...
    #[serde(default)]
    pub auth_method: Option<String>,
    /// e.g. `"firstParty"`, `"bedrock"`, `"vertex"`.
    #[serde(default)]
    pub api_provider: Option<String>,
    /// e.g. `"max"`, `"pro"`.
    #[serde(default)]
    pub subscription_type: Option<String>,
}

/// Parse the JSON printed by `claude auth status`.
pub fn parse_auth_status(json: &str) -> Result<AuthStatus, serde_json::Error> {
    serde_json::from_str(json.trim())
}

/// Classification of the preflight outcome.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum PreflightResult {
    /// Subscription login (`authMethod == "claude.ai"`): safe to launch.
    Ok(AuthStatus),
    /// Logged in, but Claude would bill an API key / third party provider. The UI must warn
    /// ("Claude would bill your API key; aborting") and proceed only on explicit confirmation.
    BillingWarning(AuthStatus),
    /// Not logged in: tell the user to run `claude` and `/login`.
    NotLoggedIn,
    /// `claude` executable not found; show [`INSTALL_INSTRUCTIONS`].
    NotInstalled {
        /// Text to show.
        instructions: String,
    },
    /// Could not determine the state (timeout, spawn failure, unparseable output).
    Failed(String),
}

/// Classify a parsed status.
pub fn classify(status: AuthStatus) -> PreflightResult {
    if !status.logged_in {
        return PreflightResult::NotLoggedIn;
    }
    let subscription = status.auth_method.as_deref() == Some("claude.ai")
        && status
            .api_provider
            .as_deref()
            .is_none_or(|p| p == "firstParty");
    if subscription {
        PreflightResult::Ok(status)
    } else {
        PreflightResult::BillingWarning(status)
    }
}

/// Find the `claude` executable: PATH first, then the native installer location.
pub fn locate_claude() -> Option<PathBuf> {
    if let Ok(p) = which::which("claude") {
        return Some(p);
    }
    let home = std::env::var_os("USERPROFILE").or_else(|| std::env::var_os("HOME"))?;
    ["claude.exe", "claude"]
        .iter()
        .map(|n| Path::new(&home).join(".local").join("bin").join(n))
        .find(|p| p.is_file())
}

/// Options for [`run_preflight`].
#[derive(Debug, Clone)]
pub struct PreflightOptions {
    /// Explicit executable; `None` = [`locate_claude`].
    pub program: Option<PathBuf>,
    /// Env stripping policy.
    pub env: EnvPolicy,
    /// Give up after this long.
    pub timeout: Duration,
}

impl Default for PreflightOptions {
    fn default() -> Self {
        Self {
            program: None,
            env: EnvPolicy::default(),
            timeout: Duration::from_secs(15),
        }
    }
}

fn not_installed() -> PreflightResult {
    PreflightResult::NotInstalled {
        instructions: INSTALL_INSTRUCTIONS.to_string(),
    }
}

/// Run the preflight synchronously (call from a worker thread; see [`spawn_preflight`]).
pub fn run_preflight(opts: &PreflightOptions, cancel: &CancellationToken) -> PreflightResult {
    let Some(program) = opts.program.clone().or_else(locate_claude) else {
        return not_installed();
    };
    let mut cmd = std::process::Command::new(&program);
    cmd.args(["auth", "status", "--json"])
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::null());
    opts.env.apply_std(&mut cmd);
    #[cfg(windows)]
    {
        use std::os::windows::process::CommandExt;
        cmd.creation_flags(0x0800_0000); // CREATE_NO_WINDOW
    }
    let mut child = match cmd.spawn() {
        Ok(c) => c,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return not_installed(),
        Err(e) => return PreflightResult::Failed(format!("cannot run claude: {e}")),
    };
    let Some(mut stdout) = child.stdout.take() else {
        let _ = child.kill();
        let _ = child.wait();
        return PreflightResult::Failed("no stdout".into());
    };
    // The reader runs detached and reports through a channel: joining it could block past the
    // timeout if a grandchild keeps the pipe open after `claude` exited or was killed.
    let (out_tx, out_rx) = std::sync::mpsc::channel::<String>();
    let reader = std::thread::Builder::new()
        .name("ks-claude-preflight-read".into())
        .spawn(move || {
            let mut s = String::new();
            let _ = (&mut stdout).take(MAX_OUTPUT).read_to_string(&mut s);
            let _ = out_tx.send(s);
        });
    if let Err(e) = reader {
        let _ = child.kill();
        let _ = child.wait();
        return PreflightResult::Failed(format!("cannot spawn reader thread: {e}"));
    }
    let deadline = Instant::now() + opts.timeout;
    let status = loop {
        match child.try_wait() {
            Ok(Some(st)) => break st,
            Ok(None) => {}
            Err(e) => {
                let _ = child.kill();
                return PreflightResult::Failed(format!("wait failed: {e}"));
            }
        }
        if cancel.is_cancelled() || Instant::now() >= deadline {
            let _ = child.kill();
            let _ = child.wait();
            return PreflightResult::Failed(if cancel.is_cancelled() {
                "cancelled".into()
            } else {
                "timed out running `claude auth status`".into()
            });
        }
        std::thread::sleep(Duration::from_millis(20));
    };
    let remaining = deadline
        .saturating_duration_since(Instant::now())
        .max(Duration::from_millis(500));
    let Ok(out) = out_rx.recv_timeout(remaining) else {
        return PreflightResult::Failed("timed out reading `claude auth status` output".into());
    };
    match parse_auth_status(&out) {
        // A logged-out `claude auth status` prints `{"loggedIn": false, ...}` (exit 1, VERIFIED),
        // which parses and classifies as NotLoggedIn; the exit code is not consulted.
        Ok(s) => classify(s),
        Err(e) if !status.success() => PreflightResult::Failed(format!(
            "`claude auth status` failed ({}) without JSON output: {e}",
            status
                .code()
                .map_or_else(|| "killed".to_string(), |c| format!("exit {c}"))
        )),
        Err(e) => PreflightResult::Failed(format!("unexpected `claude auth status` output: {e}")),
    }
}

/// Run the preflight on a new thread and report `ClaudeEvent::Preflight` to `sink`.
///
/// Exactly one event is emitted per call: a cancelled run reports `Failed("cancelled")`, so the
/// caller maps 1:1 and drops stale generations itself.
pub fn spawn_preflight(
    generation: u64,
    opts: PreflightOptions,
    cancel: CancellationToken,
    sink: EventSink,
) {
    let spawned = std::thread::Builder::new()
        .name("ks-claude-preflight".into())
        .spawn({
            let sink = sink.clone();
            move || {
                let result = run_preflight(&opts, &cancel);
                sink(ClaudeEvent::Preflight { generation, result });
            }
        });
    if let Err(e) = spawned {
        tracing::warn!("cannot spawn preflight thread: {e}");
        sink(ClaudeEvent::Preflight {
            generation,
            result: PreflightResult::Failed(format!("thread spawn failed: {e}")),
        });
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    // Redacted fixture: real field names, placeholder identifiers.
    const SUBSCRIPTION: &str = r#"{
  "loggedIn": true, "authMethod": "claude.ai", "apiProvider": "firstParty",
  "analyticsDisabled": false, "projectsDirectory": "C:\\Users\\<user>\\.claude\\projects",
  "configDirectory": "C:\\Users\\<user>\\.claude", "email": "<redacted>",
  "orgId": "<redacted>", "orgName": "<redacted>", "subscriptionType": "max" }"#;

    #[test]
    fn parses_subscription() {
        let s = parse_auth_status(SUBSCRIPTION).unwrap();
        assert!(s.logged_in);
        assert_eq!(s.auth_method.as_deref(), Some("claude.ai"));
        assert_eq!(s.subscription_type.as_deref(), Some("max"));
        assert!(matches!(classify(s), PreflightResult::Ok(_)));
    }

    #[test]
    fn api_key_is_billing_warning() {
        for m in ["api_key", "api_key_helper", "oauth_token", "weird"] {
            let j = format!(r#"{{"loggedIn":true,"authMethod":"{m}"}}"#);
            let s = parse_auth_status(&j).unwrap();
            assert!(
                matches!(classify(s), PreflightResult::BillingWarning(_)),
                "{m}"
            );
        }
    }

    #[test]
    fn third_party_provider_is_billing_warning() {
        let j = r#"{"loggedIn":true,"authMethod":"claude.ai","apiProvider":"bedrock"}"#;
        assert!(matches!(
            classify(parse_auth_status(j).unwrap()),
            PreflightResult::BillingWarning(_)
        ));
    }

    #[test]
    fn logged_out() {
        let s = parse_auth_status(r#"{"loggedIn": false}"#).unwrap();
        assert_eq!(classify(s), PreflightResult::NotLoggedIn);
        assert_eq!(
            classify(parse_auth_status("{}").unwrap()),
            PreflightResult::NotLoggedIn
        );
    }

    #[test]
    fn garbage_is_error() {
        assert!(parse_auth_status("not json").is_err());
    }

    #[test]
    fn missing_program_is_not_installed() {
        let opts = PreflightOptions {
            program: Some(PathBuf::from("Z:\\definitely\\not\\here\\claude-xyz")),
            ..Default::default()
        };
        let r = run_preflight(&opts, &CancellationToken::new());
        assert!(matches!(r, PreflightResult::NotInstalled { .. }), "{r:?}");
    }

    /// Fake `claude`: a `.cmd` script taking the place of the executable (Windows only).
    #[cfg(windows)]
    fn fake_claude(dir: &Path, body: &str) -> PathBuf {
        let p = dir.join("fake-claude.cmd");
        std::fs::write(&p, format!("@echo off\r\n{body}\r\n")).unwrap();
        p
    }

    #[cfg(windows)]
    fn fake_opts(program: PathBuf) -> PreflightOptions {
        PreflightOptions {
            program: Some(program),
            ..Default::default()
        }
    }

    #[cfg(windows)]
    #[test]
    fn fake_claude_json_is_classified() {
        let dir = tempfile::tempdir().unwrap();
        let p = fake_claude(
            dir.path(),
            r#"echo {"loggedIn":true,"authMethod":"claude.ai","apiProvider":"firstParty"}"#,
        );
        let r = run_preflight(&fake_opts(p), &CancellationToken::new());
        assert!(matches!(r, PreflightResult::Ok(_)), "{r:?}");
    }

    #[cfg(windows)]
    #[test]
    fn env_is_stripped_from_the_preflight_command() {
        // COMPUTERNAME is always set on Windows, so it stands in for ANTHROPIC_API_KEY.
        let dir = tempfile::tempdir().unwrap();
        let p = fake_claude(
            dir.path(),
            r#"if defined COMPUTERNAME (echo {"loggedIn":true,"authMethod":"api_key"}) else (echo {"loggedIn":true,"authMethod":"claude.ai"})"#,
        );
        let visible = run_preflight(&fake_opts(p.clone()), &CancellationToken::new());
        assert!(
            matches!(visible, PreflightResult::BillingWarning(_)),
            "{visible:?}"
        );
        let mut opts = fake_opts(p);
        opts.env = EnvPolicy::new(["computername"]); // names are case-insensitive on Windows
        let stripped = run_preflight(&opts, &CancellationToken::new());
        assert!(matches!(stripped, PreflightResult::Ok(_)), "{stripped:?}");
    }

    #[cfg(windows)]
    #[test]
    fn nonzero_exit_with_logged_out_json_is_not_logged_in() {
        let dir = tempfile::tempdir().unwrap();
        let p = fake_claude(
            dir.path(),
            "echo {\"loggedIn\":false,\"authMethod\":\"none\"}\r\nexit /b 1",
        );
        let r = run_preflight(&fake_opts(p), &CancellationToken::new());
        assert_eq!(r, PreflightResult::NotLoggedIn);
    }

    #[cfg(windows)]
    #[test]
    fn crash_without_json_is_failed_not_logged_out() {
        let dir = tempfile::tempdir().unwrap();
        let p = fake_claude(dir.path(), "echo boom\r\nexit /b 3");
        let r = run_preflight(&fake_opts(p), &CancellationToken::new());
        assert!(
            matches!(&r, PreflightResult::Failed(m) if m.contains("exit 3")),
            "{r:?}"
        );
    }

    #[cfg(windows)]
    #[test]
    fn garbage_with_exit_zero_is_failed() {
        let dir = tempfile::tempdir().unwrap();
        let p = fake_claude(dir.path(), "echo not json at all");
        let r = run_preflight(&fake_opts(p), &CancellationToken::new());
        assert!(matches!(r, PreflightResult::Failed(_)), "{r:?}");
    }

    #[cfg(windows)]
    #[test]
    fn output_is_capped_at_one_mebibyte() {
        let dir = tempfile::tempdir().unwrap();
        let p = fake_claude(
            dir.path(),
            "powershell -NoProfile -Command \"[Console]::Out.Write('x' * 3000000)\"",
        );
        let r = run_preflight(&fake_opts(p), &CancellationToken::new());
        assert!(matches!(r, PreflightResult::Failed(_)), "{r:?}");
    }

    #[cfg(windows)]
    #[test]
    fn timeout_kills_a_sleeping_child() {
        let dir = tempfile::tempdir().unwrap();
        let p = fake_claude(dir.path(), "ping -n 20 127.0.0.1 >nul");
        let mut opts = fake_opts(p);
        opts.timeout = Duration::from_millis(300);
        let t = Instant::now();
        let r = run_preflight(&opts, &CancellationToken::new());
        assert!(
            matches!(&r, PreflightResult::Failed(m) if m.contains("timed out")),
            "{r:?}"
        );
        assert!(t.elapsed() < Duration::from_secs(5));
    }

    #[cfg(windows)]
    #[test]
    fn cancellation_stops_the_run_and_still_reports_one_event() {
        let dir = tempfile::tempdir().unwrap();
        let p = fake_claude(dir.path(), "ping -n 20 127.0.0.1 >nul");
        let cancel = CancellationToken::new();
        let (tx, rx) = std::sync::mpsc::channel();
        let tx = std::sync::Mutex::new(tx);
        let sink: EventSink = std::sync::Arc::new(move |e| {
            let _ = tx.lock().unwrap().send(e);
        });
        spawn_preflight(5, fake_opts(p), cancel.clone(), sink);
        std::thread::sleep(Duration::from_millis(300));
        cancel.cancel();
        match rx.recv_timeout(Duration::from_secs(5)).unwrap() {
            ClaudeEvent::Preflight {
                generation: 5,
                result: PreflightResult::Failed(m),
            } => assert_eq!(m, "cancelled"),
            other => panic!("{other:?}"),
        }
        assert!(rx.recv_timeout(Duration::from_millis(200)).is_err());
    }
}
