//! The Elm-style main loop: events and messages in, `update`, effects out, redraw when dirty.
//!
//! Generic over the ratatui backend and the event source so it can be driven headlessly in
//! tests with a `TestBackend` and a scripted event stream.

use std::collections::VecDeque;
use std::sync::Arc;
use std::time::{Duration, SystemTime};

use anyhow::{anyhow, Context, Result};
use crossterm::event::Event;
use futures_util::{Stream, StreamExt};
use ks_core::{update, AppState, Effect, Level, Msg, QuitKind};
use ks_ui::Theme;
use ratatui::backend::Backend;
use ratatui::Terminal;
use tokio::sync::mpsc;
use tokio::time::{interval, interval_at, sleep, Instant, MissedTickBehavior};

use crate::effects::EffectRunner;
use crate::input::translate_event;
use crate::reload::Reloader;

/// Redraws are capped at roughly 60 frames per second.
pub const FRAME_INTERVAL: Duration = Duration::from_millis(16);
/// The job spinner animates at 10 Hz, and only while jobs are running.
pub const SPINNER_INTERVAL: Duration = Duration::from_millis(100);
/// How often the wall clock behind relative times ("3m ago") is refreshed.
pub const CLOCK_INTERVAL: Duration = Duration::from_secs(30);

const MSG_QUEUE: usize = 256;
/// How long quitting waits for the state files to be written.
const STATE_FLUSH_TIMEOUT: Duration = Duration::from_secs(2);

pub struct LoopOptions {
    /// Messages to process before the first frame (start-up notices).
    pub startup: Vec<Msg>,
    /// Panic right after the first frame (`--debug-panic`).
    pub panic_after_first_frame: bool,
}

/// Runs until the state requests quit, returning how. Config reloads, theme switches and
/// state persistence are unavailable (see [`run_with`] and [`run_with_state`]).
pub async fn run<B, E>(
    terminal: &mut Terminal<B>,
    events: E,
    state: &mut AppState,
    theme: &Theme,
    options: LoopOptions,
) -> Result<QuitKind>
where
    B: Backend,
    B::Error: Send + Sync + 'static,
    E: Stream<Item = std::io::Result<Event>>,
{
    run_with(terminal, events, state, theme, options, None).await
}

/// Results of file work done off the loop thread (config and theme reads), delivered
/// through a side channel because `Msg` cannot carry a `Theme`.
enum Side {
    Msg(Msg),
    Theme {
        name: String,
        theme: Box<Theme>,
        announce: bool,
    },
}

/// Applies `msg` and everything it leads to. Effects that change the application itself
/// (reloading the config, switching the theme) are started here, on blocking threads,
/// because the result needs the loop's theme; the rest go to the effect runner.
fn dispatch(
    state: &mut AppState,
    reloader: Option<&Arc<Reloader>>,
    side: &mpsc::UnboundedSender<Side>,
    runner: &mut EffectRunner,
    msg: Msg,
) {
    let mut queue = VecDeque::from([msg]);
    while let Some(msg) = queue.pop_front() {
        let mut for_runner = Vec::new();
        for effect in update(state, msg) {
            match effect {
                Effect::ReloadConfig => match reloader {
                    Some(reloader) => {
                        let (reloader, side) = (Arc::clone(reloader), side.clone());
                        tokio::task::spawn_blocking(move || {
                            let _ = side.send(Side::Msg(reloader.reload_config()));
                            let _ = side.send(Side::Msg(Msg::ThemesAvailable(reloader.themes())));
                        });
                    }
                    None => queue.push_back(unavailable("Config reload")),
                },
                Effect::SetTheme { name, announce } => match reloader {
                    Some(reloader) => {
                        let (reloader, side) = (Arc::clone(reloader), side.clone());
                        tokio::task::spawn_blocking(move || {
                            let result = match reloader.load_theme(&name) {
                                Ok(theme) => Side::Theme {
                                    name,
                                    theme: Box::new(theme),
                                    announce,
                                },
                                Err(message) => Side::Msg(Msg::Notify {
                                    level: Level::Error,
                                    text: message,
                                }),
                            };
                            let _ = side.send(result);
                        });
                    }
                    None => queue.push_back(unavailable("Theme switching")),
                },
                other => for_runner.push(other),
            }
        }
        runner.run(for_runner);
    }
}

