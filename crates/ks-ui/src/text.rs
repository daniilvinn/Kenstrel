//! Width-aware text helpers. All alignment uses terminal cell widths, never `len()`.

use std::path::{Component, Path};

use unicode_width::{UnicodeWidthChar, UnicodeWidthStr};

const ELLIPSIS: &str = "…";

/// Display width in terminal cells.
pub fn width(s: &str) -> usize {
    UnicodeWidthStr::width(s)
}

/// Truncates the end of `s` with `…` so that it fits in `max` cells.
pub fn truncate_end(s: &str, max: usize) -> String {
    if width(s) <= max {
        return s.to_owned();
    }
    if max == 0 {
        return String::new();
    }
    let mut out = String::new();
    let mut used = 0;
    for c in s.chars() {
        let w = c.width().unwrap_or(0);
        if used + w + 1 > max {
            break;
        }
        out.push(c);
        used += w;
    }
    out.push_str(ELLIPSIS);
    out
}

/// Truncates the start of `s` with `…` so that it fits in `max` cells.
pub fn truncate_start(s: &str, max: usize) -> String {
    if width(s) <= max {
        return s.to_owned();
    }
    if max == 0 {
        return String::new();
    }
    let mut kept: Vec<char> = Vec::new();
    let mut used = 0;
    for c in s.chars().rev() {
        let w = c.width().unwrap_or(0);
        if used + w + 1 > max {
            break;
        }
        kept.push(c);
        used += w;
    }
    kept.push('…');
    kept.iter().rev().collect()
}

/// Replaces control characters (which could move the cursor or inject escape sequences) and
/// invisible formatting characters (bidirectional overrides and isolates, which can make
/// `invoice<U+202E>fdp.exe` read as `invoiceexe.pdf`; line separators; zero-width spaces) with
/// `?` before a name is drawn. The set is shared with the content-search line sanitiser.
pub fn sanitize(text: &str) -> String {
    text.chars()
        .map(|c| if ks_search::is_unsafe_char(c) { '?' } else { c })
        .collect()
}

/// Shortens a file name to at most `max` cells by replacing the middle with `…`, keeping the
/// start, the last few characters of the stem and (when `keep_extension`) the whole extension:
/// `very_long_filename_here.rs` becomes `very_long_fi…here.rs`.
pub fn truncate_middle(name: &str, max: usize, keep_extension: bool) -> String {
    if width(name) <= max {
        return name.to_owned();
    }
    let ext_start = if keep_extension {
        match ks_core::entry::extension_of(name) {
            "" => name.len(),
            ext => name.len() - ext.len() - 1,
        }
    } else {
        name.len()
    };
    let (stem, ext) = name.split_at(ext_start);
    let ext_width = width(ext);
    // Room for at least one leading character, the ellipsis and the extension.
    if ext_width + 2 > max {
        return truncate_end(name, max);
    }
    let remaining = max - ext_width - 1;
    let tail_budget = (remaining / 3).min(4);
    let head_budget = remaining - tail_budget;

    let mut head = String::new();
    let mut used = 0;
    for c in stem.chars() {
        let w = c.width().unwrap_or(0);
        if used + w > head_budget {
            break;
        }
        head.push(c);
        used += w;
    }
    let mut tail: Vec<char> = Vec::new();
    let mut used = 0;
    for c in stem[head.len()..].chars().rev() {
        let w = c.width().unwrap_or(0);
        if used + w > tail_budget {
            break;
        }
        tail.push(c);
        used += w;
    }
    let tail: String = tail.into_iter().rev().collect();
    format!("{head}{ELLIPSIS}{tail}{ext}")
}

/// Strips `home` from the front of `path`, component by component. Windows paths compare
/// case-insensitively (`c:\users\me` is the same place as `C:\Users\me`).
fn strip_home<'a>(path: &'a Path, home: &Path) -> Option<&'a Path> {
    if !cfg!(windows) {
        return path.strip_prefix(home).ok();
    }
    let mut rest = path.components();
    for h in home.components() {
        let p = rest.next()?;
        if !p
            .as_os_str()
            .to_string_lossy()
            .eq_ignore_ascii_case(&h.as_os_str().to_string_lossy())
        {
            return None;
        }
    }
    Some(rest.as_path())
}

