//! In-memory walk cache: the last walk per (root, toggles) so reopening the finder is instant,
//! with incremental updates driven by a filesystem watcher.
//!
//! Only walks that ran to completion are cached. The cache keeps at most
//! [`WalkCache::MAX_ROOTS`] walks (least recently used evicted first). All methods are
//! thread-safe (`&self`).
//!
//! Paths are compared the way Windows does: case-insensitively, with `/` equal to `\`, and
//! ignoring the verbatim prefix (`\\?\C:\` equals `C:\`). So watcher paths and finder roots
//! match regardless of how they are spelled.

use std::collections::{HashMap, HashSet};
use std::ffi::OsStr;
use std::hash::{Hash, Hasher};
use std::path::{Path, PathBuf};
use std::sync::Arc;

use parking_lot::Mutex;

use crate::finder::FinderItem;
use crate::walk::{walk, EntryKind, WalkEntry, WalkOptions, WalkSink, SKIPPED_NAMES};
use tokio_util::sync::CancellationToken;

/// Exact names of entries whose relative path is not valid Unicode, keyed by the lossy
/// `rel` string they are stored under.
pub(crate) type RawNames = HashMap<Box<str>, Box<OsStr>>;

/// One completed walk: the items plus the rare exact names of non-Unicode entries.
#[derive(Debug, Clone, Default)]
pub(crate) struct Walked {
    pub items: Vec<FinderItem>,
    pub raws: RawNames,
}

/// Identity of a cached walk. The root is compared normalized (see the module docs).
#[derive(Debug, Clone)]
pub struct CacheKey {
    /// Walk root.
    pub root: PathBuf,
    /// Hidden files included.
    pub show_hidden: bool,
    /// Gitignore rules honoured.
    pub respect_gitignore: bool,
    /// Directories included as entries.
    pub include_dirs: bool,
}

impl PartialEq for CacheKey {
    fn eq(&self, other: &Self) -> bool {
        self.show_hidden == other.show_hidden
            && self.respect_gitignore == other.respect_gitignore
            && self.include_dirs == other.include_dirs
            && root_key(&self.root) == root_key(&other.root)
    }
}

impl Eq for CacheKey {}

impl Hash for CacheKey {
    fn hash<H: Hasher>(&self, state: &mut H) {
        root_key(&self.root).hash(state);
        self.show_hidden.hash(state);
        self.respect_gitignore.hash(state);
        self.include_dirs.hash(state);
    }
}

/// A change reported by a watcher. Paths are absolute.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum PathChange {
    /// A file/dir appeared.
    Added(PathBuf, EntryKind),
    /// A file/dir (and everything below it) disappeared.
    Removed(PathBuf),
    /// Something happened at this path and the caller does not know what (a finished file
    /// operation, a tool that edited a file): the filesystem decides. A directory is scanned
    /// like [`PathChange::Added`], a file is added, a missing path is removed.
    Touched(PathBuf),
    /// A file/dir (and everything below it) moved.
    Renamed {
        /// Old path.
        from: PathBuf,
        /// New path.
        to: PathBuf,
    },
}

type Entries = Arc<Walked>;

/// The cache. Share it as `Arc<WalkCache>`.
#[derive(Default)]
pub struct WalkCache {
    /// Least recently used first.
    inner: Mutex<Vec<(CacheKey, Entries)>>,
    /// Serializes [`apply_changes`](WalkCache::apply_changes) so the heavy work can happen
    /// outside `inner`.
    apply_lock: Mutex<()>,
}

impl WalkCache {
    /// Maximum number of distinct cached walks.
    pub const MAX_ROOTS: usize = 4;

    /// Empty cache.
    pub fn new() -> Self {
        Self::default()
    }

    /// Cached entries for `key`, if any. Marks the walk as recently used.
    pub(crate) fn get(&self, key: &CacheKey) -> Option<Entries> {
        let mut g = self.inner.lock();
        let pos = g.iter().position(|(k, _)| k == key)?;
        let slot = g.remove(pos);
        let entries = slot.1.clone();
        g.push(slot);
        Some(entries)
    }

    /// Whether a completed walk is cached for `key`.
    pub fn contains(&self, key: &CacheKey) -> bool {
        self.inner.lock().iter().any(|(k, _)| k == key)
    }