/// Turns a side-channel result into the message to apply next (adopting a loaded theme).
fn take_side(side: Side, theme: &mut Theme, tx: &mpsc::Sender<Msg>) -> Msg {
    match side {
        Side::Msg(msg) => msg,
        Side::Theme {
            name,
            theme: loaded,
            announce,
        } => {
            *theme = *loaded;
            if announce {
                let _ = tx.try_send(Msg::Notify {
                    level: Level::Info,
                    text: format!("Theme: {name}"),
                });
            }
            Msg::ThemeApplied(name)
        }
    }
}

fn unavailable(what: &str) -> Msg {
    Msg::Notify {
        level: Level::Warning,
        text: format!("{what} is not available here"),
    }
}

/// [`run`] with a [`Reloader`] that performs config reloads and theme switches.
pub async fn run_with<B, E>(
    terminal: &mut Terminal<B>,
    events: E,
    state: &mut AppState,
    theme: &Theme,
    options: LoopOptions,
    reloader: Option<Reloader>,
) -> Result<QuitKind>
where
    B: Backend,
    B::Error: Send + Sync + 'static,
    E: Stream<Item = std::io::Result<Event>>,
{
    run_with_state(terminal, events, state, theme, options, reloader, None).await
}

/// [`run_with`], persisting bookmarks and frecency in `state_dir` (loaded in the background
/// after start, the last changes flushed on exit).
pub async fn run_with_state<B, E>(
    terminal: &mut Terminal<B>,
    events: E,
    state: &mut AppState,
    theme: &Theme,
    options: LoopOptions,
    reloader: Option<Reloader>,
    state_dir: Option<std::path::PathBuf>,
) -> Result<QuitKind>
where
    B: Backend,
    B::Error: Send + Sync + 'static,
    E: Stream<Item = std::io::Result<Event>>,
{
    let mut theme = theme.clone();
    let reloader = reloader.map(Arc::new);
    let reloader = reloader.as_ref();
    let (tx, mut rx) = mpsc::channel::<Msg>(MSG_QUEUE);
    let (side_tx, mut side_rx) = mpsc::unbounded_channel::<Side>();
    let loop_tx = tx.clone();
    let mut runner = EffectRunner::new(tx.clone());
    if let Some(dir) = state_dir {
        runner.set_state_dir(dir);
    }
    spawn_interrupt_listener(tx);
    let mut events = std::pin::pin!(events);

    let mut spinner = interval(SPINNER_INTERVAL);
    spinner.set_missed_tick_behavior(MissedTickBehavior::Skip);
    let mut clock = interval_at(Instant::now() + CLOCK_INTERVAL, CLOCK_INTERVAL);
    clock.set_missed_tick_behavior(MissedTickBehavior::Skip);
    let mut last_draw: Option<Instant> = None;
    let mut panic_armed = options.panic_after_first_frame;

    if let Some(reloader) = reloader {
        let (reloader, side) = (Arc::clone(reloader), side_tx.clone());
        tokio::task::spawn_blocking(move || {
            let _ = side.send(Side::Msg(Msg::ThemesAvailable(reloader.themes())));
        });
    }
    for msg in options.startup {
        dispatch(state, reloader, &side_tx, &mut runner, msg);
    }

    let outcome = loop {
        if state.dirty && last_draw.is_none_or(|t| t.elapsed() >= FRAME_INTERVAL) {
            terminal
                .draw(|frame| ks_ui::render(frame, state, &theme))
                .map_err(anyhow::Error::new)
                .context("drawing a frame failed")?;
            state.dirty = false;
            last_draw = Some(Instant::now());
            if std::mem::take(&mut panic_armed) {
                panic!("--debug-panic: forced panic after the first frame");
            }
        }
        if let Some(kind) = state.quit {
            break kind;
        }

        // Wake for the next frame only when something is waiting to be drawn.
        let frame_due = state.dirty.then(|| {
            last_draw.map_or(Duration::ZERO, |t| {
                FRAME_INTERVAL.saturating_sub(t.elapsed())
            })
        });

        let first = tokio::select! {
            event = events.next() => match event {
                Some(Ok(event)) => translate_event(event),
                Some(Err(e)) => return Err(anyhow!(e).context("reading terminal input failed")),
                None => return Err(anyhow!("terminal input stream closed")),
            },
            Some(msg) = rx.recv() => Some(msg),
            Some(side) = side_rx.recv() => Some(take_side(side, &mut theme, &loop_tx)),
            _ = spinner.tick(), if state.has_running_jobs() => Some(Msg::Tick),
            _ = clock.tick() => Some(Msg::Clock {
                now: SystemTime::now(),
                utc_offset_secs: ks_fs::utc_offset_secs(),
            }),
            () = sleep(frame_due.unwrap_or_default()), if frame_due.is_some() => None,
        };

        // Apply everything that is already waiting before drawing, so a burst of input
        // costs one frame.
        let mut next = first;
        loop {
            if let Some(msg) = next {
                dispatch(state, reloader, &side_tx, &mut runner, msg);
            }
            match rx.try_recv() {
                Ok(msg) => next = Some(msg),
                Err(_) => break,
            }
        }
    };

    // The last visits are still inside the save debounce: write them now.
    runner.run(ks_core::pending_writes(state));
    runner.shutdown();
    let finish = tokio::task::spawn_blocking(move || {
        runner.finish_state_writes(STATE_FLUSH_TIMEOUT);
    });
    let _ = finish.await;
    Ok(outcome)
}

