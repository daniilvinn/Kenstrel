//! Snapshot and behaviour tests for the preview pane and the parent column.

use std::sync::Arc;

use ks_core::{update, AppState, Entry, KeyChord, Msg};
use ks_preview::{Preview, PreviewBody, PreviewEvent, Role, SpanStyle, StyledLine};
use ratatui::backend::TestBackend;
use ratatui::style::{Modifier, Style};
use ratatui::Terminal;

use crate::images::TEST_LOCK;
use crate::preview::{binary_lines, push_clipped, span_style};
use crate::render::render;
use crate::testutil::{dump, move_to};
use crate::testutil_preview::*;
use crate::text::width;
use crate::theme::{ColorDepth, Theme, ThemeLoader};

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

fn key(state: &mut AppState, text: &str) {
    update(state, Msg::Key(text.parse::<KeyChord>().unwrap()));
}

fn text_body(theme: &Theme) -> PreviewBody {
    PreviewBody::Text(rust_text(&theme.syntax_palette()))
}

/// Runs `setup` for every size and theme and snapshots the result as `<name>_<theme>_<WxH>`.
fn snapshots(name: &str, setup: impl Fn(&mut AppState, &Theme)) {
    for theme_name in THEMES {
        let theme = theme(theme_name);
        for (w, h) in SIZES {
            let mut state = preview_state(w, h);
            setup(&mut state, &theme);
            insta::assert_snapshot!(format!("{name}_{theme_name}_{w}x{h}"), draw(&state, &theme));
        }
    }
}

#[test]
fn text_preview_is_highlighted_with_line_numbers() {
    snapshots("text", |s, theme| {
        let meta = metadata_of(s, "vulkan_device.rs", 14_540, 0x20);
        show(s, text_body(theme), Some(meta));
    });
}

#[test]
fn text_preview_scrolled() {
    snapshots("text_scrolled", |s, theme| {
        // Let the pane issue its request first, or the first key would reset the result.
        update(s, Msg::Tick);
        show(s, text_body(theme), None);
        key(s, "J");
        key(s, "J");
    });
}

#[test]
fn directory_preview_is_listed_dimmed() {
    snapshots("directory", |s, _| {
        move_to(s, "passes");
        show(s, dir_body(), None);
    });
}

#[test]
fn hex_view() {
    snapshots("hex", |s, _| {
        move_to(s, "firmware.bin");
        show(s, hex_body(), None);
    });
}

#[test]
fn unavailable_shows_the_reason_and_metadata() {
    snapshots("unavailable", |s, _| {
        move_to(s, "photo_from_phone.jpg");
        let meta = metadata_of(s, "photo_from_phone.jpg", 4_200_000, 0x40_0020);
        show(s, unavailable_body(), Some(meta));
    });
}

#[test]
fn metadata_view() {
    snapshots("metadata", |s, _| {
        let meta = metadata_of(s, "vulkan_device.rs", 14_540, 0x21);
        key(s, "i");
        show(s, PreviewBody::Metadata, Some(meta));
    });
}

#[test]
fn markdown_preview() {
    snapshots("markdown", |s, _| {
        move_to(s, "README.md");
        show(s, PreviewBody::Markdown(markdown_text()), None);
    });
}

#[test]
fn archive_listing() {
    snapshots("archive", |s, _| {
        move_to(s, "bundle.zip");
        show(s, archive_body(), None);
    });
}

#[test]
fn image_preview_uses_halfblocks_without_a_graphics_terminal() {
    let _guard = TEST_LOCK.lock().unwrap_or_else(|p| p.into_inner());
    for theme_name in THEMES {
        for (w, h) in SIZES {
            crate::images::reset_for_tests();
            let mut s = preview_state(w, h);
            move_to(&mut s, "logo.png");
            show(&mut s, image_body(), None);
            s.preview.image_id = 1;
            insta::assert_snapshot!(
                format!("image_{theme_name}_{w}x{h}"),
                draw(&s, &theme(theme_name))
            );
        }
    }
}

#[test]
fn a_pending_preview_shows_only_the_header() {
    snapshots("loading", |s, _| {
        s.preview.target = s.view.current().map(|e| s.cwd.join(&e.name));
        s.preview.loading = true;
    });
}

