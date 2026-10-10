//! The directory entry model and its sort order.
//!
//! Entries are plain data produced by `ks-fs` (or by tests). Everything that is expensive to
//! derive from a name, namely the natural-sort key, is computed once when the entry is built,
//! so sorting 100k entries is a series of byte-slice comparisons.

use std::borrow::Cow;
use std::cmp::Ordering;
use std::ffi::{OsStr, OsString};
use std::time::SystemTime;

use crate::config::{SortConfig, SortKey};

/// What kind of file system object an entry is.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum EntryKind {
    File,
    Dir,
    /// A symbolic link (to a file or a directory, see [`Entry::is_dir`]).
    Symlink,
    /// An NTFS junction (a directory mount point). Always behaves like a directory.
    Junction,
}

/// File attribute flags that matter to the UI.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Attrs(u8);

impl Attrs {
    pub const NONE: Attrs = Attrs(0);
    /// `FILE_ATTRIBUTE_HIDDEN`.
    pub const HIDDEN: Attrs = Attrs(1);
    /// `FILE_ATTRIBUTE_SYSTEM`.
    pub const SYSTEM: Attrs = Attrs(1 << 1);
    pub const READONLY: Attrs = Attrs(1 << 2);
    /// A cloud placeholder (`RECALL_ON_DATA_ACCESS`, `RECALL_ON_OPEN` or `OFFLINE`): its
    /// content lives elsewhere and reading it would download it. Never open these.
    pub const CLOUD: Attrs = Attrs(1 << 3);
    /// Executable (by extension on Windows, by mode bits elsewhere).
    pub const EXEC: Attrs = Attrs(1 << 4);

    pub const fn contains(self, other: Attrs) -> bool {
        self.0 & other.0 == other.0
    }

    pub const fn intersects(self, other: Attrs) -> bool {
        self.0 & other.0 != 0
    }

    pub const fn with(self, other: Attrs) -> Attrs {
        Attrs(self.0 | other.0)
    }
}

impl std::ops::BitOr for Attrs {
    type Output = Attrs;

    fn bitor(self, rhs: Attrs) -> Attrs {
        self.with(rhs)
    }
}

/// One row of a directory listing.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Entry {
    pub name: OsString,
    pub kind: EntryKind,
    /// Navigable as a directory: a directory, a junction, or a link that points to one.
    pub is_dir: bool,
    /// Size in bytes (0 for directories).
    pub size: u64,
    pub mtime: Option<SystemTime>,
    pub attrs: Attrs,
    /// Number of children of a directory, filled in lazily after the listing.
    pub children: Option<u32>,
    sort_key: Box<[u8]>,
    ext: Box<str>,
}

impl Entry {
    pub fn new(name: impl Into<OsString>, kind: EntryKind) -> Self {
        let name = name.into();
        let lossy = name.to_string_lossy();
        let sort_key = natural_key(&lossy);
        let ext = extension_of(&lossy).to_lowercase().into_boxed_str();
        drop(lossy);
        Self {
            name,
            kind,
            is_dir: matches!(kind, EntryKind::Dir | EntryKind::Junction),
            size: 0,
            mtime: None,
            attrs: Attrs::NONE,
            children: None,
            sort_key,
            ext,
        }
    }

    pub fn file(name: impl Into<OsString>, size: u64) -> Self {
        Self {
            size,
            ..Self::new(name, EntryKind::File)
        }
    }

    pub fn dir(name: impl Into<OsString>) -> Self {
        Self::new(name, EntryKind::Dir)
    }

    pub fn with_mtime(mut self, mtime: SystemTime) -> Self {
        self.mtime = Some(mtime);
        self
    }

    pub fn with_attrs(mut self, attrs: Attrs) -> Self {
        self.attrs = self.attrs | attrs;
        self
    }

    /// Marks a link as pointing to a directory.
    pub fn with_dir_target(mut self) -> Self {
        self.is_dir = true;
        self
    }

    /// The name for display; invalid Unicode is replaced, never dropped.
    pub fn display_name(&self) -> Cow<'_, str> {
        self.name.to_string_lossy()
    }

    /// Lower-cased extension without the dot (`""` when there is none).
    pub fn extension(&self) -> &str {
        &self.ext
    }

    /// Hidden by attribute, or a dotfile (on every platform).
    pub fn is_hidden(&self) -> bool {
        self.attrs.intersects(Attrs::HIDDEN | Attrs::SYSTEM) || is_dotfile(&self.name)
    }

    pub fn is_cloud_placeholder(&self) -> bool {
        self.attrs.contains(Attrs::CLOUD)
    }

    pub fn is_link(&self) -> bool {
        matches!(self.kind, EntryKind::Symlink | EntryKind::Junction)
    }

    pub fn is_exec(&self) -> bool {
        self.attrs.contains(Attrs::EXEC)
    }
}

fn is_dotfile(name: &OsStr) -> bool {
    name.as_encoded_bytes().first() == Some(&b'.')
}

