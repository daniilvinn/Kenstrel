//! Git status by shelling out to `git status --porcelain=v2 -z`.
//!
//! The format (from `git status --help`, "Porcelain Format Version 2"), with `-z` so that
//! every record, headers included, ends in NUL and paths are never quoted:
//!
//! ```text
//! # branch.oid <commit> | (initial)
//! # branch.head <branch> | (detached)
//! # branch.upstream <upstream_branch>
//! # branch.ab +<ahead> -<behind>
//! 1 <XY> <sub> <mH> <mI> <mW> <hH> <hI> <path>                      ordinary change
//! 2 <XY> <sub> <mH> <mI> <mW> <hH> <hI> <X><score> <path>NUL<orig>  rename or copy
//! u <XY> <sub> <m1> <m2> <m3> <mW> <h1> <h2> <h3> <path>            unmerged
//! ? <path>                                                          untracked
//! ! <path>                                                          ignored
//! ```
//!
//! `XY` is the index status then the work-tree status, `.` meaning unchanged. Paths are
//! relative to the repository root and may contain spaces, so a record is split with a
//! field limit rather than on every space.

use std::path::Path;
use std::process::Command;

use ks_core::{GitBranch, GitCounts, GitRecord, GitSnapshot, GitState};

/// The git invocations. They are separate from running them so the argument lists are
/// testable and the runner can pick how to execute them.
pub fn status_command(dir: &Path) -> Command {
    let mut cmd = git(dir);
    cmd.args([
        "status",
        "--porcelain=v2",
        "-z",
        "--branch",
        "--ignored=traditional",
        "--untracked-files=normal",
    ]);
    cmd
}

/// `git rev-parse --show-prefix`: the directory's path relative to the repository root.
pub fn prefix_command(dir: &Path) -> Command {
    let mut cmd = git(dir);
    cmd.args(["rev-parse", "--show-prefix"]);
    cmd
}

/// The one hardened `git` builder every invocation in Kestrel goes through: no optional
/// locks, no fsmonitor, no pager, no prompts, no console window. Callers add
/// `--no-ext-diff --no-textconv` (diff) and their own arguments.
pub fn git_command(dir: &Path) -> Command {
    git(dir)
}

fn git(dir: &Path) -> Command {
    let mut cmd = Command::new("git");
    // A background refresh must never take `index.lock` and fight the user's own git
    // commands over it.
    cmd.arg("--no-optional-locks");
    cmd.arg("--no-pager");
    // The directory may be an untrusted repository (an extracted archive, a download): its
    // `.git/config` must not get a program executed just because it was browsed. The
    // fsmonitor hook is the one executable key `status` consults; propagating `-c` also
    // covers submodule status.
    cmd.args([
        "-c",
        "core.fsmonitor=false",
        "-c",
        "core.untrackedCache=false",
        "-c",
        "protocol.ext.allow=never",
        "-c",
        "status.submoduleSummary=false",
    ]);
    cmd.arg("-C").arg(dir);
    cmd.env("GIT_OPTIONAL_LOCKS", "0");
    // Never wait for a credential prompt on a background refresh.
    cmd.env("GIT_TERMINAL_PROMPT", "0");
    cmd.env("GCM_INTERACTIVE", "never");
    cmd.env("GIT_PAGER", "cat");
    hide_console_window(&mut cmd);
    cmd
}

/// Without this a console window flashes for every `git` call when `ks` runs from a GUI
/// terminal host.
#[cfg(windows)]
fn hide_console_window(cmd: &mut Command) {
    use std::os::windows::process::CommandExt;
    const CREATE_NO_WINDOW: u32 = 0x0800_0000;
    cmd.creation_flags(CREATE_NO_WINDOW);
}

#[cfg(not(windows))]
fn hide_console_window(_cmd: &mut Command) {}

