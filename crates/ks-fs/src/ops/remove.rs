//! Delete (permanent and Recycle Bin), restore and create jobs.

use std::collections::HashSet;
use std::ffi::OsString;
use std::fs::{self, OpenOptions};
use std::io;
use std::path::Path;
use std::time::Instant;

use super::names::CreateTarget;
use super::paths::{clear_readonly, link_is_dir, path_key};
use super::runner::{Flow, Runner};
use super::transfer::Counts;
use super::types::{Outcome, TrashedItem};

/// Whether Recycle Bin listing / restoring is available on this platform.
pub const TRASH_RESTORE_SUPPORTED: bool = cfg!(any(windows, all(unix, not(target_os = "macos"))));

#[cfg(any(windows, all(unix, not(target_os = "macos"))))]
pub(crate) fn trash_list() -> Result<Vec<trash::TrashItem>, trash::Error> {
    trash::os_limited::list()
}

#[cfg(not(any(windows, all(unix, not(target_os = "macos")))))]
pub(crate) fn trash_list() -> Result<Vec<trash::TrashItem>, trash::Error> {
    Err(trash::Error::Unknown {
        description: "listing the trash is not supported here".into(),
    })
}

#[cfg(any(windows, all(unix, not(target_os = "macos"))))]
fn trash_restore(item: trash::TrashItem) -> Result<(), trash::Error> {
    trash::os_limited::restore_all([item])
}

#[cfg(not(any(windows, all(unix, not(target_os = "macos")))))]
fn trash_restore(_item: trash::TrashItem) -> Result<(), trash::Error> {
    Err(trash::Error::Unknown {
        description: "restoring from the trash is not supported here".into(),
    })
}

fn remove_with_retry(path: &Path, dir_link: bool) -> io::Result<()> {
    let attempt = || {
        if dir_link {
            fs::remove_dir(path)
        } else {
            fs::remove_file(path)
        }
    };
    match attempt() {
        Err(e) if e.kind() == io::ErrorKind::PermissionDenied => {
            clear_readonly(path);
            attempt()
        }
        r => r,
    }
}

impl Runner {
    /// Recursively delete `path` for good. Links are removed, never followed. Read-only
    /// attributes are cleared first. Returns whether everything was removed.
    pub(crate) fn delete_tree(&mut self, path: &Path, count: bool) -> Flow<bool> {
        self.check()?;
        let meta = match fs::symlink_metadata(path) {
            Ok(m) => m,
            Err(e) if e.kind() == io::ErrorKind::NotFound => return Ok(true),
            Err(e) => {
                self.error(path, e);
                return Ok(false);
            }
        };
        if meta.is_dir() {
            let rd = match fs::read_dir(path) {
                Ok(rd) => rd,
                Err(e) => {
                    self.error(path, e);
                    return Ok(false);
                }
            };
            let mut ok = true;
            for entry in rd {
                match entry {
                    Ok(en) => ok &= self.delete_tree(&en.path(), count)?,
                    Err(e) => {
                        self.error(path, e);
                        ok = false;
                    }
                }
            }
            if !ok {
                return Ok(false);
            }
            clear_readonly(path);
            match fs::remove_dir(path) {
                Ok(()) => Ok(true),
                Err(e) => {
                    self.error(path, e);
                    Ok(false)
                }
            }
        } else {
            self.set_current(path);
            let dir_link = meta.file_type().is_symlink() && link_is_dir(&meta);
            match remove_with_retry(path, dir_link) {
                Ok(()) => {
                    if count {
                        self.add_bytes(meta.len());
                        self.file_done();
                    }
                    Ok(true)
                }
                Err(e) => {
                    self.error(path, e);
                    Ok(false)
                }
            }
        }
    }

    pub(crate) fn run_delete(&mut self, paths: Vec<std::path::PathBuf>) -> Flow {
        let mut counts = Counts::default();
        let mut last = Instant::now();
        for p in &paths {
            self.scan_path(p, &mut counts, true, &mut last)?;
        }
        self.files_total = counts.files;
        self.bytes_total = counts.bytes;
        self.tick(true);
        for p in paths {
            if self.delete_tree(&p, true)? {
                self.items_done += 1;
                self.outcomes.push(Outcome::Deleted { path: p });
            }
        }
        Ok(())
    }

