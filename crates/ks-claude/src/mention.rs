//! 10c: format `@file` mentions to hand context to Claude.
//!
//! Syntax (Claude Code docs, "Reference files and directories"): `@src/utils/auth.js` mentions a
//! file; `@src/components` a directory; paths may be relative or absolute; several mentions in
//! one message are separated by spaces. The line-range form `@path#L<start>-<end>` is the one
//! the plan specifies (the same form Claude Code's IDE integrations insert).
//!
//! **Unverified guess:** the docs do not describe paths containing spaces, and verifying needs a
//! live prompt (which this project must not send from tests). [`MentionStyle::Quoted`] wraps such
//! paths as `@"path with spaces"` and puts `#L` after the closing quote; that form is a guess,
//! not documented behaviour. If it turns out wrong, flip to [`MentionStyle::Plain`] through
//! [`MentionOptions`] (no code change in this module), which emits the path unquoted.
//!
//! Mentions use forward slashes and are relative to the Claude working directory when the file
//! lives under it (otherwise absolute). Paths are normalised first: verbatim prefixes
//! (`\\?\C:\`, `\\?\UNC\`) are removed and `.` / `..` components are resolved lexically, so a
//! `\\?\C:\proj\a` path matches a `C:\proj` working directory.

use std::path::{Component, Path, PathBuf};

/// How paths containing whitespace or quotes are written.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum MentionStyle {
    /// `@"my dir/a b.md"#L3-9` (unverified guess, see the module docs).
    #[default]
    Quoted,
    /// `@my dir/a b.md#L3-9`: never quoted (the safe fallback if the quoted form does not work).
    Plain,
}

/// Mention formatting options.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct MentionOptions {
    /// Quoting style for awkward paths.
    pub style: MentionStyle,
}

fn comp_eq(a: &Component<'_>, b: &Component<'_>) -> bool {
    if cfg!(windows) {
        a.as_os_str().to_string_lossy().to_lowercase()
            == b.as_os_str().to_string_lossy().to_lowercase()
    } else {
        a == b
    }
}

/// Remove a Windows verbatim prefix (`\\?\C:\x` -> `C:\x`, `\\?\UNC\srv\sh` -> `\\srv\sh`).
fn strip_verbatim(s: &str) -> String {
    if let Some(rest) = s.strip_prefix(r"\\?\UNC\") {
        format!(r"\\{rest}")
    } else if let Some(rest) = s.strip_prefix(r"\\?\") {
        rest.to_string()
    } else {
        s.to_string()
    }
}

/// Lexically normalise: drop verbatim prefixes and `.`, resolve `..` where possible.
pub fn normalize(path: &Path) -> PathBuf {
    let text = strip_verbatim(&path.to_string_lossy());
    let mut out: Vec<Component<'_>> = Vec::new();
    let owned = PathBuf::from(&text);
    for c in owned.components() {
        match c {
            Component::CurDir => {}
            Component::ParentDir => match out.last() {
                Some(Component::Normal(_)) => {
                    out.pop();
                }
                Some(Component::RootDir | Component::Prefix(_)) => {}
                _ => out.push(c),
            },
            other => out.push(other),
        }
    }
    out.iter().map(|c| c.as_os_str()).collect()
}

/// `path` relative to `cwd` if it is inside it, else `None`. Both are normalised first.
pub fn relative_to(path: &Path, cwd: &Path) -> Option<PathBuf> {
    let path = normalize(path);
    let cwd = normalize(cwd);
    let p: Vec<_> = path.components().collect();
    let c: Vec<_> = cwd.components().collect();
    if c.len() > p.len() || !c.iter().zip(&p).all(|(a, b)| comp_eq(a, b)) {
        return None;
    }
    let rest: PathBuf = p[c.len()..].iter().map(|c| c.as_os_str()).collect();
    Some(rest)
}

/// Path text as used in a mention (forward slashes, relative to `cwd` when inside it).
pub fn mention_path(path: &Path, cwd: &Path) -> String {
    let shown = match relative_to(path, cwd) {
        Some(r) if !r.as_os_str().is_empty() => r,
        Some(_) => PathBuf::from("."),
        None => normalize(path),
    };
    shown.to_string_lossy().replace('\\', "/")
}

