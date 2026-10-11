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

    #[cfg(windows)]
    #[test]
    fn the_drive_signature_holds_every_listed_letter_and_is_stable() {
        let signature = drives_signature();
        for drive in list_drives() {
            let letter = drive.root.to_string_lossy().as_bytes()[0].to_ascii_uppercase();
            assert_ne!(signature & (1 << (letter - b'A')), 0, "{drive:?}");
        }
        assert_eq!(drives_signature(), signature);
    }

    #[test]
    fn utc_offset_is_a_plausible_zone() {
        let offset = utc_offset_secs();
        assert!((-12 * 3600..=14 * 3600).contains(&offset), "{offset}");
        assert_eq!(offset % 900, 0, "zones are multiples of 15 minutes");
    }

    #[test]
    fn own_pid_is_alive_and_an_exited_child_is_not() {
        assert!(pid_alive(std::process::id()));
        let program = if cfg!(windows) { "cmd" } else { "true" };
        let mut cmd = std::process::Command::new(program);
        if cfg!(windows) {
            cmd.args(["/c", "exit", "0"]);
        }
        let mut child = cmd.spawn().unwrap();
        let pid = child.id();
        child.wait().unwrap();
        if cfg!(any(windows, target_os = "linux")) {
            assert!(!pid_alive(pid));
        }
    }

    #[cfg(windows)]
    #[test]
    fn reparse_tag_of_a_plain_directory_is_none() {
        let dir = std::env::temp_dir();
        assert_eq!(reparse_tag(&dir), None);
    }
}

#[cfg(all(test, windows))]
mod copy_tests {
    use super::*;

    #[test]
    fn copy_file_ex_reports_progress_and_can_cancel() {
        let td = tempfile::tempdir().unwrap();
        let src = td.path().join("big.bin");
        std::fs::write(&src, vec![7u8; 64 * 1024 * 1024]).unwrap();
        let mut calls = 0u32;
        let mut total = 0u64;
        let ok = copy_file_ex(&src, &td.path().join("a.bin"), &mut |n| {
            calls += 1;
            total += n;
            true
        })
        .unwrap();
        assert!(ok);
        assert_eq!(total, 64 * 1024 * 1024);
        assert!(calls > 1, "progress arrives in chunks");
        let cancelled = td.path().join("b.bin");
        let ok = copy_file_ex(&src, &cancelled, &mut |_| false).unwrap();
        assert!(!ok);
        assert!(!cancelled.exists(), "system removes the cancelled target");
        let exists = copy_file_ex(&src, &td.path().join("a.bin"), &mut |_| true);
        assert!(exists.is_err(), "never overwrites");
    }
}
