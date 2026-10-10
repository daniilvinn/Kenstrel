//! Terminal-agnostic output model: styled lines and the [`Preview`] result types.

use std::path::PathBuf;
use std::sync::Arc;
use std::time::SystemTime;

use crate::metadata::FileMetadata;

/// A 24-bit colour.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct Rgb {
    pub r: u8,
    pub g: u8,
    pub b: u8,
}

impl Rgb {
    pub const fn new(r: u8, g: u8, b: u8) -> Self {
        Self { r, g, b }
    }
}

/// Semantic colour roles the UI maps to theme tokens (PLAN.md section 4.4).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Role {
    Fg,
    FgDim,
    FgMuted,
    Accent,
    Dir,
    Exec,
    Symlink,
    Hidden,
    Success,
    Warning,
    Error,
}

/// A foreground colour: either a concrete RGB (syntax highlighting, derived from the
/// caller's palette) or a semantic [`Role`].
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Color {
    Rgb(Rgb),
    Role(Role),
}

/// Style of one span. `fg == None` means "default foreground of the pane".
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Hash)]
pub struct SpanStyle {
    pub fg: Option<Color>,
    pub bold: bool,
    pub italic: bool,
    pub underline: bool,
    pub dim: bool,
}

impl SpanStyle {
    pub const fn role(role: Role) -> Self {
        Self {
            fg: Some(Color::Role(role)),
            bold: false,
            italic: false,
            underline: false,
            dim: false,
        }
    }
    pub const fn rgb(rgb: Rgb) -> Self {
        Self {
            fg: Some(Color::Rgb(rgb)),
            bold: false,
            italic: false,
            underline: false,
            dim: false,
        }
    }
    pub const fn bold(mut self) -> Self {
        self.bold = true;
        self
    }
    pub const fn italic(mut self) -> Self {
        self.italic = true;
        self
    }
    pub const fn underline(mut self) -> Self {
        self.underline = true;
        self
    }
    pub const fn dim(mut self) -> Self {
        self.dim = true;
        self
    }
}

/// A run of text with one style. Text never contains control characters or tabs.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct StyledSpan {
    pub text: String,
    pub style: SpanStyle,
}

/// One display line (no trailing newline). Long lines are kept whole.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct StyledLine {
    pub spans: Vec<StyledSpan>,
}

impl StyledLine {
    pub fn new(spans: Vec<StyledSpan>) -> Self {
        Self { spans }
    }
    /// Single unstyled span. `text` must already be sanitized.
    pub fn plain(text: impl Into<String>) -> Self {
        Self::styled(text, SpanStyle::default())
    }
    /// Single span with `style`. `text` must already be sanitized.
    pub fn styled(text: impl Into<String>, style: SpanStyle) -> Self {
        let text = text.into();
        if text.is_empty() {
            return Self::default();
        }
        Self {
            spans: vec![StyledSpan { text, style }],
        }
    }
    /// Concatenated text of all spans.
    pub fn text(&self) -> String {
        self.spans.iter().map(|s| s.text.as_str()).collect()
    }
    pub fn is_empty(&self) -> bool {
        self.spans.iter().all(|s| s.text.is_empty())
    }
    /// Append text, merging with the previous span when the style matches.
    pub(crate) fn push(&mut self, text: String, style: SpanStyle) {
        if text.is_empty() {
            return;
        }
        if let Some(last) = self.spans.last_mut() {
            if last.style == style {
                last.text.push_str(&text);
                return;
            }
        }
        self.spans.push(StyledSpan { text, style });
    }
}

/// Text encoding detected for a text preview.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Encoding {
    Utf8,
    Utf8Bom,
    Utf16Le,
    Utf16Be,
    Utf32Le,
    Utf32Be,
    /// Not valid UTF-8; decoded lossily (invalid bytes shown as U+FFFD).
    Lossy,
}

