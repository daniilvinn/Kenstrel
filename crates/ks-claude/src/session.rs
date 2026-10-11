//! Session id persistence per project root (10b "Resume last Claude session").
//!
//! A tiny JSON map `{ "<normalised project root>": "<uuid>" }` stored at a caller-injected path.
//! Nothing credential-like is stored: only session ids that `claude --session-id` already wrote
//! into its own transcript store.
//!
//! A stored id is only *resumed* once Claude has written a transcript for it
//! ([`transcript_exists`]); an id whose session was closed before the first message is reused as
//! a new session instead, so `claude --resume <id>` is never run for a conversation that does
//! not exist.

use std::collections::BTreeMap;
use std::io;
use std::path::{Path, PathBuf};

/// Whether to start a fresh session or resume one.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SessionMode {
    /// `--session-id <uuid>`: new session with the given id.
    New(String),
    /// `--resume <uuid>`.
    Resume(String),
}

impl SessionMode {
    /// CLI arguments for `claude`.
    pub fn args(&self) -> Vec<String> {
        match self {
            SessionMode::New(id) => vec!["--session-id".into(), id.clone()],
            SessionMode::Resume(id) => vec!["--resume".into(), id.clone()],
        }
    }
}

/// A fresh random session id (UUID v4, the format `--session-id` requires).
pub fn new_session_id() -> String {
    uuid::Uuid::new_v4().to_string()
}

/// True if `s` is a syntactically valid UUID (guards `--resume` against corrupted store files).
pub fn is_valid_session_id(s: &str) -> bool {
    uuid::Uuid::parse_str(s).is_ok()
}

/// Normalise a project root into a map key (trailing separators removed, `/` -> `\`, lower-cased
/// on Windows where paths are case-insensitive).
pub fn project_key(root: &Path) -> String {
    let s = root.to_string_lossy().replace('/', "\\");
    let s = s.trim_end_matches('\\').to_string();
    if cfg!(windows) {
        s.to_lowercase()
    } else {
        s
    }
}

/// Directory where Claude Code keeps its transcripts: `$CLAUDE_CONFIG_DIR/projects`, else
/// `~/.claude/projects` (the `projectsDirectory` reported by `claude auth status`).
pub fn default_projects_dir() -> Option<PathBuf> {
    if let Some(d) = std::env::var_os("CLAUDE_CONFIG_DIR").filter(|d| !d.is_empty()) {
        return Some(PathBuf::from(d).join("projects"));
    }
    let home = std::env::var_os("USERPROFILE").or_else(|| std::env::var_os("HOME"))?;
    Some(PathBuf::from(home).join(".claude").join("projects"))
}

/// Whether Claude has written a transcript `<id>.jsonl` in any project directory below
/// `projects_dir`. Claude only creates it after the first message, so a session that was opened
/// and closed without a prompt has none and `--resume` would fail for it.
pub fn transcript_exists(projects_dir: &Path, id: &str) -> bool {
    let file = format!("{id}.jsonl");
    let Ok(entries) = std::fs::read_dir(projects_dir) else {
        return false;
    };
    entries.flatten().any(|e| e.path().join(&file).is_file())
}

