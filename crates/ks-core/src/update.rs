//! The state transition function: `update(state, msg) -> effects`.
//!
//! `update` is synchronous and does no I/O. Anything slow becomes an [`Effect`] whose result
//! returns as a later [`Msg`].

use std::time::Duration;

use crate::action::Action;
use crate::aids;
use crate::config::SortKey;
use crate::effect::Effect;
use crate::keymap::{Invocation, Resolution};
use crate::mode::{Mode, Overlay};
use crate::msg::{Msg, TimerKind};
use crate::nav;
use crate::state::{AppState, Level, QuitKind, Status};
use crate::tabs;
use crate::{help, palette};

/// How long informational messages stay on the status line.
const TOAST_DURATION: Duration = Duration::from_secs(3);
/// Errors stay longer than PLAN section 4.1's 3 s so config errors can actually be read
/// (PROGRESS.md deviation 11).
const ERROR_DURATION: Duration = Duration::from_secs(8);

/// Applies `msg` to `state` and returns the effects to run.
pub fn update(state: &mut AppState, msg: Msg) -> Vec<Effect> {
    let mut effects = Vec::new();
    match msg {
        Msg::Key(chord) => on_key(state, chord, &mut effects),
        Msg::Resize(size) => {
            if state.size != size {
                state.size = size;
                nav::fit_viewport(state);
                state.palette.refit(palette::visible_rows(size));
                state.help.clamp_scroll(help::visible_rows(size));
            }
            state.dirty = true;
        }
        Msg::Tick => {
            if state.has_running_jobs() {
                state.spinner_frame = state.spinner_frame.wrapping_add(1);
                state.dirty = true;
            }
        }
        Msg::Timer { kind, gen } => on_timer(state, kind, gen, &mut effects),
        Msg::Notify { level, text } => set_status(state, level, text, &mut effects),
        Msg::JobStarted(id) => {
            state.jobs.insert(id);
            state.dirty = true;
        }
        Msg::JobFinished(id) => {
            state.jobs.remove(&id);
            state.dirty = true;
        }
        Msg::Interrupt => state.quit = Some(QuitKind::Exit),
        Msg::Start => {
            nav::start(state, &mut effects);
            effects.push(Effect::LoadNavData);
        }
        Msg::NavDataLoaded(loaded) => aids::on_loaded(state, &mut effects, *loaded),
        Msg::Clock {
            now,
            utc_offset_secs,
        } => {
            state.now = now;
            state.utc_offset_secs = utc_offset_secs;
            state.dirty = true;
        }
        Msg::ListingChunk { gen, entries } => nav::on_chunk(state, &mut effects, gen, entries),
        Msg::ListingFinished { gen, error } => nav::on_finished(state, &mut effects, gen, error),
        Msg::DrivesLoaded { gen, drives } => nav::on_drives(state, &mut effects, gen, drives),
        Msg::ChildCounts { gen, counts } => nav::on_child_counts(state, gen, counts),
        Msg::DirChanged { gen } => nav::on_dir_changed(state, &mut effects, gen),
        Msg::GitStatus { gen, snapshot } => nav::on_git_status(state, gen, snapshot),
        Msg::ConfigReloaded {
            config,
            keymap,
            issues,
        } => on_config_reloaded(state, *config, *keymap, issues, &mut effects),
        Msg::ThemesAvailable(names) => {
            state.themes = names;
            state.dirty = true;
        }
        Msg::ThemeApplied(name) => {
            state.config.theme = name;
            state.dirty = true;
        }
    }
    // The view drops visual mode when its anchor vanishes (refresh, hidden toggle).
    if state.mode == Mode::Visual && !state.view.in_visual_mode() {
        state.mode = Mode::Normal;
        state.dirty = true;
    }
    effects
}

