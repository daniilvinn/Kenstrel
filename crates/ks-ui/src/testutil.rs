//! Snapshot helper: renders a ratatui buffer as text plus a compact style legend, so that
//! snapshots catch colour regressions (and theme differences) as well as layout ones.

use ratatui::buffer::Buffer;
use ratatui::style::{Color, Modifier, Style};

/// Text rows followed by one line per run of identically styled, non-default cells.
pub fn dump(buffer: &Buffer) -> String {
    let area = buffer.area;
    let mut out = String::new();
    let mut runs = String::new();

    for y in 0..area.height {
        let mut row = String::new();
        let mut run_start = 0u16;
        let mut run_style: Option<Style> = None;
        let flush = |runs: &mut String, y: u16, from: u16, to: u16, style: Option<Style>| {
            if let Some(style) = style {
                if style != Style::default() && to > from {
                    runs.push_str(&format!("{y:>3} {from:>3}..{to:<3} {}\n", describe(style)));
                }
            }
        };
        let mut covered = 0usize;
        for x in 0..area.width {
            let cell = &buffer[(x, y)];
            // A wide character's second cell holds a blank that is not part of the text.
            if covered > 0 {
                covered -= 1;
            } else {
                row.push_str(&visible(cell.symbol()));
                covered = unicode_width::UnicodeWidthStr::width(cell.symbol()).saturating_sub(1);
            }
            let style = Style::new()
                .fg(cell.fg)
                .bg(cell.bg)
                .add_modifier(cell.modifier);
            let normalised = if cell.symbol() == " " && cell.bg == Color::Reset {
                // Spaces have no visible foreground; do not let them split runs.
                run_style.unwrap_or_default()
            } else {
                style
            };
            if run_style != Some(normalised) {
                flush(&mut runs, y, run_start, x, run_style);
                run_start = x;
                run_style = Some(normalised);
            }
        }
        flush(&mut runs, y, run_start, area.width, run_style);
        out.push_str(row.trim_end());
        out.push('\n');
    }
    format!("{out}--- styles (row cols fg bg mods) ---\n{runs}")
}

/// Private-use characters (Nerd Font icons) are invisible in most viewers; show a marker
/// of the same width instead. Which icon is chosen is covered by the icon tests.
fn visible(symbol: &str) -> String {
    symbol
        .chars()
        .map(|c| {
            if ('\u{e000}'..='\u{f8ff}').contains(&c) {
                '◆'
            } else {
                c
            }
        })
        .collect()
}

fn describe(style: Style) -> String {
    let fg = style.fg.map_or_else(|| "-".to_owned(), color_name);
    let bg = style.bg.map_or_else(|| "-".to_owned(), color_name);
    let mods = if style.add_modifier.is_empty() {
        String::new()
    } else {
        format!(" {}", modifier_name(style.add_modifier))
    };
    format!("fg={fg} bg={bg}{mods}")
}

fn color_name(color: Color) -> String {
    match color {
        Color::Rgb(r, g, b) => format!("#{r:02x}{g:02x}{b:02x}"),
        Color::Indexed(i) => format!("idx{i}"),
        Color::Reset => "reset".to_owned(),
        other => format!("{other:?}").to_lowercase(),
    }
}

fn modifier_name(m: Modifier) -> String {
    let mut names = Vec::new();
    if m.contains(Modifier::BOLD) {
        names.push("bold");
    }
    if m.contains(Modifier::DIM) {
        names.push("dim");
    }
    if m.contains(Modifier::ITALIC) {
        names.push("italic");
    }
    if m.contains(Modifier::UNDERLINED) {
        names.push("underline");
    }
    if m.contains(Modifier::REVERSED) {
        names.push("reverse");
    }
    names.join("+")
}

// ----- fixtures -----

use std::path::PathBuf;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use ks_core::{
    AppState, Attrs, Config, DriveInfo, DriveKind, Entry, EntryKind, GitBranch, GitCounts,
    GitRecord, GitSnapshot, GitState, Keymap, Places, Size, StartParams,
};

/// 2025-03-14 12:00:00 UTC, the fixed "now" of every snapshot.
pub const NOW_SECS: u64 = 1_741_953_600;

pub fn now() -> SystemTime {
    UNIX_EPOCH + Duration::from_secs(NOW_SECS)
}

fn ago(secs: u64) -> SystemTime {
    UNIX_EPOCH + Duration::from_secs(NOW_SECS - secs)
}

pub fn empty_state(width: u16, height: u16) -> AppState {
    let mut state = AppState::new(StartParams {
        cwd: PathBuf::from("/home/me/projects/omniforce/src/renderer"),
        home: Some(PathBuf::from("/home/me")),
        places: Places::default(),
        config: Config::default(),
        keymap: Keymap::defaults(),
        size: Size::new(width, height),
        now: now(),
        utc_offset_secs: 0,
    });
    // Screens that are not about the preview keep the list full width; preview snapshots
    // switch it on with `with_preview`.
    state.preview.user_visible = Some(false);
    state
}

