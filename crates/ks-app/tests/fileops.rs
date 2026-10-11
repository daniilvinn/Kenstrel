#![allow(clippy::print_stderr)]

//! Headless end-to-end tests of phase P6: the real `update`, the real effect runner (with the
//! real job engine) and real temporary directories, no terminal. A tiny harness plays the
//! event loop's part: it feeds messages to `update` and hands the effects to the runner.

use std::fs;
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant, SystemTime};

use ks_app::effects::EffectRunner;
use ks_core::fileops::{JobMsg, JobProgress, JobStatus, Register};
use ks_core::{
    update, AppState, Config, KeyChord, Keymap, Msg, Overlay, Places, Size, StartParams,
};
use tokio::sync::mpsc::{self, Receiver};

/// How long a wait may see no message at all before it fails. A job that is slowly making
/// progress (a 200 MB copy on a starved disk) keeps sending messages and so keeps waiting.
const SLACK: Duration = Duration::from_secs(60);

/// Upper bound for any single wait, progress or not.
const HARD_LIMIT: Duration = Duration::from_secs(900);

struct Harness {
    state: AppState,
    runner: EffectRunner,
    rx: Receiver<Msg>,
    progress: Vec<JobProgress>,
}

impl Harness {
    fn new(cwd: &Path) -> Self {
        Self::with_config(cwd, Config::default())
    }

    fn with_config(cwd: &Path, config: Config) -> Self {
        let (tx, rx) = mpsc::channel(1024);
        let mut runner = EffectRunner::new(tx);
        runner.configure(&config);
        let state = AppState::new(StartParams {
            cwd: cwd.to_owned(),
            home: None,
            places: Places::default(),
            config,
            keymap: Keymap::defaults(),
            size: Size::new(100, 30),
            now: SystemTime::now(),
            utc_offset_secs: 0,
        });
        Self {
            state,
            runner,
            rx,
            progress: Vec::new(),
        }
    }

    fn send(&mut self, msg: Msg) {
        if let Msg::Job(JobMsg::Progress { progress, .. }) = &msg {
            self.progress.push(progress.clone());
        }
        let effects = update(&mut self.state, msg);
        self.runner.run(effects);
    }

    fn keys(&mut self, text: &str) {
        for key in text.split(' ') {
            let chord = if key == "space" {
                KeyChord::char(' ')
            } else {
                key.parse::<KeyChord>().unwrap()
            };
            self.send(Msg::Key(chord));
        }
    }

    fn type_text(&mut self, text: &str) {
        for c in text.chars() {
            self.send(Msg::Key(KeyChord::char(c)));
        }
    }

    async fn until(&mut self, what: &str, done: impl Fn(&AppState) -> bool) {
        let started = Instant::now();
        while !done(&self.state) {
            if started.elapsed() > HARD_LIMIT {
                panic!("timed out waiting for {what}\n{}", self.describe());
            }
            let msg = tokio::time::timeout(SLACK, self.rx.recv())
                .await
                .unwrap_or_else(|_| panic!("timed out waiting for {what}\n{}", self.describe()))
                .expect("the runner keeps a sender alive");
            self.send(msg);
        }
    }

    async fn start(&mut self) {
        self.send(Msg::Start);
        self.until("the first listing", |s| {
            !s.view.loading && !s.is_navigating() && !s.view.is_empty()
        })
        .await;
    }

    async fn listed(&mut self, dir: &Path) {
        let dir = dir.to_owned();
        self.until("a directory listing", move |s| {
            s.cwd == dir && !s.view.loading && !s.is_navigating()
        })
        .await;
    }

    /// Waits until no job is active (and at least one job ran).
    async fn jobs_idle(&mut self) {
        self.until("the jobs to finish", |s| {
            !s.fileops.jobs.is_empty() && s.fileops.jobs.iter().all(|j| !j.status.is_active())
        })
        .await;
    }

    /// Waits until the listing shows `name`.
    async fn shows(&mut self, name: &str) {
        let name = name.to_owned();
        self.until("an entry to appear", move |s| {
            !s.is_navigating() && has(s, &name)
        })
        .await;
    }

    async fn gone(&mut self, name: &str) {
        let name = name.to_owned();
        self.until("an entry to vanish", move |s| {
            !s.is_navigating() && !has(s, &name)
        })
        .await;
    }

    fn cursor(&self) -> Option<String> {
        self.state
            .view
            .current()
            .map(|e| e.display_name().into_owned())
    }

    fn status(&self) -> String {
        self.state
            .status
            .as_ref()
            .map(|s| s.text.clone())
            .unwrap_or_default()
    }

    fn describe(&self) -> String {
        format!(
            "cwd={} status={:?} overlays={:?} jobs={:?}",
            self.state.cwd.display(),
            self.status(),
            self.state.overlays,
            self.state
                .fileops
                .jobs
                .iter()
                .map(|j| (&j.description, j.status))
                .collect::<Vec<_>>()
        )
    }
}