/// The segments of `path` for display: `~` replaces the home directory, `\\?\` prefixes are
/// stripped and the separator is always `/` (PLAN §4.2).
fn breadcrumb_segments(path: &Path, home: Option<&Path>) -> Vec<String> {
    let path = dunce::simplified(path);
    let home = home.map(dunce::simplified);

    let mut segments = Vec::new();
    let mut rest = path;
    if let Some(stripped) = home.and_then(|h| strip_home(path, h)) {
        segments.push("~".to_owned());
        rest = stripped;
    }
    let mut saw_root = false;
    let mut has_normal = false;
    for component in rest.components() {
        match component {
            Component::Prefix(p) => segments.push(p.as_os_str().to_string_lossy().into_owned()),
            Component::RootDir => {
                saw_root = true;
                if segments.is_empty() {
                    segments.push(String::new()); // unix root: joins to a leading `/`
                }
            }
            Component::Normal(name) => {
                has_normal = true;
                segments.push(name.to_string_lossy().into_owned());
            }
            Component::CurDir => segments.push(".".to_owned()),
            Component::ParentDir => segments.push("..".to_owned()),
        }
    }
    // A bare root (`/` or `C:/`) needs a trailing empty segment to join as `/` or `C:/`.
    if saw_root && !has_normal {
        segments.push(String::new());
    }
    // Directory names can carry control and bidi characters too.
    segments.iter().map(|s| sanitize(s)).collect()
}

/// Formats a path for the header, shortened to at most `max` cells by eliding middle
/// segments with `…` (`~/…/renderer`), and as a last resort truncating from the left.
pub fn breadcrumb(path: &Path, home: Option<&Path>, max: usize) -> String {
    let segments = breadcrumb_segments(path, home);
    let full = join(&segments);
    if width(&full) <= max || segments.len() < 2 {
        return truncate_start(&full, max);
    }
    // Keep the first segment, an ellipsis, and as many trailing segments as fit.
    for keep in (1..segments.len()).rev() {
        let mut shortened = vec![segments[0].clone(), ELLIPSIS.to_owned()];
        shortened.extend_from_slice(&segments[segments.len() - keep..]);
        let candidate = join(&shortened);
        if width(&candidate) <= max {
            return candidate;
        }
    }
    truncate_start(&full, max)
}

fn join(segments: &[String]) -> String {
    segments.join("/")
}

#[cfg(test)]
mod tests {
    #[test]
    fn control_characters_are_replaced() {
        assert_eq!(super::sanitize("a\x1b[2Jb\rc\u{7}"), "a?[2Jb?c?");
        assert_eq!(super::sanitize("plain ünï.txt"), "plain ünï.txt");
    }

    #[test]
    fn breadcrumbs_neutralise_bidi_overrides_in_directory_names() {
        let b = breadcrumb(Path::new("/home/me/ev\u{202E}il"), None, 60);
        assert_eq!(b, "/home/me/ev?il");
    }

    #[test]
    fn bidi_and_invisible_formatting_characters_are_replaced() {
        // The classic extension spoof: a right-to-left override inside a name.
        assert_eq!(super::sanitize("invoice\u{202E}fdp.exe"), "invoice?fdp.exe");
        for c in [
            '\u{202A}', '\u{202D}', '\u{2066}', '\u{2069}', '\u{200E}', '\u{200F}', '\u{061C}',
            '\u{2028}', '\u{2029}', '\u{200B}', '\u{FEFF}', '\u{85}',
        ] {
            assert_eq!(
                super::sanitize(&format!("a{c}b")),
                "a?b",
                "U+{:04X}",
                c as u32
            );
        }
        // Joiners stay for emoji and Indic scripts.
        assert_eq!(super::sanitize("👨\u{200D}👩"), "👨\u{200D}👩");
    }

    use std::path::PathBuf;

    use super::*;

    #[test]
    fn truncation_respects_cell_width() {
        assert_eq!(truncate_end("hello", 10), "hello");
        assert_eq!(truncate_end("hello world", 8), "hello w…");
        assert_eq!(truncate_end("hello", 0), "");
        assert_eq!(truncate_start("hello world", 8), "…o world");
        // Wide characters occupy two cells and are never split.
        assert_eq!(truncate_end("日本語テキスト", 7), "日本語…");
        assert_eq!(truncate_end("日本語テキスト", 6), "日本…");
        assert_eq!(truncate_start("日本語テキスト", 7), "…キスト");
    }

