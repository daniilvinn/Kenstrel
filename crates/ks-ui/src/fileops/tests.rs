use std::path::PathBuf;
use std::time::Duration;

use ks_core::fileops::{
    ConfirmAction, ConflictInfo, JobProgress, MenuItem, OpKind, PagerLine, Register, TextPrompt,
};
use ks_core::mode::PromptKind;
use ks_core::JobId;
use ratatui::backend::TestBackend;
use ratatui::Terminal;

use super::*;
use crate::render::render;
use crate::testutil::{dump, listing_state, now};
use crate::theme::{ColorDepth, Theme};

const SIZES: [(u16, u16); 2] = [(80, 24), (160, 48)];
const THEMES: [&str; 2] = ["ember", "paper"];

fn theme(name: &str) -> Theme {
    Theme::builtin(name, ColorDepth::TrueColor).unwrap()
}

fn draw(state: &AppState, theme: &Theme) -> String {
    let backend = TestBackend::new(state.size.width, state.size.height);
    let mut terminal = Terminal::new(backend).unwrap();
    terminal.draw(|f| render(f, state, theme)).unwrap();
    dump(terminal.backend().buffer())
}

fn text_of(state: &AppState) -> String {
    draw(state, &theme("ember"))
        .split("--- styles")
        .next()
        .unwrap()
        .to_owned()
}

fn snap(name: &str, build: impl Fn(u16, u16) -> AppState) {
    for theme_name in THEMES {
        for (w, h) in SIZES {
            let state = build(w, h);
            insta::assert_snapshot!(
                format!("{name}_{theme_name}_{w}x{h}"),
                draw(&state, &theme(theme_name))
            );
        }
    }
}

fn ago(secs: u64) -> Option<std::time::SystemTime> {
    Some(now() - Duration::from_secs(secs))
}

fn row(id: u64, kind: OpKind, status: JobStatus, text: &str) -> JobRow {
    JobRow {
        id: JobId(id),
        kind,
        description: text.to_owned(),
        status,
        scanning: false,
        progress: JobProgress::default(),
        summary: None,
    }
}

fn jobs_state(w: u16, h: u16) -> AppState {
    let mut s = listing_state(w, h);
    let mut running = row(5, OpKind::Copy, JobStatus::Running, "Copying 3 items");
    running.progress = JobProgress {
        bytes_done: 420_000_000,
        bytes_total: 1_000_000_000,
        files_done: 1,
        files_total: 3,
        current: Some(PathBuf::from("big.iso")),
        throughput_bps: 118_000_000,
        eta_secs: Some(12),
    };
    let mut scanning = row(6, OpKind::Move, JobStatus::Running, "Moving photos");
    scanning.scanning = true;
    scanning.progress.files_total = 1_204;
    let mut done = row(2, OpKind::Trash, JobStatus::Done, "Trashing 2 items");
    done.summary = Some("Moved to Recycle Bin: 2 items · u to undo".into());
    let mut failed = row(3, OpKind::Copy, JobStatus::Failed, "Copying a.bin");
    failed.summary = Some("1 failed: a.bin: Access is denied.".into());
    let mut cancelled = row(1, OpKind::Delete, JobStatus::Cancelled, "Deleting tmp");
    cancelled.summary = Some("Deleting cancelled (4 done)".into());
    s.fileops.jobs = vec![
        running,
        scanning,
        row(
            7,
            OpKind::Copy,
            JobStatus::AwaitingConflict,
            "Copying report.docx",
        ),
        row(8, OpKind::Move, JobStatus::Queued, "Moving archive.zip"),
        done,
        failed,
        cancelled,
    ];
    s.fileops.jobs_selected = 1;
    for id in [5, 6, 7, 8] {
        s.jobs.insert(JobId(id));
    }
    s.spinner_frame = 3;
    s.overlays.push(Overlay::Jobs);
    s
}