/// The entries of the mock-up main screen, plus one of every special kind.
pub fn fixture_entries() -> Vec<Entry> {
    let mut shaders = Entry::dir("shaders").with_mtime(ago(2 * 86_400));
    shaders.children = Some(12);
    let mut passes = Entry::dir("passes").with_mtime(ago(5 * 3600));
    passes.children = Some(7);
    let mut link = Entry::new("current", EntryKind::Symlink).with_dir_target();
    link.mtime = Some(ago(40 * 86_400));
    vec![
        shaders,
        passes,
        link,
        Entry::file("vulkan_device.rs", 14_540).with_mtime(ago(3 * 3600)),
        Entry::file("swapchain.rs", 10_035).with_mtime(ago(12 * 60)),
        Entry::file("descriptors.rs", 6_246).with_mtime(ago(30 * 3600)),
        Entry::file("pipeline_cache.rs", 3_379).with_mtime(ago(10)),
        Entry::file("mod.rs", 412).with_mtime(ago(400 * 86_400)),
        Entry::file(
            "a_rather_long_file_name_that_must_be_cut_in_the_middle_of_the_list_row.rs",
            77_000,
        )
        .with_mtime(ago(86_400 * 3)),
        Entry::file("build.ps1", 1_900)
            .with_attrs(Attrs::EXEC)
            .with_mtime(ago(86_400 * 9)),
        Entry::file("photo_from_phone.jpg", 4_200_000)
            .with_attrs(Attrs::CLOUD)
            .with_mtime(ago(86_400 * 20)),
        Entry::file("日本語のメモ.txt", 220).with_mtime(ago(7200)),
        Entry::file(".env", 64).with_mtime(ago(100)),
        Entry::file("new_pass.rs", 0).with_mtime(ago(60)),
    ]
}

pub fn fixture_git() -> GitSnapshot {
    GitSnapshot::new(
        GitBranch {
            name: Some("main".into()),
            ahead: 1,
            ..GitBranch::default()
        },
        GitCounts {
            staged: 1,
            modified: 2,
            untracked: 1,
            conflicted: 0,
        },
        "",
        [
            GitRecord {
                path: "vulkan_device.rs".into(),
                state: GitState::Modified,
            },
            GitRecord {
                path: "descriptors.rs".into(),
                state: GitState::Added,
            },
            GitRecord {
                path: "new_pass.rs".into(),
                state: GitState::Untracked,
            },
            GitRecord {
                path: "passes/pass_a.rs".into(),
                state: GitState::Modified,
            },
            GitRecord {
                path: "shaders/".into(),
                state: GitState::Ignored,
            },
        ],
    )
}

/// Moves the cursor onto the entry called `name`.
pub fn move_to(state: &mut AppState, name: &str) {
    let rows = state.viewport_height();
    let target = (0..state.view.len())
        .find(|&i| state.view.row(i).is_some_and(|e| e.display_name() == name))
        .unwrap_or_else(|| panic!("no entry called {name}"));
    let delta = target as isize - state.view.cursor.index as isize;
    state.view.move_cursor(delta, rows);
}

/// A populated main screen: cursor on `vulkan_device.rs`, `swapchain.rs` marked, a git
/// repository with a few changes.
pub fn listing_state(width: u16, height: u16) -> AppState {
    let mut state = empty_state(width, height);
    let rows = state.viewport_height();
    state.view.push_chunk(fixture_entries(), rows);
    state.view.finish_loading();
    move_to(&mut state, "swapchain.rs");
    state.view.toggle_mark(rows);
    move_to(&mut state, "vulkan_device.rs");
    state.git = Some(fixture_git());
    state
}

pub fn drives_state(width: u16, height: u16) -> AppState {
    let mut state = empty_state(width, height);
    let mut c = DriveInfo::new("C:/");
    c.label = "Windows".into();
    c.fs = "NTFS".into();
    c.kind = DriveKind::Fixed;
    c.total = Some(496_960_008_192);
    c.free = Some(33_758_236_672);
    let mut d = DriveInfo::new("D:/");
    d.label = "Games and a very long volume label".into();
    d.fs = "NTFS".into();
    d.kind = DriveKind::Fixed;
    d.total = Some(2_000_000_000_000);
    d.free = Some(1_500_000_000_000);
    let mut e = DriveInfo::new("E:/");
    e.fs = "exFAT".into();
    e.kind = DriveKind::Removable;
    e.total = Some(64_000_000_000);
    e.free = Some(1_000_000_000);
    let z = DriveInfo {
        kind: DriveKind::Optical,
        ..DriveInfo::new("F:/")
    };
    let rows = state.viewport_height();
    state
        .drives
        .set(vec![c, d, e, z], std::path::Path::new("D:/x"), rows);
    state.show_drives = true;
    state
}