fn has(state: &AppState, name: &str) -> bool {
    (0..state.view.len())
        .filter_map(|i| state.view.row(i))
        .any(|e| e.display_name() == name)
}

fn listing(dir: &Path) -> Vec<String> {
    let mut names: Vec<String> = fs::read_dir(dir)
        .unwrap()
        .map(|e| e.unwrap().file_name().to_string_lossy().into_owned())
        .collect();
    names.sort();
    names
}

fn read(path: impl AsRef<Path>) -> String {
    fs::read_to_string(path).unwrap()
}

fn canonical(dir: &tempfile::TempDir) -> PathBuf {
    dunce::canonicalize(dir.path()).unwrap()
}

#[tokio::test]
async fn yank_paste_cut_paste_and_the_conflict_dialog() {
    let dir = tempfile::tempdir().unwrap();
    let root = canonical(&dir);
    fs::create_dir(root.join("sub")).unwrap();
    fs::write(root.join("a.txt"), "root").unwrap();

    let mut h = Harness::new(&root);
    h.start().await;
    // Directories come first: sub, a.txt.
    h.keys("j y");
    assert_eq!(h.status(), "1 yanked");

    h.keys("k l");
    h.listed(&root.join("sub")).await;
    h.keys("p");
    h.jobs_idle().await;
    assert_eq!(read(root.join("sub/a.txt")), "root");
    // The listing refreshes at once and the cursor lands on the pasted file.
    h.shows("a.txt").await;
    h.until("the cursor on the new item", |s| {
        s.view
            .current()
            .is_some_and(|e| e.display_name() == "a.txt")
    })
    .await;
    assert!(h.status().starts_with("Copied a.txt"), "{}", h.status());

    // Cut the copy and move it up: the original is in the way, so the job asks.
    fs::write(root.join("sub/a.txt"), "moved").unwrap();
    h.keys("x h");
    h.listed(&root).await;
    h.keys("p");
    h.until("the conflict dialog", |s| {
        s.overlays.last() == Some(&Overlay::Conflict)
            && s.fileops.conflicts.front().is_some_and(|d| d.armed)
    })
    .await;
    let dialog = &h.state.fileops.conflicts[0];
    assert!(dialog.info.dst.ends_with("a.txt"));
    assert_eq!(dialog.info.src_meta.len, 5);
    h.keys("k");
    h.jobs_idle().await;
    assert_eq!(
        read(root.join("a.txt")),
        "root",
        "keep both leaves the original"
    );
    assert_eq!(read(root.join("a (1).txt")), "moved");
    assert!(!root.join("sub/a.txt").exists(), "a cut is a move");
    assert!(h.state.fileops.register.is_none());
    h.shows("a (1).txt").await;
}

#[tokio::test]
async fn conflict_answers_overwrite_skip_and_rename() {
    let dir = tempfile::tempdir().unwrap();
    let root = canonical(&dir);
    let src = root.join("src");
    let dst = root.join("dst");
    fs::create_dir_all(&src).unwrap();
    fs::create_dir_all(&dst).unwrap();
    for name in ["one", "two", "three"] {
        fs::write(src.join(name), format!("new {name}")).unwrap();
        fs::write(dst.join(name), format!("old {name}")).unwrap();
    }
    let register = Register {
        paths: ["one", "two", "three"]
            .iter()
            .map(|n| src.join(n))
            .collect(),
        cut: false,
    };

    // Overwrite, and apply to all: one prompt only.
    let mut h = Harness::new(&dst);
    h.start().await;
    h.state.fileops.register = Some(register.clone());
    h.keys("p");
    h.until("the conflict dialog", |s| {
        s.fileops.conflicts.front().is_some_and(|d| d.armed)
    })
    .await;
    h.keys("a O");
    h.jobs_idle().await;
    for name in ["one", "two", "three"] {
        assert_eq!(read(dst.join(name)), format!("new {name}"));
    }

    // Skip everything.
    for name in ["one", "two", "three"] {
        fs::write(dst.join(name), format!("old {name}")).unwrap();
    }
    let mut h = Harness::new(&dst);
    h.start().await;
    h.state.fileops.register = Some(register.clone());
    h.keys("p");
    h.until("the conflict dialog", |s| {
        s.fileops.conflicts.front().is_some_and(|d| d.armed)
    })
    .await;
    h.keys("a s");
    h.jobs_idle().await;
    for name in ["one", "two", "three"] {
        assert_eq!(read(dst.join(name)), format!("old {name}"));
    }
    assert!(h.status().contains("Nothing"), "{}", h.status());

    // Answer each one differently: rename the first, skip the second, overwrite the third.
    let mut h = Harness::new(&dst);
    h.start().await;
    h.state.fileops.register = Some(register);
    h.keys("p");
    let mut answered = 0;
    while answered < 3 {
        h.until("a conflict dialog", |s| {
            s.fileops.conflicts.front().is_some_and(|d| d.armed)
        })
        .await;
        let name = h.state.fileops.conflicts[0]
            .info
            .dst
            .file_name()
            .unwrap()
            .to_string_lossy()
            .into_owned();
        match name.as_str() {
            "one" => {
                h.keys("r ctrl-u");
                h.type_text("uno");
                h.keys("enter");
            }
            "two" => h.keys("s"),
            _ => h.keys("O"),
        }
        answered += 1;
        // Wait for the dialog of this job to go away before looking for the next one.
        h.until("the dialog to close", |s| s.fileops.conflicts.is_empty())
            .await;
    }
    h.jobs_idle().await;
    assert_eq!(read(dst.join("uno")), "new one");
    assert_eq!(read(dst.join("one")), "old one");
    assert_eq!(read(dst.join("two")), "old two");
    assert_eq!(read(dst.join("three")), "new three");
}

