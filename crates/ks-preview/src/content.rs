//! Directory, image, archive and binary previews plus the top-level dispatcher.

use std::cmp::Ordering;
use std::fs::{self, File};
use std::io::{self, BufReader, Read};
use std::path::Path;
use std::sync::Arc;

use tokio_util::sync::CancellationToken;

use crate::markdown;
use crate::metadata::{FileMetadata, CLOUD_MASK};
use crate::model::*;
use crate::options::{Limits, PreviewOptions};
use crate::sanitize::sanitize_line;
use crate::text::{self, Decoded};

/// Internal error: either cancelled or an I/O failure to report as `Unavailable`.
#[derive(Debug, thiserror::Error)]
pub(crate) enum BuildError {
    #[error("cancelled")]
    Cancelled,
    #[error("io: {0}")]
    Io(#[from] io::Error),
}

type Built = Result<PreviewBody, BuildError>;

fn check(cancel: &CancellationToken) -> Result<(), BuildError> {
    if cancel.is_cancelled() {
        Err(BuildError::Cancelled)
    } else {
        Ok(())
    }
}

pub(crate) fn classify_io(e: &io::Error) -> (UnavailableReason, String) {
    match e.kind() {
        io::ErrorKind::NotFound => (UnavailableReason::NotFound, "File no longer exists".into()),
        io::ErrorKind::PermissionDenied => (
            UnavailableReason::PermissionDenied,
            "Permission denied".into(),
        ),
        _ => match e.raw_os_error() {
            // ERROR_SHARING_VIOLATION, ERROR_LOCK_VIOLATION
            Some(32) | Some(33) => (
                UnavailableReason::Locked,
                "File is locked by another process".into(),
            ),
            _ => (UnavailableReason::Io, format!("Cannot read file: {e}")),
        },
    }
}

/// Build a complete preview synchronously. Returns `None` if `cancel` fired.
///
/// This is what the worker threads run; it is public so tests and one-shot callers
/// (for example a finder overlay with its own debounce) can use it directly.
pub fn build_preview(
    path: &Path,
    opts: &PreviewOptions,
    cancel: &CancellationToken,
) -> Option<Preview> {
    let meta = match FileMetadata::read(path) {
        Ok(m) => m,
        Err(e) => {
            let (reason, message) = classify_io(&e);
            return Some(Preview {
                path: path.to_path_buf(),
                meta: None,
                body: PreviewBody::Unavailable { reason, message },
            });
        }
    };
    build_with_meta(path, meta, opts, cancel)
}

pub(crate) fn build_with_meta(
    path: &Path,
    meta: FileMetadata,
    opts: &PreviewOptions,
    cancel: &CancellationToken,
) -> Option<Preview> {
    let body = match build_body(path, &meta, opts, cancel) {
        Ok(b) => b,
        Err(BuildError::Cancelled) => return None,
        Err(BuildError::Io(e)) => {
            tracing::debug!(path = %path.display(), error = %e, "preview io error");
            let (reason, message) = classify_io(&e);
            PreviewBody::Unavailable { reason, message }
        }
    };
    if cancel.is_cancelled() {
        return None;
    }
    Some(Preview {
        path: path.to_path_buf(),
        meta: Some(meta),
        body,
    })
}

fn build_body(
    path: &Path,
    meta: &FileMetadata,
    opts: &PreviewOptions,
    cancel: &CancellationToken,
) -> Built {
    check(cancel)?;
    if opts.kind == RequestKind::Metadata {
        return Ok(PreviewBody::Metadata);
    }
    // Cloud placeholders (files and folders) must never be opened or listed: that
    // would download them.
    if meta.attributes.raw & CLOUD_MASK != 0 {
        return Ok(PreviewBody::Unavailable {
            reason: UnavailableReason::CloudPlaceholder,
            message: "Cloud-only item; open it in Explorer to download. Preview skipped.".into(),
        });
    }
    if meta.is_dir() {
        return directory(path, opts, cancel);
    }
    if meta.kind == EntryKind::Other {
        return Ok(PreviewBody::Unavailable {
            reason: UnavailableReason::Io,
            message: "Not a regular file".into(),
        });
    }
    let ext = path
        .extension()
        .map(|e| e.to_string_lossy().to_ascii_lowercase())
        .unwrap_or_default();

    if is_image_ext(&ext) {
        if meta.size > opts.limits.max_image_file_size {
            return Ok(too_large(meta.size, opts.limits.max_image_file_size));
        }
        return image_preview(path, meta, &opts.limits, cancel);
    }
    if let Some(fmt) = archive_ext(&ext) {
        return archive(path, fmt, opts, cancel);
    }
    if meta.size > opts.limits.max_file_size {
        return Ok(too_large(meta.size, opts.limits.max_file_size));
    }
    if meta.size == 0 {
        return Ok(PreviewBody::Empty);
    }

    // Read the head.
    let head = read_head(path, opts.limits.max_text_bytes.max(opts.limits.hex_bytes))?;
    check(cancel)?;
    let truncated = meta.size > head.len() as u64;

    // Magic sniffing for files with missing / misleading extensions.
    if head.starts_with(b"PK\x03\x04") {
        if let Ok(b) = archive(path, ArchiveFormat::Zip, opts, cancel) {
            return Ok(b);
        }
    }
    if is_image_magic(&head) && meta.size <= opts.limits.max_image_file_size {
        if let Ok(PreviewBody::Image(i)) = image_preview(path, meta, &opts.limits, cancel) {
            return Ok(PreviewBody::Image(i));
        }
    }
    if let Some(fmt) = archive_magic(&head) {
        return archive(path, fmt, opts, cancel);
    }

    match text::decode(
        &head[..head.len().min(opts.limits.max_text_bytes)],
        truncated,
    ) {
        Decoded::Text { text: t, encoding } => {
            let is_md = matches!(ext.as_str(), "md" | "markdown" | "mdx" | "mkd");
            let bytes_read = head.len().min(opts.limits.max_text_bytes) as u64;
            if is_md {
                match markdown::render(&t, encoding, bytes_read, truncated, opts, cancel) {
                    Some(p) => Ok(PreviewBody::Markdown(p)),
                    None => Err(BuildError::Cancelled),
                }
            } else {
                match text::render_text(path, &t, encoding, bytes_read, truncated, opts, cancel) {
                    Some(p) => Ok(PreviewBody::Text(p)),
                    None => Err(BuildError::Cancelled),
                }
            }
        }
        Decoded::Binary => Ok(PreviewBody::Binary(binary(
            &head,
            meta.size,
            &opts.limits,
            None,
        ))),
    }
}

fn too_large(size: u64, limit: u64) -> PreviewBody {
    PreviewBody::Unavailable {
        reason: UnavailableReason::TooLarge,
        message: format!(
            "File is too large to preview ({} > {})",
            crate::metadata::human_size(size),
            crate::metadata::human_size(limit)
        ),
    }
}

fn read_head(path: &Path, max: usize) -> io::Result<Vec<u8>> {
    let mut buf = Vec::with_capacity(max.min(1 << 20));
    File::open(path)?.take(max as u64).read_to_end(&mut buf)?;
    Ok(buf)
}

// ---------------------------------------------------------------- directory

/// Natural, case-insensitive comparison (`file2` < `file10`).
pub fn natural_cmp(a: &str, b: &str) -> Ordering {
    let mut ai = a.chars().peekable();
    let mut bi = b.chars().peekable();
    loop {
        match (ai.peek().copied(), bi.peek().copied()) {
            (None, None) => return Ordering::Equal,
            (None, Some(_)) => return Ordering::Less,
            (Some(_), None) => return Ordering::Greater,
            (Some(x), Some(y)) if x.is_ascii_digit() && y.is_ascii_digit() => {
                let mut na = String::new();
                while let Some(&c) = ai.peek() {
                    if !c.is_ascii_digit() {
                        break;
                    }
                    na.push(c);
                    ai.next();
                }
                let mut nb = String::new();
                while let Some(&c) = bi.peek() {
                    if !c.is_ascii_digit() {
                        break;
                    }
                    nb.push(c);
                    bi.next();
                }
                let (ta, tb) = (na.trim_start_matches('0'), nb.trim_start_matches('0'));
                let ord = ta.len().cmp(&tb.len()).then_with(|| ta.cmp(tb));
                if ord != Ordering::Equal {
                    return ord;
                }
            }
            (Some(x), Some(y)) => {
                let (lx, ly) = (
                    x.to_lowercase().next().unwrap_or(x),
                    y.to_lowercase().next().unwrap_or(y),
                );
                if lx != ly {
                    return lx.cmp(&ly);
                }
                ai.next();
                bi.next();
            }
        }
    }
}

fn directory(path: &Path, opts: &PreviewOptions, cancel: &CancellationToken) -> Built {
    let mut entries = Vec::new();
    let mut scanned = 0usize;
    let mut capped = false;
    for (i, ent) in fs::read_dir(path)?.enumerate() {
        if i % 256 == 0 {
            check(cancel)?;
        }
        let Ok(ent) = ent else { continue };
        let name = ent.file_name();
        let md = ent.metadata().ok();
        let attrs = md
            .as_ref()
            .map(|m| crate::metadata::Attributes::of(m, &name))
            .unwrap_or_default();
        let hidden = attrs.hidden || attrs.system;
        if hidden && !opts.show_hidden {
            continue;
        }
        if scanned >= opts.limits.dir_scan_limit {
            capped = true;
            break;
        }
        scanned += 1;
        let ft = ent.file_type().ok();
        let kind = match ft {
            Some(t) if t.is_symlink() => EntryKind::Symlink {
                to_dir: attrs.directory
                    || fs::metadata(ent.path())
                        .map(|m| m.is_dir())
                        .unwrap_or(false),
            },
            Some(t) if t.is_dir() => EntryKind::Dir,
            Some(t) if t.is_file() => EntryKind::File,
            _ => EntryKind::Other,
        };
        let size = match (kind, &md) {
            (EntryKind::File, Some(m)) => Some(m.len()),
            _ => None,
        };
        let mut n = sanitize_line(&name.to_string_lossy(), 4);
        if n.is_empty() {
            n.push('?');
        }
        entries.push(DirEntryInfo {
            name: n,
            kind,
            size,
            hidden,
        });
    }
    entries.sort_by(|a, b| {
        b.kind
            .is_dir_like()
            .cmp(&a.kind.is_dir_like())
            .then_with(|| natural_cmp(&a.name, &b.name))
            .then_with(|| a.name.cmp(&b.name))
    });
    let total = entries.len();
    let truncated = capped || total > opts.limits.max_dir_entries;
    entries.truncate(opts.limits.max_dir_entries);
    Ok(PreviewBody::Directory(DirPreview {
        entries,
        total,
        truncated,
        scan_capped: capped,
    }))
}

// -------------------------------------------------------------------- image

fn is_image_ext(ext: &str) -> bool {
    matches!(
        ext,
        "png" | "jpg" | "jpeg" | "jpe" | "gif" | "bmp" | "webp" | "ico" | "tif" | "tiff"
    )
}

fn is_image_magic(h: &[u8]) -> bool {
    h.starts_with(b"\x89PNG\r\n\x1a\n")
        || h.starts_with(&[0xFF, 0xD8, 0xFF])
        || h.starts_with(b"GIF87a")
        || h.starts_with(b"GIF89a")
        || h.starts_with(b"BM") && h.len() > 14 && h[6..10] == [0, 0, 0, 0]
        || (h.len() > 12 && &h[..4] == b"RIFF" && &h[8..12] == b"WEBP")
        || h.starts_with(&[0, 0, 1, 0])
        || h.starts_with(b"II*\0")
        || h.starts_with(b"MM\0*")
}

fn image_preview(
    path: &Path,
    meta: &FileMetadata,
    limits: &Limits,
    cancel: &CancellationToken,
) -> Built {
    let fallback = |note: String| -> Built {
        let head = read_head(path, limits.hex_bytes)?;
        Ok(PreviewBody::Binary(binary(
            &head,
            meta.size,
            limits,
            Some(note),
        )))
    };
    // Read the file once (bounded by `max_image_file_size`) and decode from memory.
    let bytes = fs::read(path)?;
    check(cancel)?;
    let open = || {
        let mut r = image::ImageReader::new(io::Cursor::new(&bytes[..])).with_guessed_format()?;
        let mut lim = image::Limits::default();
        lim.max_alloc = Some(limits.max_image_pixels.saturating_mul(4).max(1 << 20));
        r.limits(lim);
        Ok::<_, io::Error>(r)
    };
    let reader = match open() {
        Ok(r) => r,
        Err(e) => return fallback(format!("image could not be opened: {e}")),
    };
    let format = reader
        .format()
        .map(|f| format!("{f:?}").to_ascii_uppercase())
        .unwrap_or_else(|| "IMAGE".into());
    let (w, h) = match reader.into_dimensions() {
        Ok(d) => d,
        Err(e) => return fallback(format!("image header unreadable: {e}")),
    };
    if u64::from(w) * u64::from(h) > limits.max_image_pixels {
        return Ok(PreviewBody::Unavailable {
            reason: UnavailableReason::TooLarge,
            message: format!("{format} image is {w}x{h} pixels, too large to decode"),
        });
    }
    check(cancel)?;
    let decoded = match open()
        .map_err(|e| e.to_string())
        .and_then(|r| r.decode().map_err(|e| e.to_string()))
    {
        Ok(d) => d,
        Err(e) => return fallback(format!("image failed to decode: {e}")),
    };
    check(cancel)?;
    let max = limits.image_max_dim.max(1);
    let (img, downscaled) = if w > max || h > max {
        (decoded.thumbnail(max, max), true)
    } else {
        (decoded, false)
    };
    Ok(PreviewBody::Image(ImagePreview {
        image: Arc::new(img),
        width: w,
        height: h,
        format,
        downscaled,
    }))
}

// ------------------------------------------------------------------ archive

fn archive_ext(ext: &str) -> Option<ArchiveFormat> {
    Some(match ext {
        "zip" | "jar" | "war" | "apk" | "whl" | "nupkg" | "vsix" | "docx" | "xlsx" | "pptx"
        | "odt" | "ods" | "epub" | "xpi" => ArchiveFormat::Zip,
        "tar" => ArchiveFormat::Tar,
        "gz" | "tgz" => ArchiveFormat::Gzip,
        "bz2" | "tbz" | "tbz2" => ArchiveFormat::Bzip2,
        "xz" | "txz" => ArchiveFormat::Xz,
        "zst" | "tzst" => ArchiveFormat::Zstd,
        "7z" => ArchiveFormat::SevenZip,
        "rar" => ArchiveFormat::Rar,
        _ => return None,
    })
}

fn archive_magic(h: &[u8]) -> Option<ArchiveFormat> {
    if h.starts_with(&[0x1f, 0x8b]) {
        Some(ArchiveFormat::Gzip)
    } else if h.starts_with(b"7z\xbc\xaf\x27\x1c") {
        Some(ArchiveFormat::SevenZip)
    } else if h.starts_with(b"Rar!\x1a\x07") {
        Some(ArchiveFormat::Rar)
    } else if h.starts_with(b"BZh") && h.len() > 4 && h[3].is_ascii_digit() {
        Some(ArchiveFormat::Bzip2)
    } else if h.starts_with(&[0xfd, b'7', b'z', b'X', b'Z', 0]) {
        Some(ArchiveFormat::Xz)
    } else if h.starts_with(&[0x28, 0xb5, 0x2f, 0xfd]) {
        Some(ArchiveFormat::Zstd)
    } else if h.len() > 262 && &h[257..262] == b"ustar" {
        Some(ArchiveFormat::Tar)
    } else {
        None
    }
}

fn archive(
    path: &Path,
    format: ArchiveFormat,
    opts: &PreviewOptions,
    cancel: &CancellationToken,
) -> Built {
    if format != ArchiveFormat::Zip {
        return Ok(PreviewBody::Archive(ArchivePreview {
            format,
            listed: false,
            entries: Vec::new(),
            total_entries: 0,
            total_size: 0,
            truncated: false,
        }));
    }
    let file = File::open(path)?;
    let mut zip = match zip::ZipArchive::new(BufReader::new(file)) {
        Ok(z) => z,
        Err(zip::result::ZipError::Io(e)) => return Err(e.into()),
        Err(e) => {
            let head = read_head(path, opts.limits.hex_bytes)?;
            let size = fs::metadata(path)
                .map(|m| m.len())
                .unwrap_or(head.len() as u64);
            return Ok(PreviewBody::Binary(binary(
                &head,
                size,
                &opts.limits,
                Some(format!("not a readable zip archive: {e}")),
            )));
        }
    };
    let total_entries = zip.len();
    let cap = opts.limits.max_archive_entries;
    let mut entries = Vec::with_capacity(total_entries.min(cap));
    let mut total_size = 0u64;
    for i in 0..total_entries.min(cap) {
        if i % 256 == 0 {
            check(cancel)?;
        }
        // `by_index_raw` avoids decompression and password errors.
        let Ok(f) = zip.by_index_raw(i) else { continue };
        total_size = total_size.saturating_add(f.size());
        entries.push(ArchiveEntry {
            name: sanitize_line(&String::from_utf8_lossy(f.name_raw()), 4),
            size: f.size(),
            compressed_size: f.compressed_size(),
            is_dir: f.is_dir(),
        });
    }
    Ok(PreviewBody::Archive(ArchivePreview {
        format,
        listed: true,
        entries,
        total_entries,
        total_size,
        truncated: total_entries > cap,
    }))
}

// ------------------------------------------------------------------- binary

fn type_hint(h: &[u8]) -> Option<&'static str> {
    const TABLE: &[(&[u8], &str)] = &[
        (b"MZ", "Windows executable / DLL"),
        (b"\x7fELF", "ELF executable"),
        (b"%PDF", "PDF document"),
        (b"SQLite format 3\0", "SQLite database"),
        (b"\xca\xfe\xba\xbe", "Java class / Mach-O fat"),
        (b"\0asm", "WebAssembly module"),
        (b"\xd0\xcf\x11\xe0", "OLE2 document"),
        (b"RIFF", "RIFF container"),
        (b"OggS", "Ogg media"),
        (b"ID3", "MP3 audio"),
        (b"fLaC", "FLAC audio"),
        (b"\x1a\x45\xdf\xa3", "Matroska / WebM video"),
    ];
    TABLE
        .iter()
        .find(|(m, _)| h.starts_with(m))
        .map(|(_, n)| *n)
}