fn on_key(state: &mut AppState, key: crate::key::KeyChord, effects: &mut Vec<Effect>) {
    // The help filter field owns every key while it has focus.
    if state.overlays.last() == Some(&Overlay::Help)
        && state.help.filtering
        && help::on_filter_key(state, key)
    {
        return;
    }
    let ctx = state.key_context();
    match state.keymap.resolve(ctx, &state.pending, key) {
        Resolution::Action(invocation) => {
            clear_pending(state, effects);
            run_action(state, invocation, effects);
            state.dirty = true;
        }
        Resolution::Pending => {
            state.pending.push(key);
            state.which_key_visible = false;
            state.dirty = true;
            let gen = state.which_key_gen.bump();
            effects.push(Effect::StartTimer {
                kind: TimerKind::WhichKey,
                gen,
                delay: Duration::from_millis(state.config.ui.which_key_delay_ms),
            });
        }
        // An unbound key with nothing pending changes nothing, so it costs no redraw.
        Resolution::NoMatch => {
            // Unbound keys are text for the palette's query field.
            if state.overlays.last() == Some(&Overlay::Palette) && palette::on_text_key(state, key)
            {
                return;
            }
            let pending = clear_pending(state, effects);
            if aids::on_unbound_key(state, key) || pending {
                state.dirty = true;
            }
        }
    }
}

/// Abandons a partially entered key sequence and hides the which-key popup.
/// Returns whether anything was pending (so the screen needs a redraw).
fn clear_pending(state: &mut AppState, effects: &mut Vec<Effect>) -> bool {
    if state.pending.is_empty() && !state.which_key_visible {
        return false;
    }
    state.pending.clear();
    state.which_key_visible = false;
    state.which_key_gen.bump();
    effects.push(Effect::CancelTimer(TimerKind::WhichKey));
    true
}

fn on_timer(
    state: &mut AppState,
    kind: TimerKind,
    gen: crate::generation::Gen,
    effects: &mut Vec<Effect>,
) {
    match kind {
        TimerKind::GitRefresh => nav::on_git_timer(state, effects, gen),
        TimerKind::StateSave => aids::on_save_timer(state, gen, effects),
        TimerKind::WhichKey => {
            if state.which_key_gen.is_current(gen) && !state.pending.is_empty() {
                state.which_key_visible = true;
                state.dirty = true;
            }
        }
        TimerKind::StatusExpiry => {
            if state.status_gen.is_current(gen) && state.status.take().is_some() {
                state.dirty = true;
            }
        }
    }
}

pub(crate) fn set_status(
    state: &mut AppState,
    level: Level,
    text: String,
    effects: &mut Vec<Effect>,
) {
    let gen = state.status_gen.bump();
    state.status = Some(Status { level, text, gen });
    state.dirty = true;
    effects.push(Effect::StartTimer {
        kind: TimerKind::StatusExpiry,
        gen,
        delay: if level == Level::Error {
            ERROR_DURATION
        } else {
            TOAST_DURATION
        },
    });
}