/// Runs git for `dir`. `None` when `dir` is not inside a work tree, git is not installed, or
/// anything else goes wrong (logged at debug level; git status is a nicety, never an error).
/// Dropping the future kills the child processes.
pub async fn read_status(dir: &Path) -> Option<GitSnapshot> {
    let mut prefix = tokio::process::Command::from(prefix_command(dir));
    let mut status = tokio::process::Command::from(status_command(dir));
    prefix.kill_on_drop(true);
    status.kill_on_drop(true);
    let (prefix, status) = tokio::join!(prefix.output(), status.output());
    let (prefix, status) = match (prefix, status) {
        (Ok(prefix), Ok(status)) => (prefix, status),
        (Err(e), _) | (_, Err(e)) => {
            tracing::debug!(error = %e, "cannot run git");
            return None;
        }
    };
    if !prefix.status.success() || !status.status.success() {
        tracing::debug!(dir = %dir.display(), "not inside a git work tree");
        return None;
    }
    Some(parse_status(&status.stdout, &parse_prefix(&prefix.stdout)))
}

/// Parses the output of `rev-parse --show-prefix` (`"sub/dir/\n"`, or `"\n"` at the root).
pub fn parse_prefix(output: &[u8]) -> String {
    String::from_utf8_lossy(output)
        .trim_end_matches(['\n', '\r'])
        .to_owned()
}

/// Parses `git status --porcelain=v2 -z --branch` output for the directory `prefix`
/// (see [`GitSnapshot::new`]). Malformed records are skipped.
pub fn parse_status(output: &[u8], prefix: &str) -> GitSnapshot {
    let mut branch = GitBranch::default();
    let mut counts = GitCounts::default();
    let mut records: Vec<GitRecord> = Vec::new();

    let mut fields = output.split(|&b| b == 0).filter(|f| !f.is_empty());
    while let Some(raw) = fields.next() {
        let line = String::from_utf8_lossy(raw);
        let Some((tag, rest)) = line.split_once(' ') else {
            continue;
        };
        match tag {
            "#" => parse_header(rest, &mut branch),
            "1" => {
                if let Some((xy, path)) = split_ordinary(rest) {
                    record_change(xy, path, &mut counts, &mut records);
                }
            }
            "2" => {
                // The original path follows as a record of its own; it is not needed.
                let _original = fields.next();
                if let Some((xy, path)) = split_renamed(rest) {
                    record_change(xy, path, &mut counts, &mut records);
                }
            }
            "u" => {
                if let Some(path) = last_field(rest, 10) {
                    counts.conflicted += 1;
                    records.push(GitRecord {
                        path: path.to_owned(),
                        state: GitState::Conflict,
                    });
                }
            }
            "?" => {
                counts.untracked += 1;
                records.push(GitRecord {
                    path: rest.to_owned(),
                    state: GitState::Untracked,
                });
            }
            "!" => records.push(GitRecord {
                path: rest.to_owned(),
                state: GitState::Ignored,
            }),
            _ => {}
        }
    }
    GitSnapshot::new(branch, counts, prefix, records)
}

fn parse_header(rest: &str, branch: &mut GitBranch) {
    let Some((key, value)) = rest.split_once(' ') else {
        return;
    };
    match key {
        "branch.oid" => {
            branch.oid = (value != "(initial)").then(|| value.chars().take(7).collect());
        }
        "branch.head" => {
            branch.name = (value != "(detached)").then(|| value.to_owned());
        }
        "branch.upstream" => branch.upstream = Some(value.to_owned()),
        "branch.ab" => {
            // "+<ahead> -<behind>"
            for part in value.split(' ') {
                if let Some(n) = part.strip_prefix('+') {
                    branch.ahead = n.parse().unwrap_or(0);
                } else if let Some(n) = part.strip_prefix('-') {
                    branch.behind = n.parse().unwrap_or(0);
                }
            }
        }
        _ => {}
    }
}