#[test]
fn narrow_terminals_hide_the_preview() {
    for theme_name in THEMES {
        let mut s = listing_state_narrow();
        s.preview.user_visible = None;
        insta::assert_snapshot!(
            format!("narrow_{theme_name}_70x24"),
            draw(&s, &theme(theme_name))
        );
    }
}

fn listing_state_narrow() -> AppState {
    crate::testutil::listing_state(70, 24)
}

#[test]
fn the_parent_column_highlights_the_current_directory() {
    for theme_name in THEMES {
        for (w, h) in [(80, 24), (120, 30), (160, 48)] {
            // At 80x24 the column does not fit and is dropped (the snapshot shows that).
            let theme = theme(theme_name);
            let mut s = preview_state(w, h);
            s.preview.parent_column = true;
            show(&mut s, text_body(&theme), None);
            s.preview.parent = Some(Arc::new(Preview {
                path: "/home/me/projects/omniforce/src".into(),
                meta: None,
                body: parent_body(),
            }));
            insta::assert_snapshot!(format!("parent_{theme_name}_{w}x{h}"), draw(&s, &theme));
        }
    }
}

#[test]
fn the_pane_does_not_show_a_result_for_another_file() {
    let mut s = preview_state(120, 30);
    show(&mut s, text_body(&theme("ember")), None);
    move_to(&mut s, "swapchain.rs");
    // The cursor moved but the stale result has not been replaced yet.
    s.preview.target = s.view.current().map(|e| s.cwd.join(&e.name));
    let text = text_of(&s);
    assert!(!text.contains("pub struct VulkanDevice"), "{text}");
    assert!(text.contains("swapchain.rs"), "{text}");
}

#[test]
fn engine_results_reach_the_screen_through_update() {
    let mut s = preview_state(120, 30);
    let now = s.now;
    let effects = update(
        &mut s,
        Msg::Clock {
            now,
            utc_offset_secs: 0,
        },
    );
    let (gen, path) = effects
        .iter()
        .find_map(|e| match e {
            ks_core::Effect::RequestPreview { gen, request } => Some((*gen, request.path.clone())),
            _ => None,
        })
        .expect("a request for the cursor file");
    let result = Arc::new(Preview {
        path: path.clone(),
        meta: None,
        body: text_body(&theme("ember")),
    });
    update(
        &mut s,
        Msg::Preview(ks_core::PreviewMsg(PreviewEvent {
            generation: gen.raw(),
            path,
            result,
            from_cache: false,
        })),
    );
    assert!(text_of(&s).contains("pub struct VulkanDevice"));
}

#[test]
fn real_highlighting_colours_the_text_with_the_theme_palette() {
    use ks_preview::{build_preview, PreviewOptions};
    use tokio_util::sync::CancellationToken;

    let dir = tempfile::tempdir().unwrap();
    let file = dir.path().join("sample.rs");
    std::fs::write(&file, "pub fn main() {\n    let x = 42; // answer\n}\n").unwrap();
    let theme = theme("ember");
    let opts = PreviewOptions {
        palette: theme.syntax_palette(),
        ..PreviewOptions::default()
    };
    let preview = build_preview(&file, &opts, &CancellationToken::new()).expect("a preview");
    let PreviewBody::Text(text) = &preview.body else {
        panic!("expected text, got {:?}", preview.body);
    };
    let colours: std::collections::HashSet<_> = text
        .lines
        .iter()
        .flat_map(|l| l.spans.iter())
        .map(|s| span_style(s.style, &theme).fg)
        .collect();
    assert!(
        colours.len() >= 3,
        "keywords, numbers and comments differ: {colours:?}"
    );
    let keyword = ratatui::style::Color::Rgb(0xd9, 0x77, 0x57);
    assert!(colours.contains(&Some(keyword)), "{colours:?}");
}