    pub(crate) fn run_trash(&mut self, paths: Vec<std::path::PathBuf>, track_ids: bool) -> Flow {
        self.files_total = paths.len() as u64;
        self.tick(true);
        let before: Option<HashSet<OsString>> = if track_ids && TRASH_RESTORE_SUPPORTED {
            match trash_list() {
                Ok(l) => Some(l.into_iter().map(|i| i.id).collect()),
                Err(e) => {
                    tracing::warn!(error = %e, "cannot list trash; undo of this trash is unavailable");
                    None
                }
            }
        } else {
            None
        };
        let mut trashed = Vec::new();
        let mut cancelled = false;
        for p in paths {
            if self.check().is_err() {
                cancelled = true;
                break;
            }
            self.set_current(&p);
            match trash::delete(&p) {
                Ok(()) => {
                    self.items_done += 1;
                    self.file_done();
                    trashed.push(p);
                }
                Err(e) => self.error(&p, e),
            }
        }
        let mut found: Vec<trash::TrashItem> = Vec::new();
        if let Some(before) = &before {
            match trash_list() {
                Ok(after) => {
                    found = after
                        .into_iter()
                        .filter(|i| !before.contains(&i.id))
                        .collect()
                }
                Err(e) => tracing::warn!(error = %e, "cannot list trash after deleting"),
            }
        }
        for p in trashed {
            let key = path_key(&p);
            let item = found
                .iter()
                .filter(|i| path_key(&i.original_path()) == key)
                .max_by_key(|i| i.time_deleted)
                .map(|i| TrashedItem {
                    original: p.clone(),
                    id: i.id.clone(),
                    time_deleted: i.time_deleted,
                });
            self.outcomes.push(Outcome::Trashed { path: p, item });
        }
        if cancelled {
            Err(super::runner::Cancelled)
        } else {
            Ok(())
        }
    }

    pub(crate) fn run_restore(&mut self, items: Vec<TrashedItem>) -> Flow {
        self.files_total = items.len() as u64;
        self.tick(true);
        let listing = match trash_list() {
            Ok(l) => l,
            Err(e) => {
                self.error(Path::new(""), format!("cannot read the trash: {e}"));
                return Ok(());
            }
        };
        for item in items {
            self.check()?;
            self.set_current(&item.original);
            let Some(found) = listing.iter().find(|t| t.id == item.id).cloned() else {
                self.error(&item.original, "no longer in the trash");
                continue;
            };
            match trash_restore(found) {
                Ok(()) => {
                    self.items_done += 1;
                    self.file_done();
                    self.outcomes.push(Outcome::Restored {
                        path: item.original,
                    });
                }
                Err(e) => self.error(&item.original, e),
            }
        }
        Ok(())
    }

    pub(crate) fn run_create(&mut self, entries: Vec<CreateTarget>) -> Flow {
        self.files_total = entries.len() as u64;
        self.tick(true);
        for t in entries {
            self.check()?;
            self.set_current(&t.path);
            if fs::symlink_metadata(&t.path).is_ok() {
                self.error(&t.path, "already exists");
                continue;
            }
            // Topmost ancestor that does not exist yet: that is what undo must remove.
            let mut top = t.path.clone();
            let mut cur = t.path.parent();
            while let Some(p) = cur {
                if p.as_os_str().is_empty() || fs::symlink_metadata(p).is_ok() {
                    break;
                }
                top = p.to_path_buf();
                cur = p.parent();
            }
            let result = (|| -> io::Result<()> {
                if let Some(parent) = t.path.parent() {
                    fs::create_dir_all(parent)?;
                }
                if t.is_dir {
                    fs::create_dir(&t.path)
                } else {
                    OpenOptions::new()
                        .write(true)
                        .create_new(true)
                        .open(&t.path)
                        .map(|_| ())
                }
            })();
            match result {
                Ok(()) => {
                    self.items_done += 1;
                    self.file_done();
                    let is_dir = t.is_dir || top != t.path;
                    self.outcomes.push(Outcome::Created { path: top, is_dir });
                }
                Err(e) => self.error(&t.path, e),
            }
        }
        Ok(())
    }
}
