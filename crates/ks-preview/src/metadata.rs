//! File metadata (the `i` toggle and the preview footer).

use std::fs;
use std::io;
use std::path::{Path, PathBuf};
use std::time::{SystemTime, UNIX_EPOCH};

use crate::model::{EntryKind, Role, SpanStyle, StyledLine, StyledSpan};
use crate::sanitize::sanitize_line;

pub const FILE_ATTRIBUTE_READONLY: u32 = 0x1;
pub const FILE_ATTRIBUTE_HIDDEN: u32 = 0x2;
pub const FILE_ATTRIBUTE_SYSTEM: u32 = 0x4;
pub const FILE_ATTRIBUTE_DIRECTORY: u32 = 0x10;
pub const FILE_ATTRIBUTE_ARCHIVE: u32 = 0x20;
pub const FILE_ATTRIBUTE_REPARSE_POINT: u32 = 0x400;
pub const FILE_ATTRIBUTE_COMPRESSED: u32 = 0x800;
pub const FILE_ATTRIBUTE_OFFLINE: u32 = 0x1000;
pub const FILE_ATTRIBUTE_ENCRYPTED: u32 = 0x4000;
pub const FILE_ATTRIBUTE_RECALL_ON_OPEN: u32 = 0x40000;
pub const FILE_ATTRIBUTE_RECALL_ON_DATA_ACCESS: u32 = 0x400000;

/// Attributes that mean "reading this file would trigger a cloud download".
pub const CLOUD_MASK: u32 =
    FILE_ATTRIBUTE_OFFLINE | FILE_ATTRIBUTE_RECALL_ON_OPEN | FILE_ATTRIBUTE_RECALL_ON_DATA_ACCESS;

/// Decoded file attributes. On non-Windows only `readonly`, `hidden` (dot-file) and
/// `directory` are meaningful.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct Attributes {
    pub raw: u32,
    pub readonly: bool,
    pub hidden: bool,
    pub system: bool,
    pub archive: bool,
    pub directory: bool,
    pub reparse_point: bool,
    pub compressed: bool,
    pub encrypted: bool,
    /// OneDrive-style placeholder: `OFFLINE`, `RECALL_ON_OPEN` or `RECALL_ON_DATA_ACCESS`.
    pub cloud_placeholder: bool,
}

impl Attributes {
    pub fn from_raw(raw: u32) -> Self {
        Self {
            raw,
            readonly: raw & FILE_ATTRIBUTE_READONLY != 0,
            hidden: raw & FILE_ATTRIBUTE_HIDDEN != 0,
            system: raw & FILE_ATTRIBUTE_SYSTEM != 0,
            archive: raw & FILE_ATTRIBUTE_ARCHIVE != 0,
            directory: raw & FILE_ATTRIBUTE_DIRECTORY != 0,
            reparse_point: raw & FILE_ATTRIBUTE_REPARSE_POINT != 0,
            compressed: raw & FILE_ATTRIBUTE_COMPRESSED != 0,
            encrypted: raw & FILE_ATTRIBUTE_ENCRYPTED != 0,
            cloud_placeholder: raw & CLOUD_MASK != 0,
        }
    }

    pub(crate) fn of(md: &fs::Metadata, name: &std::ffi::OsStr) -> Self {
        #[cfg(windows)]
        {
            use std::os::windows::fs::MetadataExt;
            let _ = name;
            Self::from_raw(md.file_attributes())
        }
        #[cfg(not(windows))]
        {
            let mut a = Self {
                readonly: md.permissions().readonly(),
                hidden: name.to_string_lossy().starts_with('.'),
                directory: md.is_dir(),
                ..Self::default()
            };
            a.raw = 0;
            a
        }
    }

    /// Short flags such as `["readonly", "hidden"]`.
    pub fn labels(&self) -> Vec<&'static str> {
        let mut v = Vec::new();
        let table = [
            (self.readonly, "readonly"),
            (self.hidden, "hidden"),
            (self.system, "system"),
            (self.archive, "archive"),
            (self.compressed, "compressed"),
            (self.encrypted, "encrypted"),
            (self.reparse_point, "reparse"),
            (self.cloud_placeholder, "cloud"),
        ];
        for (on, name) in table {
            if on {
                v.push(name);
            }
        }
        v
    }
}

