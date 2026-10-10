//! Name validation, create-spec parsing, "keep both" naming and delete-confirmation helpers.

use std::path::{Path, PathBuf};

/// Why a file name is not acceptable on Windows.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum NameError {
    /// The name is empty.
    #[error("name is empty")]
    Empty,
    /// The name is `.` or `..`.
    #[error("`{0}` is not a valid name")]
    DotName(String),
    /// A character that Windows forbids in file names.
    #[error("invalid character {0:?}")]
    InvalidChar(char),
    /// A reserved device name such as `CON` or `COM1` (with or without extension).
    #[error("`{0}` is a reserved name")]
    Reserved(String),
    /// Names must not end with a dot or a space.
    #[error("name must not end with a dot or a space")]
    TrailingDotOrSpace,
    /// A single component is longer than 255 UTF-16 units.
    #[error("name is longer than 255 characters")]
    TooLong,
    /// A create spec contained an absolute path, a drive prefix or an empty component.
    #[error("path must be relative and contain no empty components")]
    NotRelative,
}

const RESERVED: &[&str] = &[
    "CON", "PRN", "AUX", "NUL", "COM1", "COM2", "COM3", "COM4", "COM5", "COM6", "COM7", "COM8",
    "COM9", "LPT1", "LPT2", "LPT3", "LPT4", "LPT5", "LPT6", "LPT7", "LPT8", "LPT9",
];

/// Validate a single path component (a file or directory name) against Windows rules.
///
/// Rules apply on every platform so that trees stay portable to Windows.
pub fn validate_name(name: &str) -> Result<(), NameError> {
    if name.is_empty() {
        return Err(NameError::Empty);
    }
    if name == "." || name == ".." {
        return Err(NameError::DotName(name.to_owned()));
    }
    if let Some(c) = name.chars().find(|c| {
        matches!(c, '<' | '>' | ':' | '"' | '/' | '\\' | '|' | '?' | '*') || (*c as u32) < 32
    }) {
        return Err(NameError::InvalidChar(c));
    }
    if name.ends_with('.') || name.ends_with(' ') {
        return Err(NameError::TrailingDotOrSpace);
    }
    if name.encode_utf16().count() > 255 {
        return Err(NameError::TooLong);
    }
    let stem = name.split('.').next().unwrap_or(name).trim_end();
    if RESERVED.iter().any(|r| r.eq_ignore_ascii_case(stem)) {
        return Err(NameError::Reserved(stem.to_owned()));
    }
    Ok(())
}

/// One thing to create: a path and whether it is a directory.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CreateTarget {
    /// Absolute path of the entry to create.
    pub path: PathBuf,
    /// `true` for a directory.
    pub is_dir: bool,
}

/// Parse user input of the `a` prompt relative to `base`.
///
/// A trailing `/` or `\` means directory. Both separators may be used for nesting
/// (`src/new/mod.rs` creates `src/new/` as needed). Every component is validated.
pub fn parse_create_input(base: &Path, input: &str) -> Result<CreateTarget, NameError> {
    let input = input.trim_start();
    let is_dir = input.ends_with('/') || input.ends_with('\\');
    let trimmed = input.trim_end_matches(['/', '\\']);
    if trimmed.is_empty() {
        return Err(NameError::Empty);
    }
    let mut path = base.to_path_buf();
    for comp in trimmed.split(['/', '\\']) {
        if comp.is_empty() {
            return Err(NameError::NotRelative);
        }
        validate_name(comp)?;
        path.push(comp);
    }
    Ok(CreateTarget { path, is_dir })
}

/// Text the user must type to confirm a permanent delete: `yes` for one item, otherwise the
/// item count (`"12"`).
pub fn confirmation_token(item_count: usize) -> String {
    if item_count <= 1 {
        "yes".to_owned()
    } else {
        item_count.to_string()
    }
}

/// Whether `input` (trimmed, case-insensitive) matches [`confirmation_token`].
pub fn confirmation_matches(item_count: usize, input: &str) -> bool {
    input
        .trim()
        .eq_ignore_ascii_case(&confirmation_token(item_count))
}

/// "Keep both" naming: `name (1).ext`, `name (2).ext`, ... The first candidate for which
/// `exists` is false wins. An existing ` (n)` suffix on the stem is bumped instead of nested.
/// Dotfiles (`.env`) become `.env (1)`; `a.tar.gz` becomes `a.tar (1).gz`.
pub fn keep_both_name(name: &str, exists: impl Fn(&str) -> bool) -> String {
    let (stem, ext) = split_stem_ext(name);
    let (base, start) = strip_counter(stem);
    let mut n = start + 1;
    loop {
        let candidate = match ext {
            Some(e) => format!("{base} ({n}).{e}"),
            None => format!("{base} ({n})"),
        };
        if !exists(&candidate) {
            return candidate;
        }
        n += 1;
    }
}