/// Name of the directory Claude uses for `cwd` below `projects`: every character that is not an
/// ASCII letter or digit becomes `-` (`C:\Users\me\app` -> `C--Users-me-app`).
pub fn project_dir_name(cwd: &Path) -> String {
    let s = cwd.to_string_lossy();
    let s = s.strip_prefix(r"\\?\").unwrap_or(&s);
    s.chars()
        .map(|c| if c.is_ascii_alphanumeric() { c } else { '-' })
        .collect()
}

/// Like [`transcript_exists`], but looks only in the project directory of `cwd`: a resume only
/// works from the cwd the session was created in, so a transcript elsewhere does not count
/// (there is deliberately no scan-all fallback).
pub fn transcript_exists_for(projects_dir: &Path, cwd: &Path, id: &str) -> bool {
    projects_dir
        .join(project_dir_name(cwd))
        .join(format!("{id}.jsonl"))
        .is_file()
}

/// File-backed store.
#[derive(Debug, Clone)]
pub struct SessionStore {
    path: PathBuf,
    projects_dir: Option<PathBuf>,
}

/// Exclusive lock file next to the store (create-new semantics), released on drop. Stale locks
/// (older than 10 s, e.g. after a crash) are broken. Gives up after about 2 s and proceeds
/// without the lock rather than failing the launch.
struct StoreLock(Option<PathBuf>);

impl StoreLock {
    fn acquire(store: &Path) -> Self {
        let lock = store.with_extension("json.lock");
        for _ in 0..600 {
            match std::fs::OpenOptions::new()
                .write(true)
                .create_new(true)
                .open(&lock)
            {
                Ok(_) => return StoreLock(Some(lock)),
                // On Windows a lock file that is being deleted reports PermissionDenied for a
                // moment: that is contention too, not a reason to go on unlocked.
                Err(e)
                    if matches!(
                        e.kind(),
                        io::ErrorKind::AlreadyExists | io::ErrorKind::PermissionDenied
                    ) =>
                {
                    let stale = std::fs::metadata(&lock)
                        .and_then(|m| m.modified())
                        .ok()
                        .and_then(|t| t.elapsed().ok())
                        .is_some_and(|age| age > std::time::Duration::from_secs(10));
                    if stale {
                        let _ = std::fs::remove_file(&lock);
                    } else {
                        std::thread::sleep(std::time::Duration::from_millis(10));
                    }
                }
                Err(_) => break,
            }
        }
        StoreLock(None)
    }
}

impl Drop for StoreLock {
    fn drop(&mut self) {
        if let Some(p) = self.0.take() {
            let _ = std::fs::remove_file(p);
        }
    }
}

impl SessionStore {
    /// Store backed by `path` (parent directories are created on save). Transcript checks use
    /// [`default_projects_dir`].
    pub fn new(path: impl Into<PathBuf>) -> Self {
        Self {
            path: path.into(),
            projects_dir: default_projects_dir(),
        }
    }

    /// Override the transcript directory used by [`SessionStore::mode_for`] (`None` disables the
    /// check, so any stored id is resumed).
    pub fn with_projects_dir(mut self, dir: Option<PathBuf>) -> Self {
        self.projects_dir = dir;
        self
    }

    /// Load the map. A corrupt file is moved aside to `<file>.corrupt` (so the next save does not
    /// silently destroy it) and treated as empty.
    fn load(&self) -> BTreeMap<String, String> {
        // A read can fail for a moment on Windows while another process replaces the file
        // (sharing violation); an empty map here would make the next save drop every entry.
        let mut attempt = 0;
        let text = loop {
            match std::fs::read_to_string(&self.path) {
                Ok(text) => break text,
                Err(e) if e.kind() == io::ErrorKind::NotFound => return BTreeMap::new(),
                Err(_) if attempt < 20 => {
                    attempt += 1;
                    std::thread::sleep(std::time::Duration::from_millis(10));
                }
                Err(_) => return BTreeMap::new(),
            }
        };
        match serde_json::from_str(&text) {
            Ok(map) => map,
            Err(e) => {
                tracing::warn!("claude session store is corrupt ({e}); moving it aside");
                let _ = std::fs::rename(&self.path, self.path.with_extension("json.corrupt"));
                BTreeMap::new()
            }
        }
    }

    fn save(&self, map: &BTreeMap<String, String>) -> io::Result<()> {
        if let Some(parent) = self.path.parent() {
            std::fs::create_dir_all(parent)?;
        }
        let tmp = self.path.with_extension(format!(
            "json.{}.{}.tmp",
            std::process::id(),
            uuid::Uuid::new_v4().simple()
        ));
        std::fs::write(
            &tmp,
            serde_json::to_vec_pretty(map).map_err(io::Error::other)?,
        )?;
        // On Windows the rename fails for a moment while a reader has the target open.
        let mut attempt = 0;
        loop {
            match std::fs::rename(&tmp, &self.path) {
                Err(_) if attempt < 20 => {
                    attempt += 1;
                    std::thread::sleep(std::time::Duration::from_millis(10));
                }
                Err(e) => {
                    let _ = std::fs::remove_file(&tmp);
                    return Err(e);
                }
                Ok(()) => return Ok(()),
            }
        }
    }

    /// Last session id for a project root, if valid.
    pub fn get(&self, root: &Path) -> Option<String> {
        self.load()
            .remove(&project_key(root))
            .filter(|id| is_valid_session_id(id))
    }

    /// Remember `id` for `root` (locked load-modify-write, atomic replace).
    pub fn set(&self, root: &Path, id: &str) -> io::Result<()> {
        if let Some(parent) = self.path.parent() {
            std::fs::create_dir_all(parent)?;
        }
        let _lock = StoreLock::acquire(&self.path);
        let mut map = self.load();
        map.remove(&format!("{}{CWD_SUFFIX}", project_key(root)));
        map.insert(project_key(root), id.to_string());
        self.save(&map)
    }

    /// Forget the session for `root` (same locked, atomic write as [`SessionStore::set`]). Call it
    /// when `--resume` reported that the conversation does not exist.
    pub fn forget(&self, root: &Path) -> io::Result<()> {
        let _lock = StoreLock::acquire(&self.path);
        let mut map = self.load();
        let key = project_key(root);
        let removed = map.remove(&key).is_some();
        let removed_confirmed = map.remove(&format!("{key}{CONFIRMED_SUFFIX}")).is_some();
        map.remove(&format!("{key}{CWD_SUFFIX}"));
        map.remove(&format!("{key}{CONFIRMED_SUFFIX}{CWD_SUFFIX}"));
        if removed || removed_confirmed {
            self.save(&map)?;
        }
        Ok(())
    }

    /// Mode for launching in `root`.
    ///
    /// * `resume` and a stored id **with a transcript on disk**: [`SessionMode::Resume`].
    /// * `resume` and a stored id **without** a transcript (the previous session was closed before
    ///   the first message, or never started): the last *confirmed* id (one that had a transcript
    ///   when it was replaced) is resumed if there is one, otherwise [`SessionMode::New`] reuses
    ///   the stored id, since Claude never saw it.
    /// * Otherwise (a fresh start): the stored id is reused if Claude never saw it (it is still
    ///   unused), else a fresh id is stored and the old, transcript-backed id is kept as the
    ///   confirmed one, so a fresh session that is closed unused cannot lose the conversation.
    pub fn mode_for(&self, root: &Path, resume: bool) -> io::Result<SessionMode> {
        self.mode_for_in(root, root, resume)
    }

    /// [`Self::launch_for`] reduced to its mode.
    pub fn mode_for_in(&self, root: &Path, cwd: &Path, resume: bool) -> io::Result<SessionMode> {
        self.launch_for(root, cwd, resume).map(|l| l.mode)
    }

    /// Like [`Self::mode_for`] for a request made from `cwd` (inside the project `root`, the
    /// store key). Claude keeps transcripts per *launch* directory and `--resume` only finds
    /// them from there, so each stored id remembers the directory it was created in: the
    /// transcript is looked up there and the returned [`SessionLaunch::cwd`] is where the
    /// process must be started (it differs from `cwd` when an existing conversation was begun
    /// elsewhere in the project). A new id is bound to `cwd`.
    pub fn launch_for(&self, root: &Path, cwd: &Path, resume: bool) -> io::Result<SessionLaunch> {
        if let Some(parent) = self.path.parent() {
            std::fs::create_dir_all(parent)?;
        }
        let _lock = StoreLock::acquire(&self.path);
        let mut map = self.load();
        let key = project_key(root);
        let confirmed_key = format!("{key}{CONFIRMED_SUFFIX}");
        let valid = |map: &BTreeMap<String, String>, k: &str| {
            map.get(k).filter(|id| is_valid_session_id(id)).cloned()
        };
        // Directory a slot was created in: the stored one while it still exists (entries from
        // before this was recorded, and vanished directories, use the requested `cwd`).
        let slot_cwd = |map: &BTreeMap<String, String>, k: &str| -> PathBuf {
            map.get(&format!("{k}{CWD_SUFFIX}"))
                .map(PathBuf::from)
                .filter(|p| p.is_dir())
                .unwrap_or_else(|| cwd.to_path_buf())
        };
        let has_transcript = |id: &str, at: &Path| match &self.projects_dir {
            Some(dir) => transcript_exists_for(dir, at, id),
            None => true,
        };
        let current = valid(&map, &key);
        let cur_cwd = slot_cwd(&map, &key);
        let conf_cwd = slot_cwd(&map, &confirmed_key);
        let confirmed = valid(&map, &confirmed_key).filter(|c| has_transcript(c, &conf_cwd));
        let set_cwd = |map: &mut BTreeMap<String, String>, k: &str, at: &Path| {
            map.insert(
                format!("{k}{CWD_SUFFIX}"),
                at.to_string_lossy().into_owned(),
            );
        };
        let promote_confirmed =
            |map: &mut BTreeMap<String, String>, old: String| -> io::Result<SessionLaunch> {
                map.insert(key.clone(), old.clone());
                set_cwd(map, &key, &conf_cwd);
                map.remove(&confirmed_key);
                map.remove(&format!("{confirmed_key}{CWD_SUFFIX}"));
                self.save(map)?;
                Ok(SessionLaunch {
                    mode: SessionMode::Resume(old),
                    cwd: conf_cwd.clone(),
                })
            };
        if let Some(id) = &current {
            if has_transcript(id, &cur_cwd) {
                if resume {
                    return Ok(SessionLaunch {
                        mode: SessionMode::Resume(id.clone()),
                        cwd: cur_cwd,
                    });
                }
                // Fresh start: keep this conversation resumable until the new one exists.
                let fresh = new_session_id();
                map.insert(confirmed_key.clone(), id.clone());
                set_cwd(&mut map, &confirmed_key, &cur_cwd);
                map.insert(key.clone(), fresh.clone());
                set_cwd(&mut map, &key, cwd);
                self.save(&map)?;
                return Ok(SessionLaunch {
                    mode: SessionMode::New(fresh),
                    cwd: cwd.to_path_buf(),
                });
            }
            // The stored id was never used by Claude.
            if let (true, Some(old)) = (resume, confirmed) {
                return promote_confirmed(&mut map, old);
            }
            // A transcript exists but not for the cwd we resolved (an entry from before cwds
            // were recorded, or its directory vanished): the id is used and cannot be
            // located, so it must not be re-issued as `--session-id` elsewhere. Start a fresh
            // conversation instead.
            if self
                .projects_dir
                .as_deref()
                .is_some_and(|dir| transcript_exists(dir, id))
            {
                let fresh = new_session_id();
                map.insert(key.clone(), fresh.clone());
                set_cwd(&mut map, &key, cwd);
                self.save(&map)?;
                return Ok(SessionLaunch {
                    mode: SessionMode::New(fresh),
                    cwd: cwd.to_path_buf(),
                });
            }
            // Never seen by Claude, so it can be bound to wherever it is launched now.
            if map.get(&format!("{key}{CWD_SUFFIX}")).map(String::as_str)
                != Some(cwd.to_string_lossy().as_ref())
            {
                set_cwd(&mut map, &key, cwd);
                self.save(&map)?;
            }
            return Ok(SessionLaunch {
                mode: SessionMode::New(id.clone()),
                cwd: cwd.to_path_buf(),
            });
        }
        // Nothing (valid) stored. A leftover confirmed id is still resumable.
        if let (true, Some(old)) = (resume, confirmed) {
            return promote_confirmed(&mut map, old);
        }
        let fresh = new_session_id();
        map.insert(key.clone(), fresh.clone());
        set_cwd(&mut map, &key, cwd);
        self.save(&map)?;
        Ok(SessionLaunch {
            mode: SessionMode::New(fresh),
            cwd: cwd.to_path_buf(),
        })
    }

    /// Like [`Self::forget`] for a `--resume` that failed after being launched in `launch_cwd`:
    /// only an id that was created there is dropped. A failure from any other directory says
    /// nothing about the conversation (Claude only resumes from its own directory), so it is kept.
    pub fn forget_launched_in(&self, root: &Path, launch_cwd: &Path) -> io::Result<bool> {
        let created_in = {
            let _lock = StoreLock::acquire(&self.path);
            self.load()
                .get(&format!("{}{CWD_SUFFIX}", project_key(root)))
                .cloned()
        };
        if created_in.is_some_and(|c| project_key(Path::new(&c)) != project_key(launch_cwd)) {
            return Ok(false);
        }
        self.forget(root).map(|()| true)
    }
}

/// How to start `claude`: the session mode and the directory to start it in.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SessionLaunch {
    /// `--session-id` / `--resume`.
    pub mode: SessionMode,
    /// Process working directory (the one the session's transcript belongs to).
    pub cwd: PathBuf,
}