/// Everything cheap we know about a path.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FileMetadata {
    pub path: PathBuf,
    pub kind: EntryKind,
    /// Size in bytes (of the link target for working links).
    pub size: u64,
    pub created: Option<SystemTime>,
    pub modified: Option<SystemTime>,
    pub accessed: Option<SystemTime>,
    pub attributes: Attributes,
    /// Target of a symlink or junction (`read_link`).
    pub link_target: Option<PathBuf>,
    /// The link target does not exist.
    pub link_broken: bool,
    /// Owner name. Always `None`: resolving a SID to a name needs `unsafe` Win32 calls
    /// (`GetNamedSecurityInfoW`), which this workspace forbids. See the engine notes.
    pub owner: Option<String>,
}

impl FileMetadata {
    /// Stat `path` without opening it (never triggers cloud hydration).
    pub fn read(path: &Path) -> io::Result<Self> {
        let lmd = fs::symlink_metadata(path)?;
        let name = path.file_name().unwrap_or_default();
        let mut attributes = Attributes::of(&lmd, name);
        // Only real symlinks and junctions: other reparse points (OneDrive placeholders,
        // AppExecLinks, dedup) are ordinary files for our purposes and must not be
        // flagged as broken links.
        let is_link = lmd.file_type().is_symlink();
        let mut link_target = None;
        let mut link_broken = false;
        let mut md = lmd.clone();
        if is_link {
            link_target = fs::read_link(path).ok().map(strip_verbatim);
            match fs::metadata(path) {
                Ok(target_md) => {
                    // Directory-ness / size of the target are what the user cares about.
                    attributes.directory = target_md.is_dir();
                    md = target_md;
                }
                Err(_) => link_broken = true,
            }
        }
        let kind = if is_link && link_target.is_some() {
            EntryKind::Symlink {
                to_dir: !link_broken && md.is_dir(),
            }
        } else if md.is_dir() {
            EntryKind::Dir
        } else if md.is_file() {
            EntryKind::File
        } else {
            EntryKind::Other
        };
        Ok(Self {
            path: path.to_path_buf(),
            kind,
            size: if md.is_dir() { 0 } else { md.len() },
            created: md.created().ok(),
            modified: md.modified().ok(),
            accessed: md.accessed().ok(),
            attributes,
            link_target,
            link_broken,
            owner: None,
        })
    }

    pub fn is_dir(&self) -> bool {
        self.kind.is_dir_like()
    }

    /// Metadata view as styled lines: muted label column and plain values.
    /// Times are UTC (`YYYY-MM-DD HH:MM:SS`); the UI may prefer local time and use the
    /// raw `SystemTime` fields instead.
    pub fn to_lines(&self) -> Vec<StyledLine> {
        let label = SpanStyle::role(Role::FgMuted);
        let row = |name: &str, value: String, style: SpanStyle| -> StyledLine {
            StyledLine::new(vec![
                StyledSpan {
                    text: format!("{name:<10}"),
                    style: label,
                },
                StyledSpan { text: value, style },
            ])
        };
        let plain = SpanStyle::default();
        let mut out = vec![row(
            "Name",
            sanitize_line(
                &self.path.file_name().unwrap_or_default().to_string_lossy(),
                4,
            ),
            plain,
        )];
        let kind = match self.kind {
            EntryKind::Dir => "directory",
            EntryKind::File => "file",
            EntryKind::Symlink { to_dir: true } => "link to directory",
            EntryKind::Symlink { to_dir: false } => "link",
            EntryKind::Other => "other",
        };
        out.push(row("Type", kind.into(), plain));
        if !self.is_dir() {
            out.push(row(
                "Size",
                format!("{} ({} bytes)", human_size(self.size), self.size),
                plain,
            ));
        }
        for (name, t) in [
            ("Created", self.created),
            ("Modified", self.modified),
            ("Accessed", self.accessed),
        ] {
            if let Some(t) = t {
                out.push(row(name, format_utc(t), plain));
            }
        }
        let labels = self.attributes.labels();
        if !labels.is_empty() {
            out.push(row("Attrs", labels.join(", "), plain));
        }
        if self.link_target.is_none() && self.link_broken {
            out.push(row(
                "Target",
                "(unreadable, broken link)".into(),
                SpanStyle::role(Role::Error),
            ));
        }
        if let Some(target) = &self.link_target {
            let style = if self.link_broken {
                SpanStyle::role(Role::Error)
            } else {
                SpanStyle::role(Role::Symlink)
            };
            let mut text = sanitize_line(&target.to_string_lossy(), 4);
            if self.link_broken {
                text.push_str(" (broken)");
            }
            out.push(row("Target", text, style));
        }
        if let Some(owner) = &self.owner {
            out.push(row("Owner", sanitize_line(owner, 4), plain));
        }
        out
    }
}