    /// Number of entries cached for `key`.
    pub fn len_of(&self, key: &CacheKey) -> Option<usize> {
        let g = self.inner.lock();
        g.iter().find(|(k, _)| k == key).map(|(_, v)| v.items.len())
    }

    /// Stores a completed walk (replacing any earlier one for the key).
    pub fn put(&self, key: CacheKey, items: Vec<FinderItem>) {
        self.put_walked(
            key,
            Arc::new(Walked {
                items,
                raws: RawNames::new(),
            }),
        );
    }

    /// Stores `fresh` for `key` only if the cache still holds exactly `expected` (or nothing
    /// for the key): a newer incremental update ([`apply_changes`](Self::apply_changes)) that
    /// landed while `fresh` was being computed must not be overwritten by it.
    pub(crate) fn put_walked_if_unchanged(
        &self,
        key: CacheKey,
        expected: &Entries,
        fresh: Entries,
    ) {
        let mut g = self.inner.lock();
        match g.iter().find(|(k, _)| *k == key) {
            Some((_, cur)) if !Arc::ptr_eq(cur, expected) => {}
            _ => {
                g.retain(|(k, _)| *k != key);
                g.push((key, fresh));
                while g.len() > Self::MAX_ROOTS {
                    g.remove(0);
                }
            }
        }
    }

    pub(crate) fn put_walked(&self, key: CacheKey, walked: Entries) {
        let mut g = self.inner.lock();
        g.retain(|(k, _)| *k != key);
        g.push((key, walked));
        while g.len() > Self::MAX_ROOTS {
            g.remove(0);
        }
    }

    /// Drops every cached walk whose root equals `root` (normalized comparison).
    pub fn invalidate(&self, root: &Path) {
        let target = root_key(root);
        self.inner
            .lock()
            .retain(|(k, _)| root_key(&k.root) != target);
    }

    /// Drops everything.
    pub fn clear(&self) {
        self.inner.lock().clear();
    }

    /// Applies watcher changes to every cached walk whose root contains the changed paths.
    ///
    /// Cost is linear in the cached entry count per call (one pass for removals/renames; a
    /// hash set for duplicate detection of added paths), so batch events where possible. The
    /// work runs on a private copy outside the cache lock (the filesystem is also only
    /// queried there), so concurrent finder starts are never blocked by it; the result is then
    /// swapped in. Limits: `Added` cannot evaluate gitignore rules (the hidden and built-in
    /// skip rules are honoured). An added directory is scanned (on the calling thread,
    /// honouring the walk toggles of each cached root) so its contents appear too. Sessions that are
    /// already open keep their snapshot; reopen to see the change. Added paths are stored in
    /// their lossy UTF-8 form, and renamed entries lose their exact original name (see
    /// [`FinderItem::raw`]).
    ///
    /// [`invalidate`]: WalkCache::invalidate
    pub fn apply_changes(&self, changes: &[PathChange]) {
        let _serial = self.apply_lock.lock();
        // Filesystem queries happen here, not under the cache lock.
        let mut resolved: Vec<PathChange> = Vec::with_capacity(changes.len());
        let mut kinds: Vec<EntryKind> = Vec::with_capacity(changes.len());
        for c in changes {
            let (change, kind) = match c {
                PathChange::Renamed { to, .. } if to.is_dir() => (c.clone(), EntryKind::Dir),
                PathChange::Touched(p) => match std::fs::symlink_metadata(p) {
                    Ok(m) if m.is_dir() => {
                        (PathChange::Added(p.clone(), EntryKind::Dir), EntryKind::Dir)
                    }
                    Ok(m) => {
                        let kind = if m.file_type().is_symlink() {
                            EntryKind::Symlink
                        } else {
                            EntryKind::File
                        };
                        (PathChange::Added(p.clone(), kind), kind)
                    }
                    Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
                        (PathChange::Removed(p.clone()), EntryKind::File)
                    }
                    // Locked, access denied, transient network error: the file still
                    // exists as far as we know, so leave the cache alone.
                    Err(_) => continue,
                },
                _ => (c.clone(), EntryKind::File),
            };
            resolved.push(change);
            kinds.push(kind);
        }
        let changes = resolved;
        let snapshot: Vec<(CacheKey, Entries)> = self.inner.lock().clone();
        for (key, orig) in snapshot {
            let mut work = orig.clone();
            let mut state = ApplyState::default();
            for (change, kind) in changes.iter().zip(&kinds) {
                apply_one(&key, &mut work, change, *kind, &mut state);
            }
            if Arc::ptr_eq(&work, &orig) {
                continue;
            }
            let mut g = self.inner.lock();
            match g.iter().position(|(k, _)| *k == key) {
                Some(i) if Arc::ptr_eq(&g[i].1, &orig) => g[i].1 = work,
                // Replaced by a concurrent walk that may predate these changes: drop it.
                Some(i) => {
                    g.remove(i);
                }
                None => {}
            }
        }
    }
}