/// Suffix appended to a slot's key for the launch directory it was created in.
const CWD_SUFFIX: &str = "\u{1}cwd";

/// Suffix of the map key that holds the last transcript-backed id of a project.
const CONFIRMED_SUFFIX: &str = "\u{1}confirmed";

#[cfg(test)]
mod tests {
    use super::*;

    fn store_at(p: impl Into<PathBuf>) -> SessionStore {
        SessionStore::new(p).with_projects_dir(None)
    }

    #[test]
    fn roundtrip_and_normalisation() {
        let dir = tempfile::tempdir().unwrap();
        let store = store_at(dir.path().join("sub").join("sessions.json"));
        let root = Path::new("E:\\dev\\Kestrel\\");
        assert_eq!(store.get(root), None);
        let id = new_session_id();
        store.set(root, &id).unwrap();
        assert_eq!(store.get(Path::new("E:\\dev\\Kestrel")), Some(id.clone()));
        if cfg!(windows) {
            assert_eq!(store.get(Path::new("e:/dev/kestrel")), Some(id));
        }
    }

    #[test]
    fn corrupted_file_and_invalid_id_ignored() {
        let dir = tempfile::tempdir().unwrap();
        let p = dir.path().join("s.json");
        std::fs::write(&p, "{ not json").unwrap();
        let store = store_at(&p);
        assert_eq!(store.get(Path::new("x")), None);
        store.set(Path::new("x"), "not-a-uuid").unwrap();
        assert_eq!(store.get(Path::new("x")), None);
    }