#[tokio::test]
async fn escape_in_the_conflict_dialog_cancels_the_job() {
    let dir = tempfile::tempdir().unwrap();
    let root = canonical(&dir);
    fs::create_dir_all(root.join("src")).unwrap();
    fs::create_dir_all(root.join("dst")).unwrap();
    fs::write(root.join("src/a"), "new").unwrap();
    fs::write(root.join("dst/a"), "old").unwrap();
    let mut h = Harness::new(&root.join("dst"));
    h.start().await;
    h.state.fileops.register = Some(Register {
        paths: vec![root.join("src/a")],
        cut: false,
    });
    h.keys("p");
    h.until("the conflict dialog", |s| {
        s.fileops.conflicts.front().is_some_and(|d| d.armed)
    })
    .await;
    h.keys("esc");
    h.jobs_idle().await;
    assert_eq!(h.state.fileops.jobs[0].status, JobStatus::Cancelled);
    assert_eq!(read(root.join("dst/a")), "old");
}

#[tokio::test]
async fn create_rename_and_undo_each_step() {
    let dir = tempfile::tempdir().unwrap();
    let root = canonical(&dir);
    fs::write(root.join("keep.txt"), "k").unwrap();
    let mut h = Harness::new(&root);
    h.start().await;

    // `a`: a trailing slash makes a directory; the cursor lands on it.
    h.keys("a");
    h.type_text("proj/");
    h.keys("enter");
    h.shows("proj").await;
    h.until("the cursor on the new directory", |s| {
        s.view.current().is_some_and(|e| e.display_name() == "proj")
    })
    .await;
    assert!(root.join("proj").is_dir());

    h.keys("a");
    h.type_text("notes.md");
    h.keys("enter");
    h.shows("notes.md").await;
    assert!(root.join("notes.md").is_file());
    assert_eq!(h.cursor().as_deref(), Some("notes.md"));

    // `r`: the prompt starts before the extension, so typing edits the stem.
    h.keys("r");
    assert_eq!(h.state.fileops.prompt.as_ref().unwrap().input.cursor(), 5);
    h.type_text("_v2");
    h.keys("enter");
    h.shows("notes_v2.md").await;
    h.gone("notes.md").await;
    assert_eq!(h.cursor().as_deref(), Some("notes_v2.md"));

    // An invalid name is reported, not applied.
    h.keys("r ctrl-a ctrl-k");
    h.type_text("bad:name");
    h.keys("enter");
    h.until("the error", |s| {
        s.status
            .as_ref()
            .is_some_and(|st| st.text.starts_with("Cannot rename"))
    })
    .await;

    // Undo: rename, then create, then create.
    h.jobs_idle().await;
    h.keys("u");
    h.shows("notes.md").await;
    h.gone("notes_v2.md").await;
    h.jobs_idle().await;
    h.keys("u");
    h.gone("notes.md").await;
    h.jobs_idle().await;
    h.keys("u");
    h.gone("proj").await;
    assert_eq!(listing(&root), ["keep.txt"]);
    h.jobs_idle().await;
    h.keys("u");
    h.until("nothing to undo", |s| {
        s.status
            .as_ref()
            .is_some_and(|st| st.text.contains("nothing to undo"))
    })
    .await;
}

#[tokio::test]
async fn move_undo() {
    let dir = tempfile::tempdir().unwrap();
    let root = canonical(&dir);
    fs::create_dir(root.join("dest")).unwrap();
    fs::write(root.join("m.txt"), "move me").unwrap();
    let mut h = Harness::new(&root);
    h.start().await;

    // Move m.txt into dest by hand-filling the register, then undo.
    h.state.fileops.register = Some(Register {
        paths: vec![root.join("m.txt")],
        cut: true,
    });
    h.keys("enter");
    h.listed(&root.join("dest")).await;
    h.keys("p");
    h.jobs_idle().await;
    assert!(root.join("dest/m.txt").is_file() && !root.join("m.txt").exists());
    h.jobs_idle().await;
    h.keys("u");
    h.until("the move to be undone", |_| root.join("m.txt").is_file())
        .await;
    assert!(!root.join("dest/m.txt").exists());
}