#[test]
fn theme_syntax_tokens_have_defaults_and_overrides() {
    let ember = theme("ember");
    assert_eq!(
        ember.syntax_palette().keyword,
        ks_preview::Rgb::new(0xd9, 0x77, 0x57)
    );
    let dir = tempfile::tempdir().unwrap();
    std::fs::write(
        dir.path().join("mine.toml"),
        "base = \"ember\"\n[colors]\naccent = \"#112233\"\n[syntax]\nstring = \"#445566\"\n",
    )
    .unwrap();
    let loader = ThemeLoader::new(Some(dir.path().to_owned()), ColorDepth::TrueColor);
    let palette = loader.load("mine").unwrap().syntax_palette();
    assert_eq!(palette.string, ks_preview::Rgb::new(0x44, 0x55, 0x66));
    // Keys the base sets explicitly are inherited, not re-derived from the new accent.
    assert_eq!(palette.keyword, ks_preview::Rgb::new(0xd9, 0x77, 0x57));

    // A theme file without any [syntax] table derives everything from its colours.
    let colors = [
        ("bg", "reset"),
        ("fg", "#ffffff"),
        ("fg_dim", "#111111"),
        ("fg_muted", "#222222"),
        ("accent", "#333333"),
        ("accent_fg", "#000000"),
        ("selection_bg", "#000000"),
        ("border", "#000000"),
        ("border_focus", "#000000"),
        ("dir", "#444444"),
        ("exec", "#000000"),
        ("symlink", "#555555"),
        ("hidden", "#000000"),
        ("success", "#666666"),
        ("warning", "#777777"),
        ("error", "#000000"),
        ("git_added", "#000000"),
        ("git_modified", "#000000"),
        ("git_untracked", "#888888"),
        ("git_ignored", "#000000"),
        ("git_conflict", "#000000"),
        ("match", "#000000"),
    ];
    let mut text = String::from("[colors]\n");
    for (name, value) in colors {
        text.push_str(&format!("{name} = \"{value}\"\n"));
    }
    std::fs::write(dir.path().join("plain.toml"), text).unwrap();
    let plain = loader.load("plain").unwrap();
    // `plain` has no `base`, so it inherits ember's explicit syntax colours...
    assert_eq!(
        plain.syntax_palette().keyword,
        ks_preview::Rgb::new(0xd9, 0x77, 0x57)
    );
    // ...while a theme with no explicit syntax anywhere would derive them: check the
    // derivation through the base-less built-in resolution.
    let derived = crate::syntax::SyntaxTheme::resolve(
        crate::syntax::SyntaxFile::default(),
        crate::syntax::DeriveFrom {
            fg: crate::theme::ThemeColor::Rgb(1, 1, 1),
            accent: crate::theme::ThemeColor::Rgb(2, 2, 2),
            success: crate::theme::ThemeColor::Rgb(3, 3, 3),
            fg_muted: crate::theme::ThemeColor::Rgb(4, 4, 4),
            fg_dim: crate::theme::ThemeColor::Rgb(5, 5, 5),
            dir: crate::theme::ThemeColor::Rgb(6, 6, 6),
            warning: crate::theme::ThemeColor::Rgb(7, 7, 7),
            symlink: crate::theme::ThemeColor::Rgb(8, 8, 8),
            git_untracked: crate::theme::ThemeColor::Rgb(9, 9, 9),
        },
    )
    .palette();
    assert_eq!(derived.keyword, ks_preview::Rgb::new(2, 2, 2));
    assert_eq!(derived.string, ks_preview::Rgb::new(3, 3, 3));
    assert_eq!(derived.comment, ks_preview::Rgb::new(4, 4, 4));
    assert_eq!(derived.function, ks_preview::Rgb::new(6, 6, 6));
    assert_eq!(derived.type_, ks_preview::Rgb::new(7, 7, 7));
    assert_eq!(derived.number, ks_preview::Rgb::new(8, 8, 8));
    assert_eq!(derived.constant, ks_preview::Rgb::new(9, 9, 9));
    assert_eq!(derived.operator, ks_preview::Rgb::new(5, 5, 5));
    assert_eq!(derived.punctuation, ks_preview::Rgb::new(5, 5, 5));
}