/// Forwards process-level interrupts (ctrl-c outside raw mode, console control events) as a
/// message. In raw mode ctrl-c is an ordinary key event and is handled by the keymap.
fn spawn_interrupt_listener(tx: mpsc::Sender<Msg>) {
    tokio::spawn(async move {
        if tokio::signal::ctrl_c().await.is_ok() {
            let _ = tx.send(Msg::Interrupt).await;
        }
    });
}

#[cfg(test)]
mod tests {
    use std::path::PathBuf;

    use crossterm::event::{KeyCode, KeyEvent, KeyEventKind, KeyEventState, KeyModifiers};
    use futures_util::stream;
    use ks_core::{Config, JobId, Keymap, Level, Places, Size, StartParams};
    use ks_ui::ColorDepth;
    use ratatui::backend::TestBackend;

    use super::*;

    fn key(c: char, kind: KeyEventKind) -> std::io::Result<Event> {
        Ok(Event::Key(KeyEvent {
            code: KeyCode::Char(c),
            modifiers: KeyModifiers::NONE,
            kind,
            state: KeyEventState::NONE,
        }))
    }

    fn state() -> AppState {
        AppState::new(StartParams {
            cwd: PathBuf::from("/home/me/code"),
            home: Some(PathBuf::from("/home/me")),
            places: Places::default(),
            config: Config::default(),
            keymap: Keymap::defaults(),
            size: Size::new(80, 24),
            now: SystemTime::UNIX_EPOCH,
            utc_offset_secs: 0,
        })
    }

    fn options() -> LoopOptions {
        LoopOptions {
            startup: Vec::new(),
            panic_after_first_frame: false,
        }
    }

    /// Runs the loop on scripted events that never end (so only `quit` can stop it).
    async fn drive(
        events: Vec<std::io::Result<Event>>,
        state: &mut AppState,
        options: LoopOptions,
    ) -> (Result<QuitKind>, Terminal<TestBackend>) {
        let mut terminal = Terminal::new(TestBackend::new(80, 24)).unwrap();
        let theme = Theme::builtin("ember", ColorDepth::TrueColor).unwrap();
        let stream = stream::iter(events).chain(stream::pending());
        let result = run(&mut terminal, stream, state, &theme, options).await;
        (result, terminal)
    }

    fn screen(terminal: &Terminal<TestBackend>) -> String {
        let buffer = terminal.backend().buffer();
        (0..buffer.area.height)
            .map(|y| {
                (0..buffer.area.width)
                    .map(|x| buffer[(x, y)].symbol())
                    .collect::<String>()
            })
            .collect::<Vec<_>>()
            .join("\n")
    }

    #[tokio::test(start_paused = true)]
    async fn q_quits_and_the_first_frame_was_drawn() {
        let mut state = state();
        let (result, terminal) =
            drive(vec![key('q', KeyEventKind::Press)], &mut state, options()).await;
        assert_eq!(result.unwrap(), QuitKind::Exit);
        assert!(screen(&terminal).contains("~/code"));
    }

    #[tokio::test(start_paused = true)]
    async fn capital_q_asks_for_cd() {
        let mut state = state();
        let (result, _) = drive(vec![key('Q', KeyEventKind::Press)], &mut state, options()).await;
        assert_eq!(result.unwrap(), QuitKind::ChangeDir);
    }