/// The last of `total` space-separated fields. It is the path, which may itself contain
/// spaces, so the split stops after `total - 1` separators. `None` if there are fewer fields.
fn last_field(rest: &str, total: usize) -> Option<&str> {
    rest.splitn(total, ' ').nth(total - 1)
}

/// `rest` is everything after the leading `1 `: `XY sub mH mI mW hH hI path`.
fn split_ordinary(rest: &str) -> Option<(&str, &str)> {
    let xy = rest.split(' ').next()?;
    Some((xy, last_field(rest, 8)?))
}

/// `rest` is everything after `2 `: `XY sub mH mI mW hH hI Xscore path`.
fn split_renamed(rest: &str) -> Option<(&str, &str)> {
    let xy = rest.split(' ').next()?;
    Some((xy, last_field(rest, 9)?))
}

fn record_change(xy: &str, path: &str, counts: &mut GitCounts, records: &mut Vec<GitRecord>) {
    let mut chars = xy.chars();
    let (Some(x), Some(y)) = (chars.next(), chars.next()) else {
        return;
    };
    if x != '.' {
        counts.staged += 1;
    }
    if y != '.' {
        counts.modified += 1;
    }
    let state = [state_of(x), state_of(y)].into_iter().flatten().max();
    if let Some(state) = state {
        records.push(GitRecord {
            path: path.to_owned(),
            state,
        });
    }
}