/// Whether the machine's trash accepts a file and lists it back (undo needs both). A CI runner
/// or container may accept deletions yet list nothing; such a machine skips `trash_undo`.
fn recycle_bin_round_trips() -> bool {
    let dir = tempfile::tempdir().unwrap();
    let probe = dir.path().join("kestrel-trash-probe.txt");
    fs::write(&probe, "probe").unwrap();
    if trash::delete(&probe).is_err() || probe.exists() {
        return false;
    }
    #[cfg(any(windows, all(unix, not(target_os = "macos"))))]
    {
        let key = |p: &Path| p.to_string_lossy().to_lowercase();
        let want = key(dunce::simplified(dir.path()));
        let Ok(items) = trash::os_limited::list() else {
            return false;
        };
        let mine: Vec<_> = items
            .into_iter()
            .filter(|i| key(&i.original_parent).starts_with(&want))
            .collect();
        let found = !mine.is_empty();
        let _ = trash::os_limited::purge_all(mine);
        found
    }
    #[cfg(not(any(windows, all(unix, not(target_os = "macos")))))]
    {
        false
    }
}

/// Trash and restore through the machine's real Recycle Bin. Deterministic: every step waits
/// for the job to finish (the undo entry exists only then) rather than for a fixed time, and
/// the restore looks the item up by its original path, so other bin contents do not matter.
#[tokio::test]
async fn trash_undo() {
    if !recycle_bin_round_trips() {
        eprintln!("SKIP: this machine has no Recycle Bin / trash that lists what it received");
        return;
    }
    let dir = tempfile::tempdir().unwrap();
    let root = canonical(&dir);
    fs::write(root.join("t.txt"), "trash me").unwrap();
    let mut h = Harness::new(&root);
    h.start().await;
    h.shows("t.txt").await;
    while h.cursor().as_deref() != Some("t.txt") {
        h.keys("j");
    }
    h.keys("d");
    h.until("the file to be trashed", |_| !root.join("t.txt").exists())
        .await;
    assert!(h.status().contains("Recycle Bin") || h.state.has_running_jobs());
    h.gone("t.txt").await;
    // The undo entry is recorded when the trash job finishes; wait for that first.
    h.jobs_idle().await;
    h.keys("u");
    h.until("the file to be restored", |_| root.join("t.txt").exists())
        .await;
    assert_eq!(read(root.join("t.txt")), "trash me");
}

#[tokio::test]
async fn permanent_delete_requires_the_typed_word() {
    let dir = tempfile::tempdir().unwrap();
    let root = canonical(&dir);
    fs::write(root.join("a.txt"), "a").unwrap();
    fs::write(root.join("b.txt"), "b").unwrap();
    let mut h = Harness::new(&root);
    h.start().await;

    h.keys("D enter");
    assert_eq!(
        h.state.overlays,
        vec![Overlay::Confirm],
        "no word, no delete"
    );
    h.type_text("nope");
    h.keys("enter");
    assert!(root.join("a.txt").exists());
    h.keys("esc");
    assert!(h.state.overlays.is_empty());

    h.keys("D");
    h.type_text("yes");
    h.keys("enter");
    h.gone("a.txt").await;
    assert!(!root.join("a.txt").exists());
    assert!(root.join("b.txt").exists());

    // Several items: the count is the word.
    fs::write(root.join("c.txt"), "c").unwrap();
    h.shows("c.txt").await;
    h.keys("ctrl-a D");
    h.type_text("2");
    h.keys("enter");
    h.until("both to be deleted", |_| {
        !root.join("b.txt").exists() && !root.join("c.txt").exists()
    })
    .await;
}

