//! Generators for the per-spawn `--mcp-config` and `--settings` files (10d / 10e).
//!
//! Nothing is installed at user scope and nothing is written into the user's projects: the files
//! live in a temp directory and are passed to `claude` on the command line.
//!
//! Schemas (VERIFIED against code.claude.com docs `mcp.md` / `hooks.md`):
//! * MCP config: `{"mcpServers": {"<name>": {"type": "stdio", "command": ..., "args": [...], "env": {...}}}}`
//! * Settings: `{"hooks": {"PostToolUse": [{"matcher": "Edit|Write|...", "hooks": [{"type": "command", "command": ..., "timeout": N}]}]}}`
//!   (a handler may alternatively use exec form: `command` + `args`, which spawns without a shell).
//! * `claude --help` (2.1.296): `--mcp-config <configs...>` (JSON files or strings),
//!   `--settings <file-or-json>`, `--strict-mcp-config` (we do **not** pass it, so the user's own
//!   MCP servers keep working).

use std::path::{Path, PathBuf};

use serde_json::{json, Value};

use crate::ipc::IPC_ENV;

/// Name under which Kestrel's MCP server is registered (tools appear as `mcp__kestrel__*`).
pub const MCP_SERVER_NAME: &str = "kestrel";
/// Hook matcher for edit-like tools.
pub const HOOK_MATCHER: &str = "Edit|Write|MultiEdit|NotebookEdit";

/// How the hook command is written.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum HookCommandStyle {
    /// `"command": "\"C:/path/ks.exe\" hook"` (works with the default Git Bash shell; the path
    /// uses forward slashes). This is the plan's form and works on every Claude Code version.
    #[default]
    ShellString,
    /// `"command": "C:\\path\\ks.exe", "args": ["hook"]`: no shell, no quoting problems, but
    /// needs a Claude Code version that supports `args` (an older one would run the bare exe).
    Exec,
}

fn exe_str(p: &Path) -> String {
    p.to_string_lossy().into_owned()
}

/// `--mcp-config` JSON: command = the `ks` executable, args `["mcp"]`, env `KESTREL_IPC`.
pub fn mcp_config_json(ks_exe: &Path, ipc_name: &str) -> Value {
    json!({
        "mcpServers": {
            MCP_SERVER_NAME: {
                "type": "stdio",
                "command": exe_str(ks_exe),
                "args": ["mcp"],
                "env": { IPC_ENV: ipc_name }
            }
        }
    })
}

/// `--settings` JSON with the PostToolUse hook running `ks hook`.
pub fn settings_json(ks_exe: &Path, style: HookCommandStyle) -> Value {
    let handler = match style {
        HookCommandStyle::ShellString => {
            let fwd = exe_str(ks_exe).replace('\\', "/");
            json!({ "type": "command", "command": format!("\"{fwd}\" hook"), "timeout": 10 })
        }
        HookCommandStyle::Exec => json!({
            "type": "command", "command": exe_str(ks_exe), "args": ["hook"], "timeout": 10
        }),
    };
    json!({
        "hooks": {
            "PostToolUse": [ { "matcher": HOOK_MATCHER, "hooks": [handler] } ]
        }
    })
}

/// Files written for one spawn.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LaunchFiles {
    /// Path of the MCP config JSON.
    pub mcp_config: PathBuf,
    /// Path of the settings JSON.
    pub settings: PathBuf,
}

impl LaunchFiles {
    /// Arguments to append to the `claude` command line.
    pub fn claude_args(&self) -> Vec<String> {
        vec![
            "--mcp-config".into(),
            exe_str(&self.mcp_config),
            "--settings".into(),
            exe_str(&self.settings),
        ]
    }
}

/// Default directory for generated files: `<temp>/kestrel-<pid>`. `%TEMP%` is private to the
/// user on Windows (per-user profile ACL); the files hold no secrets, only the exe path and the
/// IPC pipe name. See [`sweep_stale_launch_dirs`] for cleaning up after crashes.
pub fn default_launch_dir() -> PathBuf {
    std::env::temp_dir().join(format!("kestrel-{}", std::process::id()))
}

/// Remove launch directories left behind by Kestrel processes that were killed or crashed
/// (`ClaudeHost::shutdown` only runs on a clean exit).
///
/// Looks in `base` (normally `std::env::temp_dir()`) for directories named `kestrel-<digits>`
/// whose files were last modified more than `max_age` ago, skipping the directory of `own_pid`,
/// and only deletes directories that contain nothing but `mcp.json` / `settings.json`.
/// `pid_alive` is the liveness check (this crate forbids `unsafe`, so the OS call lives in
/// `ks_fs::pid_alive`); a directory whose process is alive is never touched. `max_age` is an
/// extra safety margin for processes that just exited (the files are only read when `claude`
/// starts). Returns how many directories were removed. Call it once at startup, off the UI
/// thread.
pub fn sweep_stale_launch_dirs(
    base: &Path,
    own_pid: u32,
    max_age: std::time::Duration,
    pid_alive: impl Fn(u32) -> bool,
) -> usize {
    let Ok(entries) = std::fs::read_dir(base) else {
        return 0;
    };
    let own = format!("kestrel-{own_pid}");
    let mut removed = 0;
    for entry in entries.flatten() {
        let name = entry.file_name().to_string_lossy().into_owned();
        let Some(pid) = name.strip_prefix("kestrel-") else {
            continue;
        };
        if name == own || pid.is_empty() || !pid.bytes().all(|b| b.is_ascii_digit()) {
            continue;
        }
        match pid.parse::<u32>() {
            Ok(n) if !pid_alive(n) => {}
            _ => continue,
        }
        let dir = entry.path();
        let Ok(files) = std::fs::read_dir(&dir) else {
            continue;
        };
        let mut newest = std::time::UNIX_EPOCH;
        let mut only_ours = true;
        for f in files.flatten() {
            let fname = f.file_name().to_string_lossy().into_owned();
            if fname != "mcp.json" && fname != "settings.json" {
                only_ours = false;
                break;
            }
            if let Ok(m) = f.metadata().and_then(|m| m.modified()) {
                newest = newest.max(m);
            }
        }
        let old = newest.elapsed().map(|age| age > max_age).unwrap_or(false);
        if only_ours && old && std::fs::remove_dir_all(&dir).is_ok() {
            removed += 1;
        }
    }
    removed
}

