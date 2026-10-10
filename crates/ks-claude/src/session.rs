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
        for _ in 0..80 {
            match std::fs::OpenOptions::new()
                .write(true)
                .create_new(true)
                .open(&lock)
            {
                Ok(_) => return StoreLock(Some(lock)),
                Err(e) if e.kind() == io::ErrorKind::AlreadyExists => {
                    let stale = std::fs::metadata(&lock)
                        .and_then(|m| m.modified())
                        .ok()
                        .and_then(|t| t.elapsed().ok())
                        .is_some_and(|age| age > std::time::Duration::from_secs(10));
                    if stale {
                        let _ = std::fs::remove_file(&lock);
                    } else {
                        std::thread::sleep(std::time::Duration::from_millis(25));
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
        let Ok(text) = std::fs::read_to_string(&self.path) else {
            return BTreeMap::new();
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
        std::fs::rename(&tmp, &self.path).inspect_err(|_| {
            let _ = std::fs::remove_file(&tmp);
        })
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
        map.insert(project_key(root), id.to_string());
        self.save(&map)
    }

    /// Forget the session for `root` (same locked, atomic write as [`SessionStore::set`]). Call it
    /// when `--resume` reported that the conversation does not exist.
    pub fn forget(&self, root: &Path) -> io::Result<()> {
        let _lock = StoreLock::acquire(&self.path);
        let mut map = self.load();
        if map.remove(&project_key(root)).is_some() {
            self.save(&map)?;
        }
        Ok(())
    }

    /// Mode for launching in `root`.
    ///
    /// * `resume` and a stored id **with a transcript on disk**: [`SessionMode::Resume`].
    /// * `resume` and a stored id **without** a transcript (the previous session was closed before
    ///   the first message, or never started): [`SessionMode::New`] reusing that id, since Claude
    ///   never saw it.
    /// * Otherwise a fresh id, stored immediately.
    pub fn mode_for(&self, root: &Path, resume: bool) -> io::Result<SessionMode> {
        if resume {
            if let Some(id) = self.get(root) {
                let confirmed = match &self.projects_dir {
                    Some(dir) => transcript_exists(dir, &id),
                    None => true,
                };
                return Ok(if confirmed {
                    SessionMode::Resume(id)
                } else {
                    SessionMode::New(id)
                });
            }
        }
        let id = new_session_id();
        self.set(root, &id)?;
        Ok(SessionMode::New(id))
    }
}

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
    fn forget_removes() {
        let dir = tempfile::tempdir().unwrap();
        let store = store_at(dir.path().join("s.json"));
        store.set(Path::new("p"), &new_session_id()).unwrap();
        store.forget(Path::new("p")).unwrap();
        assert_eq!(store.get(Path::new("p")), None);
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
        let proj = projects.join("E--dev-proj");
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