/// The extension of a file name: text after the last dot, if the dot is not the first
/// character (`.gitignore` has none) and something follows it.
pub fn extension_of(name: &str) -> &str {
    match name.rfind('.') {
        Some(dot) if dot > 0 && dot + 1 < name.len() => &name[dot + 1..],
        _ => "",
    }
}

/// A byte string whose plain lexicographic order is the natural order of `name`:
/// case-insensitive, with digit runs compared by numeric value (`file2` < `file10`).
///
/// A digit run is encoded as `'0'`, the number of significant digits (saturating at 255) and
/// the digits without leading zeros, so longer numbers sort after shorter ones.
pub fn natural_key(name: &str) -> Box<[u8]> {
    let mut key = Vec::with_capacity(name.len() + 2);
    let mut chars = name.chars().peekable();
    let mut buf = [0u8; 4];
    while let Some(c) = chars.next() {
        if c.is_ascii_digit() {
            let mut digits = String::new();
            digits.push(c);
            while let Some(d) = chars.next_if(char::is_ascii_digit) {
                digits.push(d);
            }
            let trimmed = digits.trim_start_matches('0');
            key.push(b'0');
            key.push(u8::try_from(trimmed.len()).unwrap_or(u8::MAX));
            key.extend_from_slice(trimmed.as_bytes());
        } else {
            for lower in c.to_lowercase() {
                key.extend_from_slice(lower.encode_utf8(&mut buf).as_bytes());
            }
        }
    }
    key.into_boxed_slice()
}

/// Total order of two entries under `sort`.
///
/// Directories stay first (when enabled) whatever the direction. Within a group the natural
/// direction is: name and extension ascending, size and modification time descending
/// (largest and newest first); `reverse` flips it. Ties fall back to the natural name order
/// and finally to the exact name, so the order is total and deterministic.
pub fn compare(a: &Entry, b: &Entry, sort: &SortConfig) -> Ordering {
    if sort.dirs_first && a.is_dir != b.is_dir {
        return b.is_dir.cmp(&a.is_dir);
    }
    let by_name = || {
        a.sort_key
            .cmp(&b.sort_key)
            .then_with(|| a.name.cmp(&b.name))
    };
    let primary = match sort.key {
        SortKey::Name => by_name(),
        SortKey::Ext => a.ext.cmp(&b.ext).then_with(by_name),
        // Directory sizes are unknown, so directories order by name.
        SortKey::Size if a.is_dir && b.is_dir => by_name(),
        SortKey::Size => b.size.cmp(&a.size).then_with(by_name),
        // Entries without a timestamp sort last.
        SortKey::Mtime => b.mtime.cmp(&a.mtime).then_with(by_name),
    };
    if sort.reverse {
        primary.reverse()
    } else {
        primary
    }
}

/// Sorts entries in place (stable).
pub fn sort_entries(entries: &mut [Entry], sort: &SortConfig) {
    entries.sort_by(|a, b| compare(a, b, sort));
}

#[cfg(test)]
mod tests {
    use std::time::{Duration, UNIX_EPOCH};

    use super::*;

    fn names(entries: &[Entry]) -> Vec<String> {
        entries
            .iter()
            .map(|e| e.display_name().into_owned())
            .collect()
    }

    fn sorted(mut entries: Vec<Entry>, sort: &SortConfig) -> Vec<String> {
        sort_entries(&mut entries, sort);
        names(&entries)
    }

    #[test]
    fn natural_order_compares_numbers_by_value() {
        let list = vec![
            Entry::file("file10.txt", 0),
            Entry::file("file2.txt", 0),
            Entry::file("file1.txt", 0),
            Entry::file("file100.txt", 0),
        ];
        assert_eq!(
            sorted(list, &SortConfig::default()),
            ["file1.txt", "file2.txt", "file10.txt", "file100.txt"]
        );
    }

    #[test]
    fn natural_order_is_case_insensitive() {
        let list = vec![
            Entry::file("beta", 0),
            Entry::file("Alpha", 0),
            Entry::file("alpha2", 0),
            Entry::file("Gamma", 0),
        ];
        assert_eq!(
            sorted(list, &SortConfig::default()),
            ["Alpha", "alpha2", "beta", "Gamma"]
        );
    }

    #[test]
    fn leading_zeros_and_huge_numbers_do_not_break_the_order() {
        let key = |s: &str| natural_key(s);
        assert!(key("a2") < key("a10"));
        assert!(key("a02") < key("a10"));
        assert!(key("a9") < key("a010"));
        // Equal value, different spelling: the exact-name tie-break decides.
        assert_eq!(key("a01"), key("a1"));
        let long = format!("a{}", "9".repeat(400));
        let longer = format!("a1{}", "0".repeat(400));
        assert!(key("a5") < key(&long));
        assert!(key(&long).len() > 2);
        let _ = key(&longer);
    }