pub(crate) fn binary(
    head: &[u8],
    total_size: u64,
    limits: &Limits,
    note: Option<String>,
) -> BinaryPreview {
    let shown = &head[..head.len().min(limits.hex_bytes)];
    let rows = shown
        .chunks(16)
        .enumerate()
        .map(|(i, c)| HexRow {
            offset: (i * 16) as u64,
            bytes: c.to_vec(),
        })
        .collect();
    BinaryPreview {
        rows,
        total_size,
        type_hint: type_hint(head),
        note,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn natural_order() {
        let mut v = vec!["file10", "File2", "file1", "a", "B"];
        v.sort_by(|a, b| natural_cmp(a, b));
        assert_eq!(v, vec!["a", "B", "file1", "File2", "file10"]);
        assert_eq!(natural_cmp("a01", "a1"), Ordering::Equal);
    }

    #[test]
    fn hex_row_formatting() {
        let r = HexRow {
            offset: 0,
            bytes: b"Hello\x00\xff".to_vec(),
        };
        assert!(r.hex().starts_with("48 65 6c 6c 6f 00 ff"));
        assert_eq!(r.ascii(), "Hello..");
        assert_eq!(r.hex().len(), 48);
    }
}

#[cfg(test)]
mod placeholder_tests {
    use super::*;
    use crate::metadata::{
        Attributes, FILE_ATTRIBUTE_OFFLINE, FILE_ATTRIBUTE_RECALL_ON_DATA_ACCESS,
        FILE_ATTRIBUTE_RECALL_ON_OPEN,
    };

    fn body_with_attrs(path: &Path, raw: u32) -> PreviewBody {
        let mut meta = FileMetadata::read(path).expect("stat");
        meta.attributes = Attributes::from_raw(raw);
        build_with_meta(
            path,
            meta,
            &PreviewOptions::default(),
            &CancellationToken::new(),
        )
        .expect("not cancelled")
        .body
    }

    #[test]
    fn cloud_attributes_make_files_and_folders_unavailable() {
        let dir = tempfile::tempdir().expect("tempdir");
        let f = dir.path().join("a.txt");
        fs::write(&f, "x").expect("write");
        for bit in [
            FILE_ATTRIBUTE_RECALL_ON_DATA_ACCESS,
            FILE_ATTRIBUTE_OFFLINE,
            FILE_ATTRIBUTE_RECALL_ON_OPEN,
        ] {
            assert!(Attributes::from_raw(bit).cloud_placeholder);
            for p in [f.as_path(), dir.path()] {
                assert!(
                    matches!(
                        body_with_attrs(p, bit),
                        PreviewBody::Unavailable {
                            reason: UnavailableReason::CloudPlaceholder,
                            ..
                        }
                    ),
                    "{bit:#x} {p:?}"
                );
            }
        }
        assert!(!Attributes::from_raw(0x20).cloud_placeholder);
    }

    #[test]
    fn io_errors_are_classified() {
        let denied = io::Error::from(io::ErrorKind::PermissionDenied);
        assert_eq!(classify_io(&denied).0, UnavailableReason::PermissionDenied);
        let locked = io::Error::from_raw_os_error(32);
        assert_eq!(classify_io(&locked).0, UnavailableReason::Locked);
        let nf = io::Error::from(io::ErrorKind::NotFound);
        assert_eq!(classify_io(&nf).0, UnavailableReason::NotFound);
    }
}