    #[test]
    fn mode_for_new_then_resume() {
        let dir = tempfile::tempdir().unwrap();
        let store = store_at(dir.path().join("s.json"));
        let root = Path::new("proj");
        let m1 = store.mode_for(root, true).unwrap();
        let SessionMode::New(id) = m1.clone() else {
            panic!("expected New")
        };
        assert_eq!(m1.args(), vec!["--session-id".to_string(), id.clone()]);
        let m2 = store.mode_for(root, true).unwrap();
        assert_eq!(m2, SessionMode::Resume(id.clone()));
        assert_eq!(m2.args(), vec!["--resume".to_string(), id.clone()]);
        let m3 = store.mode_for(root, false).unwrap();
        assert!(matches!(m3, SessionMode::New(ref n) if *n != id));
    }

    #[test]
    fn a_fresh_unused_session_does_not_displace_the_resumable_one() {
        let dir = tempfile::tempdir().unwrap();
        let projects = dir.path().join("projects");
        let store =
            SessionStore::new(dir.path().join("s.json")).with_projects_dir(Some(projects.clone()));
        let root = Path::new("proj");
        let SessionMode::New(a) = store.mode_for(root, false).unwrap() else {
            panic!("expected New");
        };
        // A real conversation happens in session A.
        let proj = projects.join(project_dir_name(root));
        std::fs::create_dir_all(&proj).unwrap();
        std::fs::write(proj.join(format!("{a}.jsonl")), "{}").unwrap();
        // `c` starts a fresh session B that is closed without a prompt; so does a second `c`.
        let SessionMode::New(b) = store.mode_for(root, false).unwrap() else {
            panic!("expected New");
        };
        assert_ne!(a, b);
        assert_eq!(
            store.mode_for(root, false).unwrap(),
            SessionMode::New(b.clone()),
            "an unused id is reused, not churned"
        );
        // `g r` resumes the last real conversation, not the empty B.
        assert_eq!(
            store.mode_for(root, true).unwrap(),
            SessionMode::Resume(a.clone())
        );
        assert_eq!(store.get(root), Some(a.clone()));
        // And forgetting drops both slots.
        store.forget(root).unwrap();
        assert!(matches!(
            store.mode_for(root, true).unwrap(),
            SessionMode::New(n) if n != a && n != b
        ));
    }