impl Encoding {
    pub fn label(self) -> &'static str {
        match self {
            Encoding::Utf8 => "UTF-8",
            Encoding::Utf8Bom => "UTF-8 BOM",
            Encoding::Utf16Le => "UTF-16 LE",
            Encoding::Utf16Be => "UTF-16 BE",
            Encoding::Utf32Le => "UTF-32 LE",
            Encoding::Utf32Be => "UTF-32 BE",
            Encoding::Lossy => "lossy",
        }
    }
}

/// Body of a text or markdown preview.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TextPreview {
    pub lines: Vec<StyledLine>,
    /// Number of lines in `lines` (the renderer sizes the line-number gutter from this).
    /// When `truncated` is true the real file has more lines (see `total_lines`).
    pub line_count: usize,
    /// Total lines in the file when the whole file was read and rendered; `None` when
    /// the preview is truncated (then `line_count` is only a lower bound).
    pub total_lines: Option<usize>,
    /// The file has more content than was read / rendered (256 KB or 2,000 lines).
    pub truncated: bool,
    pub encoding: Encoding,
    /// Name of the syntax used for highlighting, if any.
    pub syntax: Option<String>,
    /// Number of bytes read from the file.
    pub bytes_read: u64,
}

/// Kind of a directory entry.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum EntryKind {
    Dir,
    File,
    /// Symlink or junction. `to_dir` is true when it points at a directory.
    Symlink {
        to_dir: bool,
    },
    Other,
}

