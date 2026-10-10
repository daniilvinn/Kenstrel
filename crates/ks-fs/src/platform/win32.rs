//! Win32 calls. This is the only module in the workspace that uses `unsafe`; each block
//! wraps exactly one FFI call whose arguments are owned, correctly sized buffers.
#![allow(unsafe_code)]

use std::ffi::OsString;
use std::os::windows::ffi::{OsStrExt, OsStringExt};
use std::path::{Path, PathBuf};

use ks_core::{DriveInfo, DriveKind};
use windows::core::{PCWSTR, PWSTR};
use windows::Win32::Foundation::{LocalFree, ERROR_SUCCESS, HLOCAL};
use windows::Win32::Security::Authorization::{
    ConvertSidToStringSidW, GetNamedSecurityInfoW, SE_FILE_OBJECT,
};
use windows::Win32::Security::{
    LookupAccountSidW, OWNER_SECURITY_INFORMATION, PSECURITY_DESCRIPTOR, PSID, SID_NAME_USE,
};
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
/// drive" system dialog; drives that cannot be queried are listed without details.
pub fn list_drives() -> Vec<DriveInfo> {
    // SAFETY: no pointers involved.
    let mask = unsafe { GetLogicalDrives() };
    let mut previous_mode = Default::default();
    // SAFETY: `previous_mode` is a valid out-pointer for the call.
    let mode_set =
        unsafe { SetThreadErrorMode(SEM_FAILCRITICALERRORS, Some(&mut previous_mode)) }.is_ok();

    let drives = (0..26u8)
        .filter(|bit| mask & (1 << bit) != 0)
        .map(|bit| query_drive(PathBuf::from(format!("{}:\\", char::from(b'A' + bit)))))
        .collect();

    if mode_set {
        // SAFETY: restores the value returned by the call above.
        let _ = unsafe { SetThreadErrorMode(previous_mode, None) };
    }
    drives
}

fn query_drive(root: PathBuf) -> DriveInfo {
    let wide_root = wide(&root);
    let root_ptr = PCWSTR(wide_root.as_ptr());
    let mut info = DriveInfo::new(root);

    // SAFETY: `root_ptr` points at a NUL-terminated buffer that outlives the call.
    info.kind = match unsafe { GetDriveTypeW(root_ptr) } {
        DRIVE_REMOVABLE => DriveKind::Removable,
        DRIVE_FIXED => DriveKind::Fixed,
        DRIVE_REMOTE => DriveKind::Network,
        DRIVE_CDROM => DriveKind::Optical,
        DRIVE_RAMDISK => DriveKind::RamDisk,
        _ => DriveKind::Unknown,
    };

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

/// Owner of `path` as `DOMAIN\user`, or the SID string when the account cannot be resolved
/// (orphaned SID, offline domain). `None` when the owner cannot be read at all (no security
/// descriptor on FAT/exFAT, access denied). Blocking: the preview worker threads call it.
pub fn file_owner(path: &Path) -> Option<String> {
    let name = wide(path);
    let mut owner = PSID::default();
    let mut descriptor = PSECURITY_DESCRIPTOR::default();
    // SAFETY: `name` is NUL-terminated and outlives the call; the out-pointers reference live
    // locals. On success `owner` points into `descriptor`, which is freed below.
    let status = unsafe {
        GetNamedSecurityInfoW(
            PCWSTR(name.as_ptr()),
            SE_FILE_OBJECT,
            OWNER_SECURITY_INFORMATION,
            Some(&mut owner),
            None,
            None,
            None,
            &mut descriptor,
        )
    };
    if status != ERROR_SUCCESS {
        return None;
    }
    // A descriptor without an owner still has to be freed.
    let result = if owner.0.is_null() {
        None
    } else {
        account_name(owner).or_else(|| sid_string(owner))
    };
    // SAFETY: `descriptor` was allocated by GetNamedSecurityInfoW with LocalAlloc.
    let _ = unsafe { LocalFree(Some(HLOCAL(descriptor.0))) };
    result
}

/// `DOMAIN\user` for a SID.
fn account_name(sid: PSID) -> Option<String> {
    let (mut name_len, mut domain_len) = (0u32, 0u32);
    let mut kind = SID_NAME_USE::default();
    // SAFETY: the first call only asks for the buffer sizes (null buffers, zero lengths).
    let _ = unsafe {
        LookupAccountSidW(
            PCWSTR::null(),
            sid,
            None,
            &mut name_len,
            None,
            &mut domain_len,
            &mut kind,
        )
    };
    if name_len == 0 {
        return None;
    }
    let mut name = vec![0u16; name_len as usize];
    let mut domain = vec![0u16; domain_len.max(1) as usize];
    // SAFETY: both buffers have the lengths reported by the first call.
    unsafe {
        LookupAccountSidW(
            PCWSTR::null(),
            sid,
            Some(PWSTR(name.as_mut_ptr())),
            &mut name_len,
            Some(PWSTR(domain.as_mut_ptr())),
            &mut domain_len,
            &mut kind,
        )
    }
    .ok()?;
    let user = from_wide_until_nul(&name);
    let domain = from_wide_until_nul(&domain);
    Some(if domain.is_empty() {
        user
    } else {
        format!("{domain}\\{user}")
    })
}

/// `S-1-5-21-...` for a SID.
fn sid_string(sid: PSID) -> Option<String> {
    let mut text = PWSTR::null();
    // SAFETY: `text` receives a LocalAlloc'd NUL-terminated string, freed below.
    unsafe { ConvertSidToStringSidW(sid, &mut text) }.ok()?;
    // SAFETY: on success `text` is a valid NUL-terminated wide string.
    let out = unsafe { text.to_string() }.ok();
    // SAFETY: allocated by ConvertSidToStringSidW with LocalAlloc.
    let _ = unsafe { LocalFree(Some(HLOCAL(text.0.cast()))) };
    out
}

#[cfg(test)]
mod owner_tests {
    use super::*;

    #[test]
    fn owner_of_a_temp_file_is_a_domain_user_or_sid() {
        let dir = tempfile::tempdir().unwrap();
        let file = dir.path().join("a.txt");
        std::fs::write(&file, "x").unwrap();
        let owner = file_owner(&file).expect("owner of a file we just made");
        // The creator normally owns it (or BUILTIN\Administrators for an elevated token).
        let user = std::env::var("USERNAME").unwrap_or_default();
        assert!(
            owner.contains('\\') && owner.to_lowercase().contains(&user.to_lowercase())
                || owner.starts_with("S-1-")
                || owner.contains("Administrators"),
            "{owner}"
        );
        assert_eq!(file_owner(&dir.path().join("missing")), None);
    }
}
