//! Errors that cross from background work into `update`, as plain data.

use std::fmt;
use std::io;

/// Coarse classification of an I/O failure; `update` reacts differently to a directory that
/// vanished than to one it may not read.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum FsErrorKind {
    NotFound,
    PermissionDenied,
    NotADirectory,
    Other,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct FsError {
    pub kind: FsErrorKind,
    pub message: String,
}

impl FsError {
    pub fn new(kind: FsErrorKind, message: impl Into<String>) -> Self {
        Self {
            kind,
            message: message.into(),
        }
    }
}

impl From<&io::Error> for FsError {
    fn from(e: &io::Error) -> Self {
        let kind = match e.kind() {
            io::ErrorKind::NotFound => FsErrorKind::NotFound,
            io::ErrorKind::PermissionDenied => FsErrorKind::PermissionDenied,
            io::ErrorKind::NotADirectory => FsErrorKind::NotADirectory,
            _ => FsErrorKind::Other,
        };
        Self {
            kind,
            message: e.to_string(),
        }
    }
}

impl fmt::Display for FsError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.message)
    }
}