pub(crate) fn run_action(state: &mut AppState, invocation: Invocation, effects: &mut Vec<Effect>) {
    let handled = match state.overlays.last() {
        Some(Overlay::Palette) => palette::on_action(state, invocation, effects),
        Some(Overlay::Help) => help::on_action(state, invocation, effects),
        _ => false,
    };
    if handled {
        return;
    }
    match invocation.action {
        Action::PaletteOpen => palette::open(state),
        Action::HelpOpen => help::open(state),
        Action::ThemeSwitch => palette::open_page(state, palette::Page::Themes),
        Action::IconsSwitch => palette::open_page(state, palette::Page::Icons),
        Action::ConfigReload => effects.push(Effect::ReloadConfig),
        Action::Quit => state.quit = Some(QuitKind::Exit),
        Action::QuitCd => state.quit = Some(QuitKind::ChangeDir),
        Action::Cancel => cancel(state),
        Action::CursorDown if aids::owns_overlay(state) => aids::move_selection(state, 1),
        Action::CursorUp if aids::owns_overlay(state) => aids::move_selection(state, -1),
        Action::Accept if aids::owns_overlay(state) => aids::accept(state, effects),
        Action::BookmarkSet => aids::set_bookmark(state, effects, invocation.arg),
        Action::BookmarkJump => aids::jump_bookmark(state, effects, invocation.arg),
        Action::BookmarkList => aids::open_bookmarks(state),
        Action::BookmarkDelete => aids::delete_bookmark(state, effects),
        Action::FrecencyJump => aids::open_jump(state),
        Action::TabNew => tabs::new_tab(state, effects),
        Action::TabGoto => tabs::goto(state, effects, invocation.arg),
        Action::TabClose => tabs::close(state, effects),
        Action::CursorDown => nav::move_cursor(state, 1),
        Action::CursorUp => nav::move_cursor(state, -1),
        Action::CursorTop => nav::cursor_edge(state, false),
        Action::CursorBottom => nav::cursor_edge(state, true),
        Action::PageHalfDown => nav::half_page(state, true),
        Action::PageHalfUp => nav::half_page(state, false),
        Action::DirEnter => nav::enter(state, effects),
        Action::DirParent => nav::parent(state, effects),
        Action::HistoryBack => nav::history_back(state, effects),
        Action::HistoryForward => nav::history_forward(state, effects),
        Action::GoHome
        | Action::GoDownloads
        | Action::GoDesktop
        | Action::GoConfig
        | Action::GoDrives => nav::go_place(state, effects, invocation.action),
        Action::MarkToggle => nav::toggle_mark(state),
        Action::MarkAll => nav::mark_all(state),
        Action::VisualToggle => nav::toggle_visual(state),
        Action::HiddenToggle => nav::toggle_hidden(state, effects),
        Action::SortName => nav::sort_by(state, effects, SortKey::Name),
        Action::SortSize => nav::sort_by(state, effects, SortKey::Size),
        Action::SortMtime => nav::sort_by(state, effects, SortKey::Mtime),
        Action::SortExt => nav::sort_by(state, effects, SortKey::Ext),
        other => set_status(
            state,
            Level::Info,
            format!("{} is not available yet", other.title()),
            effects,
        ),
    }
}

fn on_config_reloaded(
    state: &mut AppState,
    mut config: crate::config::Config,
    keymap: crate::keymap::Keymap,
    issues: Vec<String>,
    effects: &mut Vec<Effect>,
) {
    // The active theme only changes once it has actually loaded (`Msg::ThemeApplied`).
    let wanted_theme = std::mem::replace(&mut config.theme, state.config.theme.clone());
    state.config = config;
    state.keymap = keymap;
    let (level, text) = match issues.split_first() {
        None => (Level::Info, "Config reloaded".to_owned()),
        Some((first, [])) => (Level::Error, format!("Config reloaded; {first}")),
        Some((first, rest)) => (
            Level::Error,
            format!("Config reloaded; {first} (+{} more)", rest.len()),
        ),
    };
    set_status(state, level, text, effects);
    effects.push(Effect::SetTheme {
        name: wanted_theme,
        announce: false,
    });
}

/// `esc`: close the top overlay, else leave the current mode (visual mode keeps its marks),
/// else clear the marks, else dismiss the status line.
fn cancel(state: &mut AppState) {
    if state.overlays.pop().is_some() {
        return;
    }
    if state.mode != Mode::Normal {
        state.mode = Mode::Normal;
        state.view.exit_visual();
        return;
    }
    if state.view.marks_count() > 0 {
        state.view.clear_marks();
        return;
    }
    state.status = None;
    state.status_gen.bump();
}

#[cfg(test)]
mod tests {
    use std::path::PathBuf;

    use super::*;
    use crate::config::Config;
    use crate::key::KeyChord;
    use crate::keymap::Keymap;
    use crate::mode::Overlay;
    use crate::state::{JobId, Places, Size, StartParams};

