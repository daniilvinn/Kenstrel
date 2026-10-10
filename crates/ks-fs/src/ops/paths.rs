//! Path helpers: absolutizing, `\\?\` verbatim paths for long paths, display and comparison.
//!
//! Rust's `std::fs` already switches to verbatim paths internally for absolute paths longer than
//! `MAX_PATH`, so the engine works with plain absolute paths and only uses [`verbatim`] where a
//! path is handed to something other than `std::fs` (external crates, child processes).

use std::fs::Metadata;
use std::io;
use std::path::{Component, Path, PathBuf};
use std::time::SystemTime;

/// Make `path` absolute without touching the filesystem (no symlink resolution, `..` is
/// collapsed lexically on Windows by `std::path::absolute`).
pub fn absolutize(path: &Path) -> PathBuf {
    std::path::absolute(path).unwrap_or_else(|_| path.to_path_buf())
}

/// Windows: convert an absolute path to its `\\?\` verbatim form (`\\?\UNC\server\share` for UNC
/// paths). Paths that are already verbatim, relative, or any path on other platforms are
/// returned unchanged. Verbatim paths bypass the 260-character limit but also disable `.`/`..`
/// normalisation, so the path is absolutized (and thus normalised) first.
pub fn verbatim(path: &Path) -> PathBuf {
    #[cfg(windows)]
    {
        let abs = absolutize(path);
        let s = abs.to_string_lossy();
        if s.starts_with(r"\\?\") || !abs.is_absolute() {
            return abs;
        }
        if let Some(rest) = s.strip_prefix(r"\\") {
            return PathBuf::from(format!(r"\\?\UNC\{rest}"));
        }
        PathBuf::from(format!(r"\\?\{s}"))
    }
    #[cfg(not(windows))]
    {
        path.to_path_buf()
    }
}

/// User-facing form of a path: strips the `\\?\` prefix when it is not needed.
pub fn display(path: &Path) -> String {
    dunce::simplified(path).to_string_lossy().into_owned()
}

/// Key for comparing paths: simplified and, on Windows, lower-cased (NTFS is case-insensitive).
pub fn path_key(path: &Path) -> String {
    let raw = path.to_string_lossy().replace('/', "\\");
    // Strip verbatim prefixes unconditionally (dunce keeps them for paths over MAX_PATH, and the
    // Recycle Bin reports long paths in verbatim form).
    let s = if let Some(rest) = raw.strip_prefix(r"\\?\UNC\") {
        format!(r"\\{rest}")
    } else if let Some(rest) = raw.strip_prefix(r"\\?\") {
        rest.to_owned()
    } else {
        raw
    };
    let s = s.trim_end_matches('\\').to_owned();
    if cfg!(windows) {
        s.to_lowercase()
    } else {
        s
    }
}

/// `path` relative to `base` (both should be absolute). Falls back to the absolute path when
/// there is no common root (different drives).
pub fn relative_to(path: &Path, base: &Path) -> PathBuf {
    let p: Vec<Component> = dunce::simplified(path).components().collect();
    let b: Vec<Component> = dunce::simplified(base).components().collect();
    let eq = |x: &Component, y: &Component| {
        if cfg!(windows) {
            x.as_os_str().to_string_lossy().to_lowercase()
                == y.as_os_str().to_string_lossy().to_lowercase()
        } else {
            x == y
        }
    };
    let common = p.iter().zip(&b).take_while(|(x, y)| eq(x, y)).count();
    if common == 0 {
        return dunce::simplified(path).to_path_buf();
    }
    let mut out = PathBuf::new();
    for _ in common..b.len() {
        out.push("..");
    }
    for c in &p[common..] {
        out.push(c.as_os_str());
    }
    if out.as_os_str().is_empty() {
        out.push(".");
    }
    out
}

/// A cheap identity of a file used to detect "changed since" in the undo log.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Fingerprint {
    /// Directory flag.
    pub is_dir: bool,
    /// Size in bytes (0 for directories; directory contents are not fingerprinted).
    pub len: u64,
    /// Modification time (`None` for directories and unsupported platforms).
    pub modified: Option<SystemTime>,
}

impl Fingerprint {
    /// Fingerprint of `path` (not following symlinks), or `None` if it does not exist.
    pub fn of(path: &Path) -> Option<Self> {
        let m = std::fs::symlink_metadata(path).ok()?;
        Some(Self::from_metadata(&m))
    }

    /// Fingerprint from existing metadata.
    pub fn from_metadata(m: &Metadata) -> Self {
        if m.is_dir() {
            Self {
                is_dir: true,
                len: 0,
                modified: None,
            }
        } else {
            Self {
                is_dir: false,
                len: m.len(),
                modified: m.modified().ok(),
            }
        }
    }
}

/// Whether the error is "source and destination are on different volumes".
pub(crate) fn is_cross_device(e: &io::Error) -> bool {
    if e.kind() == io::ErrorKind::CrossesDevices {
        return true;
    }
    cfg!(windows) && e.raw_os_error() == Some(17)
}

/// Whether the metadata (from `symlink_metadata`) is a directory-flavoured link
/// (directory symlink or junction on Windows).
pub(crate) fn link_is_dir(m: &Metadata) -> bool {
    #[cfg(windows)]
    {
        use std::os::windows::fs::MetadataExt;
        m.file_attributes() & 0x10 != 0
    }
    #[cfg(not(windows))]
    {
        let _ = m;
        false
    }
}

/// Clear the read-only attribute (Windows only; elsewhere permissions are left alone).
pub(crate) fn clear_readonly(path: &Path) {
    #[cfg(windows)]
    {
        if let Ok(m) = std::fs::symlink_metadata(path) {
            let mut perm = m.permissions();
            if perm.readonly() {
                #[allow(clippy::permissions_set_readonly_false)]
                perm.set_readonly(false);
                let _ = std::fs::set_permissions(path, perm);
            }
        }
    }
    #[cfg(not(windows))]
    {
        let _ = path;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn relative_paths() {
        let base = Path::new(if cfg!(windows) { r"C:\a\b" } else { "/a/b" });
        let p = base.join("c").join("d.txt");
        assert_eq!(relative_to(&p, base), Path::new("c").join("d.txt"));
        let sib = base.parent().unwrap().join("x");
        assert_eq!(relative_to(&sib, base), Path::new("..").join("x"));
        assert_eq!(relative_to(base, base), Path::new("."));
    }

    #[cfg(windows)]
    #[test]
    fn verbatim_forms() {
        assert_eq!(verbatim(Path::new(r"C:\a\b")), Path::new(r"\\?\C:\a\b"));
        assert_eq!(
            verbatim(Path::new(r"\\srv\share\x")),
            Path::new(r"\\?\UNC\srv\share\x")
        );
        assert_eq!(verbatim(Path::new(r"\\?\C:\a")), Path::new(r"\\?\C:\a"));
        assert_eq!(display(Path::new(r"\\?\C:\a")), r"C:\a");
        assert_eq!(path_key(Path::new(r"C:\A\B\")), r"c:\a\b");
        assert_eq!(
            relative_to(Path::new(r"D:\x"), Path::new(r"C:\a")),
            Path::new(r"D:\x")
        );
    }
}
