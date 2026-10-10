//! Parallel, cancellable directory walker built on [`ignore::WalkParallel`].
//!
//! * Never follows symlinks or junctions (`follow_links(false)`); link entries are reported
//!   with [`EntryKind::Symlink`] but never descended into.
//! * Skips `System Volume Information`, `$Recycle.Bin`, `pagefile.sys`, `hiberfil.sys`,
//!   `swapfile.sys` and always the `.git` directory.
//! * Access-denied errors are skipped quietly and counted in [`WalkSummary::denied`].
//! * Cancellation: every visitor callback checks the token and returns
//!   [`ignore::WalkState::Quit`], which makes all walker threads stop at their next entry.
//!
//! [`walk`] is **synchronous**: it blocks the calling thread until the walk finishes (it spawns
//! its own worker threads internally). Call it from a dedicated thread.

use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Arc;

use ignore::overrides::Override;
use ignore::{DirEntry, WalkBuilder, WalkState};
use tokio_util::sync::CancellationToken;

/// File names that are never visited (compared case-insensitively).
pub const SKIPPED_NAMES: &[&str] = &[
    "system volume information",
    "$recycle.bin",
    "pagefile.sys",
    "hiberfil.sys",
    "swapfile.sys",
];

/// Kind of a walked entry.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum EntryKind {
    /// Regular file (or anything that is not a dir or link).
    File,
    /// Directory.
    Dir,
    /// Symlink or junction (never followed).
    Symlink,
}

/// Walker configuration.
#[derive(Debug, Clone)]
pub struct WalkOptions {
    /// Directory to walk.
    pub root: PathBuf,
    /// Include hidden files and directories. The `.git` directory itself and the built-in
    /// [`SKIPPED_NAMES`] are **always** skipped, even when this is `true`.
    pub show_hidden: bool,
    /// Honour `.gitignore`, `.ignore`, global and `.git/info/exclude` rules (also outside git
    /// repositories, and in parent directories).
    pub respect_gitignore: bool,
    /// Report directories as entries too (files and symlinks are always reported).
    pub include_dirs: bool,
    /// Walker threads; `0` lets the `ignore` crate decide.
    pub threads: usize,
    /// Optional glob overrides (see [`build_overrides`]).
    pub overrides: Option<Override>,
    /// Do not descend into directories carrying cloud-placeholder attributes (OneDrive and
    /// similar, see [`is_cloud_placeholder`]); enumerating them can trigger downloads. Off by
    /// default because such folders are usually still worth listing by name. Skipped
    /// directories are counted in [`WalkSummary::skipped_cloud`].
    pub skip_cloud_dirs: bool,
}

impl WalkOptions {
    /// Defaults: hidden files off, gitignore on, files only, automatic threads.
    pub fn new(root: impl Into<PathBuf>) -> Self {
        Self {
            root: root.into(),
            show_hidden: false,
            respect_gitignore: true,
            include_dirs: false,
            threads: 0,
            overrides: None,
            skip_cloud_dirs: false,
        }
    }
}

/// Builds glob overrides from a user-typed filter field. Globs are separated by whitespace or
/// commas; `*.rs` whitelists, `!target/**` excludes.
pub fn build_overrides(root: &Path, globs: &str) -> Result<Option<Override>, ignore::Error> {
    let mut any = false;
    let mut b = ignore::overrides::OverrideBuilder::new(root);
    for g in globs
        .split(|c: char| c.is_whitespace() || c == ',')
        .filter(|g| !g.is_empty())
    {
        b.add(g)?;
        any = true;
    }
    if any {
        Ok(Some(b.build()?))
    } else {
        Ok(None)
    }
}

/// One walked entry.
#[derive(Debug)]
pub struct WalkEntry {
    /// Underlying directory entry (gives access to metadata).
    pub inner: DirEntry,
    /// Classified kind.
    pub kind: EntryKind,
}

impl WalkEntry {
    /// Absolute (root-joined) path.
    pub fn path(&self) -> &Path {
        self.inner.path()
    }

