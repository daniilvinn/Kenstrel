//! The "This PC" view: the list of drives and the cursor over it.

use std::path::{Path, PathBuf};

use crate::cursor::Cursor;

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum DriveKind {
    #[default]
    Unknown,
    Fixed,
    Removable,
    Network,
    Optical,
    RamDisk,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct DriveInfo {
    /// The root directory, e.g. `C:\`.
    pub root: PathBuf,
    /// Volume label (may be empty).
    pub label: String,
    /// File system name (`NTFS`, `exFAT`, ...), empty when unknown.
    pub fs: String,
    pub kind: DriveKind,
    /// Capacity in bytes, `None` when the drive cannot be queried (e.g. no disc).
    pub total: Option<u64>,
    pub free: Option<u64>,
}

impl DriveInfo {
    pub fn new(root: impl Into<PathBuf>) -> Self {
        Self {
            root: root.into(),
            label: String::new(),
            fs: String::new(),
            kind: DriveKind::Unknown,
            total: None,
            free: None,
        }
    }

    /// Fraction of the capacity in use, `0.0..=1.0`.
    pub fn used_fraction(&self) -> Option<f64> {
        let (total, free) = (self.total?, self.free?);
        if total == 0 {
            return None;
        }
        Some((total.saturating_sub(free)) as f64 / total as f64)
    }
}

/// State of the drives view.
#[derive(Clone, Debug, Default)]
pub struct DrivesView {
    pub drives: Vec<DriveInfo>,
    pub cursor: Cursor,
}

impl DrivesView {
    /// Installs a fresh drive list and puts the cursor on the drive containing `near`.
    pub fn set(&mut self, drives: Vec<DriveInfo>, near: &Path, height: usize) {
        let index = drive_index_for(&drives, near).unwrap_or(0);
        self.drives = drives;
        self.cursor = Cursor::default();
        self.cursor.set(index, self.drives.len());
        self.cursor.reveal(self.drives.len(), height);
    }

    pub fn current(&self) -> Option<&DriveInfo> {
        self.drives.get(self.cursor.index)
    }
}

/// Index of the drive whose root is the longest prefix of `path` (case-insensitive, as
/// Windows paths are).
fn drive_index_for(drives: &[DriveInfo], path: &Path) -> Option<usize> {
    let wanted = lower(path);
    drives
        .iter()
        .enumerate()
        .filter(|(_, d)| wanted.starts_with(&lower(&d.root)))
        .max_by_key(|(_, d)| d.root.as_os_str().len())
        .map(|(i, _)| i)
}

fn lower(path: &Path) -> String {
    path.to_string_lossy().to_lowercase()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn drives() -> Vec<DriveInfo> {
        ["C:/", "D:/", "E:/"]
            .into_iter()
            .map(DriveInfo::new)
            .collect()
    }

    #[test]
    fn cursor_lands_on_the_drive_of_the_previous_directory() {
        let mut view = DrivesView::default();
        view.set(drives(), Path::new("d:/Users/me"), 10);
        assert_eq!(view.current().unwrap().root, Path::new("D:/"));
        view.set(drives(), Path::new("Z:/nowhere"), 10);
        assert_eq!(view.cursor.index, 0);
    }

    #[test]
    fn longest_root_wins() {
        let mut list = drives();
        list.push(DriveInfo::new("C:/mnt/data"));
        assert_eq!(drive_index_for(&list, Path::new("C:/mnt/data/x")), Some(3));
        assert_eq!(drive_index_for(&list, Path::new("C:/mnt/other")), Some(0));
    }

    #[test]
    fn used_fraction() {
        let mut d = DriveInfo::new("C:/");
        assert_eq!(d.used_fraction(), None);
        d.total = Some(200);
        d.free = Some(50);
        assert_eq!(d.used_fraction(), Some(0.75));
        d.total = Some(0);
        assert_eq!(d.used_fraction(), None);
    }
}
