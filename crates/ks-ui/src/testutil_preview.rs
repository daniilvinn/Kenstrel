//! Preview fixtures for snapshot tests. Hand-built engine results (no syntect, no file
//! system) so the snapshots do not move when the highlighter or the machine does.

use std::sync::Arc;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use ks_core::{AppState, Entry};
use ks_preview::{
    ArchiveEntry, ArchiveFormat, ArchivePreview, Attributes, BinaryPreview, DirEntryInfo,
    DirPreview, Encoding, EntryKind as K, FileMetadata, HexRow, ImagePreview, Palette, Preview,
    PreviewBody, Role, SpanStyle, StyledLine, StyledSpan, TextPreview, UnavailableReason,
};

use crate::testutil::{listing_state, NOW_SECS};

fn ago(secs: u64) -> SystemTime {
    UNIX_EPOCH + Duration::from_secs(NOW_SECS - secs)
}

/// White above, black below: every halfblock cell pairs two different colours.
pub fn two_tone(width: u32, height: u32) -> image::DynamicImage {
    image::DynamicImage::ImageRgb8(image::RgbImage::from_fn(width, height, |_, y| {
        if y < height / 2 {
            image::Rgb([255, 255, 255])
        } else {
            image::Rgb([0, 0, 0])
        }
    }))
}

/// Switches the preview pane on at any width.
pub fn with_preview(mut state: AppState) -> AppState {
    state.preview.user_visible = Some(true);
    state
}

/// Makes `body` the engine result for the entry under the cursor.
pub fn show(state: &mut AppState, body: PreviewBody, meta: Option<FileMetadata>) {
    let name = state.view.current().expect("a cursor entry").name.clone();
    let path = fixture_path(state, &name);
    state.preview.target = Some(path.clone());
    state.preview.current = Some(Arc::new(Preview { path, meta, body }));
    state.preview.loading = false;
}

fn spans(parts: &[(&str, SpanStyle)]) -> StyledLine {
    StyledLine::new(
        parts
            .iter()
            .map(|(text, style)| StyledSpan {
                text: (*text).to_owned(),
                style: *style,
            })
            .collect(),
    )
}

/// A highlighted Rust file built by hand from the palette.
pub fn rust_text(palette: &Palette) -> TextPreview {
    let c = SpanStyle::rgb;
    let plain = SpanStyle::default();
    let (kw, ty, punct, comment, num, func) = (
        c(palette.keyword),
        c(palette.type_),
        c(palette.punctuation),
        c(palette.comment).italic(),
        c(palette.number),
        c(palette.function),
    );
    let mut lines = vec![
        spans(&[("use", kw), (" ash::vk", plain), (";", punct)]),
        spans(&[
            ("use", kw),
            (" crate::core::", plain),
            ("{", punct),
            ("Result", ty),
            (", ", punct),
            ("Handle", ty),
            ("}", punct),
            (";", punct),
        ]),
        StyledLine::default(),
        spans(&[
            ("pub", kw),
            (" ", plain),
            ("struct", kw),
            (" ", plain),
            ("VulkanDevice", ty),
            (" {", punct),
        ]),
        spans(&[
            ("    instance", plain),
            (": ", punct),
            ("ash::Instance", ty),
            (",", punct),
        ]),
        spans(&[
            ("    physical", plain),
            (": ", punct),
            ("vk::PhysicalDevice", ty),
            (",", punct),
        ]),
        spans(&[("    // queue families, filled in by pick()", comment)]),
    ];
    for i in 0..30 {
        lines.push(spans(&[
            (&format!("    slot_{i}"), plain),
            (": ", punct),
            ("u32", ty),
            (", ", punct),
            (&format!("// {i}"), comment),
        ]));
    }
    lines.push(spans(&[("}", punct)]));
    lines.push(spans(&[
        ("const", kw),
        (" MAX_FRAMES", plain),
        (": ", punct),
        ("usize", ty),
        (" = ", punct),
        ("2", num),
        (";", punct),
        (" // ", comment),
        ("render", func),
    ]));
    let count = lines.len();
    TextPreview {
        lines,
        first_line: 0,
        line_count: count,
        total_lines: Some(count),
        truncated: false,
        encoding: Encoding::Utf8,
        syntax: Some("Rust".to_owned()),
        bytes_read: 14_540,
    }
}

pub fn markdown_text() -> TextPreview {
    let heading = SpanStyle::role(Role::Accent).bold();
    let bullet = SpanStyle::role(Role::FgMuted);
    let plain = SpanStyle::default();
    let lines = vec![
        spans(&[("Kestrel", heading)]),
        StyledLine::default(),
        spans(&[("A keyboard-first file explorer for ", plain)]),
        spans(&[("Windows Terminal", plain.bold())]),
        StyledLine::default(),
        spans(&[("  • ", bullet), ("fast", plain)]),
        spans(&[("  • ", bullet), ("calm", plain)]),
        StyledLine::default(),
        spans(&[("  cargo install kestrel", SpanStyle::role(Role::FgDim))]),
    ];
    let count = lines.len();
    TextPreview {
        lines,
        first_line: 0,
        line_count: count,
        total_lines: Some(count),
        truncated: false,
        encoding: Encoding::Utf8,
        syntax: Some("Markdown".to_owned()),
        bytes_read: 200,
    }
}