    #[test]
    fn equal_keys_order_by_exact_name() {
        let list = vec![Entry::file("a1", 0), Entry::file("a01", 0)];
        assert_eq!(sorted(list, &SortConfig::default()), ["a01", "a1"]);
    }

    #[test]
    fn unicode_names_sort_case_insensitively() {
        let list = vec![Entry::file("Zebra", 0), Entry::file("élan", 0)];
        assert_eq!(sorted(list, &SortConfig::default()), ["Zebra", "élan"]);
        let list = vec![Entry::file("Ärger", 0), Entry::file("ärger2", 0)];
        assert_eq!(sorted(list, &SortConfig::default()), ["Ärger", "ärger2"]);
    }

    #[test]
    fn directories_come_first_and_stay_first_when_reversed() {
        let list = || {
            vec![
                Entry::file("a.txt", 0),
                Entry::dir("zeta"),
                Entry::file("b.txt", 0),
                Entry::dir("alpha"),
            ]
        };
        assert_eq!(
            sorted(list(), &SortConfig::default()),
            ["alpha", "zeta", "a.txt", "b.txt"]
        );
        let reversed = SortConfig {
            reverse: true,
            ..SortConfig::default()
        };
        assert_eq!(
            sorted(list(), &reversed),
            ["zeta", "alpha", "b.txt", "a.txt"]
        );
        let mixed = SortConfig {
            dirs_first: false,
            ..SortConfig::default()
        };
        assert_eq!(sorted(list(), &mixed), ["a.txt", "alpha", "b.txt", "zeta"]);
    }

    #[test]
    fn size_sort_is_largest_first_with_directories_by_name() {
        let list = vec![
            Entry::file("small", 1),
            Entry::file("big", 1000),
            Entry::dir("d2"),
            Entry::dir("d1"),
            Entry::file("mid", 50),
        ];
        let sort = SortConfig {
            key: SortKey::Size,
            ..SortConfig::default()
        };
        assert_eq!(
            sorted(list.clone(), &sort),
            ["d1", "d2", "big", "mid", "small"]
        );
        let rev = SortConfig {
            reverse: true,
            ..sort
        };
        assert_eq!(sorted(list, &rev), ["d2", "d1", "small", "mid", "big"]);
    }

    #[test]
    fn mtime_sort_is_newest_first_and_unknown_last() {
        let at = |s| UNIX_EPOCH + Duration::from_secs(s);
        let list = vec![
            Entry::file("old", 0).with_mtime(at(10)),
            Entry::file("unknown", 0),
            Entry::file("new", 0).with_mtime(at(1000)),
        ];
        let sort = SortConfig {
            key: SortKey::Mtime,
            ..SortConfig::default()
        };
        assert_eq!(sorted(list, &sort), ["new", "old", "unknown"]);
    }

    #[test]
    fn extension_sort_groups_by_extension_then_name() {
        let list = vec![
            Entry::file("b.rs", 0),
            Entry::file("a.toml", 0),
            Entry::file("README", 0),
            Entry::file("a.RS", 0),
        ];
        let sort = SortConfig {
            key: SortKey::Ext,
            ..SortConfig::default()
        };
        assert_eq!(sorted(list, &sort), ["README", "a.RS", "b.rs", "a.toml"]);
    }

    #[test]
    fn extension_rules() {
        assert_eq!(extension_of("main.rs"), "rs");
        assert_eq!(extension_of("archive.tar.gz"), "gz");
        assert_eq!(extension_of(".gitignore"), "");
        assert_eq!(extension_of("trailing."), "");
        assert_eq!(extension_of("noext"), "");
        assert_eq!(Entry::file("Photo.JPG", 0).extension(), "jpg");
    }

    #[test]
    fn hidden_means_attribute_or_dotfile() {
        assert!(Entry::file(".env", 0).is_hidden());
        assert!(Entry::file("x", 0).with_attrs(Attrs::HIDDEN).is_hidden());
        assert!(Entry::file("x", 0).with_attrs(Attrs::SYSTEM).is_hidden());
        assert!(!Entry::file("x", 0).with_attrs(Attrs::READONLY).is_hidden());
        assert!(!Entry::dir("src").is_hidden());
    }

    #[test]
    fn links_to_directories_behave_like_directories() {
        let link = Entry::new("l", EntryKind::Symlink);
        assert!(!link.is_dir);
        assert!(link.with_dir_target().is_dir);
        assert!(Entry::new("j", EntryKind::Junction).is_dir);
    }

    #[test]
    fn sorting_is_deterministic_for_identical_keys() {
        let mut a = vec![
            Entry::file("x", 1),
            Entry::file("X", 1),
            Entry::file("x", 1),
        ];
        let mut b = a.clone();
        b.reverse();
        let sort = SortConfig::default();
        sort_entries(&mut a, &sort);
        sort_entries(&mut b, &sort);
        assert_eq!(names(&a), names(&b));
    }
}