#[tokio::test]
async fn shell_command_streams_into_the_pager_and_can_be_cancelled() {
    let dir = tempfile::tempdir().unwrap();
    let root = canonical(&dir);
    fs::write(root.join("marker.txt"), "x").unwrap();
    let mut h = Harness::new(&root);
    h.start().await;

    h.keys("!");
    h.type_text("echo hello-from-ks");
    h.keys("enter");
    h.until("the command to exit", |s| {
        s.fileops.pager.as_ref().is_some_and(|p| !p.is_running())
    })
    .await;
    let pager = h.state.fileops.pager.as_ref().unwrap();
    assert!(
        pager.lines.iter().any(|l| l.text.contains("hello-from-ks")),
        "{:?}",
        pager.lines
    );
    assert_eq!(pager.status, ks_core::fileops::PagerStatus::Exited(Some(0)));
    h.keys("esc");
    assert!(h.state.overlays.is_empty());

    // It runs in the current directory.
    h.keys("!");
    h.type_text(if cfg!(windows) { "dir" } else { "ls" });
    h.keys("enter");
    h.until("the listing output", |s| {
        s.fileops.pager.as_ref().is_some_and(|p| !p.is_running())
    })
    .await;
    assert!(h
        .state
        .fileops
        .pager
        .as_ref()
        .unwrap()
        .lines
        .iter()
        .any(|l| l.text.contains("marker.txt")));
    h.keys("esc");

    // ctrl-c kills a long-running command.
    h.keys("!");
    h.type_text(if cfg!(windows) {
        "Start-Sleep -Seconds 60"
    } else {
        "sleep 60"
    });
    h.keys("enter");
    h.until("the command to be running", |s| {
        s.fileops
            .pager
            .as_ref()
            .is_some_and(|p| p.status == ks_core::fileops::PagerStatus::Running)
    })
    .await;
    let started = Instant::now();
    h.keys("ctrl-c");
    h.until("the command to be cancelled", |s| {
        s.fileops
            .pager
            .as_ref()
            .is_some_and(|p| p.status == ks_core::fileops::PagerStatus::Cancelled)
    })
    .await;
    assert!(
        started.elapsed() < Duration::from_secs(40),
        "cancel must not wait for the sleep"
    );
    assert_eq!(h.state.quit, None, "ctrl-c in the pager does not quit");
    h.keys("esc");
}

/// Both bulk-rename tests create and delete edit files in the shared temp directory.
#[cfg(windows)]
static BULK_LOCK: tokio::sync::Mutex<()> = tokio::sync::Mutex::const_new(());

/// The edit files a fake editor was handed, from the log it appends to (one path per line).
/// Counting `kestrel-bulk-rename-*` in the shared temp dir would also see the files of other
/// test processes running at the same time.
#[cfg(windows)]
fn edit_files(script: &Path) -> Vec<PathBuf> {
    let log = script.with_extension("paths");
    let mut files: Vec<PathBuf> = fs::read_to_string(log)
        .unwrap_or_default()
        .lines()
        .map(|l| PathBuf::from(l.trim()))
        .filter(|p| !p.as_os_str().is_empty())
        .collect();
    files.dedup();
    files
}

/// Every edit file the editor was given has been removed.
#[cfg(windows)]
fn assert_edit_files_gone(script: &Path, what: &str) {
    assert!(!edit_files(script).is_empty(), "the editor never ran");
    let left: Vec<PathBuf> = edit_files(script)
        .into_iter()
        .filter(|p| p.exists())
        .collect();
    assert!(left.is_empty(), "{what}: {left:?}");
}

/// A script that stands in for `$EDITOR`: prefixes every line of the file it is given. Pure
/// `cmd` (no PowerShell start-up, which starves under load) and it logs the path it was given.
/// Limits of `for /f`: it handles plain file names only (an empty line, or `&`, `%`, `^`, `!`
/// in a name, would be dropped or mangled); `eol=` keeps lines starting with `;`.
#[cfg(windows)]
fn fake_editor(dir: &Path) -> PathBuf {
    let script = dir.join("fake-editor.cmd");
    let lines = [
        "@echo off",
        "echo %~1 >> \"%~dpn0.paths\"",
        "(for /f \"usebackq eol= delims=\" %%l in (\"%~1\") do @echo x_%%l)> \"%~1.new\"",
        "move /y \"%~1.new\" \"%~1\" >nul",
        "",
    ];
    fs::write(
        &script,
        lines.join(
            "
",
        ),
    )
    .unwrap();
    script
}

#[cfg(windows)]
#[tokio::test]
async fn bulk_rename_through_an_editor_preview_confirm_and_undo() {
    let dir = tempfile::tempdir().unwrap();
    let root = canonical(&dir);
    let work = root.join("work");
    fs::create_dir(&work).unwrap();
    for name in ["a.txt", "b.txt", "c.txt"] {
        fs::write(work.join(name), name).unwrap();
    }
    let _serial = BULK_LOCK.lock().await;
    let editor = fake_editor(&root);
    let mut config = Config::default();
    config.editor.open = editor.display().to_string();
    let mut h = Harness::with_config(&work, config);
    h.start().await;

    h.keys("R");
    h.until("the rename preview", |s| {
        s.overlays.last() == Some(&Overlay::Confirm)
    })
    .await;
    let confirm = h.state.fileops.confirm.as_ref().unwrap();
    assert_eq!(confirm.title, "Rename 3 items?");
    assert!(
        confirm.lines.contains(&"a.txt -> x_a.txt".to_owned()),
        "{:?}",
        confirm.lines
    );
    // Nothing happened yet.
    assert_eq!(listing(&work), ["a.txt", "b.txt", "c.txt"]);

    h.keys("enter");
    h.jobs_idle().await;
    assert_eq!(listing(&work), ["x_a.txt", "x_b.txt", "x_c.txt"]);
    assert_eq!(read(work.join("x_b.txt")), "b.txt");

    // One undo restores the whole batch.
    h.jobs_idle().await;
    h.keys("u");
    h.until("the batch undo", |_| work.join("a.txt").exists())
        .await;
    h.until("the batch undo to finish", |_| {
        listing(&work) == ["a.txt", "b.txt", "c.txt"]
    })
    .await;
    // The temp edit file is gone: nothing new is left in the temp dir.
    assert_edit_files_gone(&editor, "the edit file was not removed");
}

