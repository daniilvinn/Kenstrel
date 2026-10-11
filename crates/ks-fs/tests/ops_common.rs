//! Shared helpers for the `ops_*` integration tests (included with `#[path]`).
#![allow(dead_code)]

use std::fs;
use std::path::{Path, PathBuf};
use std::sync::mpsc::{channel, Receiver};
use std::time::Duration;

use ks_fs::ops::{
    ConflictPolicy, ConflictPrompt, Generation, JobEvent, JobId, JobManager, JobOptions, JobReport,
    JobSpec, Resolution,
};

pub struct Harness {
    pub mgr: JobManager,
    pub rx: Receiver<JobEvent>,
}

pub struct Run {
    pub id: JobId,
    pub events: Vec<JobEvent>,
}

impl Run {
    pub fn last(&self) -> &JobEvent {
        self.events.last().expect("no events")
    }
    pub fn report(&self) -> &JobReport {
        match self.last() {
            JobEvent::Done(r) | JobEvent::Failed(r) | JobEvent::Cancelled(r) => r,
            other => panic!("job not finished: {other:?}"),
        }
    }
    pub fn assert_done(&self) -> &JobReport {
        match self.last() {
            JobEvent::Done(r) => r,
            other => panic!("expected Done, got {other:?}"),
        }
    }
    pub fn prompts(&self) -> Vec<&ConflictPrompt> {
        self.events
            .iter()
            .filter_map(|e| match e {
                JobEvent::ConflictPrompt(p) => Some(p),
                _ => None,
            })
            .collect()
    }
}

impl Default for Harness {
    fn default() -> Self {
        Self::new()
    }
}

impl Harness {
    pub fn new() -> Self {
        let (tx, rx) = channel();
        Self {
            mgr: JobManager::new(tx),
            rx,
        }
    }

    /// Submit and collect events until the job ends. `answer` is called for each prompt.
    pub fn run_with(
        &self,
        spec: JobSpec,
        opts: JobOptions,
        mut answer: impl FnMut(&ConflictPrompt) -> Resolution,
    ) -> Run {
        let id = self.mgr.submit_with(spec, Generation(42), opts);
        let mut events = Vec::new();
        loop {
            let ev = self
                .rx
                .recv_timeout(Duration::from_secs(120))
                .expect("timed out waiting for job");
            if ev.job() != id {
                continue;
            }
            assert_eq!(ev.generation(), Generation(42));
            if let JobEvent::ConflictPrompt(p) = &ev {
                let r = answer(p);
                assert!(self.mgr.resolve(id, r));
            }
            let fin = ev.is_final();
            events.push(ev);
            if fin {
                return Run { id, events };
            }
        }
    }

    pub fn run(&self, spec: JobSpec) -> Run {
        self.run_policy(spec, ConflictPolicy::Ask)
    }

    pub fn run_policy(&self, spec: JobSpec, policy: ConflictPolicy) -> Run {
        self.run_with(
            spec,
            JobOptions {
                conflict: policy,
                ..JobOptions::default()
            },
            |p| panic!("unexpected conflict prompt: {p:?}"),
        )
    }

    /// Run the manager's undo and wait for it.
    pub fn undo(&self) -> Run {
        let id = self.mgr.undo(Generation(42)).expect("undo");
        let mut events = Vec::new();
        loop {
            let ev = self
                .rx
                .recv_timeout(Duration::from_secs(120))
                .expect("timed out");
            if ev.job() != id {
                continue;
            }
            let fin = ev.is_final();
            events.push(ev);
            if fin {
                return Run { id, events };
            }
        }
    }
}

pub fn write(path: &Path, content: &str) {
    if let Some(p) = path.parent() {
        fs::create_dir_all(p).unwrap();
    }
    fs::write(path, content).unwrap();
}

pub fn read(path: &Path) -> String {
    fs::read_to_string(path).unwrap()
}