fn split_stem_ext(name: &str) -> (&str, Option<&str>) {
    match name.rfind('.') {
        Some(i) if i > 0 && i + 1 < name.len() => (&name[..i], Some(&name[i + 1..])),
        _ => (name, None),
    }
}

fn strip_counter(stem: &str) -> (&str, u64) {
    if let Some(open) = stem.rfind(" (") {
        if let Some(num) = stem[open + 2..].strip_suffix(')') {
            if !num.is_empty() && num.bytes().all(|b| b.is_ascii_digit()) {
                if let Ok(n) = num.parse::<u64>() {
                    return (&stem[..open], n);
                }
            }
        }
    }
    (stem, 0)
}

/// [`keep_both_name`] applied to a full path: finds a free sibling of `target`.
pub fn keep_both_path(target: &Path) -> PathBuf {
    let dir = target.parent().map(Path::to_path_buf).unwrap_or_default();
    let name = target
        .file_name()
        .map(|n| n.to_string_lossy().into_owned())
        .unwrap_or_default();
    let free = keep_both_name(&name, |c| std::fs::symlink_metadata(dir.join(c)).is_ok());
    dir.join(free)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn validates_names() {
        assert!(validate_name("ok.txt").is_ok());
        assert!(validate_name("файл ✓.md").is_ok());
        assert_eq!(validate_name(""), Err(NameError::Empty));
        assert_eq!(validate_name("a:b"), Err(NameError::InvalidChar(':')));
        assert_eq!(validate_name("a?"), Err(NameError::InvalidChar('?')));
        assert_eq!(
            validate_name("a\u{1}"),
            Err(NameError::InvalidChar('\u{1}'))
        );
        assert_eq!(validate_name("bad."), Err(NameError::TrailingDotOrSpace));
        assert_eq!(validate_name("bad "), Err(NameError::TrailingDotOrSpace));
        assert!(matches!(validate_name("con"), Err(NameError::Reserved(_))));
        assert!(matches!(
            validate_name("NUL.txt"),
            Err(NameError::Reserved(_))
        ));
        assert!(matches!(validate_name("lpt9"), Err(NameError::Reserved(_))));
        assert!(validate_name("com10").is_ok());
        assert!(validate_name("console").is_ok());
        assert!(matches!(validate_name(".."), Err(NameError::DotName(_))));
        assert_eq!(validate_name(&"x".repeat(256)), Err(NameError::TooLong));
        assert!(validate_name(&"x".repeat(255)).is_ok());
    }

    #[test]
    fn parses_create_input() {
        let base = Path::new("base");
        let t = parse_create_input(base, "a/b/c.txt").unwrap();
        assert!(!t.is_dir);
        assert_eq!(t.path, base.join("a").join("b").join("c.txt"));
        let t = parse_create_input(base, "dir\\sub\\").unwrap();
        assert!(t.is_dir);
        assert_eq!(t.path, base.join("dir").join("sub"));
        assert!(parse_create_input(base, "/").is_err());
        assert_eq!(
            parse_create_input(base, "a//b"),
            Err(NameError::NotRelative)
        );
        assert!(parse_create_input(base, "C:\\x").is_err());
        assert!(parse_create_input(base, "../x").is_err());
        assert!(parse_create_input(base, "aux").is_err());
    }

    #[test]
    fn confirmation() {
        assert_eq!(confirmation_token(1), "yes");
        assert_eq!(confirmation_token(7), "7");
        assert!(confirmation_matches(1, " YES "));
        assert!(!confirmation_matches(3, "yes"));
        assert!(confirmation_matches(3, "3"));
    }

    #[test]
    fn keep_both() {
        assert_eq!(keep_both_name("a.txt", |_| false), "a (1).txt");
        assert_eq!(keep_both_name("a.txt", |c| c == "a (1).txt"), "a (2).txt");
        assert_eq!(keep_both_name("a (1).txt", |_| false), "a (2).txt");
        assert_eq!(keep_both_name("dir", |_| false), "dir (1)");
        assert_eq!(keep_both_name(".env", |_| false), ".env (1)");
        assert_eq!(keep_both_name("a.tar.gz", |_| false), "a.tar (1).gz");
    }
}