    /// Path relative to `root`.
    pub fn rel<'a>(&'a self, root: &Path) -> &'a Path {
        self.inner.path().strip_prefix(root).unwrap_or(self.path())
    }

    /// Consumes the entry, returning its path.
    pub fn into_path(self) -> PathBuf {
        self.inner.into_path()
    }
}

/// Per-thread receiver of walked entries. A fresh sink is created for every walker thread (via
/// the factory passed to [`walk`]) and dropped when that thread finishes, so buffering sinks
/// can flush in `Drop`.
pub trait WalkSink: Send {
    /// Receives an entry. Return `false` to stop the whole walk.
    fn entry(&mut self, entry: WalkEntry) -> bool;
}

impl<F: FnMut(WalkEntry) -> bool + Send> WalkSink for F {
    fn entry(&mut self, entry: WalkEntry) -> bool {
        self(entry)
    }
}

/// Outcome counters of a walk.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct WalkSummary {
    /// Entries delivered to sinks.
    pub entries: u64,
    /// Access-denied errors that were skipped quietly.
    pub denied: u64,
    /// Other IO errors that were skipped (vanished files, ...).
    pub errors: u64,
    /// Entries skipped by the built-in skip list.
    pub skipped_system: u64,
    /// Cloud-placeholder directories not descended into (see [`WalkOptions::skip_cloud_dirs`]).
    pub skipped_cloud: u64,
    /// Whether the walk ended early because of cancellation (or a sink returning `false`).
    pub cancelled: bool,
}

#[derive(Default)]
struct Counters {
    entries: AtomicU64,
    denied: AtomicU64,
    errors: AtomicU64,
    skipped_system: AtomicU64,
    skipped_cloud: AtomicU64,
}

fn is_denied(err: &ignore::Error) -> bool {
    match err {
        ignore::Error::Partial(errs) => errs.iter().any(is_denied),
        other => other
            .io_error()
            .is_some_and(|e| e.kind() == std::io::ErrorKind::PermissionDenied),
    }
}

fn is_skipped_name(name: &std::ffi::OsStr) -> bool {
    let Some(s) = name.to_str() else { return false };
    s == ".git"
        || SKIPPED_NAMES
            .iter()
            .any(|skip| s.len() == skip.len() && s.eq_ignore_ascii_case(skip))
}