    #[test]
    fn forget_removes() {
        let dir = tempfile::tempdir().unwrap();
        let store = store_at(dir.path().join("s.json"));
        store.set(Path::new("p"), &new_session_id()).unwrap();
        store.forget(Path::new("p")).unwrap();
        assert_eq!(store.get(Path::new("p")), None);
    }

    #[test]
    fn transcripts_are_looked_up_in_the_launch_cwds_project_dir() {
        let dir = tempfile::tempdir().unwrap();
        let projects = dir.path().join("projects");
        let id = new_session_id();
        let other = projects.join("D--other-app");
        std::fs::create_dir_all(&other).unwrap();
        std::fs::write(other.join(format!("{id}.jsonl")), "{}").unwrap();
        let mine = Path::new("proj");
        // Another project's transcript never counts, whether or not our dir exists yet.
        assert!(transcript_exists(&projects, &id));
        assert!(!transcript_exists_for(&projects, mine, &id));
        std::fs::create_dir_all(projects.join(project_dir_name(mine))).unwrap();
        assert!(!transcript_exists_for(&projects, mine, &id));
        assert_eq!(
            project_dir_name(Path::new(r"\\?\C:\Users\me\my app")),
            "C--Users-me-my-app"
        );
    }

    #[test]
    fn a_transcript_in_a_subdirectory_counts_for_the_project_root() {
        let dir = tempfile::tempdir().unwrap();
        let projects = dir.path().join("projects");
        let store =
            SessionStore::new(dir.path().join("s.json")).with_projects_dir(Some(projects.clone()));
        let root = Path::new("repo");
        let cwd = Path::new("repo/sub");
        // The root has a project dir of its own from earlier runs.
        std::fs::create_dir_all(projects.join(project_dir_name(root))).unwrap();
        let SessionMode::New(id) = store.mode_for_in(root, cwd, true).unwrap() else {
            panic!("expected New");
        };
        let sub = projects.join(project_dir_name(cwd));
        std::fs::create_dir_all(&sub).unwrap();
        std::fs::write(sub.join(format!("{id}.jsonl")), "{}").unwrap();
        assert_eq!(
            store.mode_for_in(root, cwd, true).unwrap(),
            SessionMode::Resume(id)
        );
    }