fn conflict_dialog(rename: bool) -> ConflictDialog {
    ConflictDialog {
        info: ConflictInfo {
            job: JobId(7),
            src: PathBuf::from("/home/me/Downloads/report.final.docx"),
            dst: PathBuf::from("/home/me/projects/omniforce/src/renderer/report.final.docx"),
            src_meta: ItemMeta {
                is_dir: false,
                is_link: false,
                len: 2_400_000,
                modified: ago(3 * 3600),
            },
            dst_meta: ItemMeta {
                is_dir: false,
                is_link: false,
                len: 1_100_000,
                modified: ago(9 * 86_400),
            },
        },
        apply_to_all: true,
        rename: rename.then(|| PromptInput::with_text("report.final.docx", 12)),
        armed: true,
    }
}

fn conflict_state(w: u16, h: u16, rename: bool, waiting: usize) -> AppState {
    let mut s = listing_state(w, h);
    s.fileops.conflicts.push_back(conflict_dialog(rename));
    for _ in 0..waiting {
        s.fileops.conflicts.push_back(conflict_dialog(false));
    }
    s.overlays.push(Overlay::Conflict);
    s
}

fn confirm_state(w: u16, h: u16, typed: &str, mismatch: bool) -> AppState {
    let mut s = listing_state(w, h);
    s.fileops.confirm = Some(ConfirmDialog {
        title: "Delete 3 items permanently?".into(),
        lines: vec![
            "vulkan_device.rs".into(),
            "swapchain.rs".into(),
            "日本語のメモ.txt".into(),
        ],
        more: 0,
        danger: true,
        token: Some("3".into()),
        typed: PromptInput::with_text_at_end(typed),
        mismatch,
        action: Some(ConfirmAction::DeletePermanent { paths: vec![] }),
        accept_label: "delete".into(),
    });
    s.overlays.push(Overlay::Confirm);
    s
}

fn bulk_state(w: u16, h: u16) -> AppState {
    let mut s = listing_state(w, h);
    s.fileops.confirm = Some(ConfirmDialog {
        title: "Rename 14 items?".into(),
        lines: (1..=12)
            .map(|i| format!("img_{i:03}.jpg -> holiday_{i:03}.jpg"))
            .collect(),
        more: 2,
        danger: false,
        token: None,
        typed: PromptInput::new(),
        mismatch: false,
        action: Some(ConfirmAction::BulkRename { steps: vec![] }),
        accept_label: "rename".into(),
    });
    s.overlays.push(Overlay::Confirm);
    s
}

fn bulk_invalid_state(w: u16, h: u16) -> AppState {
    let mut s = listing_state(w, h);
    s.fileops.confirm = Some(ConfirmDialog {
        title: "Bulk rename: 2 problems found".into(),
        lines: vec![
            "line 2: duplicate name `same.txt`".into(),
            "line 5: name is empty".into(),
            "Your edits are kept: enter reopens the file, esc discards it.".into(),
        ],
        more: 0,
        danger: true,
        token: None,
        typed: PromptInput::new(),
        mismatch: false,
        action: Some(ConfirmAction::BulkReedit(ks_core::fileops::BulkRetry {
            dir: PathBuf::from("/tmp"),
            names: vec![],
            file: PathBuf::from("/tmp/x"),
        })),
        accept_label: "edit again".into(),
    });
    s.overlays.push(Overlay::Confirm);
    s
}

/// `descriptors.rs` and the directory `shaders` wait in the cut register; `swapchain.rs` is
/// marked as well as cut (the mark wins the glyph).
fn cut_state(w: u16, h: u16) -> AppState {
    let mut s = listing_state(w, h);
    let cwd = s.cwd.clone();
    s.fileops.register = Some(Register {
        paths: ["descriptors.rs", "shaders", "swapchain.rs"]
            .iter()
            .map(|n| cwd.join(n))
            .collect(),
        cut: true,
    });
    s
}