pub fn dir_body() -> PreviewBody {
    let info = |name: &str, kind, size: Option<u64>, hidden| DirEntryInfo {
        name: name.to_owned(),
        kind,
        size,
        hidden,
    };
    let entries = vec![
        info("common", K::Dir, None, false),
        info("includes", K::Dir, None, false),
        info("pass_a.rs", K::File, Some(2_300), false),
        info("pass_b.rs", K::File, Some(4_100), false),
        info("blur.frag", K::File, Some(812), false),
        info(".gitkeep", K::File, Some(0), true),
    ];
    PreviewBody::Directory(DirPreview {
        total: entries.len(),
        entries,
        truncated: false,
        scan_capped: false,
    })
}

/// A listing for the parent column.
pub fn parent_body() -> PreviewBody {
    let info = |name: &str, kind| DirEntryInfo {
        name: name.to_owned(),
        kind,
        size: Some(0),
        hidden: false,
    };
    let entries = vec![
        info("assets", K::Dir),
        info("core", K::Dir),
        info("renderer", K::Dir),
        info("scene", K::Dir),
        info("main.rs", K::File),
    ];
    PreviewBody::Directory(DirPreview {
        total: entries.len(),
        entries,
        truncated: false,
        scan_capped: false,
    })
}

pub fn hex_body() -> PreviewBody {
    let bytes: Vec<u8> = (0u8..=95).map(|b| b.wrapping_mul(7)).collect();
    let rows = bytes
        .chunks(16)
        .enumerate()
        .map(|(i, chunk)| HexRow {
            offset: (i * 16) as u64,
            bytes: chunk.to_vec(),
        })
        .collect();
    PreviewBody::Binary(BinaryPreview {
        rows,
        total_size: 4096,
        type_hint: Some("firmware image"),
        note: None,
    })
}

pub fn archive_body() -> PreviewBody {
    let entry = |name: &str, size: u64, is_dir: bool| ArchiveEntry {
        name: name.to_owned(),
        size,
        compressed_size: size / 2,
        is_dir,
    };
    PreviewBody::Archive(ArchivePreview {
        format: ArchiveFormat::Zip,
        listed: true,
        entries: vec![
            entry("src/", 0, true),
            entry("src/main.rs", 1_204, false),
            entry("src/lib.rs", 8_820, false),
            entry("Cargo.toml", 412, false),
            entry("README.md", 3_100, false),
        ],
        total_entries: 5,
        total_size: 13_536,
        truncated: false,
    })
}

pub fn image_body() -> PreviewBody {
    PreviewBody::Image(ImagePreview {
        image: Arc::new(two_tone(1200, 700)),
        width: 1920,
        height: 1080,
        format: "PNG".to_owned(),
        downscaled: true,
    })
}

pub fn unavailable_body() -> PreviewBody {
    PreviewBody::Unavailable {
        reason: UnavailableReason::CloudPlaceholder,
        message: "Cloud placeholder: the content is not on this PC, so it is not downloaded"
            .to_owned(),
    }
}

/// `name` under the fixture cwd, always joined with `/` so the rendered path (and so the
/// snapshots) is the same on every platform.
fn fixture_path(state: &AppState, name: impl AsRef<std::ffi::OsStr>) -> std::path::PathBuf {
    let cwd = state.cwd.to_string_lossy();
    format!(
        "{}/{}",
        cwd.trim_end_matches(['/', '\\']),
        name.as_ref().to_string_lossy()
    )
    .into()
}

/// Metadata for the entry called `name` below the fixture directory.
pub fn metadata_of(state: &AppState, name: &str, size: u64, raw_attrs: u32) -> FileMetadata {
    FileMetadata {
        path: fixture_path(state, name),
        kind: K::File,
        size,
        created: Some(ago(40 * 86_400)),
        modified: Some(ago(3 * 3600)),
        accessed: Some(ago(600)),
        attributes: Attributes::from_raw(raw_attrs),
        link_target: None,
        link_broken: false,
        owner: None,
    }
}

/// The fixture listing (preview pane on) plus files that exercise each preview kind.
pub fn preview_state(width: u16, height: u16) -> AppState {
    let mut state = with_preview(listing_state(width, height));
    let rows = state.viewport_height();
    state.view.push_chunk(
        vec![
            Entry::file("firmware.bin", 4096).with_mtime(ago(86_400 * 2)),
            Entry::file("logo.png", 523_000).with_mtime(ago(86_400 * 4)),
            Entry::file("bundle.zip", 22_000).with_mtime(ago(86_400 * 6)),
            Entry::file("README.md", 3_100).with_mtime(ago(86_400 * 8)),
        ],
        rows,
    );
    state
}
