//! Querying many drives with a deadline: a dead network share must not hold up the whole
//! "This PC" list (`GetVolumeInformationW` on an unreachable server can block for tens of
//! seconds and cannot be cancelled).

use std::path::{Path, PathBuf};
use std::sync::mpsc;
use std::sync::Arc;
use std::time::{Duration, Instant};

use ks_core::DriveInfo;

/// How long the whole drive list waits for slow drives.
pub const QUERY_TIMEOUT: Duration = Duration::from_millis(1500);

/// Runs `query` for every root on a thread of its own and collects the answers that arrive
/// within `timeout` (counted from the call). A drive that has not answered by then is listed
/// as `unavailable(root)` and its thread is left to finish (and be ignored) on its own.
/// The result keeps the order of `roots`.
pub fn query_all(
    roots: Vec<PathBuf>,
    timeout: Duration,
    query: Arc<dyn Fn(&Path) -> DriveInfo + Send + Sync>,
    unavailable: impl Fn(&Path) -> DriveInfo,
) -> Vec<DriveInfo> {
    let (tx, rx) = mpsc::channel::<(usize, DriveInfo)>();
    let mut answers: Vec<Option<DriveInfo>> = vec![None; roots.len()];
    let mut pending = 0;
    for (index, root) in roots.iter().enumerate() {
        let (thread_tx, thread_query, thread_root) = (tx.clone(), query.clone(), root.clone());
        let spawned = std::thread::Builder::new()
            .name("ks-drive-query".into())
            .spawn(move || {
                let info = thread_query(&thread_root);
                let _ = thread_tx.send((index, info));
            });
        match spawned {
            Ok(_) => pending += 1,
            Err(error) => {
                // Querying inline could block on a dead share, which is what the deadline
                // exists to avoid: list the drive as unavailable instead.
                tracing::warn!(%error, "cannot start a drive query thread; drive listed as unavailable");
            }
        }
    }
    drop(tx);
    let deadline = Instant::now() + timeout;
    while pending > 0 {
        match rx.recv_timeout(deadline.saturating_duration_since(Instant::now())) {
            Ok((index, info)) => {
                answers[index] = Some(info);
                pending -= 1;
            }
            Err(_) => break,
        }
    }
    roots
        .iter()
        .zip(answers)
        .map(|(root, answer)| {
            answer.unwrap_or_else(|| {
                tracing::warn!(root = %root.display(), "drive did not answer in time");
                let mut info = unavailable(root);
                info.unavailable = true;
                info
            })
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn fast_info(root: &Path) -> DriveInfo {
        let mut info = DriveInfo::new(root);
        info.label = "ok".into();
        info.total = Some(10);
        info.free = Some(5);
        info
    }

    #[test]
    fn all_answers_are_kept_in_order() {
        let roots: Vec<PathBuf> = ["A:/", "B:/", "C:/"].iter().map(PathBuf::from).collect();
        let got = query_all(
            roots.clone(),
            Duration::from_secs(5),
            Arc::new(fast_info),
            |root| DriveInfo::new(root),
        );
        assert_eq!(
            got.iter().map(|d| d.root.clone()).collect::<Vec<_>>(),
            roots
        );
        assert!(got.iter().all(|d| !d.unavailable && d.label == "ok"));
    }

    #[test]
    fn a_hung_drive_is_listed_as_unavailable_without_blocking_the_rest() {
        let roots: Vec<PathBuf> = ["A:/", "Z:/", "C:/"].iter().map(PathBuf::from).collect();
        let query = |root: &Path| {
            if root.starts_with("Z:") {
                // A dead network share.
                std::thread::sleep(Duration::from_secs(30));
            }
            fast_info(root)
        };
        let started = Instant::now();
        let got = query_all(roots, Duration::from_millis(300), Arc::new(query), |root| {
            DriveInfo::new(root)
        });
        let took = started.elapsed();
        assert!(took < Duration::from_secs(5), "waited {took:?}");
        assert_eq!(got.len(), 3);
        assert!(!got[0].unavailable && got[0].label == "ok");
        assert!(got[1].unavailable && got[1].root == Path::new("Z:/"));
        assert!(!got[2].unavailable && got[2].label == "ok");
    }
}