fn menu_state(w: u16, h: u16, open_with: bool) -> AppState {
    let mut s = listing_state(w, h);
    let item = |label: &str, hotkey: char| MenuItem {
        label: label.to_owned(),
        hotkey: Some(hotkey),
    };
    s.fileops.menu = Some(if open_with {
        Menu {
            kind: MenuKind::OpenWith,
            title: "Open vulkan_device.rs with".into(),
            items: vec![
                item("Notepad", '1'),
                item("VS Code", '2'),
                item("Explorer (reveal)", '3'),
                item("Choose another app...", '4'),
            ],
            selected: 1,
            paths: vec![],
        }
    } else {
        Menu {
            kind: MenuKind::CopyPath,
            title: "Copy 2 items as".into(),
            items: vec![
                item("Absolute path", 'a'),
                item("Relative path", 'r'),
                item("File name", 'n'),
            ],
            selected: 0,
            paths: vec![],
        }
    });
    s.overlays.push(Overlay::Menu);
    s
}

fn pager_state(w: u16, h: u16, status: PagerStatus, lines: usize) -> AppState {
    let mut s = listing_state(w, h);
    let mut pager = Pager {
        id: 0,
        command: "cargo build --release".into(),
        cwd: PathBuf::from("/home/me/projects/omniforce"),
        lines: std::collections::VecDeque::new(),
        dropped: 0,
        scroll: 0,
        follow: true,
        status,
    };
    for i in 0..lines {
        pager.lines.push_back(PagerLine {
            text: if i % 7 == 3 {
                format!("warning: unused variable `x{i}` in src/renderer/very/long/path/to/a/module_with_a_long_name.rs")
            } else {
                format!("   Compiling crate-{i} v0.{i}.0")
            },
            stderr: i % 7 == 3,
        });
    }
    pager.scroll = pager.lines.len().saturating_sub(pager_rows(s.size));
    s.fileops.pager = Some(pager);
    s.overlays.push(Overlay::Pager);
    s
}

fn prompt_state(w: u16, h: u16, kind: PromptKind) -> AppState {
    let mut s = listing_state(w, h);
    let (label, input) = match kind {
        PromptKind::Rename => ("rename", PromptInput::with_text("vulkan_device.rs", 13)),
        PromptKind::Create => (
            "new (end with / for a folder)",
            PromptInput::with_text_at_end("shaders/new_pass"),
        ),
        _ => ("!", PromptInput::with_text_at_end("git status --short")),
    };
    s.fileops.prompt = Some(TextPrompt {
        kind,
        label: label.into(),
        input,
        target: None,
    });
    s.mode = Mode::Prompt(kind);
    s
}

#[test]
fn jobs_overlay_snapshots() {
    snap("jobs", jobs_state);
    snap("jobs_empty", |w, h| {
        let mut s = listing_state(w, h);
        s.overlays.push(Overlay::Jobs);
        s
    });
}

#[test]
fn conflict_overlay_snapshots() {
    snap("conflict", |w, h| conflict_state(w, h, false, 0));
    snap("conflict_rename", |w, h| conflict_state(w, h, true, 0));
    snap("conflict_queue", |w, h| conflict_state(w, h, false, 2));
    snap("conflict_disarmed", |w, h| {
        let mut s = conflict_state(w, h, false, 0);
        s.fileops.conflicts[0].armed = false;
        s
    });
}

#[test]
fn confirm_overlay_snapshots() {
    snap("confirm_delete", |w, h| confirm_state(w, h, "", false));
    snap("confirm_delete_typed", |w, h| {
        confirm_state(w, h, "yes", true)
    });
    snap("confirm_bulk", bulk_state);
    snap("confirm_bulk_invalid", bulk_invalid_state);
}

#[test]
fn cut_entries_are_dimmed_and_flagged() {
    snap("list_cut", cut_state);
    let text = text_of(&cut_state(80, 24));
    let flagged = |name: &str| {
        text.lines()
            .find(|l| l.contains(name))
            .is_some_and(|l| l.contains('×'))
    };
    assert!(flagged("descriptors.rs") && flagged("shaders"));
    assert!(!flagged("mod.rs"));
    // a copy (yank) does not dim anything
    let mut s = cut_state(80, 24);
    s.fileops.register.as_mut().unwrap().cut = false;
    assert!(!text_of(&s).contains('×'));
}

#[test]
fn menu_snapshots() {
    snap("menu_copy_path", |w, h| menu_state(w, h, false));
    snap("menu_open_with", |w, h| menu_state(w, h, true));
}