/// Walks `opts.root` in parallel, creating one sink per walker thread with `make_sink`.
///
/// Blocks until finished or cancelled. Walker threads stop within one entry of `cancel` being
/// triggered.
pub fn walk<S, F>(opts: &WalkOptions, cancel: &CancellationToken, make_sink: F) -> WalkSummary
where
    S: WalkSink,
    F: Fn() -> S + Sync,
{
    let counters = Arc::new(Counters::default());
    let stopped = Arc::new(std::sync::atomic::AtomicBool::new(false));

    let mut b = WalkBuilder::new(&opts.root);
    let git = opts.respect_gitignore;
    b.follow_links(false)
        .hidden(!opts.show_hidden)
        .git_ignore(git)
        .git_global(git)
        .git_exclude(git)
        .ignore(git)
        .parents(git)
        .require_git(false)
        .threads(opts.threads);
    if let Some(o) = &opts.overrides {
        b.overrides(o.clone());
    }
    let skip_counters = counters.clone();
    let skip_cloud_dirs = opts.skip_cloud_dirs;
    b.filter_entry(move |e| {
        if e.depth() == 0 {
            return true;
        }
        if is_skipped_name(e.file_name()) {
            skip_counters.skipped_system.fetch_add(1, Ordering::Relaxed);
            return false;
        }
        // Metadata of a directory entry comes from the enumeration data on Windows, so this is
        // not an extra stat.
        if skip_cloud_dirs
            && e.file_type().is_some_and(|t| t.is_dir())
            && e.metadata()
                .is_ok_and(|md| is_cloud_placeholder(file_attributes(&md)))
        {
            skip_counters.skipped_cloud.fetch_add(1, Ordering::Relaxed);
            return false;
        }
        true
    });

    let include_dirs = opts.include_dirs;
    b.build_parallel().run(|| {
        let mut sink = make_sink();
        let counters = counters.clone();
        let stopped = stopped.clone();
        let cancel = cancel.clone();
        Box::new(move |res| {
            if cancel.is_cancelled() || stopped.load(Ordering::Relaxed) {
                return WalkState::Quit;
            }
            let inner = match res {
                Ok(e) => e,
                Err(err) => {
                    if is_denied(&err) {
                        counters.denied.fetch_add(1, Ordering::Relaxed);
                    } else {
                        counters.errors.fetch_add(1, Ordering::Relaxed);
                        tracing::debug!(error = %err, "walk error skipped");
                    }
                    return WalkState::Continue;
                }
            };
            if inner.depth() == 0 {
                return WalkState::Continue;
            }
            let kind = match inner.file_type() {
                Some(ft) if ft.is_dir() => EntryKind::Dir,
                Some(ft) if ft.is_symlink() => EntryKind::Symlink,
                _ => EntryKind::File,
            };
            if kind == EntryKind::Dir && !include_dirs {
                return WalkState::Continue;
            }
            counters.entries.fetch_add(1, Ordering::Relaxed);
            if sink.entry(WalkEntry { inner, kind }) {
                WalkState::Continue
            } else {
                stopped.store(true, Ordering::Relaxed);
                WalkState::Quit
            }
        })
    });

    WalkSummary {
        entries: counters.entries.load(Ordering::Relaxed),
        denied: counters.denied.load(Ordering::Relaxed),
        errors: counters.errors.load(Ordering::Relaxed),
        skipped_system: counters.skipped_system.load(Ordering::Relaxed),
        skipped_cloud: counters.skipped_cloud.load(Ordering::Relaxed),
        cancelled: cancel.is_cancelled() || stopped.load(Ordering::Relaxed),
    }
}

/// Windows file attributes of a path's metadata (always `0` on other platforms).
pub fn file_attributes(md: &std::fs::Metadata) -> u32 {
    #[cfg(windows)]
    {
        use std::os::windows::fs::MetadataExt;
        md.file_attributes()
    }
    #[cfg(not(windows))]
    {
        let _ = md;
        0
    }
}

/// `FILE_ATTRIBUTE_OFFLINE`
pub const ATTR_OFFLINE: u32 = 0x1000;
/// `FILE_ATTRIBUTE_RECALL_ON_OPEN`
pub const ATTR_RECALL_ON_OPEN: u32 = 0x40000;
/// `FILE_ATTRIBUTE_RECALL_ON_DATA_ACCESS`
pub const ATTR_RECALL_ON_DATA_ACCESS: u32 = 0x400000;

/// Whether the attributes mark a cloud placeholder whose content must not be read.
pub fn is_cloud_placeholder(attrs: u32) -> bool {
    attrs & (ATTR_OFFLINE | ATTR_RECALL_ON_OPEN | ATTR_RECALL_ON_DATA_ACCESS) != 0
}

#[cfg(test)]
#[allow(clippy::print_stderr)]
mod tests {
    use super::*;
    use std::collections::BTreeSet;
    use std::fs;
    use std::sync::Mutex;

    fn touch(root: &Path, rel: &str) {
        let p = root.join(rel);
        fs::create_dir_all(p.parent().unwrap()).unwrap();
        fs::write(p, "x").unwrap();
    }

    fn collect(opts: &WalkOptions) -> BTreeSet<String> {
        let out = Mutex::new(BTreeSet::new());
        let root = opts.root.clone();
        walk(opts, &CancellationToken::new(), || {
            let out = &out;
            let root = root.clone();
            move |e: WalkEntry| {
                out.lock()
                    .unwrap()
                    .insert(e.rel(&root).to_string_lossy().replace('\\', "/"));
                true
            }
        });
        out.into_inner().unwrap()
    }