    #[tokio::test(start_paused = true)]
    async fn key_releases_do_not_double_fire() {
        let mut state = state();
        // `y` (press + release) then `q`. The release must be ignored, so exactly one
        // "not available" notice is produced and the loop still quits on `q`.
        let events = vec![
            key('y', KeyEventKind::Press),
            key('y', KeyEventKind::Release),
            key('q', KeyEventKind::Press),
            key('q', KeyEventKind::Release),
        ];
        let (result, _) = drive(events, &mut state, options()).await;
        assert_eq!(result.unwrap(), QuitKind::Exit);
        assert_eq!(
            state.status.as_ref().unwrap().text,
            "Copy is not available yet"
        );
    }

    #[tokio::test(start_paused = true)]
    async fn resize_events_update_the_state() {
        let mut state = state();
        let events = vec![Ok(Event::Resize(100, 30)), key('q', KeyEventKind::Press)];
        let (result, _) = drive(events, &mut state, options()).await;
        assert!(result.is_ok());
        assert_eq!(state.size, Size::new(100, 30));
    }

    #[tokio::test(start_paused = true)]
    async fn startup_notices_reach_the_status_line() {
        let mut state = state();
        let options = LoopOptions {
            startup: vec![Msg::Notify {
                level: Level::Error,
                text: "config.toml: bad".into(),
            }],
            panic_after_first_frame: false,
        };
        let (result, terminal) =
            drive(vec![key('q', KeyEventKind::Press)], &mut state, options).await;
        assert!(result.is_ok());
        assert!(screen(&terminal).contains("config.toml: bad"));
    }

    #[tokio::test(start_paused = true)]
    async fn status_toast_expires_via_the_effect_runner() {
        let mut state = state();
        let options = LoopOptions {
            startup: vec![Msg::Notify {
                level: Level::Info,
                text: "hello".into(),
            }],
            panic_after_first_frame: false,
        };
        let mut terminal = Terminal::new(TestBackend::new(80, 24)).unwrap();
        let theme = Theme::builtin("ember", ColorDepth::TrueColor).unwrap();
        // After 4 virtual seconds a `q` arrives; by then the 3 s toast must have expired.
        let events = stream::once(async {
            sleep(Duration::from_secs(4)).await;
            key('q', KeyEventKind::Press)
        })
        .chain(stream::pending());
        let result = run(&mut terminal, events, &mut state, &theme, options).await;
        assert_eq!(result.unwrap(), QuitKind::Exit);
        assert!(state.status.is_none(), "toast should have expired");
    }

    #[tokio::test(start_paused = true)]
    async fn spinner_ticks_only_while_jobs_run() {
        let mut state = state();
        state.jobs.insert(JobId(1));
        let mut terminal = Terminal::new(TestBackend::new(80, 24)).unwrap();
        let theme = Theme::builtin("ember", ColorDepth::TrueColor).unwrap();
        let events = stream::once(async {
            sleep(Duration::from_millis(550)).await;
            key('q', KeyEventKind::Press)
        })
        .chain(stream::pending());
        run(&mut terminal, events, &mut state, &theme, options())
            .await
            .unwrap();
        // 10 Hz for 550 ms: roughly five or six frames of animation.
        assert!(
            (4..=7).contains(&state.spinner_frame),
            "{}",
            state.spinner_frame
        );

        let mut idle = self::state();
        let events = stream::once(async {
            sleep(Duration::from_millis(550)).await;
            key('q', KeyEventKind::Press)
        })
        .chain(stream::pending());
        run(&mut terminal, events, &mut idle, &theme, options())
            .await
            .unwrap();
        assert_eq!(idle.spinner_frame, 0, "no ticks without running jobs");
    }

    #[tokio::test(start_paused = true)]
    #[should_panic(expected = "--debug-panic")]
    async fn debug_panic_fires_after_the_first_frame() {
        let mut state = state();
        let options = LoopOptions {
            startup: Vec::new(),
            panic_after_first_frame: true,
        };
        let _ = drive(vec![], &mut state, options).await;
    }

    #[tokio::test(start_paused = true)]
    async fn input_stream_errors_surface() {
        let mut state = state();
        let (result, _) = drive(
            vec![Err(std::io::Error::other("console gone"))],
            &mut state,
            options(),
        )
        .await;
        let message = format!("{:#}", result.unwrap_err());
        assert!(message.contains("console gone"), "{message}");
    }

    fn special(code: KeyCode) -> std::io::Result<Event> {
        Ok(Event::Key(KeyEvent {
            code,
            modifiers: KeyModifiers::NONE,
            kind: KeyEventKind::Press,
            state: KeyEventState::NONE,
        }))
    }

