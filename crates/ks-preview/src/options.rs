//! Request options: palette, limits and flags.

use std::hash::{Hash, Hasher};
use std::time::Duration;

use crate::model::{RequestKind, Rgb};

/// Colours for syntax highlighting, derived by the caller from its theme tokens.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct Palette {
    pub fg: Rgb,
    pub keyword: Rgb,
    pub string: Rgb,
    pub comment: Rgb,
    pub function: Rgb,
    pub type_: Rgb,
    pub number: Rgb,
    pub constant: Rgb,
    pub operator: Rgb,
    pub punctuation: Rgb,
}

impl Palette {
    /// A warm dark palette (matches the `ember` theme's feel).
    pub const fn dark() -> Self {
        Self {
            fg: Rgb::new(0xd8, 0xd2, 0xc8),
            keyword: Rgb::new(0xd9, 0x77, 0x57),
            string: Rgb::new(0x9c, 0xc2, 0x7a),
            comment: Rgb::new(0x7a, 0x74, 0x6c),
            function: Rgb::new(0x7a, 0xb0, 0xd8),
            type_: Rgb::new(0xe5, 0xc0, 0x7b),
            number: Rgb::new(0xc6, 0x8f, 0xd9),
            constant: Rgb::new(0xd1, 0x9a, 0x66),
            operator: Rgb::new(0xb8, 0xb0, 0xa4),
            punctuation: Rgb::new(0x9a, 0x94, 0x8a),
        }
    }

    /// A light palette (matches the `paper` theme's feel).
    pub const fn light() -> Self {
        Self {
            fg: Rgb::new(0x2b, 0x2a, 0x27),
            keyword: Rgb::new(0xb0, 0x4a, 0x2a),
            string: Rgb::new(0x3f, 0x7a, 0x2e),
            comment: Rgb::new(0x8a, 0x84, 0x7a),
            function: Rgb::new(0x1f, 0x5f, 0x99),
            type_: Rgb::new(0x9a, 0x6a, 0x10),
            number: Rgb::new(0x8a, 0x3f, 0xa8),
            constant: Rgb::new(0xa0, 0x5a, 0x1a),
            operator: Rgb::new(0x55, 0x50, 0x48),
            punctuation: Rgb::new(0x6f, 0x6a, 0x60),
        }
    }
}

impl Default for Palette {
    fn default() -> Self {
        Self::dark()
    }
}

/// Size and time limits. Defaults follow PLAN.md section 8.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct Limits {
    /// Bytes read for text / markdown (256 KB).
    pub max_text_bytes: usize,
    /// Lines rendered for text / markdown (2,000).
    pub max_text_lines: usize,
    /// Text, markdown and binary files larger than this are `Unavailable::TooLarge`
    /// (only the head is ever read, but multi-GB files are usually not worth previewing).
    pub max_file_size: u64,
    /// Entries returned for a directory (500).
    pub max_dir_entries: usize,
    /// Entries examined for a directory before giving up on sorting the rest.
    pub dir_scan_limit: usize,
    /// Zip entries returned.
    pub max_archive_entries: usize,
    /// Bytes shown in the hex view (4 KB).
    pub hex_bytes: usize,
    /// Image files larger than this are `Unavailable::TooLarge`.
    pub max_image_file_size: u64,
    /// Images with more pixels than this are not decoded (decompression-bomb guard).
    pub max_image_pixels: u64,
    /// Decoded images are downscaled so neither side exceeds this.
    pub image_max_dim: u32,
    /// Lines longer than this (bytes) are not syntax highlighted.
    pub max_highlight_line: usize,
    /// After this much highlighting time the rest of the file is shown unhighlighted.
    pub highlight_budget: Duration,
}

impl Default for Limits {
    fn default() -> Self {
        Self {
            max_text_bytes: 256 * 1024,
            max_text_lines: 2_000,
            max_file_size: 100 * 1024 * 1024,
            max_dir_entries: 500,
            dir_scan_limit: 20_000,
            max_archive_entries: 1_000,
            hex_bytes: 4 * 1024,
            max_image_file_size: 64 * 1024 * 1024,
            max_image_pixels: 50_000_000,
            image_max_dim: 2_048,
            max_highlight_line: 4_000,
            highlight_budget: Duration::from_millis(1_500),
        }
    }
}

/// Resolves the owner of a path for the metadata view (domain and user name). The preview crate
/// cannot call Win32 itself (no `unsafe`), so the app injects one. Never part of cache keys.
#[derive(Clone, Copy)]
pub struct OwnerResolver(pub fn(&std::path::Path) -> Option<String>);

impl std::fmt::Debug for OwnerResolver {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("OwnerResolver")
    }
}

impl PartialEq for OwnerResolver {
    fn eq(&self, _: &Self) -> bool {
        true
    }
}

impl Eq for OwnerResolver {}

impl Hash for OwnerResolver {
    fn hash<H: Hasher>(&self, _: &mut H) {}
}

/// Options for one request.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct PreviewOptions {
    pub kind: RequestKind,
    pub palette: Palette,
    pub limits: Limits,
    pub tab_width: usize,
    /// List hidden / system entries in directory previews.
    pub show_hidden: bool,
    /// Syntax highlighting on/off (off gives plain text).
    pub highlight: bool,
    /// Fills `FileMetadata::owner` for metadata requests.
    pub owner: Option<OwnerResolver>,
}

impl Default for PreviewOptions {
    fn default() -> Self {
        Self {
            kind: RequestKind::Auto,
            palette: Palette::default(),
            limits: Limits::default(),
            tab_width: 4,
            show_hidden: false,
            highlight: true,
            owner: None,
        }
    }
}

impl PreviewOptions {
    pub fn metadata() -> Self {
        Self {
            kind: RequestKind::Metadata,
            ..Self::default()
        }
    }

    /// Hash of every option that influences the output (cache-key component).
    pub(crate) fn fingerprint(&self) -> u64 {
        let mut h = std::collections::hash_map::DefaultHasher::new();
        self.hash(&mut h);
        h.finish()
    }
}