/// Write both files into `dir` (created if needed). Blocking filesystem IO on tiny files.
pub fn write_launch_files(
    dir: &Path,
    ks_exe: &Path,
    ipc_name: &str,
    style: HookCommandStyle,
) -> std::io::Result<LaunchFiles> {
    std::fs::create_dir_all(dir)?;
    let mcp_config = dir.join("mcp.json");
    let settings = dir.join("settings.json");
    std::fs::write(
        &mcp_config,
        serde_json::to_vec_pretty(&mcp_config_json(ks_exe, ipc_name))?,
    )?;
    std::fs::write(
        &settings,
        serde_json::to_vec_pretty(&settings_json(ks_exe, style))?,
    )?;
    Ok(LaunchFiles {
        mcp_config,
        settings,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn mcp_config_shape() {
        let v = mcp_config_json(Path::new("C:\\bin\\ks.exe"), "kestrel-42");
        let s = &v["mcpServers"]["kestrel"];
        assert_eq!(s["type"], "stdio");
        assert_eq!(s["command"], "C:\\bin\\ks.exe");
        assert_eq!(s["args"], json!(["mcp"]));
        assert_eq!(s["env"]["KESTREL_IPC"], "kestrel-42");
    }

    #[test]
    fn settings_shell_style() {
        let v = settings_json(
            Path::new("C:\\Program Files\\ks\\ks.exe"),
            HookCommandStyle::ShellString,
        );
        let g = &v["hooks"]["PostToolUse"][0];
        assert_eq!(g["matcher"], "Edit|Write|MultiEdit|NotebookEdit");
        assert_eq!(g["hooks"][0]["type"], "command");
        assert_eq!(
            g["hooks"][0]["command"],
            "\"C:/Program Files/ks/ks.exe\" hook"
        );
    }

    #[test]
    fn settings_exec_style() {
        let v = settings_json(Path::new("C:\\ks.exe"), HookCommandStyle::Exec);
        let h = &v["hooks"]["PostToolUse"][0]["hooks"][0];
        assert_eq!(h["command"], "C:\\ks.exe");
        assert_eq!(h["args"], json!(["hook"]));
    }

    #[test]
    fn files_are_written_and_parse_back() {
        let dir = tempfile::tempdir().unwrap();
        let f = write_launch_files(
            dir.path(),
            Path::new("ks.exe"),
            "kestrel-1",
            HookCommandStyle::default(),
        )
        .unwrap();
        let mcp: Value = serde_json::from_slice(&std::fs::read(&f.mcp_config).unwrap()).unwrap();
        assert_eq!(
            mcp["mcpServers"]["kestrel"]["env"]["KESTREL_IPC"],
            "kestrel-1"
        );
        let set: Value = serde_json::from_slice(&std::fs::read(&f.settings).unwrap()).unwrap();
        assert!(set["hooks"]["PostToolUse"].is_array());
        let args = f.claude_args();
        assert_eq!(args[0], "--mcp-config");
        assert_eq!(args[2], "--settings");
        assert!(!args.iter().any(|a| a == "--bare"));
    }

    #[test]
    fn sweep_removes_only_old_kestrel_dirs_with_our_files() {
        let base = tempfile::tempdir().unwrap();
        let mk = |name: &str, files: &[&str]| {
            let d = base.path().join(name);
            std::fs::create_dir_all(&d).unwrap();
            for f in files {
                std::fs::write(d.join(f), "{}").unwrap();
            }
        };
        mk("kestrel-111", &["mcp.json", "settings.json"]); // stale
        mk("kestrel-222", &["mcp.json", "notes.txt"]); // foreign file: keep
        mk("kestrel-333", &["mcp.json"]); // own pid: keep
        mk("kestrel-abc", &["mcp.json"]); // not a pid: keep
        mk("other-444", &["mcp.json"]); // not ours: keep
                                        // everything is "new": nothing may be removed with a generous age
        assert_eq!(
            sweep_stale_launch_dirs(
                base.path(),
                333,
                std::time::Duration::from_secs(3600),
                |_| false
            ),
            0
        );
        std::thread::sleep(std::time::Duration::from_millis(30));
        // a live process keeps its directory, however old
        assert_eq!(
            sweep_stale_launch_dirs(
                base.path(),
                333,
                std::time::Duration::from_millis(5),
                |_| true
            ),
            0
        );
        assert!(base.path().join("kestrel-111").exists());
        assert_eq!(
            sweep_stale_launch_dirs(
                base.path(),
                333,
                std::time::Duration::from_millis(5),
                |pid| pid != 111
            ),
            1
        );
        assert!(!base.path().join("kestrel-111").exists());
        for keep in ["kestrel-222", "kestrel-333", "kestrel-abc", "other-444"] {
            assert!(base.path().join(keep).exists(), "{keep}");
        }
    }
}