    fn typed(text: &str) -> Vec<std::io::Result<Event>> {
        text.chars().map(|c| key(c, KeyEventKind::Press)).collect()
    }

    /// Delays `events` so the start-up theme listing (read on a blocking thread) has
    /// arrived before the first key.
    fn settled(events: Vec<std::io::Result<Event>>) -> impl Stream<Item = std::io::Result<Event>> {
        stream::once(async { sleep(Duration::from_millis(300)).await })
            .filter_map(|()| async { None })
            .chain(stream::iter(events))
    }

    /// Like `drive`, but with a reloader over a temporary config directory.
    async fn drive_with_config(
        config_toml: Option<&str>,
        events: Vec<std::io::Result<Event>>,
        state: &mut AppState,
    ) -> (Result<QuitKind>, Terminal<TestBackend>, tempfile::TempDir) {
        let dir = tempfile::tempdir().unwrap();
        let paths = crate::paths::AppPaths::under(dir.path());
        if let Some(text) = config_toml {
            std::fs::create_dir_all(paths.config_dir()).unwrap();
            std::fs::write(paths.config_file(), text).unwrap();
        }
        let mut terminal = Terminal::new(TestBackend::new(80, 24)).unwrap();
        let theme = Theme::builtin("ember", ColorDepth::TrueColor).unwrap();
        // A pause before quitting lets the final frame be drawn.
        let quit = stream::once(async {
            sleep(Duration::from_millis(200)).await;
            Ok(Event::Key(KeyEvent {
                code: KeyCode::Char('c'),
                modifiers: KeyModifiers::CONTROL,
                kind: KeyEventKind::Press,
                state: KeyEventState::NONE,
            }))
        });
        let stream = settled(events).chain(quit).chain(stream::pending());
        let result = run_with(
            &mut terminal,
            stream,
            state,
            &theme,
            options(),
            Some(Reloader::new(paths, ColorDepth::TrueColor)),
        );
        let result = tokio::time::timeout(Duration::from_secs(20), result)
            .await
            .expect("loop did not quit");
        (result, terminal, dir)
    }

    // Real time: config and theme reads run on blocking threads.
    #[tokio::test]
    async fn palette_reload_config_applies_keymap_icons_and_theme() {
        let mut state = state();
        let mut events = typed(":reload config");
        events.push(special(KeyCode::Enter));
        let config = "theme = \"paper\"
icons = \"ascii\"
[keymap.normal]
\"q\" = \"none\"
\"T\" = \"file.trash\"
";
        let (result, terminal, _dir) = drive_with_config(Some(config), events, &mut state).await;
        assert_eq!(result.unwrap(), QuitKind::Exit);
        assert_eq!(state.config.theme, "paper");
        assert_eq!(state.config.icons, ks_core::config::IconSet::Ascii);
        assert_eq!(state.themes, ["ember", "paper"]);
        assert!(state
            .keymap
            .keys_for(ks_core::KeyContext::Normal, ks_core::Action::Trash)
            .contains(&"T".to_owned()));
        assert!(state
            .keymap
            .keys_for(ks_core::KeyContext::Normal, ks_core::Action::Quit)
            .iter()
            .all(|k| k != "q"));
        // The frame was redrawn with the paper theme (light text colour differs from ember).
        let paper = Theme::builtin("paper", ColorDepth::TrueColor).unwrap();
        let buffer = terminal.backend().buffer();
        assert!(
            (0..buffer.area.width).any(|x| buffer[(x, 0)].fg == paper.fg_dim),
            "header should use the paper palette"
        );
    }

    // Real time: config and theme reads run on blocking threads.
    #[tokio::test]
    async fn palette_switch_theme_changes_the_drawn_colours() {
        let mut state = state();
        let mut events = typed(":switch theme");
        events.push(special(KeyCode::Enter));
        events.push(special(KeyCode::Down));
        events.push(special(KeyCode::Enter));
        let (result, terminal, _dir) = drive_with_config(None, events, &mut state).await;
        assert!(result.is_ok());
        assert_eq!(state.config.theme, "paper");
        let paper = Theme::builtin("paper", ColorDepth::TrueColor).unwrap();
        let buffer = terminal.backend().buffer();
        assert!(
            (0..buffer.area.width).any(|x| buffer[(x, 0)].fg == paper.fg_dim),
            "header should use the paper palette"
        );
    }