/// An editor that writes duplicate names the first time and sane ones when it is run again.
#[cfg(windows)]
fn flaky_editor(dir: &Path) -> PathBuf {
    let script = dir.join("dup-editor.cmd");
    let marker = dir.join("dup-editor.ran");
    fs::write(
        &script,
        format!(
            "@echo off
echo %~1 >> \"%~dpn0.paths\"
if exist \"{m}\" goto fixed
echo x> \"{m}\"
(echo same.txt&echo same.txt)> \"%~1\"
goto :eof
:fixed
(echo one.txt&echo two.txt)> \"%~1\"
",
            m = marker.display()
        ),
    )
    .unwrap();
    script
}

#[cfg(windows)]
#[tokio::test]
async fn invalid_bulk_rename_is_reopened_with_the_file_kept() {
    let dir = tempfile::tempdir().unwrap();
    let root = canonical(&dir);
    let work = root.join("work");
    fs::create_dir(&work).unwrap();
    for name in ["a.txt", "b.txt"] {
        fs::write(work.join(name), "x").unwrap();
    }
    let _serial = BULK_LOCK.lock().await;
    let mut config = Config::default();
    let editor = flaky_editor(&root);
    config.editor.open = editor.display().to_string();
    let mut h = Harness::with_config(&work, config);
    h.start().await;
    h.keys("R");
    h.until("the problem list", |s| {
        s.overlays.last() == Some(&Overlay::Confirm)
    })
    .await;
    let confirm = h.state.fileops.confirm.as_ref().unwrap();
    assert!(confirm.action.is_some(), "an invalid plan can be re-edited");
    assert!(confirm.title.contains("problem"), "{}", confirm.title);
    let kept = edit_files(&editor);
    assert_eq!(kept.len(), 1, "{kept:?}");
    assert!(kept[0].exists(), "file is kept");
    assert_eq!(listing(&work), ["a.txt", "b.txt"]);

    // Enter reopens the editor on the same file; this time the names are fine.
    h.keys("enter");
    h.until("the rename preview", |s| {
        s.fileops
            .confirm
            .as_ref()
            .is_some_and(|c| c.title.starts_with("Rename"))
    })
    .await;
    assert_edit_files_gone(&editor, "the edit file was removed");
    h.keys("enter");
    h.jobs_idle().await;
    assert_eq!(listing(&work), ["one.txt", "two.txt"]);
}

#[cfg(windows)]
#[tokio::test]
async fn abandoning_an_invalid_bulk_rename_removes_the_edit_file() {
    let dir = tempfile::tempdir().unwrap();
    let root = canonical(&dir);
    let work = root.join("work");
    fs::create_dir(&work).unwrap();
    for name in ["a.txt", "b.txt"] {
        fs::write(work.join(name), "x").unwrap();
    }
    let _serial = BULK_LOCK.lock().await;
    let mut config = Config::default();
    let editor = flaky_editor(&root);
    config.editor.open = editor.display().to_string();
    let mut h = Harness::with_config(&work, config);
    h.start().await;
    h.keys("R");
    h.until("the problem list", |s| {
        s.overlays.last() == Some(&Overlay::Confirm)
    })
    .await;
    h.keys("esc");
    assert!(h.state.overlays.is_empty());
    h.until("the edit file to be removed", |_| {
        edit_files(&editor).iter().all(|p| !p.exists())
    })
    .await;
    assert_edit_files_gone(&editor, "the edit file was discarded");
    assert_eq!(listing(&work), ["a.txt", "b.txt"]);
}

fn write_file(path: &Path, mebibytes: usize) {
    use std::io::Write;
    let mut file = fs::File::create(path).unwrap();
    // Incompressible-looking data so nothing can shortcut the copy.
    let chunk: Vec<u8> = (0..(1 << 20))
        .map(|i: u32| (i.wrapping_mul(2_654_435_761) >> 13) as u8)
        .collect();
    for _ in 0..mebibytes {
        file.write_all(&chunk).unwrap();
    }
    file.sync_all().unwrap();
}

