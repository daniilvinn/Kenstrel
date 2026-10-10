//! Operating-system specific pieces behind one interface.

#[cfg(not(windows))]
mod fallback;
#[cfg(windows)]
mod win32;

#[cfg(not(windows))]
pub use fallback::*;
#[cfg(windows)]
pub use win32::*;

#[cfg(test)]
mod tests {
    #![allow(clippy::print_stderr)]

    use super::*;

    #[test]
    fn there_is_at_least_one_drive_with_a_root() {
        let drives = list_drives();
        assert!(!drives.is_empty());
        assert!(drives.iter().all(|d| d.root.is_absolute()));
    }

    #[cfg(windows)]
    #[test]
    fn windows_drives_report_label_filesystem_and_space() {
        let drives = list_drives();
        let system = std::env::var("SystemDrive").unwrap_or_else(|_| "C:".into());
        let drive = drives
            .iter()
            .find(|d| {
                d.root
                    .to_string_lossy()
                    .eq_ignore_ascii_case(&format!("{system}\\"))
            })
            .expect("the system drive is listed");
        assert_eq!(drive.kind, ks_core::DriveKind::Fixed);
        assert!(!drive.fs.is_empty(), "file system name");
        let (total, free) = (drive.total.unwrap(), drive.free.unwrap());
        assert!(total > 0 && free <= total, "{free} / {total}");
        let fraction = drive.used_fraction().unwrap();
        assert!((0.0..=1.0).contains(&fraction));
        eprintln!("{drive:?}");
    }

    #[test]
    fn utc_offset_is_a_plausible_zone() {
        let offset = utc_offset_secs();
        assert!((-12 * 3600..=14 * 3600).contains(&offset), "{offset}");
        assert_eq!(offset % 900, 0, "zones are multiples of 15 minutes");
    }

    #[cfg(windows)]
    #[test]
    fn reparse_tag_of_a_plain_directory_is_none() {
        let dir = std::env::temp_dir();
        assert_eq!(reparse_tag(&dir), None);
    }
}