    fn dirs(dir: &Path) -> (PathBuf, PathBuf, PathBuf) {
        let root = dir.join("repo");
        let sub = root.join("src");
        std::fs::create_dir_all(&sub).unwrap();
        (dir.join("projects"), root, sub)
    }

    fn write_transcript(projects: &Path, cwd: &Path, id: &str) {
        let d = projects.join(project_dir_name(cwd));
        std::fs::create_dir_all(&d).unwrap();
        std::fs::write(d.join(format!("{id}.jsonl")), "{}").unwrap();
    }

    #[test]
    fn resume_from_a_subdirectory_launches_where_the_session_was_created() {
        let dir = tempfile::tempdir().unwrap();
        let (projects, root, sub) = dirs(dir.path());
        let store =
            SessionStore::new(dir.path().join("s.json")).with_projects_dir(Some(projects.clone()));
        let first = store.launch_for(&root, &root, true).unwrap();
        let SessionMode::New(id) = first.mode else {
            panic!("expected New");
        };
        assert_eq!(first.cwd, root);
        write_transcript(&projects, &root, &id);
        // (1) Browsing in src: the conversation is still resumed, from the root.
        let again = store.launch_for(&root, &sub, true).unwrap();
        assert_eq!(again.mode, SessionMode::Resume(id.clone()));
        assert_eq!(again.cwd, root);
        // (2) Even when src has a project dir of its own, the id is not re-issued as New.
        std::fs::create_dir_all(projects.join(project_dir_name(&sub))).unwrap();
        let again = store.launch_for(&root, &sub, true).unwrap();
        assert_eq!(again.mode, SessionMode::Resume(id.clone()));
        assert_eq!(again.cwd, root);
        // `c` from src keeps the conversation as the confirmed id, launched from the root.
        let fresh = store.launch_for(&root, &sub, false).unwrap();
        let SessionMode::New(new_id) = fresh.mode else {
            panic!("expected New");
        };
        assert_ne!(new_id, id);
        assert_eq!(fresh.cwd, sub);
        let back = store.launch_for(&root, &root, true).unwrap();
        assert_eq!(back.mode, SessionMode::Resume(id));
        assert_eq!(back.cwd, root);
    }