    fn state() -> AppState {
        let mut state = AppState::new(StartParams {
            cwd: PathBuf::from("/home/me/project"),
            home: Some(PathBuf::from("/home/me")),
            places: Places::default(),
            config: Config::default(),
            keymap: Keymap::defaults(),
            size: Size::new(80, 24),
            now: std::time::SystemTime::UNIX_EPOCH,
            utc_offset_secs: 0,
        });
        state.dirty = false;
        state
    }

    fn key(state: &mut AppState, text: &str) -> Vec<Effect> {
        update(state, Msg::Key(text.parse::<KeyChord>().unwrap()))
    }

    #[test]
    fn q_quits_and_capital_q_requests_cd() {
        let mut s = state();
        key(&mut s, "q");
        assert_eq!(s.quit, Some(QuitKind::Exit));

        let mut s = state();
        key(&mut s, "Q");
        assert_eq!(s.quit, Some(QuitKind::ChangeDir));

        let mut s = state();
        key(&mut s, "ctrl-c");
        assert_eq!(s.quit, Some(QuitKind::Exit));

        let mut s = state();
        update(&mut s, Msg::Interrupt);
        assert_eq!(s.quit, Some(QuitKind::Exit));
    }

    #[test]
    fn unbound_keys_do_nothing_visible() {
        let mut s = state();
        let effects = key(&mut s, "ctrl-z");
        assert!(effects.is_empty());
        assert!(s.status.is_none());
        assert!(s.quit.is_none());
        assert!(
            !s.dirty,
            "an unbound key with nothing pending needs no redraw"
        );
    }

    #[test]
    fn abandoning_a_pending_sequence_redraws() {
        let mut s = state();
        key(&mut s, "g");
        s.dirty = false;
        key(&mut s, "ctrl-z");
        assert!(s.pending.is_empty());
        assert!(s.dirty);
    }

    #[test]
    fn prefix_key_arms_which_key_timer_and_completes() {
        let mut s = state();
        let effects = key(&mut s, "g");
        assert_eq!(s.pending.len(), 1);
        let gen = match effects.as_slice() {
            [Effect::StartTimer {
                kind: TimerKind::WhichKey,
                gen,
                delay,
            }] => {
                assert_eq!(*delay, Duration::from_millis(300));
                *gen
            }
            other => panic!("unexpected effects {other:?}"),
        };

        // The timer fires: the popup appears.
        update(
            &mut s,
            Msg::Timer {
                kind: TimerKind::WhichKey,
                gen,
            },
        );
        assert!(s.which_key_visible);

        // The second key completes the sequence, clears everything and cancels the timer.
        let effects = key(&mut s, "g");
        assert!(s.pending.is_empty());
        assert!(!s.which_key_visible);
        assert!(effects.contains(&Effect::CancelTimer(TimerKind::WhichKey)));
        // `g g` is "jump to top": it ran, and nothing was reported as missing.
        assert!(s.status.is_none());
    }

    #[test]
    fn stale_which_key_timer_is_ignored() {
        let mut s = state();
        let effects = key(&mut s, "g");
        let Effect::StartTimer { gen, .. } = effects[0] else {
            panic!("expected a timer");
        };
        // The user pressed an unrelated key before the timer fired.
        key(&mut s, "z");
        assert!(s.pending.is_empty());
        s.dirty = false;
        update(
            &mut s,
            Msg::Timer {
                kind: TimerKind::WhichKey,
                gen,
            },
        );
        assert!(!s.which_key_visible);
        assert!(!s.dirty, "a stale result must not cause a redraw");
    }

    #[test]
    fn non_matching_second_key_abandons_the_sequence() {
        let mut s = state();
        key(&mut s, "g");
        let effects = key(&mut s, "x");
        assert!(s.pending.is_empty());
        assert!(effects.contains(&Effect::CancelTimer(TimerKind::WhichKey)));
        assert!(s.status.is_none(), "x must not fire Cut after a stray g");
    }