#[derive(Default)]
struct ApplyState {
    /// Folded rel paths of the working copy; built lazily for `Added` bursts and discarded
    /// by any other mutation.
    known: Option<HashSet<String>>,
}

impl ApplyState {
    fn known(&mut self, entries: &Walked) -> &mut HashSet<String> {
        self.known
            .get_or_insert_with(|| entries.items.iter().map(|e| fold(e.rel())).collect())
    }
}

/// Most entries one newly appeared directory contributes; bounds how long `apply_changes`
/// (which serializes behind `apply_lock`) can be held by a huge copy. The next reopen's
/// background re-walk picks up the rest.
const SCAN_NEW_DIR_CAP: usize = 200_000;

struct ScanSink<'a> {
    local: Vec<(PathBuf, EntryKind)>,
    out: &'a Mutex<Vec<(PathBuf, EntryKind)>>,
    count: &'a std::sync::atomic::AtomicUsize,
}

impl WalkSink for ScanSink<'_> {
    fn entry(&mut self, entry: WalkEntry) -> bool {
        self.local.push((entry.path().to_path_buf(), entry.kind));
        self.count
            .fetch_add(1, std::sync::atomic::Ordering::Relaxed)
            < SCAN_NEW_DIR_CAP
    }
}

impl Drop for ScanSink<'_> {
    fn drop(&mut self) {
        self.out.lock().append(&mut self.local);
    }
}

/// Scans a directory that appeared under the root and adds what it contains (honouring the
/// walk toggles of `key`). Runs on the caller's thread, outside the cache lock.
fn scan_new_dir(key: &CacheKey, dir: &Path, entries: &mut Entries, state: &mut ApplyState) {
    let opts = WalkOptions {
        root: dir.to_path_buf(),
        show_hidden: key.show_hidden,
        respect_gitignore: key.respect_gitignore,
        include_dirs: key.include_dirs,
        threads: 0,
        overrides: None,
        skip_cloud_dirs: false,
    };
    let found: Mutex<Vec<(PathBuf, EntryKind)>> = Mutex::new(Vec::new());
    let count = std::sync::atomic::AtomicUsize::new(0);
    walk(&opts, &CancellationToken::new(), || ScanSink {
        local: Vec::new(),
        out: &found,
        count: &count,
    });
    let found = found.into_inner();
    if found.is_empty() {
        return;
    }
    let known = state.known(entries);
    let mut fresh = Vec::new();
    for (path, kind) in found {
        let Some(rel) = rel_string(&key.root, &path) else {
            continue;
        };
        if excluded_by_rules(key, &rel) || !known.insert(fold(&rel)) {
            continue;
        }
        fresh.push(FinderItem::new(rel.as_str(), kind));
    }
    Arc::make_mut(entries).items.extend(fresh);
}

fn is_sep(c: char) -> bool {
    c == '\\' || c == '/'
}

fn same_char(a: char, b: char) -> bool {
    if is_sep(a) && is_sep(b) {
        return true;
    }
    a == b || (cfg!(windows) && a.to_lowercase().eq(b.to_lowercase()))
}

/// Removes the verbatim prefix: `\\?\C:\x` -> `C:\x`, `\\?\UNC\h\s` -> `\\h\s`.
fn strip_verbatim(s: &str) -> std::borrow::Cow<'_, str> {
    if let Some(rest) = s.strip_prefix(r"\\?\UNC\") {
        return format!(r"\\{rest}").into();
    }
    if let Some(rest) = s.strip_prefix(r"\\?\") {
        return rest.into();
    }
    s.into()
}

