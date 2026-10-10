//! Translating raw file attributes into the UI's [`Attrs`]. Pure functions, so the Windows
//! rules are tested on every platform.

use std::ffi::OsStr;
use std::path::Path;

use ks_core::Attrs;

/// `FILE_ATTRIBUTE_*` values from the Windows SDK.
pub mod win {
    pub const READONLY: u32 = 0x0000_0001;
    pub const HIDDEN: u32 = 0x0000_0002;
    pub const SYSTEM: u32 = 0x0000_0004;
    pub const DIRECTORY: u32 = 0x0000_0010;
    pub const REPARSE_POINT: u32 = 0x0000_0400;
    /// `FILE_ATTRIBUTE_OFFLINE`: the data is not available locally.
    pub const OFFLINE: u32 = 0x0000_1000;
    /// `FILE_ATTRIBUTE_RECALL_ON_OPEN`: opening it (or a directory) fetches content.
    pub const RECALL_ON_OPEN: u32 = 0x0004_0000;
    /// `FILE_ATTRIBUTE_RECALL_ON_DATA_ACCESS`: reading it downloads it (OneDrive).
    pub const RECALL_ON_DATA_ACCESS: u32 = 0x0040_0000;

    /// `IO_REPARSE_TAG_MOUNT_POINT`: an NTFS junction.
    pub const REPARSE_TAG_MOUNT_POINT: u32 = 0xA000_0003;
}

/// Extensions Windows runs directly.
const WINDOWS_EXECUTABLE: &[&str] = &["exe", "bat", "cmd", "com", "ps1", "msi", "scr"];

/// Attributes from a Windows `dwFileAttributes` value.
pub fn from_windows(raw: u32) -> Attrs {
    let mut attrs = Attrs::NONE;
    let flag = |attrs: Attrs, mask: u32, flag: Attrs| {
        if raw & mask != 0 {
            attrs | flag
        } else {
            attrs
        }
    };
    attrs = flag(attrs, win::HIDDEN, Attrs::HIDDEN);
    attrs = flag(attrs, win::SYSTEM, Attrs::SYSTEM);
    attrs = flag(attrs, win::READONLY, Attrs::READONLY);
    attrs = flag(
        attrs,
        win::OFFLINE | win::RECALL_ON_OPEN | win::RECALL_ON_DATA_ACCESS,
        Attrs::CLOUD,
    );
    attrs
}

/// True when Windows would run a file with this name directly.
pub fn is_windows_executable(name: &OsStr) -> bool {
    Path::new(name)
        .extension()
        .and_then(OsStr::to_str)
        .is_some_and(|ext| {
            WINDOWS_EXECUTABLE
                .iter()
                .any(|known| known.eq_ignore_ascii_case(ext))
        })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn plain_attributes() {
        assert_eq!(from_windows(0), Attrs::NONE);
        let attrs = from_windows(win::HIDDEN | win::SYSTEM | win::READONLY);
        assert!(attrs.contains(Attrs::HIDDEN));
        assert!(attrs.contains(Attrs::SYSTEM));
        assert!(attrs.contains(Attrs::READONLY));
        assert!(!attrs.contains(Attrs::CLOUD));
    }

    #[test]
    fn every_cloud_flavour_is_a_cloud_placeholder() {
        for raw in [
            win::OFFLINE,
            win::RECALL_ON_OPEN,
            win::RECALL_ON_DATA_ACCESS,
            win::RECALL_ON_DATA_ACCESS | win::REPARSE_POINT,
        ] {
            assert!(from_windows(raw).contains(Attrs::CLOUD), "{raw:#x}");
        }
        // A plain reparse point (symlink, junction) is not a cloud file.
        assert!(!from_windows(win::REPARSE_POINT | win::DIRECTORY).contains(Attrs::CLOUD));
    }

    #[test]
    fn executable_extensions_ignore_case() {
        for name in ["setup.EXE", "run.bat", "x.Ps1", "a.cmd"] {
            assert!(is_windows_executable(OsStr::new(name)), "{name}");
        }
        for name in ["readme.md", "exe", "archive.exe.txt", ".exe"] {
            assert!(!is_windows_executable(OsStr::new(name)), "{name}");
        }
    }
}