    // Real time: config and theme reads run on blocking threads.
    #[tokio::test]
    async fn a_broken_user_theme_is_reported_and_the_old_theme_kept() {
        let mut state = state();
        let dir = tempfile::tempdir().unwrap();
        let paths = crate::paths::AppPaths::under(dir.path());
        std::fs::create_dir_all(paths.themes_dir()).unwrap();
        std::fs::write(
            paths.themes_dir().join("broken.toml"),
            "colors = 5
",
        )
        .unwrap();
        let mut events = typed(":switch theme");
        events.push(special(KeyCode::Enter));
        events.extend(typed("broken"));
        events.push(special(KeyCode::Enter));
        let mut terminal = Terminal::new(TestBackend::new(80, 24)).unwrap();
        let theme = Theme::builtin("ember", ColorDepth::TrueColor).unwrap();
        let quit = stream::once(async {
            sleep(Duration::from_millis(300)).await;
            Ok(Event::Key(KeyEvent {
                code: KeyCode::Char('c'),
                modifiers: KeyModifiers::CONTROL,
                kind: KeyEventKind::Press,
                state: KeyEventState::NONE,
            }))
        });
        let stream = settled(events).chain(quit).chain(stream::pending());
        let reloader = Reloader::new(paths, ColorDepth::TrueColor);
        let result = run_with(
            &mut terminal,
            stream,
            &mut state,
            &theme,
            options(),
            Some(reloader),
        );
        tokio::time::timeout(Duration::from_secs(20), result)
            .await
            .expect("loop did not quit")
            .unwrap();
        assert_eq!(state.config.theme, "ember", "theme unchanged");
        let status = state.status.as_ref().unwrap();
        assert_eq!(status.level, Level::Error);
        assert!(status.text.contains("broken"), "{}", status.text);
    }

    #[tokio::test(start_paused = true)]
    async fn without_a_reloader_switching_reports_instead_of_failing() {
        let mut state = state();
        let mut events = typed(":reload config");
        events.push(special(KeyCode::Enter));
        events.push(key('q', KeyEventKind::Press));
        let (result, _) = drive(events, &mut state, options()).await;
        assert!(result.is_ok());
        assert!(state
            .status
            .as_ref()
            .unwrap()
            .text
            .contains("not available here"));
    }

    fn ctrl(c: char) -> std::io::Result<Event> {
        Ok(Event::Key(KeyEvent {
            code: KeyCode::Char(c),
            modifiers: KeyModifiers::CONTROL,
            kind: KeyEventKind::Press,
            state: KeyEventState::NONE,
        }))
    }

    #[tokio::test(start_paused = true)]
    async fn palette_ctrl_n_ctrl_p_move_selection() {
        let mut state = state();
        let mut events = vec![key(':', KeyEventKind::Press)];
        events.extend([ctrl('n'), ctrl('n'), ctrl('n'), ctrl('p')]);
        events.push(special(KeyCode::Esc));
        events.push(key('q', KeyEventKind::Press));
        let (result, _) = drive(events, &mut state, options()).await;
        assert!(result.is_ok());
        assert_eq!(state.palette.selected, 2);
    }

    #[tokio::test(start_paused = true)]
    async fn the_which_key_popup_appears_after_the_delay_through_the_loop() {
        let mut state = state();
        let mut terminal = Terminal::new(TestBackend::new(80, 24)).unwrap();
        let theme = Theme::builtin("ember", ColorDepth::TrueColor).unwrap();
        let stream = stream::iter(vec![key('g', KeyEventKind::Press)]).chain(stream::pending());
        // The loop never quits on its own; cut it off once the timer should have fired.
        let early = tokio::time::timeout(
            Duration::from_millis(100),
            run(&mut terminal, stream, &mut state, &theme, options()),
        )
        .await;
        assert!(early.is_err(), "loop keeps running");
        assert!(!state.which_key_visible, "not before the delay");
        assert!(!screen(&terminal).contains("Go to home"));

        let stream = stream::iter(vec![key('g', KeyEventKind::Press)]).chain(stream::pending());
        let mut state2 = self::state();
        let late = tokio::time::timeout(
            Duration::from_millis(600),
            run(&mut terminal, stream, &mut state2, &theme, options()),
        )
        .await;
        assert!(late.is_err());
        assert!(state2.which_key_visible);
        assert!(
            screen(&terminal).contains("Go to home"),
            "{}",
            screen(&terminal)
        );
    }
}