#[test]
fn pager_snapshots() {
    snap("pager_running", |w, h| {
        pager_state(w, h, PagerStatus::Running, 60)
    });
    snap("pager_done", |w, h| {
        pager_state(w, h, PagerStatus::Exited(Some(101)), 12)
    });
    snap("pager_empty", |w, h| {
        pager_state(w, h, PagerStatus::Running, 0)
    });
}

#[test]
fn prompt_snapshots() {
    snap("prompt_rename", |w, h| {
        prompt_state(w, h, PromptKind::Rename)
    });
    snap("prompt_create", |w, h| {
        prompt_state(w, h, PromptKind::Create)
    });
    snap("prompt_shell", |w, h| prompt_state(w, h, PromptKind::Shell));
}

#[test]
fn running_job_and_register_show_in_the_prompt_line() {
    snap("main_job_register", |w, h| {
        let mut s = jobs_state(w, h);
        s.overlays.clear();
        s.fileops.jobs.truncate(1);
        s.fileops.register = Some(Register {
            paths: vec![PathBuf::from("a"), PathBuf::from("b"), PathBuf::from("c")],
            cut: false,
        });
        s
    });
}

#[test]
fn job_segment_summarises_progress() {
    let s = jobs_state(160, 48);
    // Four active jobs: a combined percentage, no per-job detail.
    let multi = job_segment(&s).unwrap();
    assert!(multi.contains("4 jobs"), "{multi}");

    let mut one = jobs_state(160, 48);
    one.fileops.jobs.truncate(1);
    let seg = job_segment(&one).unwrap();
    assert!(
        seg.contains("42%") && seg.contains("/s") && seg.contains("0:12"),
        "{seg}"
    );

    let mut idle = jobs_state(80, 24);
    idle.fileops.jobs.retain(|j| !j.status.is_active());
    assert_eq!(job_segment(&idle), None);
}

#[test]
fn register_segment_counts() {
    let mut s = listing_state(80, 24);
    assert_eq!(register_segment(&s), None);
    s.fileops.register = Some(Register {
        paths: vec![PathBuf::from("a"), PathBuf::from("b")],
        cut: true,
    });
    assert_eq!(register_segment(&s).as_deref(), Some("2 cut"));
    assert!(text_of(&s).contains("2 cut"));
}

#[test]
fn input_cursor_stays_visible_in_a_narrow_field() {
    let t = theme("ember");
    let input = PromptInput::with_text_at_end("a_very_long_name_that_overflows.txt");
    let spans = input_spans(&input, 10, &t);
    let text: String = spans.iter().map(|s| s.content.to_string()).collect();
    assert!(width(&text) <= 10, "{text}");
    // The reversed cursor cell is the last one, and the tail of the name is what shows.
    assert!(spans[1].style.add_modifier.contains(Modifier::REVERSED));
    assert!(text.contains("s.txt"), "{text}");
}

#[test]
fn dialogs_fit_a_tiny_terminal_without_panicking() {
    for (w, h) in [(20, 8), (30, 10), (12, 5)] {
        for build in [
            jobs_state as fn(u16, u16) -> AppState,
            |w, h| conflict_state(w, h, true, 1),
            |w, h| confirm_state(w, h, "x", true),
            bulk_state,
            |w, h| menu_state(w, h, true),
            |w, h| pager_state(w, h, PagerStatus::Running, 30),
            |w, h| prompt_state(w, h, PromptKind::Create),
        ] {
            let _ = draw(&build(w, h), &theme("paper"));
        }
    }
}

#[test]
fn confirm_counts_lines_hidden_by_a_short_terminal() {
    // 12 listed + 2 more = 14 items; on a short terminal the hidden names join the count.
    let text = text_of(&bulk_state(80, 12));
    let shown = text.matches("holiday_").count();
    assert!(shown < 12, "some lines must be cut: {text}");
    let want = format!("and {} more", 14 - shown);
    assert!(text.contains(&want), "expected {want:?} in\n{text}");
}