#[tokio::test]
async fn copying_a_200_mb_file_reports_progress() {
    let dir = tempfile::tempdir().unwrap();
    let root = canonical(&dir);
    fs::create_dir(root.join("out")).unwrap();
    write_file(&root.join("big.bin"), 200);
    let mut h = Harness::new(&root.join("out"));
    h.start_in_empty().await;
    h.state.fileops.register = Some(Register {
        paths: vec![root.join("big.bin")],
        cut: false,
    });
    h.keys("p");
    h.jobs_idle().await;

    assert_eq!(
        fs::metadata(root.join("out/big.bin")).unwrap().len(),
        200 << 20
    );
    assert!(!h.progress.is_empty(), "no progress message arrived");
    let last = h.progress.last().unwrap();
    assert!(
        last.bytes_total >= 200 << 20 || last.bytes_done > 0,
        "{last:?}"
    );
    assert!(h
        .progress
        .windows(2)
        .all(|w| w[0].bytes_done <= w[1].bytes_done));
    assert_eq!(h.state.fileops.jobs[0].status, JobStatus::Done);
    assert_eq!(
        listing(&root.join("out")),
        ["big.bin"],
        "no temp files left"
    );
    h.shows("big.bin").await;
}

impl Harness {
    /// `start` for a directory that has nothing in it yet.
    async fn start_in_empty(&mut self) {
        self.send(Msg::Start);
        self.until("the first listing", |s| {
            !s.view.loading && !s.is_navigating()
        })
        .await;
    }
}

#[tokio::test]
async fn cancelling_a_big_copy_leaves_no_partial_file() {
    let dir = tempfile::tempdir().unwrap();
    let root = canonical(&dir);
    let out = root.join("out");
    fs::create_dir(&out).unwrap();
    // The copy of a big file takes a good fraction of a second; a smaller one could finish
    // before the cancel arrives, so grow the file if that happens.
    let mut size = 512;
    for attempt in 0..3 {
        let name = format!("big{attempt}.bin");
        write_file(&root.join(&name), size);
        let mut h = Harness::new(&out);
        h.start_in_empty().await;
        h.state.fileops.register = Some(Register {
            paths: vec![root.join(&name)],
            cut: false,
        });
        h.keys("p");
        // The first progress message proves the copy is under way.
        h.until("the first progress", |s| {
            s.fileops
                .jobs
                .first()
                .is_some_and(|j| j.progress.bytes_done > 0)
        })
        .await;
        // Cancel through the Jobs overlay, the way a user would.
        h.keys("g j x");
        h.jobs_idle().await;
        if h.state.fileops.jobs[0].status == JobStatus::Cancelled {
            assert!(
                !out.join(&name).exists(),
                "a cancelled copy must not leave the target behind"
            );
            assert!(listing(&out).is_empty(), "leftovers: {:?}", listing(&out));
            assert!(h.status().contains("cancelled"), "{}", h.status());
            return;
        }
        // Finished before the cancel got through: retry with a bigger file.
        fs::remove_file(out.join(&name)).unwrap();
        fs::remove_file(root.join(&name)).unwrap();
        size *= 2;
    }
    panic!("the copy always finished before it could be cancelled");
}

/// A clipboard the tests can read back; `fail` makes every write an error.
#[derive(Clone, Default)]
struct TestClipboard {
    text: std::sync::Arc<std::sync::Mutex<Option<String>>>,
    fail: bool,
}

impl ks_fs::launch::clipboard::Clipboard for TestClipboard {
    fn set_text(&mut self, text: &str) -> Result<(), ks_fs::launch::clipboard::ClipboardError> {
        if self.fail {
            return Err(ks_fs::launch::clipboard::ClipboardError("boom".into()));
        }
        *self.text.lock().unwrap() = Some(text.to_owned());
        Ok(())
    }
}

#[tokio::test]
async fn copy_paths_reach_the_clipboard_in_each_format() {
    let dir = tempfile::tempdir().unwrap();
    let root = canonical(&dir);
    fs::write(root.join("a.txt"), "a").unwrap();
    fs::write(root.join("b.txt"), "b").unwrap();
    let clip = TestClipboard::default();
    let mut h = Harness::new(&root);
    h.runner.set_clipboard(clip.clone());
    h.start().await;

    for (hotkey, want) in [
        ("a", root.join("a.txt").to_string_lossy().into_owned()),
        ("r", "a.txt".to_owned()),
        ("n", "a.txt".to_owned()),
    ] {
        *clip.text.lock().unwrap() = None;
        h.keys(&format!("Y {hotkey}"));
        let probe = clip.clone();
        h.until("the clipboard write", move |_| {
            probe.text.lock().unwrap().is_some()
        })
        .await;
        assert_eq!(clip.text.lock().unwrap().as_deref(), Some(want.as_str()));
        h.until("the toast", |s| {
            s.status
                .as_ref()
                .is_some_and(|st| st.text == "Copied 1 path to the clipboard")
        })
        .await;
    }

    // Two marked files: one path per line and a plural toast.
    h.keys("space space Y n");
    h.until("the toast", |s| {
        s.status
            .as_ref()
            .is_some_and(|st| st.text == "Copied 2 paths to the clipboard")
    })
    .await;
    assert_eq!(
        clip.text
            .lock()
            .unwrap()
            .as_deref()
            .map(|t| t.lines().count()),
        Some(2)
    );
}