    #[test]
    fn unimplemented_actions_report_instead_of_failing_silently() {
        let mut s = state();
        let effects = key(&mut s, "y");
        let status = s.status.as_ref().unwrap();
        assert_eq!(status.level, Level::Info);
        assert_eq!(status.text, "Copy is not available yet");
        assert!(matches!(
            effects.as_slice(),
            [Effect::StartTimer {
                kind: TimerKind::StatusExpiry,
                ..
            }]
        ));
    }

    #[test]
    fn status_expires_only_for_its_own_generation() {
        let mut s = state();
        let first = update(
            &mut s,
            Msg::Notify {
                level: Level::Error,
                text: "bad config".into(),
            },
        );
        let Effect::StartTimer {
            gen: first_gen,
            delay,
            ..
        } = first[0]
        else {
            panic!("expected a timer");
        };
        assert_eq!(delay, ERROR_DURATION);

        update(
            &mut s,
            Msg::Notify {
                level: Level::Info,
                text: "newer".into(),
            },
        );
        update(
            &mut s,
            Msg::Timer {
                kind: TimerKind::StatusExpiry,
                gen: first_gen,
            },
        );
        assert_eq!(s.status.as_ref().unwrap().text, "newer");

        let gen = s.status_gen.current();
        update(
            &mut s,
            Msg::Timer {
                kind: TimerKind::StatusExpiry,
                gen,
            },
        );
        assert!(s.status.is_none());
    }

    #[test]
    fn escape_unwinds_overlays_then_mode_then_status() {
        let mut s = state();
        s.view
            .push_chunk(vec![crate::entry::Entry::file("a", 1)], 10);
        s.view.enter_visual();
        s.mode = Mode::Visual;
        s.overlays.push(Overlay::Help);
        assert_eq!(s.key_context(), crate::mode::KeyContext::Help);

        key(&mut s, "esc");
        assert!(s.overlays.is_empty());
        assert_eq!(s.mode, Mode::Visual);

        key(&mut s, "esc");
        assert_eq!(s.mode, Mode::Normal);
        // The range the visual mode marked is cleared by the next escape.
        key(&mut s, "esc");

        update(
            &mut s,
            Msg::Notify {
                level: Level::Info,
                text: "hi".into(),
            },
        );
        key(&mut s, "esc");
        assert!(s.status.is_none());
    }

    #[test]
    fn spinner_only_advances_while_jobs_run() {
        let mut s = state();
        update(&mut s, Msg::Tick);
        assert_eq!(s.spinner_frame, 0);
        assert!(!s.dirty);

        update(&mut s, Msg::JobStarted(JobId(1)));
        assert!(s.has_running_jobs());
        update(&mut s, Msg::Tick);
        assert_eq!(s.spinner_frame, 1);
        assert!(s.dirty);

        update(&mut s, Msg::JobFinished(JobId(1)));
        assert!(!s.has_running_jobs());
    }

    #[test]
    fn clock_messages_update_the_time_used_for_relative_dates() {
        let mut s = state();
        let later = std::time::SystemTime::UNIX_EPOCH + Duration::from_secs(1_000);
        update(
            &mut s,
            Msg::Clock {
                now: later,
                utc_offset_secs: 7200,
            },
        );
        assert_eq!(s.now, later);
        assert_eq!(s.utc_offset_secs, 7200);
        assert!(s.dirty);
    }

    #[test]
    fn resize_updates_size_and_marks_dirty() {
        let mut s = state();
        update(&mut s, Msg::Resize(Size::new(120, 40)));
        assert_eq!(s.size, Size::new(120, 40));
        assert!(s.dirty);
    }

    #[test]
    fn user_keymap_overrides_take_effect_through_update() {
        let mut s = state();
        let overrides = std::collections::BTreeMap::from([(
            "normal".to_owned(),
            std::collections::BTreeMap::from([("x".to_owned(), "quit".to_owned())]),
        )]);
        assert!(s.keymap.apply_overrides(&overrides).is_empty());
        key(&mut s, "x");
        assert_eq!(s.quit, Some(QuitKind::Exit));
    }
}
