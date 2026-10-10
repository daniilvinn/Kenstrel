//! Win32 calls. This is the only module in the workspace that uses `unsafe`; each block
//! wraps exactly one FFI call whose arguments are owned, correctly sized buffers.
#![allow(unsafe_code)]

use std::ffi::OsString;
use std::os::windows::ffi::{OsStrExt, OsStringExt};
use std::path::{Path, PathBuf};

use ks_core::{DriveInfo, DriveKind};
use windows::core::PCWSTR;
use windows::Win32::Storage::FileSystem::{
    FindClose, FindFirstFileW, GetDiskFreeSpaceExW, GetDriveTypeW, GetLogicalDrives,
    GetVolumeInformationW, WIN32_FIND_DATAW,
};
use windows::Win32::System::Diagnostics::Debug::{SetThreadErrorMode, SEM_FAILCRITICALERRORS};
use windows::Win32::System::Time::{GetTimeZoneInformation, TIME_ZONE_INFORMATION};

use crate::attrs::win;

const DRIVE_REMOVABLE: u32 = 2;
const DRIVE_FIXED: u32 = 3;
const DRIVE_REMOTE: u32 = 4;
const DRIVE_CDROM: u32 = 5;
const DRIVE_RAMDISK: u32 = 6;

/// NUL-terminated UTF-16 for the wide Win32 APIs.
fn wide(path: &Path) -> Vec<u16> {
    path.as_os_str().encode_wide().chain(Some(0)).collect()
}

/// Beyond this length the wide Win32 APIs need a `\?\` prefix (MAX_PATH is 260).
const LONG_PATH_THRESHOLD: usize = 240;

/// `path` with a verbatim prefix when it is long and absolute, so `FindFirstFileW` copes
/// with the deep trees `std::fs::read_dir` already handles.
fn long_path_safe(path: &Path) -> PathBuf {
    let text = path.as_os_str().to_string_lossy();
    if text.len() < LONG_PATH_THRESHOLD || text.starts_with(r"\?\") {
        return path.to_owned();
    }
    if let Some(unc) = text.strip_prefix(r"\\") {
        return PathBuf::from(format!(r"\?\UNC\{unc}"));
    }
    let bytes = text.as_bytes();
    if bytes.len() > 2 && bytes[1] == b':' && bytes[2] == b'\\' {
        return PathBuf::from(format!(r"\?\{text}"));
    }
    path.to_owned()
}

fn from_wide_until_nul(buf: &[u16]) -> String {
    let end = buf.iter().position(|&c| c == 0).unwrap_or(buf.len());
    OsString::from_wide(&buf[..end])
        .to_string_lossy()
        .into_owned()
}

/// The reparse tag of the link at `path` (read from `FindFirstFileW`, which reports it in
/// `dwReserved0` without opening the target).
pub fn reparse_tag(path: &Path) -> Option<u32> {
    let name = wide(&long_path_safe(path));
    let mut data = WIN32_FIND_DATAW::default();
    // SAFETY: `name` is NUL-terminated and outlives the call; `data` is a valid out-buffer.
    let handle = unsafe { FindFirstFileW(PCWSTR(name.as_ptr()), &mut data) }.ok()?;
    // SAFETY: `handle` was just returned by a successful `FindFirstFileW`.
    let _ = unsafe { FindClose(handle) };
    (data.dwFileAttributes & win::REPARSE_POINT != 0).then_some(data.dwReserved0)
}

/// Drives with label, file system and free/total space. Never shows the "no disk in
/// drive" system dialog; drives that cannot be queried are listed without details, and a
/// drive that does not answer within [`crate::drives::QUERY_TIMEOUT`] (a dead network
/// share) is listed as unavailable instead of blocking the list.
pub fn list_drives() -> Vec<DriveInfo> {
    list_drives_within(crate::drives::QUERY_TIMEOUT)
}

fn list_drives_within(timeout: std::time::Duration) -> Vec<DriveInfo> {
    // SAFETY: no pointers involved.
    let mask = unsafe { GetLogicalDrives() };
    let roots: Vec<PathBuf> = (0..26u8)
        .filter(|bit| mask & (1 << bit) != 0)
        .map(|bit| PathBuf::from(format!("{}:\\", char::from(b'A' + bit))))
        .collect();
    crate::drives::query_all(
        roots,
        timeout,
        std::sync::Arc::new(|root: &Path| query_drive(root.to_path_buf())),
        |root| {
            let mut info = DriveInfo::new(root);
            info.kind = drive_kind(&wide(root));
            info
        },
    )
}

fn drive_kind(wide_root: &[u16]) -> DriveKind {
    // SAFETY: `wide_root` is NUL-terminated and outlives the call.
    match unsafe { GetDriveTypeW(PCWSTR(wide_root.as_ptr())) } {
        DRIVE_REMOVABLE => DriveKind::Removable,
        DRIVE_FIXED => DriveKind::Fixed,
        DRIVE_REMOTE => DriveKind::Network,
        DRIVE_CDROM => DriveKind::Optical,
        DRIVE_RAMDISK => DriveKind::RamDisk,
        _ => DriveKind::Unknown,
    }
}

/// Queries one drive. Runs on a thread of its own (the error mode is per thread).
fn query_drive(root: PathBuf) -> DriveInfo {
    let mut previous_mode = Default::default();
    // SAFETY: `previous_mode` is a valid out-pointer for the call.
    let mode_set =
        unsafe { SetThreadErrorMode(SEM_FAILCRITICALERRORS, Some(&mut previous_mode)) }.is_ok();
    let info = query_drive_inner(root);
    if mode_set {
        // SAFETY: restores the value returned by the call above.
        let _ = unsafe { SetThreadErrorMode(previous_mode, None) };
    }
    info
}

fn query_drive_inner(root: PathBuf) -> DriveInfo {
    let wide_root = wide(&root);
    let root_ptr = PCWSTR(wide_root.as_ptr());
    let mut info = DriveInfo::new(root);

    info.kind = drive_kind(&wide_root);

    let mut label = [0u16; 261];
    let mut fs = [0u16; 261];
    // SAFETY: the buffers are valid for the lengths implied by the slices.
    let volume = unsafe {
        GetVolumeInformationW(root_ptr, Some(&mut label), None, None, None, Some(&mut fs))
    };
    if volume.is_ok() {
        info.label = from_wide_until_nul(&label);
        info.fs = from_wide_until_nul(&fs);
    }

    let (mut free, mut total) = (0u64, 0u64);
    // SAFETY: the out-pointers reference live locals.
    let space = unsafe { GetDiskFreeSpaceExW(root_ptr, Some(&mut free), Some(&mut total), None) };
    if space.is_ok() {
        info.free = Some(free);
        info.total = Some(total);
    }
    info
}

/// Local time's offset from UTC in seconds, as of now (includes daylight saving).
pub fn utc_offset_secs() -> i32 {
    const TIME_ZONE_ID_DAYLIGHT: u32 = 2;
    let mut tz = TIME_ZONE_INFORMATION::default();
    // SAFETY: `tz` is a valid out-buffer.
    let id = unsafe { GetTimeZoneInformation(&mut tz) };
    // `Bias` is minutes *west* of UTC: UTC = local + bias.
    let bias = tz.Bias
        + if id == TIME_ZONE_ID_DAYLIGHT {
            tz.DaylightBias
        } else {
            tz.StandardBias
        };
    -bias * 60
}