/// A small tree: files, nested dirs, an empty dir, Unicode names.
pub fn make_tree(root: &Path) {
    write(&root.join("a.txt"), "alpha");
    write(&root.join("sub").join("b.txt"), "bravo");
    write(
        &root.join("sub").join("deep").join("c.bin"),
        &"c".repeat(10_000),
    );
    write(&root.join("sub").join("файл ✓ 日本.txt"), "unicode");
    fs::create_dir_all(root.join("empty")).unwrap();
}

/// Sorted relative listing of every entry under `root` (dirs end with `/`).
pub fn listing(root: &Path) -> Vec<String> {
    fn walk(dir: &Path, root: &Path, out: &mut Vec<String>) {
        for e in fs::read_dir(dir).unwrap() {
            let e = e.unwrap();
            let p = e.path();
            let rel = p
                .strip_prefix(root)
                .unwrap()
                .to_string_lossy()
                .replace('\\', "/");
            if e.file_type().unwrap().is_dir() {
                out.push(format!("{rel}/"));
                walk(&p, root, out);
            } else {
                out.push(rel);
            }
        }
    }
    let mut out = Vec::new();
    walk(root, root, &mut out);
    out.sort();
    out
}

pub fn pb(p: &Path, name: &str) -> PathBuf {
    p.join(name)
}

/// Remove Recycle Bin entries that originate from under `root`, so tests do not litter the
/// user's bin. Best effort.
pub fn purge_trash_under(root: &Path) {
    #[cfg(any(windows, all(unix, not(target_os = "macos"))))]
    if let Ok(items) = trash::os_limited::list() {
        let key = |p: &Path| p.to_string_lossy().to_lowercase();
        let root = key(dunce::simplified(root));
        let mine: Vec<_> = items
            .into_iter()
            .filter(|i| key(&i.original_parent).starts_with(&root))
            .collect();
        let _ = trash::os_limited::purge_all(mine);
    }
    #[cfg(not(any(windows, all(unix, not(target_os = "macos")))))]
    let _ = root;
}

/// Whether the machine has a working Recycle Bin / trash: probed once by trashing a scratch
/// file. A CI runner or container without one skips the tests that need it (with a message)
/// instead of failing them.
pub fn recycle_bin_usable() -> bool {
    static USABLE: std::sync::OnceLock<bool> = std::sync::OnceLock::new();
    *USABLE.get_or_init(|| {
        let dir = tempfile::tempdir().expect("scratch dir");
        let probe = dir.path().join("kestrel-trash-probe.txt");
        std::fs::write(&probe, "probe").expect("probe file");
        let mut usable = trash::delete(&probe).is_ok() && !probe.exists();
        // Deleting is not enough: undo needs the bin to list the item back (a CI runner's
        // Recycle Bin accepts files but lists nothing).
        #[cfg(any(windows, all(unix, not(target_os = "macos"))))]
        if usable {
            let key = |p: &Path| p.to_string_lossy().to_lowercase();
            let want = key(dunce::simplified(dir.path()));
            usable = trash::os_limited::list()
                .map(|items| {
                    items
                        .iter()
                        .any(|i| key(&i.original_parent).starts_with(&want))
                })
                .unwrap_or(false);
        }
        purge_trash_under(dir.path());
        if !usable {
            #[allow(clippy::print_stderr)]
            {
                eprintln!("SKIP: this machine has no usable Recycle Bin / trash");
            }
        }
        usable
    })
}

/// Whether trash restore tests can run. On Windows support is mandatory, so a regression in
/// capability detection fails loudly instead of silently skipping; elsewhere the test skips. A
/// machine without a working bin (see [`recycle_bin_usable`]) skips too.
pub fn trash_supported() -> bool {
    let supported = std::hint::black_box(ks_fs::ops::TRASH_RESTORE_SUPPORTED);
    #[cfg(windows)]
    assert!(supported, "trash restore must be supported on Windows");
    supported && recycle_bin_usable()
}
