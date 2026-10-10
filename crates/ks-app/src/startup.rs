//! Start-up and shutdown helpers that do not need a terminal.

use std::fs;
use std::io;
use std::path::{Component, Path, PathBuf};

use anyhow::{bail, Context, Result};
use ks_core::QuitKind;

static PROCESS_START: std::sync::OnceLock<std::time::Instant> = std::sync::OnceLock::new();

/// Records "now" as the start of the process (first call wins). `run` calls it first thing.
pub fn mark_process_start() {
    let _ = PROCESS_START.set(std::time::Instant::now());
}

/// Time since [`mark_process_start`], if it was called.
pub fn since_process_start() -> Option<std::time::Duration> {
    PROCESS_START.get().map(std::time::Instant::elapsed)
}

/// Picks the directory to open: the CLI argument (a file means its parent), or `cwd`.
pub fn resolve_start_dir(arg: Option<&Path>, cwd: &Path) -> Result<PathBuf> {
    let Some(arg) = arg else {
        return Ok(dunce::simplified(cwd).to_owned());
    };
    // Absolute and lexically normalised, but symlinks/junctions/subst drives stay as typed.
    let absolute =
        std::path::absolute(arg).with_context(|| format!("cannot open `{}`", arg.display()))?;
    let resolved = normalise(dunce::simplified(&absolute));
    let meta =
        fs::metadata(&resolved).with_context(|| format!("cannot open `{}`", arg.display()))?;
    if meta.is_dir() {
        return Ok(resolved);
    }
    match resolved.parent() {
        Some(parent) => Ok(parent.to_owned()),
        None => bail!("`{}` has no parent directory", arg.display()),
    }
}

/// Removes `.` and resolves `..` lexically (without touching the file system).
fn normalise(path: &Path) -> PathBuf {
    let mut out = PathBuf::new();
    for component in path.components() {
        match component {
            Component::CurDir => {}
            Component::ParentDir => {
                if !out.pop() {
                    out.push(component);
                }
            }
            other => out.push(other),
        }
    }
    out
}

/// Writes the directory for the shell wrapper (`--cwd-file`): UTF-8, no trailing newline.
/// A path that is not valid Unicode cannot be represented, so it is an error rather than a
/// silently wrong `cd` target.
pub fn write_cwd_file(file: &Path, dir: &Path) -> io::Result<()> {
    let dir = dunce::simplified(dir);
    let text = dir.to_str().ok_or_else(|| {
        io::Error::new(
            io::ErrorKind::InvalidData,
            format!("`{}` is not valid Unicode", dir.display()),
        )
    })?;
    fs::write(file, text.as_bytes())
}

/// What happens after the TUI exits: only `Q` (`QuitKind::ChangeDir`) writes the cwd file,
/// so plain `q` leaves the shell where it was.
pub fn finish(quit: QuitKind, cwd_file: Option<&Path>, cwd: &Path) -> Result<()> {
    if quit != QuitKind::ChangeDir {
        return Ok(());
    }
    if let Some(file) = cwd_file {
        write_cwd_file(file, cwd).with_context(|| format!("cannot write `{}`", file.display()))?;
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn no_argument_uses_the_current_directory() {
        let dir = tempfile::tempdir().unwrap();
        assert_eq!(
            resolve_start_dir(None, dir.path()).unwrap(),
            dunce::simplified(dir.path())
        );
    }

    #[test]
    fn directory_argument_is_made_absolute_and_normalised() {
        let dir = tempfile::tempdir().unwrap();
        let sub = dir.path().join("a");
        fs::create_dir(&sub).unwrap();
        let dotted = sub.join("..").join("a");
        let resolved = resolve_start_dir(Some(&dotted), Path::new("/unused")).unwrap();
        assert_eq!(resolved, dunce::simplified(&sub));
        assert!(!resolved.to_string_lossy().starts_with(r"\\?\"));
    }

    #[test]
    fn file_argument_opens_its_parent() {
        let dir = tempfile::tempdir().unwrap();
        let file = dir.path().join("note.txt");
        fs::write(&file, "x").unwrap();
        let resolved = resolve_start_dir(Some(&file), Path::new("/unused")).unwrap();
        assert_eq!(resolved, dunce::simplified(dir.path()));
    }

    #[test]
    fn missing_path_is_a_clear_error() {
        let dir = tempfile::tempdir().unwrap();
        let err = resolve_start_dir(Some(&dir.path().join("nope")), dir.path()).unwrap_err();
        assert!(err.to_string().contains("cannot open"), "{err:#}");
    }

    #[test]
    fn cwd_file_contains_exactly_the_directory() {
        let dir = tempfile::tempdir().unwrap();
        let out = dir.path().join("cwd.txt");
        write_cwd_file(&out, Path::new("/some/where")).unwrap();
        assert_eq!(fs::read_to_string(&out).unwrap(), "/some/where");
    }

    #[test]
    fn only_quit_and_cd_writes_the_cwd_file() {
        let dir = tempfile::tempdir().unwrap();
        let out = dir.path().join("cwd.txt");
        let cwd = Path::new("/some/where");

        finish(QuitKind::Exit, Some(&out), cwd).unwrap();
        assert!(!out.exists(), "plain quit must not touch the cwd file");

        finish(QuitKind::ChangeDir, None, cwd).unwrap();

        finish(QuitKind::ChangeDir, Some(&out), cwd).unwrap();
        assert_eq!(fs::read_to_string(&out).unwrap(), "/some/where");
    }

    #[test]
    fn finish_reports_unwritable_cwd_files() {
        let dir = tempfile::tempdir().unwrap();
        let out = dir.path().join("missing").join("cwd.txt");
        let err = finish(QuitKind::ChangeDir, Some(&out), Path::new("/x")).unwrap_err();
        assert!(err.to_string().contains("cannot write"), "{err:#}");
    }

    #[cfg(windows)]
    #[test]
    fn non_unicode_directories_are_an_error() {
        use std::os::windows::ffi::OsStringExt;
        let dir = tempfile::tempdir().unwrap();
        let out = dir.path().join("cwd.txt");
        let bad = PathBuf::from(std::ffi::OsString::from_wide(&[0x43, 0x3a, 0x5c, 0xd800]));
        assert!(write_cwd_file(&out, &bad).is_err());
        assert!(!out.exists());
    }

    #[test]
    fn cwd_file_errors_are_reported() {
        let dir = tempfile::tempdir().unwrap();
        let out = dir.path().join("missing-dir").join("cwd.txt");
        assert!(write_cwd_file(&out, Path::new("/x")).is_err());
    }
}