/// One mention with the default options, with an optional 1-based inclusive line range.
/// A reversed range (`end < start`) is swapped.
pub fn format_mention(path: &Path, cwd: &Path, lines: Option<(usize, usize)>) -> String {
    format_mention_with(path, cwd, lines, MentionOptions::default())
}

/// [`format_mention`] with explicit options.
pub fn format_mention_with(
    path: &Path,
    cwd: &Path,
    lines: Option<(usize, usize)>,
    opts: MentionOptions,
) -> String {
    let p = mention_path(path, cwd);
    let range = match lines {
        Some((s, e)) => {
            let (s, e) = if e < s { (e, s) } else { (s, e) };
            if s >= 1 && e > s {
                format!("#L{s}-{e}")
            } else if s >= 1 {
                format!("#L{s}")
            } else {
                String::new()
            }
        }
        None => String::new(),
    };
    let awkward = p.chars().any(|c| c.is_whitespace() || c == '"');
    if awkward && opts.style == MentionStyle::Quoted {
        format!("@\"{}\"{range}", p.replace('"', "\\\""))
    } else {
        format!("@{p}{range}")
    }
}

/// One line-range mention with a trailing space, as typed into the prompt: several sends in a
/// row stay separate mentions and the user can keep typing.
pub fn format_range_mention(path: &Path, cwd: &Path, start: usize, end: usize) -> String {
    let mut out = format_mention(path, cwd, Some((start, end)));
    out.push(' ');
    out
}

/// Space-separated mentions with a trailing space (so the user can keep typing).
pub fn format_mentions<P: AsRef<Path>>(paths: &[P], cwd: &Path) -> String {
    format_mentions_with(paths, cwd, MentionOptions::default())
}