    #[test]
    fn gitignore_and_hidden_toggles() {
        let t = tempfile::tempdir().unwrap();
        touch(t.path(), "a.txt");
        touch(t.path(), ".hidden");
        touch(t.path(), "ignored/x.txt");
        touch(t.path(), "sub/b.txt");
        fs::write(t.path().join(".gitignore"), "ignored/\n").unwrap();

        let mut o = WalkOptions::new(t.path());
        let base = collect(&o);
        assert!(base.contains("a.txt") && base.contains("sub/b.txt"));
        assert!(!base.contains(".hidden"));
        assert!(!base.contains("ignored/x.txt"));

        o.show_hidden = true;
        let hidden = collect(&o);
        assert!(hidden.contains(".hidden") && hidden.contains(".gitignore"));
        assert!(!hidden.contains("ignored/x.txt"));

        o.respect_gitignore = false;
        let all = collect(&o);
        assert!(all.contains("ignored/x.txt"));
    }

    #[test]
    fn system_names_and_dot_git_skipped() {
        let t = tempfile::tempdir().unwrap();
        touch(t.path(), "keep.txt");
        touch(t.path(), "$Recycle.Bin/x");
        touch(t.path(), "System Volume Information/y");
        touch(t.path(), "pagefile.sys");
        touch(t.path(), ".git/config");
        let mut o = WalkOptions::new(t.path());
        o.show_hidden = true;
        o.respect_gitignore = false;
        let got = collect(&o);
        assert_eq!(got.into_iter().collect::<Vec<_>>(), vec!["keep.txt"]);
    }

    #[test]
    fn symlinks_not_followed() {
        let t = tempfile::tempdir().unwrap();
        touch(t.path(), "real/inner.txt");
        let link = t.path().join("link");
        #[cfg(unix)]
        let made = std::os::unix::fs::symlink(t.path().join("real"), &link).is_ok();
        #[cfg(windows)]
        let made = std::os::windows::fs::symlink_dir(t.path().join("real"), &link).is_ok();
        if !made {
            eprintln!("SKIPPED symlinks_not_followed: symlink_dir not permitted here");
            return;
        }
        let got = collect(&WalkOptions::new(t.path()));
        assert!(got.contains("real/inner.txt"));
        assert!(!got.contains("link/inner.txt"), "{got:?}");
    }

    #[test]
    fn cancel_before_start_yields_nothing() {
        let t = tempfile::tempdir().unwrap();
        for i in 0..50 {
            touch(t.path(), &format!("d{i}/f.txt"));
        }
        let cancel = CancellationToken::new();
        cancel.cancel();
        let n = Mutex::new(0u32);
        let s = walk(&WalkOptions::new(t.path()), &cancel, || {
            |_e: WalkEntry| {
                *n.lock().unwrap() += 1;
                true
            }
        });
        assert!(s.cancelled);
        assert_eq!(*n.lock().unwrap(), 0);
    }

    #[test]
    fn include_dirs_reports_dirs() {
        let t = tempfile::tempdir().unwrap();
        touch(t.path(), "d/f.txt");
        let mut o = WalkOptions::new(t.path());
        o.include_dirs = true;
        let got = collect(&o);
        assert!(got.contains("d") && got.contains("d/f.txt"));
    }

    #[test]
    fn overrides_parse() {
        let t = tempfile::tempdir().unwrap();
        assert!(build_overrides(t.path(), "  ").unwrap().is_none());
        assert!(build_overrides(t.path(), "*.rs, !target/**")
            .unwrap()
            .is_some());
    }

    #[test]
    fn cloud_attrs() {
        assert!(is_cloud_placeholder(ATTR_RECALL_ON_DATA_ACCESS));
        assert!(is_cloud_placeholder(ATTR_OFFLINE | 0x20));
        assert!(!is_cloud_placeholder(0x20));
    }
}