#[test]
fn span_styles_map_roles_and_rgb() {
    let t = theme("ember");
    let style = span_style(SpanStyle::role(Role::Error).bold().dim(), &t);
    assert_eq!(style.fg, Some(t.error));
    assert!(style.add_modifier.contains(Modifier::BOLD | Modifier::DIM));
    assert_eq!(span_style(SpanStyle::default(), &t), Style::new());
    let rgb = span_style(SpanStyle::rgb(ks_preview::Rgb::new(1, 2, 3)), &t);
    assert_eq!(rgb.fg, Some(ratatui::style::Color::Rgb(1, 2, 3)));
    // At 256 colours RGB spans are mapped into the palette.
    let t256 = Theme::builtin("ember", ColorDepth::Indexed256).unwrap();
    let rgb = span_style(SpanStyle::rgb(ks_preview::Rgb::new(255, 255, 255)), &t256);
    assert!(matches!(rgb.fg, Some(ratatui::style::Color::Indexed(_))));
}

#[test]
fn long_lines_are_clipped_without_splitting_wide_characters() {
    let theme = theme("ember");
    let mut out = Vec::new();
    let line = StyledLine::plain("日本語のとても長い行です");
    push_clipped(&mut out, &line, 7, &theme);
    let text: String = out.iter().map(|s| s.content.as_ref()).collect();
    assert_eq!(text, "日本語");
    assert!(width(&text) <= 7);
}

#[test]
fn hex_rows_are_8_wide_in_a_narrow_pane() {
    let theme = theme("ember");
    let PreviewBody::Binary(binary) = hex_body() else {
        unreachable!()
    };
    let wide = binary_lines(&binary, 80, 0, 100, &theme);
    let narrow = binary_lines(&binary, 48, 0, 100, &theme);
    assert_eq!(wide.len(), 6);
    assert_eq!(narrow.len(), 12);
    assert!(narrow.iter().all(|l| l.width() <= 46), "fits 48 columns");
}

/// Keeps the per-keypress budget honest (PLAN section 11: under 16 ms): `update` plus a full
/// render of a 1000 entry directory with a text preview. Debug builds are several times
/// slower than release, so the assertion is generous there; the measured numbers are in
/// docs/progress/p2.md (`cargo test -p ks-ui --release keypress -- --nocapture`).
#[test]
#[allow(clippy::print_stdout)]
fn keypress_to_frame_stays_within_budget() {
    use std::time::{Duration, Instant};

    let theme = theme("ember");
    let mut s = preview_state(160, 48);
    let rows = s.viewport_height();
    let many: Vec<Entry> = (0..1000)
        .map(|i| Entry::file(format!("file_{i:04}.rs"), 1000 + i))
        .collect();
    s.view.push_chunk(many, rows);
    let mut terminal = Terminal::new(TestBackend::new(160, 48)).unwrap();
    let mut times = Vec::new();
    let presses = 200u32;
    // One warm-up press: the first frame builds caches.
    for press in 0..=presses {
        let start = Instant::now();
        key(&mut s, "j");
        // The engine answers instantly in this test.
        show(&mut s, text_body(&theme), None);
        terminal.draw(|f| render(f, &s, &theme)).unwrap();
        if press > 0 {
            times.push(start.elapsed());
        }
    }
    times.sort();
    let mean = times.iter().sum::<Duration>() / presses;
    let p99 = times[times.len() * 99 / 100];
    println!(
        "keypress -> frame: mean {mean:?}, p99 {p99:?}, max {:?} over {presses} presses",
        times[times.len() - 1]
    );
    // Other tests share the machine (the image tests scale large images in debug builds), so
    // the assertion is on the mean in debug builds and on the p99 in release builds.
    if cfg!(debug_assertions) {
        assert!(mean.as_millis() < 40, "mean {mean:?} exceeds 40 ms");
    } else {
        assert!(p99.as_millis() < 16, "p99 {p99:?} exceeds 16 ms");
    }
}

#[test]
fn wrapping_breaks_at_spaces_and_cuts_long_words() {
    use crate::preview::wrap;
    assert_eq!(wrap("aaa bbb ccc", 7), ["aaa bbb", "ccc"]);
    assert_eq!(wrap("short", 40), ["short"]);
    assert_eq!(wrap("abcdefghij kl", 5), ["abcd…", "kl"]);
    assert!(wrap("", 10).is_empty());
}