#[tokio::test]
async fn clipboard_failure_is_reported() {
    let dir = tempfile::tempdir().unwrap();
    let root = canonical(&dir);
    fs::write(root.join("a.txt"), "a").unwrap();
    let mut h = Harness::new(&root);
    h.runner.set_clipboard(TestClipboard {
        fail: true,
        ..TestClipboard::default()
    });
    h.start().await;
    h.keys("Y a");
    h.until("the error toast", |s| {
        s.status
            .as_ref()
            .is_some_and(|st| st.level == ks_core::Level::Error && st.text.contains("boom"))
    })
    .await;
}

#[tokio::test]
async fn copy_keep_both_with_enter_then_undo_removes_the_copy() {
    let dir = tempfile::tempdir().unwrap();
    let root = canonical(&dir);
    let src = root.join("src");
    let dst = root.join("dst");
    fs::create_dir_all(&src).unwrap();
    fs::create_dir_all(&dst).unwrap();
    fs::write(src.join("kb_unique_a.txt"), "new").unwrap();
    fs::write(dst.join("kb_unique_a.txt"), "old").unwrap();

    let mut h = Harness::new(&dst);
    h.start().await;
    h.state.fileops.register = Some(Register {
        paths: vec![src.join("kb_unique_a.txt")],
        cut: false,
    });
    h.keys("p");
    h.until("the conflict dialog", |s| {
        s.fileops.conflicts.front().is_some_and(|d| d.armed)
    })
    .await;
    // Enter is the documented default answer: keep both.
    h.keys("enter");
    h.jobs_idle().await;
    assert_eq!(read(dst.join("kb_unique_a.txt")), "old");
    assert_eq!(read(dst.join("kb_unique_a (1).txt")), "new");
    h.shows("kb_unique_a (1).txt").await;

    // Undoing a copy removes what it created, and only that.
    h.jobs_idle().await;
    h.keys("u");
    h.gone("kb_unique_a (1).txt").await;
    assert_eq!(listing(&dst), ["kb_unique_a.txt"]);
    assert_eq!(read(src.join("kb_unique_a.txt")), "new");
}

#[tokio::test]
async fn shutdown_cancels_a_running_copy_and_cleans_up() {
    let dir = tempfile::tempdir().unwrap();
    let root = canonical(&dir);
    let out = root.join("out");
    fs::create_dir(&out).unwrap();
    let mut size = 512;
    for attempt in 0..3 {
        let name = format!("big{attempt}.bin");
        write_file(&root.join(&name), size);
        let mut h = Harness::new(&out);
        h.start_in_empty().await;
        h.state.fileops.register = Some(Register {
            paths: vec![root.join(&name)],
            cut: false,
        });
        h.keys("p");
        h.until("the first progress", |s| {
            s.fileops
                .jobs
                .first()
                .is_some_and(|j| j.progress.bytes_done > 0)
        })
        .await;
        let started = Instant::now();
        h.runner.shutdown();
        let took = started.elapsed();
        assert!(took < Duration::from_secs(5), "shutdown took {took:?}");
        // `shutdown` only waits a short grace; under I/O starvation the job thread may still
        // be removing its temp file. Wait for it before judging the outcome.
        assert!(
            h.runner.jobs().wait_idle(Duration::from_secs(30)),
            "the job did not finish after the cancel"
        );
        if !out.join(&name).exists() {
            assert!(listing(&out).is_empty(), "{:?}", listing(&out));
            return;
        }
        // The copy completed before the cancel got through: retry with a bigger file.
        assert_eq!(listing(&out), std::slice::from_ref(&name));
        fs::remove_file(out.join(&name)).unwrap();
        fs::remove_file(root.join(&name)).unwrap();
        size *= 2;
    }
    panic!("the copy always finished before shutdown");
}

#[cfg(windows)]
#[tokio::test]
async fn a_kept_edit_file_removed_meanwhile_is_written_afresh() {
    let dir = tempfile::tempdir().unwrap();
    let root = canonical(&dir);
    let work = root.join("work");
    fs::create_dir(&work).unwrap();
    for name in ["a.txt", "b.txt"] {
        fs::write(work.join(name), "x").unwrap();
    }
    let _serial = BULK_LOCK.lock().await;
    let mut config = Config::default();
    config.editor.open = fake_editor(&root).display().to_string();
    let mut h = Harness::with_config(&work, config);
    h.start().await;
    // Re-edit a file that no longer exists: the runner must not hand that path to the editor.
    let missing = root.join("kestrel-bulk-rename-gone.txt");
    h.runner.run(vec![ks_core::Effect::BulkRenameEdit {
        dir: work.clone(),
        names: vec!["a.txt".into(), "b.txt".into()],
        reuse: Some(missing),
    }]);
    h.until("the rename preview", |s| {
        s.fileops
            .confirm
            .as_ref()
            .is_some_and(|c| c.title.starts_with("Rename"))
    })
    .await;
}