/// [`format_mentions`] with explicit options.
pub fn format_mentions_with<P: AsRef<Path>>(
    paths: &[P],
    cwd: &Path,
    opts: MentionOptions,
) -> String {
    let mut out = String::new();
    for p in paths {
        out.push_str(&format_mention_with(p.as_ref(), cwd, None, opts));
        out.push(' ');
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn range_mentions_end_with_a_space() {
        let cwd = Path::new(r"C:\work\proj");
        let a = format_range_mention(Path::new(r"C:\work\proj\a.rs"), cwd, 3, 3);
        let b = format_range_mention(Path::new(r"C:\work\proj\b c.rs"), cwd, 2, 5);
        assert_eq!(format!("{a}{b}"), "@a.rs#L3 @\"b c.rs\"#L2-5 ");
    }

    #[test]
    fn relative_and_absolute() {
        let cwd = Path::new("C:\\work\\proj");
        assert_eq!(
            format_mention(Path::new("C:\\work\\proj\\src\\main.rs"), cwd, None),
            "@src/main.rs"
        );
        assert_eq!(
            format_mention(Path::new("C:\\other\\x.txt"), cwd, None),
            "@C:/other/x.txt"
        );
        assert_eq!(format_mention(Path::new("C:\\work\\proj"), cwd, None), "@.");
        // prefix of a different dir name must not match
        assert_eq!(
            format_mention(Path::new("C:\\work\\project2\\a"), cwd, None),
            "@C:/work/project2/a"
        );
    }

    #[test]
    fn windows_case_insensitive() {
        if cfg!(windows) {
            assert_eq!(
                format_mention(
                    Path::new("c:\\WORK\\proj\\A.rs"),
                    Path::new("C:\\work\\proj"),
                    None
                ),
                "@A.rs"
            );
        }
    }

    #[test]
    fn spaces_are_quoted() {
        let cwd = Path::new("C:\\w");
        assert_eq!(
            format_mention(Path::new("C:\\w\\my dir\\a b.md"), cwd, None),
            "@\"my dir/a b.md\""
        );
        assert_eq!(
            format_mention(Path::new("C:\\w\\my dir\\a.md"), cwd, Some((3, 9))),
            "@\"my dir/a.md\"#L3-9"
        );
    }

    #[test]
    fn plain_style_never_quotes() {
        let plain = MentionOptions {
            style: MentionStyle::Plain,
        };
        let cwd = Path::new("C:\\w");
        assert_eq!(
            format_mention_with(Path::new("C:\\w\\my dir\\a b.md"), cwd, Some((1, 2)), plain),
            "@my dir/a b.md#L1-2"
        );
        assert_eq!(
            format_mentions_with(&[Path::new("C:\\w\\a b")], cwd, plain),
            "@a b "
        );
    }

    #[test]
    fn line_ranges() {
        let cwd = Path::new("C:\\w");
        let p = Path::new("C:\\w\\a.rs");
        assert_eq!(format_mention(p, cwd, Some((10, 20))), "@a.rs#L10-20");
        assert_eq!(format_mention(p, cwd, Some((7, 7))), "@a.rs#L7");
        assert_eq!(format_mention(p, cwd, Some((0, 0))), "@a.rs");
        // reversed ranges are swapped
        assert_eq!(format_mention(p, cwd, Some((20, 10))), "@a.rs#L10-20");
    }

    #[test]
    fn hash_and_quote_in_paths() {
        let cwd = Path::new("C:\\w");
        // '#' is left alone (only the trailing #L range is syntax)
        assert_eq!(
            format_mention(Path::new("C:\\w\\c#\\a.cs"), cwd, Some((2, 4))),
            "@c#/a.cs#L2-4"
        );
        // a quote forces quoting + escaping, and the range follows the closing quote
        assert_eq!(
            format_mention(Path::new("C:\\w\\a\"b.txt"), cwd, Some((2, 4))),
            "@\"a\\\"b.txt\"#L2-4"
        );
    }

    #[test]
    fn unc_and_other_drive_with_spaces_stay_absolute() {
        let cwd = Path::new("C:\\w");
        assert_eq!(
            format_mention(Path::new("D:\\my files\\x.txt"), cwd, None),
            "@\"D:/my files/x.txt\""
        );
        assert_eq!(
            format_mention(Path::new("\\\\srv\\share dir\\x.txt"), cwd, None),
            "@\"//srv/share dir/x.txt\""
        );
    }

    #[test]
    fn directory_with_trailing_separator() {
        let cwd = Path::new("C:\\w");
        assert_eq!(format_mention(Path::new("C:\\w\\src\\"), cwd, None), "@src");
        assert_eq!(format_mention(Path::new("C:\\w\\"), cwd, None), "@.");
    }

    #[test]
    fn verbatim_and_dotdot_are_normalised() {
        let cwd = Path::new("C:\\w\\proj");
        assert_eq!(
            format_mention(Path::new("\\\\?\\C:\\w\\proj\\src\\a.rs"), cwd, None),
            "@src/a.rs"
        );
        assert_eq!(
            format_mention(
                Path::new("\\\\?\\C:\\w\\proj"),
                Path::new("C:\\w\\proj"),
                None
            ),
            "@."
        );
        assert_eq!(
            format_mention(Path::new("C:\\w\\proj\\src\\..\\lib\\.\\b.rs"), cwd, None),
            "@lib/b.rs"
        );
        // escaping the working directory with .. is outside, hence absolute and normalised
        assert_eq!(
            format_mention(Path::new("C:\\w\\proj\\..\\other\\c.rs"), cwd, None),
            "@C:/w/other/c.rs"
        );
        assert_eq!(
            format_mention(Path::new("\\\\?\\UNC\\srv\\sh\\a"), cwd, None),
            "@//srv/sh/a"
        );
    }

    #[test]
    fn multiple_space_separated() {
        let cwd = Path::new("C:\\w");
        let v = [
            PathBuf::from("C:\\w\\a.rs"),
            PathBuf::from("C:\\w\\b\\c.rs"),
        ];
        assert_eq!(format_mentions(&v, cwd), "@a.rs @b/c.rs ");
    }
}