/// One side of `XY`.
fn state_of(code: char) -> Option<GitState> {
    match code {
        'M' | 'T' => Some(GitState::Modified),
        'A' => Some(GitState::Added),
        'D' => Some(GitState::Deleted),
        'R' | 'C' => Some(GitState::Renamed),
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    #![allow(clippy::print_stderr)]

    use super::*;

    #[test]
    fn the_shared_builder_is_hardened() {
        let cmd = git_command(Path::new("somewhere"));
        let args: Vec<_> = cmd
            .get_args()
            .map(|a| a.to_string_lossy().into_owned())
            .collect();
        for want in ["--no-pager", "--no-optional-locks", "core.fsmonitor=false"] {
            assert!(args.iter().any(|a| a == want), "missing {want}: {args:?}");
        }
        let envs: Vec<_> = cmd
            .get_envs()
            .map(|(k, v)| {
                (
                    k.to_string_lossy().into_owned(),
                    v.map(|v| v.to_string_lossy().into_owned()),
                )
            })
            .collect();
        for (k, v) in [("GIT_TERMINAL_PROMPT", "0"), ("GIT_PAGER", "cat")] {
            assert!(
                envs.contains(&(k.into(), Some(v.into()))),
                "missing {k}: {envs:?}"
            );
        }
    }

    /// Joins records the way `git status -z` does.
    fn nul(records: &[&str]) -> Vec<u8> {
        let mut out = Vec::new();
        for record in records {
            out.extend_from_slice(record.as_bytes());
            out.push(0);
        }
        out
    }

    const HASH: &str = "1111111111111111111111111111111111111111";

    fn ordinary(xy: &str, path: &str) -> String {
        format!("1 {xy} N... 100644 100644 100644 {HASH} {HASH} {path}")
    }

    #[test]
    fn branch_headers() {
        let out = nul(&[
            "# branch.oid 0123456789abcdef0123456789abcdef01234567",
            "# branch.head main",
            "# branch.upstream origin/main",
            "# branch.ab +3 -2",
        ]);
        let snap = parse_status(&out, "");
        assert_eq!(snap.branch.name.as_deref(), Some("main"));
        assert_eq!(snap.branch.oid.as_deref(), Some("0123456"));
        assert_eq!(snap.branch.upstream.as_deref(), Some("origin/main"));
        assert_eq!((snap.branch.ahead, snap.branch.behind), (3, 2));
        assert!(snap.counts.is_clean());
    }

    #[test]
    fn detached_head_and_initial_commit() {
        let out = nul(&["# branch.oid (initial)", "# branch.head (detached)"]);
        let snap = parse_status(&out, "");
        assert_eq!(snap.branch.name, None);
        assert_eq!(snap.branch.oid, None);
        assert_eq!(snap.branch.label(), "(detached)");

        let out = nul(&["# branch.oid (initial)", "# branch.head main"]);
        let snap = parse_status(&out, "");
        assert_eq!(snap.branch.label(), "main");

        let out = nul(&[
            "# branch.oid abcdef1234567890abcdef1234567890abcdef12",
            "# branch.head (detached)",
        ]);
        assert_eq!(parse_status(&out, "").branch.label(), "(abcdef1)");
    }

    #[test]
    fn ordinary_changes_and_their_counts() {
        let out = nul(&[
            "# branch.head main",
            &ordinary(".M", "worktree.rs"),
            &ordinary("M.", "staged.rs"),
            &ordinary("A.", "added.rs"),
            &ordinary(".D", "deleted.rs"),
            &ordinary("AM", "both.rs"),
        ]);
        let snap = parse_status(&out, "");
        assert_eq!(snap.state_of("worktree.rs"), Some(GitState::Modified));
        assert_eq!(snap.state_of("staged.rs"), Some(GitState::Modified));
        assert_eq!(snap.state_of("added.rs"), Some(GitState::Added));
        assert_eq!(snap.state_of("deleted.rs"), Some(GitState::Deleted));
        assert_eq!(snap.state_of("both.rs"), Some(GitState::Modified));
        assert_eq!(snap.state_of("clean.rs"), None);
        assert_eq!(
            snap.counts,
            GitCounts {
                staged: 3,
                modified: 3,
                untracked: 0,
                conflicted: 0
            }
        );
    }

    #[test]
    fn paths_with_spaces_are_kept_whole() {
        let out = nul(&[&ordinary(".M", "my docs/read me.txt")]);
        let snap = parse_status(&out, "");
        assert_eq!(snap.state_of("my docs"), Some(GitState::Modified));
        let snap = parse_status(&out, "my docs/");
        assert_eq!(snap.state_of("read me.txt"), Some(GitState::Modified));
    }

    #[test]
    fn renamed_entries_consume_the_extra_path_record() {
        let out = nul(&[
            &format!("2 R. N... 100644 100644 100644 {HASH} {HASH} R100 new name.rs"),
            "old name.rs",
            &ordinary(".M", "after.rs"),
        ]);
        let snap = parse_status(&out, "");
        assert_eq!(snap.state_of("new name.rs"), Some(GitState::Renamed));
        // The original path is not a change of its own.
        assert_eq!(snap.state_of("old name.rs"), None);
        // And the record after it is not swallowed.
        assert_eq!(snap.state_of("after.rs"), Some(GitState::Modified));
        assert_eq!(snap.counts.staged, 1);
        assert_eq!(snap.counts.modified, 1);
    }

    #[test]
    fn copies_count_as_renames() {
        let out = nul(&[
            &format!("2 C. N... 100644 100644 100644 {HASH} {HASH} C75 copy.rs"),
            "orig.rs",
        ]);
        assert_eq!(
            parse_status(&out, "").state_of("copy.rs"),
            Some(GitState::Renamed)
        );
    }

    #[test]
    fn unmerged_entries_are_conflicts() {
        let out = nul(&[
            &format!(
                "u UU N... 100644 100644 100644 100644 {HASH} {HASH} {HASH} src/conflict me.rs"
            ),
            &ordinary(".M", "src/other.rs"),
        ]);
        let snap = parse_status(&out, "");
        assert_eq!(snap.state_of("src"), Some(GitState::Conflict));
        let inside = parse_status(&out, "src/");
        assert_eq!(inside.state_of("conflict me.rs"), Some(GitState::Conflict));
        assert_eq!(inside.state_of("other.rs"), Some(GitState::Modified));
        assert_eq!(snap.counts.conflicted, 1);
    }

    #[test]
    fn untracked_and_ignored() {
        let out = nul(&[
            "? scratch.txt",
            "? newdir/",
            "! target/",
            "! debug.log",
            &ordinary(".M", "tracked.rs"),
        ]);
        let snap = parse_status(&out, "");
        assert_eq!(snap.state_of("scratch.txt"), Some(GitState::Untracked));
        assert_eq!(snap.state_of("newdir"), Some(GitState::Untracked));
        assert_eq!(snap.state_of("target"), Some(GitState::Ignored));
        assert_eq!(snap.state_of("debug.log"), Some(GitState::Ignored));
        assert_eq!(snap.counts.untracked, 2, "ignored paths are not counted");
    }

    #[test]
    fn a_directory_inside_an_ignored_directory_inherits_it() {
        let out = nul(&["! node_modules/"]);
        let snap = parse_status(&out, "node_modules/left-pad/");
        assert_eq!(snap.state_of("index.js"), Some(GitState::Ignored));
    }

    #[test]
    fn non_utf8_paths_and_garbage_do_not_break_parsing() {
        let mut out = nul(&[
            "# branch.head main",
            "garbage",
            "x",
            "1 .M",
            &ordinary(".M", "ok.rs"),
        ]);
        out.extend_from_slice(b"? bad\xff\xfename\0");
        let snap = parse_status(&out, "");
        assert_eq!(snap.state_of("ok.rs"), Some(GitState::Modified));
        assert_eq!(snap.counts.untracked, 1);
        assert!(parse_status(b"", "").counts.is_clean());
        assert!(parse_status(b"\0\0\0", "").counts.is_clean());
    }

    #[test]
    fn prefix_output() {
        assert_eq!(parse_prefix(b"\n"), "");
        assert_eq!(parse_prefix(b"src/ui/\n"), "src/ui/");
        assert_eq!(parse_prefix(b"src/\r\n"), "src/");
        assert_eq!(parse_prefix(b""), "");
    }

    #[test]
    fn commands_are_quiet_and_lock_free() {
        let cmd = status_command(Path::new("/some/dir"));
        let args: Vec<_> = cmd
            .get_args()
            .map(|a| a.to_string_lossy().into_owned())
            .collect();
        assert_eq!(args[0], "--no-optional-locks");
        assert!(args.windows(2).any(|w| w == ["-C", "/some/dir"]));
        for key in ["core.fsmonitor=false", "core.untrackedCache=false"] {
            assert!(args.contains(&key.to_owned()), "{key} must be forced");
        }
        // Config overrides come before the subcommand.
        let sub = args.iter().position(|a| a == "status").unwrap();
        assert!(args.iter().rposition(|a| a == "-c").unwrap() < sub);
        let env = |name: &str| {
            cmd.get_envs()
                .find(|(k, _)| *k == name)
                .and_then(|(_, v)| v)
                .and_then(|v| v.to_str().map(str::to_owned))
        };
        assert_eq!(env("GIT_TERMINAL_PROMPT").as_deref(), Some("0"));
        assert!(args.contains(&"--porcelain=v2".to_owned()));
        assert!(args.contains(&"-z".to_owned()));
        assert!(args.contains(&"--branch".to_owned()));
        let locks = cmd
            .get_envs()
            .find(|(k, _)| *k == "GIT_OPTIONAL_LOCKS")
            .and_then(|(_, v)| v);
        assert_eq!(locks.and_then(|v| v.to_str()), Some("0"));
    }

    fn git_available() -> bool {
        Command::new("git")
            .arg("--version")
            .output()
            .is_ok_and(|o| o.status.success())
    }

    fn run_git(dir: &Path, args: &[&str]) {
        let status = Command::new("git")
            .arg("-C")
            .arg(dir)
            .args(["-c", "user.name=t", "-c", "user.email=t@example.com"])
            .args(["-c", "commit.gpgsign=false", "-c", "core.autocrlf=false"])
            .args(args)
            .output()
            .unwrap()
            .status;
        assert!(status.success(), "git {args:?} failed");
    }

    #[tokio::test]
    async fn against_a_real_repository() {
        if !git_available() {
            eprintln!("git not installed; skipping");
            return;
        }
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path();
        run_git(root, &["init", "-q", "-b", "main"]);
        std::fs::write(root.join(".gitignore"), "ignored.log\nbuild/\n").unwrap();
        std::fs::create_dir(root.join("src")).unwrap();
        std::fs::write(root.join("src/lib.rs"), "fn a() {}\n").unwrap();
        std::fs::write(root.join("kept.txt"), "k\n").unwrap();
        run_git(root, &["add", "."]);
        run_git(root, &["commit", "-q", "-m", "init"]);

        std::fs::write(root.join("src/lib.rs"), "fn b() {}\n").unwrap();
        std::fs::write(root.join("new file.txt"), "n\n").unwrap();
        std::fs::write(root.join("staged.txt"), "s\n").unwrap();
        run_git(root, &["add", "staged.txt"]);
        std::fs::write(root.join("ignored.log"), "i\n").unwrap();
        std::fs::create_dir(root.join("build")).unwrap();
        std::fs::write(root.join("build/out.o"), "o\n").unwrap();

        let snap = read_status(root).await.expect("a repository");
        assert_eq!(snap.branch.name.as_deref(), Some("main"));
        assert_eq!(snap.state_of("src"), Some(GitState::Modified));
        assert_eq!(snap.state_of("new file.txt"), Some(GitState::Untracked));
        assert_eq!(snap.state_of("staged.txt"), Some(GitState::Added));
        assert_eq!(snap.state_of("ignored.log"), Some(GitState::Ignored));
        assert_eq!(snap.state_of("build"), Some(GitState::Ignored));
        assert_eq!(snap.state_of("kept.txt"), None);
        assert_eq!(snap.counts.untracked, 1);
        assert_eq!(snap.counts.staged, 1);
        assert_eq!(snap.counts.modified, 1);

        // From a subdirectory, the prefix makes paths relative to that directory.
        let sub = read_status(&root.join("src")).await.expect("a repository");
        assert_eq!(sub.state_of("lib.rs"), Some(GitState::Modified));

        // Outside any repository there is simply no status.
        let outside = tempfile::tempdir().unwrap();
        assert!(read_status(outside.path()).await.is_none());
    }

    #[tokio::test]
    async fn repo_configured_fsmonitor_is_never_executed() {
        if !git_available() {
            eprintln!("git not installed; skipping");
            return;
        }
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path().join("repo");
        std::fs::create_dir(&root).unwrap();
        run_git(&root, &["init", "-q", "-b", "main"]);
        std::fs::write(
            root.join("a.txt"),
            "a
",
        )
        .unwrap();
        let marker = dir.path().join("marker-ran");
        let marker_text = marker.to_string_lossy().replace(char::from(92), "/");
        // The fsmonitor value is run through the shell; it records that it ran.
        let hook = format!("echo ran > \"{marker_text}\"; echo");
        run_git(&root, &["config", "core.fsmonitor", &hook]);

        // Control: plain `git status` does run the hook, so the assertion below means something.
        let control = Command::new("git")
            .arg("-C")
            .arg(&root)
            .args(["status", "--porcelain"])
            .output()
            .unwrap();
        let hook_fires = marker.exists();
        eprintln!(
            "control status ok={} hook fired={hook_fires}",
            control.status.success()
        );
        let _ = std::fs::remove_file(&marker);

        let snap = read_status(&root).await;
        assert!(snap.is_some(), "status still works");
        assert!(
            !marker.exists(),
            "browsing a repo must not run its fsmonitor"
        );
        assert!(
            hook_fires,
            "control: git did not run the hook, test is vacuous"
        );
    }
}