/// Canonical comparison form of a string path: no verbatim prefix, `\` separators,
/// lowercase on Windows.
fn fold(s: &str) -> String {
    let s = strip_verbatim(s);
    let mut out = String::with_capacity(s.len());
    for c in s.chars() {
        if is_sep(c) {
            out.push('\\');
        } else if cfg!(windows) {
            out.extend(c.to_lowercase());
        } else {
            out.push(c);
        }
    }
    out
}

/// Hash of the folded form of `s` (see [`fold`]); `buf` is scratch space reused across calls.
pub(crate) fn fold_hash(buf: &mut String, s: &str) -> u64 {
    use std::hash::BuildHasher;
    buf.clear();
    let s = strip_verbatim(s);
    for c in s.chars() {
        if is_sep(c) {
            buf.push('\\');
        } else if cfg!(windows) {
            buf.extend(c.to_lowercase());
        } else {
            buf.push(c);
        }
    }
    std::hash::BuildHasherDefault::<std::collections::hash_map::DefaultHasher>::default()
        .hash_one(buf.as_str())
}

/// Normalized identity of a root directory (no trailing separator).
fn root_key(root: &Path) -> String {
    let mut k = fold(&root.to_string_lossy());
    while k.ends_with('\\') && k.len() > 1 {
        k.pop();
    }
    k
}

/// If `full` starts with `prefix` (normalized comparison), the remainder of `full`.
fn strip_ci<'a>(full: &'a str, prefix: &str) -> Option<&'a str> {
    let mut it = full.char_indices();
    for pc in prefix.chars() {
        let (_, fc) = it.next()?;
        if !same_char(fc, pc) {
            return None;
        }
    }
    Some(&full[it.next().map_or(full.len(), |(i, _)| i)..])
}

/// How a stored rel path relates to a changed rel path.
enum Rel<'a> {
    Same,
    /// Below the changed path; payload is the remainder including the leading separator.
    Below(&'a str),
}

fn relate<'a>(stored: &'a str, changed: &str) -> Option<Rel<'a>> {
    let rem = strip_ci(stored, changed)?;
    if rem.is_empty() {
        Some(Rel::Same)
    } else if rem.starts_with(is_sep) {
        Some(Rel::Below(rem))
    } else {
        None
    }
}

fn rel_string(root: &Path, p: &Path) -> Option<String> {
    let root_s = root.to_string_lossy();
    let root_s = strip_verbatim(&root_s);
    let root_s = root_s.trim_end_matches(is_sep);
    let p_s = p.to_string_lossy();
    let p_s = strip_verbatim(&p_s);
    let rem = strip_ci(&p_s, root_s)?;
    if !rem.is_empty() && !rem.starts_with(is_sep) {
        return None;
    }
    let rem = rem.trim_start_matches(is_sep);
    if rem.is_empty() {
        return None;
    }
    Some(if cfg!(windows) {
        rem.replace('/', "\\")
    } else {
        rem.to_owned()
    })
}

fn excluded_by_rules(key: &CacheKey, rel: &str) -> bool {
    rel.split(is_sep).any(|c| {
        (!key.show_hidden && c.starts_with('.'))
            || c == ".git"
            || SKIPPED_NAMES.iter().any(|s| c.eq_ignore_ascii_case(s))
    })
}

