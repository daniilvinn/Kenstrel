//! File logging. Nothing is ever written to stdout/stderr while the TUI is up.

use std::io;
use std::path::Path;

use tracing_appender::non_blocking::WorkerGuard;
use tracing_appender::rolling::{Builder, RollingFileAppender, Rotation};
use tracing_subscriber::EnvFilter;

/// Environment variable overriding the log filter (`KESTREL_LOG=debug`).
const FILTER_ENV: &str = "KESTREL_LOG";
const KEEP_FILES: usize = 7;

fn open_log_file(dir: &Path) -> io::Result<RollingFileAppender> {
    std::fs::create_dir_all(dir)?;
    Builder::new()
        .rotation(Rotation::DAILY)
        .filename_prefix("kestrel")
        .filename_suffix("log")
        .max_log_files(KEEP_FILES)
        .build(dir)
        .map_err(|e| io::Error::other(e.to_string()))
}

/// Starts logging to `<dir>/kestrel.<date>.log`. Keep the returned guard alive until exit
/// so buffered lines are flushed. If the log directory is unusable the app runs without
/// logging rather than failing (or, worse, printing over the UI).
pub fn init(dir: &Path) -> Option<WorkerGuard> {
    let appender = open_log_file(dir).ok()?;
    let (writer, guard) = tracing_appender::non_blocking(appender);
    let filter = EnvFilter::try_from_env(FILTER_ENV).unwrap_or_else(|_| EnvFilter::new("info"));
    tracing_subscriber::fmt()
        .with_writer(writer)
        .with_ansi(false)
        .with_env_filter(filter)
        .try_init()
        .ok()?;
    Some(guard)
}

#[cfg(test)]
mod tests {
    use std::io::Write;

    use super::*;

    #[test]
    fn creates_the_directory_and_a_dated_log_file() {
        let root = tempfile::tempdir().unwrap();
        let dir = root.path().join("nested").join("logs");
        let mut appender = open_log_file(&dir).unwrap();
        appender.write_all(b"hello\n").unwrap();
        appender.flush().unwrap();

        let names: Vec<String> = std::fs::read_dir(&dir)
            .unwrap()
            .map(|e| e.unwrap().file_name().to_string_lossy().into_owned())
            .collect();
        assert_eq!(names.len(), 1, "{names:?}");
        assert!(
            names[0].starts_with("kestrel.") && names[0].ends_with(".log"),
            "{names:?}"
        );
    }

    #[test]
    fn unusable_directory_disables_logging_instead_of_failing() {
        let root = tempfile::tempdir().unwrap();
        let blocker = root.path().join("file");
        std::fs::write(&blocker, "").unwrap();
        assert!(open_log_file(&blocker.join("logs")).is_err());
        assert!(init(&blocker.join("logs")).is_none());
    }
}
