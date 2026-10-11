//! Search scope roots: cwd, git root, home, drive root (`ctrl-r`).

use std::path::{Component, Path, PathBuf};

/// Scope of a recursive search; `ctrl-r` cycles through these in order.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Scope {
    /// The current directory.
    Cwd,
    /// Nearest ancestor containing `.git` (falls back to cwd).
    GitRoot,
    /// The user's home directory.
    Home,
    /// The drive root of the current directory.
    Drive,
}

impl Scope {
    /// Next scope in the cwd, git root, home, drive cycle.
    pub fn next(self) -> Self {
        match self {
            Scope::Cwd => Scope::GitRoot,
            Scope::GitRoot => Scope::Home,
            Scope::Home => Scope::Drive,
            Scope::Drive => Scope::Cwd,
        }
    }
}

/// Nearest ancestor of `start` (inclusive) that contains a `.git` entry (dir or file).
pub fn find_git_root(start: &Path) -> Option<PathBuf> {
    start
        .ancestors()
        .find(|dir| dir.join(".git").exists())
        .map(Path::to_path_buf)
}

/// The user's home directory (`USERPROFILE` on Windows, `HOME` elsewhere).
pub fn home_dir() -> Option<PathBuf> {
    let var = if cfg!(windows) { "USERPROFILE" } else { "HOME" };
    std::env::var_os(var)
        .filter(|v| !v.is_empty())
        .map(PathBuf::from)
}

/// The root of the drive/filesystem that contains `path` (`C:\` on Windows, `/` elsewhere).
/// Pass an absolute path; for a relative one the result is empty.
pub fn drive_root(path: &Path) -> PathBuf {
    let mut root = PathBuf::new();
    for comp in path.components() {
        match comp {
            Component::Prefix(p) => root.push(p.as_os_str()),
            Component::RootDir => {
                root.push(Component::RootDir.as_os_str());
                break;
            }
            _ => break,
        }
    }
    root
}

/// Resolves a [`Scope`] to a concrete directory for the given cwd. Falls back to `cwd` when
/// the scope cannot be determined (no git repo, no home).
pub fn resolve_scope(scope: Scope, cwd: &Path) -> PathBuf {
    let resolved = match scope {
        Scope::Cwd => None,
        Scope::GitRoot => find_git_root(cwd),
        Scope::Home => home_dir(),
        Scope::Drive => Some(drive_root(cwd)).filter(|p| !p.as_os_str().is_empty()),
    };
    resolved.unwrap_or_else(|| cwd.to_path_buf())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn git_root_found_upward() {
        let t = tempfile::tempdir().unwrap();
        std::fs::create_dir_all(t.path().join(".git")).unwrap();
        let deep = t.path().join("a").join("b");
        std::fs::create_dir_all(&deep).unwrap();
        assert_eq!(find_git_root(&deep).unwrap(), t.path());
        assert_eq!(resolve_scope(Scope::GitRoot, &deep), t.path());
    }

    #[test]
    fn cycle_is_closed() {
        let mut s = Scope::Cwd;
        for _ in 0..4 {
            s = s.next();
        }
        assert_eq!(s, Scope::Cwd);
    }

    #[test]
    fn drive_root_is_prefix_and_root() {
        let t = tempfile::tempdir().unwrap();
        let root = drive_root(t.path());
        assert!(root.is_absolute());
        assert!(t.path().starts_with(&root));
        assert_eq!(root.components().next_back(), Some(Component::RootDir));
    }

    #[test]
    fn cwd_scope_is_identity() {
        let p = Path::new("whatever");
        assert_eq!(resolve_scope(Scope::Cwd, p), p);
    }
}