/// Remove the Win32 verbatim prefix that `read_link` returns for junctions and absolute
/// symlinks: `\\?\C:\x` becomes `C:\x`, `\\?\UNC\srv\share` becomes `\\srv\share`.
pub fn strip_verbatim(p: PathBuf) -> PathBuf {
    let s = p.to_string_lossy();
    if let Some(rest) = s.strip_prefix(r"\\?\UNC\") {
        return PathBuf::from(format!(r"\\{rest}"));
    }
    if let Some(rest) = s.strip_prefix(r"\\?\") {
        // Only plain drive paths; other verbatim forms (Volume{{guid}}) stay as they are.
        let b = rest.as_bytes();
        if b.len() >= 2 && b[0].is_ascii_alphabetic() && b[1] == b':' {
            return PathBuf::from(rest);
        }
    }
    p
}

/// `14.2 KB` style size, base 1024 with decimal-style unit names.
pub fn human_size(bytes: u64) -> String {
    const UNITS: [&str; 5] = ["B", "KB", "MB", "GB", "TB"];
    if bytes < 1024 {
        return format!("{bytes} B");
    }
    let mut v = bytes as f64;
    let mut i = 0;
    while v >= 1024.0 && i < UNITS.len() - 1 {
        v /= 1024.0;
        i += 1;
    }
    format!("{v:.1} {}", UNITS[i])
}

/// `YYYY-MM-DD HH:MM:SS UTC`.
pub fn format_utc(t: SystemTime) -> String {
    let secs = match t.duration_since(UNIX_EPOCH) {
        Ok(d) => d.as_secs() as i64,
        Err(e) => -(e.duration().as_secs() as i64),
    };
    let days = secs.div_euclid(86_400);
    let rem = secs.rem_euclid(86_400);
    // Howard Hinnant's civil_from_days.
    let z = days + 719_468;
    let era = z.div_euclid(146_097);
    let doe = z.rem_euclid(146_097);
    let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365;
    let y = yoe + era * 400;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = doy - (153 * mp + 2) / 5 + 1;
    let m = if mp < 10 { mp + 3 } else { mp - 9 };
    let y = if m <= 2 { y + 1 } else { y };
    format!(
        "{y:04}-{m:02}-{d:02} {:02}:{:02}:{:02} UTC",
        rem / 3600,
        rem % 3600 / 60,
        rem % 60
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::Duration;

    #[test]
    fn utc_format() {
        assert_eq!(format_utc(UNIX_EPOCH), "1970-01-01 00:00:00 UTC");
        assert_eq!(
            format_utc(UNIX_EPOCH + Duration::from_secs(1_700_000_000)),
            "2023-11-14 22:13:20 UTC"
        );
    }

    #[test]
    fn verbatim_prefix_is_stripped() {
        assert_eq!(
            strip_verbatim(PathBuf::from(r"\\?\C:\Users\x")),
            PathBuf::from(r"C:\Users\x")
        );
        assert_eq!(
            strip_verbatim(PathBuf::from(r"\\?\UNC\srv\share\f")),
            PathBuf::from(r"\\srv\share\f")
        );
        assert_eq!(
            strip_verbatim(PathBuf::from(r"\\?\Volume{abc}\f")),
            PathBuf::from(r"\\?\Volume{abc}\f")
        );
        assert_eq!(
            strip_verbatim(PathBuf::from("rel/x")),
            PathBuf::from("rel/x")
        );
    }

    #[test]
    fn sizes() {
        assert_eq!(human_size(512), "512 B");
        assert_eq!(human_size(1536), "1.5 KB");
    }
}