    #[test]
    fn middle_truncation_keeps_the_extension() {
        assert_eq!(truncate_middle("short.rs", 20, true), "short.rs");
        assert_eq!(
            truncate_middle("very_long_filename_here.rs", 20, true),
            "very_long_fi…here.rs"
        );
        assert_eq!(
            truncate_middle("abcdefghijklmnop.txt", 12, true),
            "abcde…op.txt"
        );
        // Too narrow to keep the extension: fall back to cutting the end.
        assert_eq!(truncate_middle("abcdefgh.markdown", 6, true), "abcde…");
        // Names without an extension, dotfiles and directory names are cut in the middle.
        assert_eq!(truncate_middle("abcdefghijklmnop", 10, true), "abcdef…nop");
        assert_eq!(
            truncate_middle(".gitignore_template", 10, true),
            ".gitig…ate"
        );
        assert_eq!(
            truncate_middle("some.directory.name/", 12, false),
            "some.dir…me/"
        );
    }

    #[test]
    fn middle_truncation_never_exceeds_the_budget() {
        let names = [
            "very_long_filename_here.rs",
            "日本語のファイル名前.txt",
            "emoji🎉name🎉name🎉.md",
            "a.b.c.d.e.f.g.h.i.j.k",
            "noext_but_long_name",
        ];
        for name in names {
            for keep in [true, false] {
                for max in 0..40 {
                    let out = truncate_middle(name, max, keep);
                    assert!(width(&out) <= max, "{name} -> {out} ({max})");
                }
            }
        }
    }

    fn p(s: &str) -> PathBuf {
        PathBuf::from(s)
    }

    #[test]
    fn home_is_abbreviated() {
        let home = p("/home/me");
        assert_eq!(
            breadcrumb(&p("/home/me/projects/kestrel"), Some(&home), 80),
            "~/projects/kestrel"
        );
        assert_eq!(breadcrumb(&p("/home/me"), Some(&home), 80), "~");
        assert_eq!(
            breadcrumb(&p("/etc/hosts.d"), Some(&home), 80),
            "/etc/hosts.d"
        );
        // A sibling that merely shares a string prefix is not under home.
        assert_eq!(
            breadcrumb(&p("/home/mellow/x"), Some(&home), 80),
            "/home/mellow/x"
        );
    }

    #[test]
    fn root_paths() {
        assert_eq!(breadcrumb(&p("/"), None, 80), "/");
    }

    #[test]
    fn long_paths_elide_the_middle() {
        let home = p("/home/me");
        let path = p("/home/me/projects/omniforce/src/renderer");
        assert_eq!(
            breadcrumb(&path, Some(&home), 80),
            "~/projects/omniforce/src/renderer"
        );
        assert_eq!(
            breadcrumb(&path, Some(&home), 28),
            "~/…/omniforce/src/renderer"
        );
        assert_eq!(breadcrumb(&path, Some(&home), 14), "~/…/renderer");
        // Even the last segment does not fit: truncate from the left.
        let narrow = breadcrumb(&path, Some(&home), 6);
        assert_eq!(width(&narrow), 6);
        assert!(narrow.starts_with('…'));
    }

    #[test]
    fn never_exceeds_the_budget() {
        let path = p("/a/bb/ccc/dddd/eeeee/ffffff");
        for max in 0..40 {
            assert!(width(&breadcrumb(&path, None, max)) <= max, "max={max}");
        }
    }

    #[cfg(windows)]
    #[test]
    fn windows_drive_and_verbatim_paths() {
        assert_eq!(
            breadcrumb(&p(r"C:\Users\me\dev"), None, 80),
            "C:/Users/me/dev"
        );
        assert_eq!(
            breadcrumb(&p(r"\\?\C:\Users\me\dev"), None, 80),
            "C:/Users/me/dev"
        );
        assert_eq!(breadcrumb(&p(r"C:\"), None, 80), "C:/");
        // Windows paths are case-insensitive: the typed casing may differ from the home's.
        assert_eq!(
            breadcrumb(&p(r"c:\users\me\dev"), Some(&p(r"C:\Users\Me")), 80),
            "~/dev"
        );
        assert_eq!(
            breadcrumb(&p(r"C:\Users\mellow"), Some(&p(r"C:\Users\me")), 80),
            "C:/Users/mellow"
        );
        assert_eq!(
            breadcrumb(&p(r"\\?\C:\Users\me\dev"), Some(&p(r"C:\Users\me")), 80),
            "~/dev"
        );
    }
}