fn apply_one(
    key: &CacheKey,
    entries: &mut Entries,
    change: &PathChange,
    renamed_in_kind: EntryKind,
    state: &mut ApplyState,
) {
    match change {
        PathChange::Added(path, kind) => {
            let Some(rel) = rel_string(&key.root, path) else {
                return;
            };
            if excluded_by_rules(key, &rel) {
                return;
            }
            let is_dir = *kind == EntryKind::Dir;
            if !is_dir || key.include_dirs {
                let known = state.known(entries);
                if known.insert(fold(&rel)) {
                    Arc::make_mut(entries)
                        .items
                        .push(FinderItem::new(rel.as_str(), *kind));
                }
            }
            if is_dir {
                scan_new_dir(key, path, entries, state);
            }
        }
        PathChange::Touched(_) => {}
        PathChange::Removed(path) => {
            let Some(rel) = rel_string(&key.root, path) else {
                // The root itself vanished: empty the walk.
                if root_key(path) == root_key(&key.root) && !entries.items.is_empty() {
                    Arc::make_mut(entries).items.clear();
                    state.known = None;
                }
                return;
            };
            if entries
                .items
                .iter()
                .any(|e| relate(e.rel(), &rel).is_some())
            {
                Arc::make_mut(entries)
                    .items
                    .retain(|e| relate(e.rel(), &rel).is_none());
                state.known = None;
            }
        }
        PathChange::Renamed { from, to } => {
            let from_rel = rel_string(&key.root, from);
            let to_rel = rel_string(&key.root, to);
            match (from_rel, to_rel) {
                (Some(f), Some(t)) => {
                    if excluded_by_rules(key, &t) {
                        apply_one(
                            key,
                            entries,
                            &PathChange::Removed(from.clone()),
                            renamed_in_kind,
                            state,
                        );
                        return;
                    }
                    if !entries.items.iter().any(|e| relate(e.rel(), &f).is_some()) {
                        return;
                    }
                    for e in Arc::make_mut(entries).items.iter_mut() {
                        let new_rel = match relate(e.rel(), &f) {
                            Some(Rel::Same) => t.clone(),
                            Some(Rel::Below(rem)) => format!("{t}{rem}"),
                            None => continue,
                        };
                        *e = e.with_rel(&new_rel);
                    }
                    state.known = None;
                }
                // Moved out of this root.
                (Some(_), None) => apply_one(
                    key,
                    entries,
                    &PathChange::Removed(from.clone()),
                    renamed_in_kind,
                    state,
                ),
                // Moved into this root: treat as an added entry (kind resolved up front).
                (None, Some(_)) => apply_one(
                    key,
                    entries,
                    &PathChange::Added(to.clone(), renamed_in_kind),
                    renamed_in_kind,
                    state,
                ),
                (None, None) => {}
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn item(rel: &str) -> FinderItem {
        FinderItem::new(rel, EntryKind::File)
    }

    fn setup() -> (WalkCache, CacheKey) {
        let key = CacheKey {
            root: PathBuf::from("/r"),
            show_hidden: false,
            respect_gitignore: true,
            include_dirs: false,
        };
        let c = WalkCache::new();
        c.put(
            key.clone(),
            vec![item("a.txt"), item("d/b.txt"), item("d/e/c.txt")],
        );
        (c, key)
    }

    fn rels(c: &WalkCache, k: &CacheKey) -> Vec<String> {
        let mut v: Vec<_> = c
            .get(k)
            .unwrap()
            .items
            .iter()
            .map(|e| e.rel().to_string())
            .collect();
        v.sort();
        v
    }

    #[test]
    fn add_remove_rename() {
        let (c, k) = setup();
        c.apply_changes(&[PathChange::Added(
            PathBuf::from("/r/new.txt"),
            EntryKind::File,
        )]);
        c.apply_changes(&[PathChange::Added(
            PathBuf::from("/r/new.txt"),
            EntryKind::File,
        )]);
        assert_eq!(c.len_of(&k), Some(4));
        c.apply_changes(&[PathChange::Removed(PathBuf::from("/r/a.txt"))]);
        assert!(!rels(&c, &k).contains(&"a.txt".to_string()));
        c.apply_changes(&[PathChange::Renamed {
            from: PathBuf::from("/r/d"),
            to: PathBuf::from("/r/z"),
        }]);
        assert_eq!(rels(&c, &k), vec!["new.txt", "z/b.txt", "z/e/c.txt"]);
        c.apply_changes(&[PathChange::Removed(PathBuf::from("/r/z"))]);
        assert_eq!(rels(&c, &k), vec!["new.txt"]);
    }

    fn disk_setup(include_dirs: bool) -> (tempfile::TempDir, WalkCache, CacheKey) {
        let t = tempfile::tempdir().unwrap();
        let key = CacheKey {
            root: t.path().to_path_buf(),
            show_hidden: false,
            respect_gitignore: true,
            include_dirs,
        };
        let c = WalkCache::new();
        c.put(key.clone(), vec![item("a.txt")]);
        (t, c, key)
    }

    fn write(root: &Path, rel: &str) {
        let p = root.join(rel);
        std::fs::create_dir_all(p.parent().unwrap()).unwrap();
        std::fs::write(p, "x").unwrap();
    }

    #[test]
    fn added_directory_is_scanned() {
        let (t, c, k) = disk_setup(false);
        write(t.path(), "newdir/a.rs");
        write(t.path(), "newdir/deep/er/b.rs");
        write(t.path(), "newdir/.hidden");
        c.apply_changes(&[PathChange::Added(t.path().join("newdir"), EntryKind::Dir)]);
        let sep = std::path::MAIN_SEPARATOR;
        let mut want = vec![
            "a.txt".to_string(),
            format!("newdir{sep}a.rs"),
            format!("newdir{sep}deep{sep}er{sep}b.rs"),
        ];
        want.sort();
        assert_eq!(rels(&c, &k), want);
        // Applying it again adds nothing.
        c.apply_changes(&[PathChange::Added(t.path().join("newdir"), EntryKind::Dir)]);
        assert_eq!(c.len_of(&k), Some(3));
    }

    #[test]
    fn added_directory_is_listed_and_scanned_with_include_dirs() {
        let (t, c, k) = disk_setup(true);
        write(t.path(), "d/e/f.txt");
        c.apply_changes(&[PathChange::Added(t.path().join("d"), EntryKind::Dir)]);
        let sep = std::path::MAIN_SEPARATOR;
        assert_eq!(
            rels(&c, &k),
            vec![
                "a.txt".to_string(),
                "d".to_string(),
                format!("d{sep}e"),
                format!("d{sep}e{sep}f.txt"),
            ]
        );
    }

    #[test]
    fn touched_resolves_through_the_filesystem() {
        let (t, c, k) = disk_setup(false);
        write(t.path(), "made/x.txt");
        write(t.path(), "single.txt");
        c.apply_changes(&[
            PathChange::Touched(t.path().join("made")),
            PathChange::Touched(t.path().join("single.txt")),
            PathChange::Touched(t.path().join("a.txt")),
        ]);
        // a.txt does not exist on disk: Touched removes it.
        let sep = std::path::MAIN_SEPARATOR;
        assert_eq!(
            rels(&c, &k),
            vec![format!("made{sep}x.txt"), "single.txt".to_string()]
        );
    }

    #[test]
    fn moved_in_directory_is_scanned() {
        let (t, c, k) = disk_setup(false);
        write(t.path(), "moved/in.txt");
        let outside = tempfile::tempdir().unwrap();
        c.apply_changes(&[PathChange::Renamed {
            from: outside.path().join("moved"),
            to: t.path().join("moved"),
        }]);
        let sep = std::path::MAIN_SEPARATOR;
        assert!(rels(&c, &k).contains(&format!("moved{sep}in.txt")));
    }

    #[test]
    fn hidden_and_foreign_ignored() {
        let (c, k) = setup();
        c.apply_changes(&[
            PathChange::Added(PathBuf::from("/r/.secret"), EntryKind::File),
            PathChange::Added(PathBuf::from("/other/x"), EntryKind::File),
            PathChange::Added(PathBuf::from("/r/sub"), EntryKind::Dir),
        ]);
        assert_eq!(c.len_of(&k), Some(3));
    }

    #[test]
    fn eviction_and_invalidate() {
        let c = WalkCache::new();
        for i in 0..(WalkCache::MAX_ROOTS + 2) {
            c.put(
                CacheKey {
                    root: PathBuf::from(format!("/r{i}")),
                    show_hidden: false,
                    respect_gitignore: true,
                    include_dirs: false,
                },
                vec![],
            );
        }
        assert_eq!(c.inner.lock().len(), WalkCache::MAX_ROOTS);
        c.invalidate(Path::new(&format!("/r{}", WalkCache::MAX_ROOTS + 1)));
        assert_eq!(c.inner.lock().len(), WalkCache::MAX_ROOTS - 1);
    }

    #[test]
    fn lru_eviction_refreshes_on_get() {
        let c = WalkCache::new();
        let key = |i: usize| CacheKey {
            root: PathBuf::from(format!("/lru{i}")),
            show_hidden: false,
            respect_gitignore: true,
            include_dirs: false,
        };
        for i in 0..WalkCache::MAX_ROOTS {
            c.put(key(i), vec![]);
        }
        // Touch the oldest, then overflow: the touched one must survive.
        assert!(c.get(&key(0)).is_some());
        c.put(key(99), vec![]);
        assert!(c.contains(&key(0)));
        assert!(!c.contains(&key(1)));
    }

    #[test]
    fn toggles_make_distinct_keys() {
        let (c, k) = setup();
        let mut hidden = k.clone();
        hidden.show_hidden = true;
        assert_ne!(k, hidden);
        assert!(!c.contains(&hidden));
        let mut gi = k.clone();
        gi.respect_gitignore = false;
        assert!(!c.contains(&gi));
    }

    #[test]
    fn many_added_in_one_batch_dedup() {
        let (c, k) = setup();
        let batch: Vec<_> = (0..500)
            .map(|i| {
                PathChange::Added(
                    PathBuf::from(format!("/r/n{}.txt", i % 250)),
                    EntryKind::File,
                )
            })
            .collect();
        c.apply_changes(&batch);
        assert_eq!(c.len_of(&k), Some(3 + 250));
    }

    #[cfg(windows)]
    #[test]
    fn windows_spellings_match() {
        let key = CacheKey {
            root: PathBuf::from(r"C:\Work\Proj"),
            show_hidden: false,
            respect_gitignore: true,
            include_dirs: false,
        };
        let c = WalkCache::new();
        c.put(
            key.clone(),
            vec![item(r"src\main.rs"), item(r"src\deep\x.rs"), item("a.txt")],
        );
        // Same root, different spelling: same key.
        let alt = CacheKey {
            root: PathBuf::from(r"\\?\c:\work\PROJ\"),
            ..key.clone()
        };
        assert_eq!(alt, key);
        assert!(c.contains(&alt));
        // Verbatim + different case watcher paths still apply.
        c.apply_changes(&[
            PathChange::Added(PathBuf::from(r"\\?\c:\WORK\proj\New.rs"), EntryKind::File),
            PathChange::Added(PathBuf::from("c:/work/proj/NEW.RS"), EntryKind::File),
            PathChange::Removed(PathBuf::from(r"c:\work\proj\A.TXT")),
            PathChange::Renamed {
                from: PathBuf::from(r"C:\WORK\PROJ\SRC"),
                to: PathBuf::from(r"C:\work\proj\lib"),
            },
        ]);
        let mut got = rels(&c, &key);
        got.sort();
        assert_eq!(got, vec![r"New.rs", r"lib\deep\x.rs", r"lib\main.rs"]);
        c.invalidate(Path::new(r"\\?\C:\work\proj"));
        assert!(!c.contains(&key));
    }

    #[test]
    fn a_touched_path_that_cannot_be_inspected_is_not_removed() {
        let t = tempfile::tempdir().unwrap();
        let key = CacheKey {
            root: t.path().to_path_buf(),
            show_hidden: false,
            respect_gitignore: true,
            include_dirs: false,
        };
        let cache = WalkCache::new();
        cache.put(
            key.clone(),
            vec![FinderItem::new("keep.txt", EntryKind::File)],
        );
        // A path with an interior NUL fails with InvalidInput, not NotFound.
        let bad = t.path().join("keep.txt x");
        cache.apply_changes(&[PathChange::Touched(bad)]);
        assert_eq!(cache.len_of(&key), Some(1));
        // A genuinely missing file is removed.
        cache.apply_changes(&[PathChange::Touched(t.path().join("keep.txt"))]);
        assert_eq!(cache.len_of(&key), Some(0));
    }

    #[test]
    fn a_stale_refresh_does_not_overwrite_a_newer_incremental_update() {
        let t = tempfile::tempdir().unwrap();
        let key = CacheKey {
            root: t.path().to_path_buf(),
            show_hidden: false,
            respect_gitignore: true,
            include_dirs: false,
        };
        let cache = WalkCache::new();
        cache.put(key.clone(), vec![FinderItem::new("a.txt", EntryKind::File)]);
        let snapshot = cache.get(&key).unwrap();
        // An incremental change lands while the re-walk is running ...
        cache.apply_changes(&[PathChange::Added(t.path().join("b.txt"), EntryKind::File)]);
        // ... so the (older) re-walk result must not replace it.
        let fresh = Arc::new(Walked {
            items: vec![FinderItem::new("a.txt", EntryKind::File)],
            raws: RawNames::new(),
        });
        cache.put_walked_if_unchanged(key.clone(), &snapshot, fresh.clone());
        assert_eq!(cache.len_of(&key), Some(2));
        // With nothing in between the result is stored.
        let current = cache.get(&key).unwrap();
        cache.put_walked_if_unchanged(key.clone(), &current, fresh);
        assert_eq!(cache.len_of(&key), Some(1));
    }
}