impl EntryKind {
    /// Directory or link to a directory (sorted with directories).
    pub fn is_dir_like(self) -> bool {
        matches!(self, EntryKind::Dir | EntryKind::Symlink { to_dir: true })
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DirEntryInfo {
    pub name: String,
    pub kind: EntryKind,
    /// Size in bytes for files.
    pub size: Option<u64>,
    pub hidden: bool,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DirPreview {
    /// Sorted: directories first, then files, natural case-insensitive order.
    pub entries: Vec<DirEntryInfo>,
    /// Entries seen (after hidden filtering), possibly capped by the scan limit.
    pub total: usize,
    /// More entries exist than listed in `entries`.
    pub truncated: bool,
    /// The scan stopped at `Limits::dir_scan_limit` in filesystem order, so `entries`
    /// are the first ones sorted among the scanned subset, not necessarily the first
    /// overall, and `total` is a lower bound. Show e.g. "20,000+ entries, partial".
    pub scan_capped: bool,
}

/// A decoded image, ready to hand to `ratatui-image`
/// (`Picker::new_resize_protocol(image.as_ref().clone())` or `new_protocol`).
#[derive(Debug, Clone)]
pub struct ImagePreview {
    /// Decoded (and possibly downscaled) pixels. Shared so cache hits are free.
    pub image: Arc<image::DynamicImage>,
    /// Dimensions of the original file.
    pub width: u32,
    pub height: u32,
    /// `"PNG"`, `"JPEG"`, ...
    pub format: String,
    /// `image` holds a downscaled copy of the original.
    pub downscaled: bool,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ArchiveFormat {
    Zip,
    Tar,
    Gzip,
    Bzip2,
    Xz,
    Zstd,
    SevenZip,
    Rar,
}

impl ArchiveFormat {
    pub fn label(self) -> &'static str {
        match self {
            ArchiveFormat::Zip => "ZIP",
            ArchiveFormat::Tar => "TAR",
            ArchiveFormat::Gzip => "GZIP",
            ArchiveFormat::Bzip2 => "BZIP2",
            ArchiveFormat::Xz => "XZ",
            ArchiveFormat::Zstd => "ZSTD",
            ArchiveFormat::SevenZip => "7-Zip",
            ArchiveFormat::Rar => "RAR",
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ArchiveEntry {
    /// Sanitized path inside the archive.
    pub name: String,
    pub size: u64,
    pub compressed_size: u64,
    pub is_dir: bool,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ArchivePreview {
    pub format: ArchiveFormat,
    /// False for formats we do not list (only metadata is shown).
    pub listed: bool,
    pub entries: Vec<ArchiveEntry>,
    /// Entry count in the archive (may exceed `entries.len()`).
    pub total_entries: usize,
    /// Sum of uncompressed sizes of the listed entries.
    pub total_size: u64,
    pub truncated: bool,
}

/// One hex-dump row.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct HexRow {
    pub offset: u64,
    /// 1..=16 bytes.
    pub bytes: Vec<u8>,
}

impl HexRow {
    /// `"48 65 6c 6c 6f ..."` padded to 16 columns with an extra gap after 8 bytes.
    pub fn hex(&self) -> String {
        let mut s = String::with_capacity(49);
        for i in 0..16 {
            if i == 8 {
                s.push(' ');
            }
            if i > 0 {
                s.push(' ');
            }
            match self.bytes.get(i) {
                Some(b) => s.push_str(&format!("{b:02x}")),
                None => s.push_str("  "),
            }
        }
        s
    }
    /// Printable-ASCII gutter; everything else is `.`.
    pub fn ascii(&self) -> String {
        self.bytes
            .iter()
            .map(|&b| {
                if (0x20..0x7f).contains(&b) {
                    b as char
                } else {
                    '.'
                }
            })
            .collect()
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct BinaryPreview {
    pub rows: Vec<HexRow>,
    pub total_size: u64,
    /// Short guess such as `"PE executable"` from magic bytes.
    pub type_hint: Option<&'static str>,
    /// Why a richer view was not possible (for example "image failed to decode").
    pub note: Option<String>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum UnavailableReason {
    TooLarge,
    CloudPlaceholder,
    PermissionDenied,
    Locked,
    NotFound,
    Io,
}

/// What was produced for a path.
#[derive(Debug, Clone)]
pub enum PreviewBody {
    Empty,
    Text(TextPreview),
    Markdown(TextPreview),
    Directory(DirPreview),
    Image(ImagePreview),
    Archive(ArchivePreview),
    Binary(BinaryPreview),
    /// Metadata view was requested; render [`Preview::meta`].
    Metadata,
    /// Content cannot be shown; show `message` plus the metadata.
    Unavailable {
        reason: UnavailableReason,
        message: String,
    },
}

/// A finished preview. Carries the metadata whenever the path could be stat'ed
/// (usable for the footer).
#[derive(Debug, Clone)]
pub struct Preview {
    pub path: PathBuf,
    pub meta: Option<FileMetadata>,
    pub body: PreviewBody,
}

impl Preview {
    /// Rough heap footprint in bytes, used for the cache byte budget.
    pub fn approx_bytes(&self) -> usize {
        let line_bytes = |t: &TextPreview| -> usize {
            t.lines
                .iter()
                .map(|l| 32 + l.spans.iter().map(|s| 48 + s.text.len()).sum::<usize>())
                .sum()
        };
        let body = match &self.body {
            PreviewBody::Text(t) | PreviewBody::Markdown(t) => line_bytes(t),
            PreviewBody::Directory(d) => d.entries.iter().map(|e| 64 + e.name.len()).sum(),
            PreviewBody::Image(i) => i.image.as_bytes().len(),
            PreviewBody::Archive(a) => a.entries.iter().map(|e| 64 + e.name.len()).sum(),
            PreviewBody::Binary(b) => b.rows.len() * 48,
            PreviewBody::Unavailable { message, .. } => message.len(),
            PreviewBody::Empty | PreviewBody::Metadata => 0,
        };
        512 + body
    }
}

/// Which view is wanted.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Default)]
pub enum RequestKind {
    /// Pick by content: text, markdown, directory, image, archive, binary.
    #[default]
    Auto,
    /// Only the metadata view (the `i` toggle).
    Metadata,
}

/// Nanoseconds since the Unix epoch, for cache keys.
pub(crate) fn mtime_nanos(t: Option<SystemTime>) -> Option<i128> {
    let t = t?;
    Some(match t.duration_since(SystemTime::UNIX_EPOCH) {
        Ok(d) => d.as_nanos() as i128,
        Err(e) => -(e.duration().as_nanos() as i128),
    })
}