    #[test]
    fn a_legacy_entry_used_elsewhere_is_not_rebound_to_a_new_directory() {
        let dir = tempfile::tempdir().unwrap();
        let (projects, root, sub) = dirs(dir.path());
        let store_path = dir.path().join("s.json");
        let store = SessionStore::new(&store_path).with_projects_dir(Some(projects.clone()));
        // Legacy entry: an id with no cwd record, whose transcript lives in some other dir.
        let id = new_session_id();
        store.set(&root, &id).unwrap();
        write_transcript(&projects, Path::new("elsewhere"), &id);
        let launch = store.launch_for(&root, &sub, true).unwrap();
        let SessionMode::New(fresh) = launch.mode else {
            panic!("expected New");
        };
        assert_ne!(fresh, id, "a used id must not be re-issued as --session-id");
        assert_eq!(launch.cwd, sub);
    }

    #[test]
    fn a_failed_resume_from_another_directory_keeps_the_stored_id() {
        let dir = tempfile::tempdir().unwrap();
        let (projects, root, sub) = dirs(dir.path());
        let store =
            SessionStore::new(dir.path().join("s.json")).with_projects_dir(Some(projects.clone()));
        let SessionMode::New(id) = store.launch_for(&root, &root, true).unwrap().mode else {
            panic!("expected New");
        };
        write_transcript(&projects, &root, &id);
        assert!(!store.forget_launched_in(&root, &sub).unwrap());
        assert_eq!(store.get(&root), Some(id));
        assert!(store.forget_launched_in(&root, &root).unwrap());
        assert_eq!(store.get(&root), None);
    }

    #[test]
    fn resume_requires_a_transcript() {
        let dir = tempfile::tempdir().unwrap();
        let projects = dir.path().join("projects");
        let store =
            SessionStore::new(dir.path().join("s.json")).with_projects_dir(Some(projects.clone()));
        let root = Path::new("proj");
        let SessionMode::New(id) = store.mode_for(root, true).unwrap() else {
            panic!("expected New");
        };
        // Opened and closed without a message: no transcript, so the id is reused as New.
        assert_eq!(
            store.mode_for(root, true).unwrap(),
            SessionMode::New(id.clone())
        );
        // Claude wrote a transcript in some project directory: now it resumes.
        let proj = projects.join(project_dir_name(root));
        std::fs::create_dir_all(&proj).unwrap();
        std::fs::write(proj.join(format!("{id}.jsonl")), "{}").unwrap();
        assert_eq!(store.mode_for(root, true).unwrap(), SessionMode::Resume(id));
    }

    #[test]
    fn corrupt_store_is_kept_aside_and_writes_are_atomic() {
        let dir = tempfile::tempdir().unwrap();
        let p = dir.path().join("s.json");
        std::fs::write(&p, "{ broken").unwrap();
        let store = store_at(&p);
        store.set(Path::new("a"), &new_session_id()).unwrap();
        assert_eq!(
            std::fs::read_to_string(p.with_extension("json.corrupt")).unwrap(),
            "{ broken"
        );
        assert!(store.get(Path::new("a")).is_some());
        let leftovers = std::fs::read_dir(dir.path())
            .unwrap()
            .flatten()
            .filter(|e| e.file_name().to_string_lossy().ends_with(".tmp"))
            .count();
        assert_eq!(leftovers, 0);
    }

    #[test]
    fn concurrent_sets_do_not_lose_entries() {
        let dir = tempfile::tempdir().unwrap();
        let p = dir.path().join("s.json");
        let handles: Vec<_> = (0..8)
            .map(|i| {
                let p = p.clone();
                std::thread::spawn(move || {
                    store_at(p)
                        .set(Path::new(&format!("proj{i}")), &new_session_id())
                        .unwrap();
                })
            })
            .collect();
        for h in handles {
            h.join().unwrap();
        }
        let store = store_at(&p);
        for i in 0..8 {
            assert!(store.get(Path::new(&format!("proj{i}"))).is_some(), "{i}");
        }
    }
}
