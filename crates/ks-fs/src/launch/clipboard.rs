//! Copy paths to the clipboard.

use std::path::{Path, PathBuf};

use crate::ops::paths::{display, relative_to};

/// How a path is rendered when copied.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PathFormat {
    /// Full path (`C:\a\b.txt`, no `\\?\` prefix).
    Absolute,
    /// Relative to the current directory (`sub\b.txt`, `..\x`).
    RelativeToCwd,
    /// File name only.
    Name,
}

/// Render `paths` in `format`, joined with `\n` (no trailing newline).
pub fn format_paths(paths: &[PathBuf], format: PathFormat, cwd: &Path) -> String {
    paths
        .iter()
        .map(|p| match format {
            PathFormat::Absolute => display(p),
            PathFormat::RelativeToCwd => display(&relative_to(p, cwd)),
            PathFormat::Name => p
                .file_name()
                .map(|n| n.to_string_lossy().into_owned())
                .unwrap_or_else(|| display(p)),
        })
        .collect::<Vec<_>>()
        .join("\n")
}

/// Clipboard failure.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
#[error("clipboard: {0}")]
pub struct ClipboardError(pub String);

/// Something text can be copied to. The real implementation is [`SystemClipboard`]; tests use
/// [`MemoryClipboard`].
pub trait Clipboard {
    /// Replace the clipboard text.
    fn set_text(&mut self, text: &str) -> Result<(), ClipboardError>;
}

/// The OS clipboard via `arboard`. Opens the clipboard per call; the content outlives the
/// process on Windows.
#[derive(Debug, Default)]
pub struct SystemClipboard;

impl Clipboard for SystemClipboard {
    fn set_text(&mut self, text: &str) -> Result<(), ClipboardError> {
        let mut cb = arboard::Clipboard::new().map_err(|e| ClipboardError(e.to_string()))?;
        cb.set_text(text.to_owned())
            .map_err(|e| ClipboardError(e.to_string()))
    }
}

/// In-memory clipboard for tests and headless runs.
#[derive(Debug, Default)]
pub struct MemoryClipboard {
    /// Last text set.
    pub text: Option<String>,
}

impl Clipboard for MemoryClipboard {
    fn set_text(&mut self, text: &str) -> Result<(), ClipboardError> {
        self.text = Some(text.to_owned());
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn base() -> PathBuf {
        PathBuf::from(if cfg!(windows) {
            r"C:\work\proj"
        } else {
            "/work/proj"
        })
    }

    #[test]
    fn formats() {
        let b = base();
        let paths = vec![
            b.join("src").join("a.rs"),
            b.parent().unwrap().join("other.txt"),
        ];
        let abs = format_paths(&paths, PathFormat::Absolute, &b);
        assert_eq!(abs.lines().count(), 2);
        assert!(abs.lines().next().unwrap().ends_with("a.rs"));
        let rel = format_paths(&paths, PathFormat::RelativeToCwd, &b);
        let sep = std::path::MAIN_SEPARATOR;
        assert_eq!(rel, format!("src{sep}a.rs\n..{sep}other.txt"));
        assert_eq!(
            format_paths(&paths, PathFormat::Name, &b),
            "a.rs\nother.txt"
        );
        assert_eq!(format_paths(&[], PathFormat::Name, &b), "");
    }

    #[test]
    fn memory_clipboard() {
        let mut c = MemoryClipboard::default();
        c.set_text("x").unwrap();
        assert_eq!(c.text.as_deref(), Some("x"));
    }

    #[cfg(windows)]
    #[test]
    fn strips_verbatim_prefix() {
        let p = vec![PathBuf::from(r"\\?\C:\a\b.txt")];
        assert_eq!(
            format_paths(&p, PathFormat::Absolute, Path::new(r"C:\")),
            r"C:\a\b.txt"
        );
    }
}
