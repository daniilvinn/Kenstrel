//! The Elm-style main loop: events and messages in, `update`, effects out, redraw when dirty.
//!
//! Generic over the ratatui backend and the event source so it can be driven headlessly in
//! tests with a `TestBackend` and a scripted event stream.

use std::sync::atomic::{AtomicBool, Ordering};
use std::time::{Duration, SystemTime};

use anyhow::{anyhow, Context, Result};
use crossterm::event::Event;
use futures_util::{Stream, StreamExt};
use ks_core::{update, AppState, Effect, Msg, QuitKind};
use ks_ui::Theme;
use ratatui::backend::Backend;
use ratatui::Terminal;
use tokio::sync::mpsc;
use tokio::time::{interval, interval_at, sleep, Instant, MissedTickBehavior};

use crate::effects::EffectRunner;
use crate::foreground::Foreground;
use crate::input::translate_event;

/// Redraws are capped at roughly 60 frames per second.
pub const FRAME_INTERVAL: Duration = Duration::from_millis(16);
/// The job spinner animates at 10 Hz, and only while jobs are running.
pub const SPINNER_INTERVAL: Duration = Duration::from_millis(100);
/// How often the wall clock behind relative times ("3m ago") is refreshed.
pub const CLOCK_INTERVAL: Duration = Duration::from_secs(30);

const MSG_QUEUE: usize = 256;

pub struct LoopOptions {
    /// Messages to process before the first frame (start-up notices).
    pub startup: Vec<Msg>,
    /// Panic right after the first frame (`--debug-panic`).
    pub panic_after_first_frame: bool,
    /// Runs programs attached to the terminal (terminal editors); `None` in headless runs.
    pub foreground: Option<Box<dyn Foreground>>,
}

/// Runs until the state requests quit, returning how.
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
    let (tx, mut rx) = mpsc::channel::<Msg>(MSG_QUEUE);
    let mut runner = EffectRunner::new(tx.clone());
    runner.configure(&state.config);
    let mut foreground = options.foreground;
    spawn_interrupt_listener(tx);
    let mut events = std::pin::pin!(events);

    let mut spinner = interval(SPINNER_INTERVAL);
    spinner.set_missed_tick_behavior(MissedTickBehavior::Skip);
    let mut clock = interval_at(Instant::now() + CLOCK_INTERVAL, CLOCK_INTERVAL);
    clock.set_missed_tick_behavior(MissedTickBehavior::Skip);
    let mut last_draw: Option<Instant> = None;
    let mut panic_armed = options.panic_after_first_frame;

    for msg in options.startup {
        apply(msg, state, &mut runner, &mut foreground, terminal);
    }

    let outcome = loop {
        if state.dirty && last_draw.is_none_or(|t| t.elapsed() >= FRAME_INTERVAL) {
            terminal
                .draw(|frame| ks_ui::render(frame, state, theme))
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
                apply(msg, state, &mut runner, &mut foreground, terminal);
            }
            match rx.try_recv() {
                Ok(msg) => next = Some(msg),
                Err(_) => break,
            }
        }
    };

    runner.shutdown();
    Ok(outcome)
}

/// Runs `update` for `msg` and starts the effects. `Effect::RunForeground` is handled here
/// because it needs the terminal: the TUI is suspended, the program runs, the TUI comes back
/// with a full redraw, and the result re-enters `update` as `Msg::ForegroundDone`.
fn apply<B: Backend>(
    msg: Msg,
    state: &mut AppState,
    runner: &mut EffectRunner,
    foreground: &mut Option<Box<dyn Foreground>>,
    terminal: &mut Terminal<B>,
) {
    let mut queue = std::collections::VecDeque::from([msg]);
    while let Some(msg) = queue.pop_front() {
        for effect in update(state, msg) {
            let Effect::RunForeground { id, spec } = effect else {
                runner.run([effect]);
                continue;
            };
            let guard = ForegroundGuard::enter();
            let outcome = match foreground.as_mut() {
                Some(host) => host.run(&spec),
                None => Err("no terminal available to run programs in the foreground".to_owned()),
            };
            drop(guard);
            let error = match outcome {
                Ok(Some(0) | None) => None,
                Ok(Some(code)) => Some(format!("{} exited with code {code}", spec.program)),
                Err(error) => Some(error),
            };
            // The screen was left behind; draw everything again.
            if let Err(error) = terminal.clear() {
                tracing::warn!(%error, "cannot clear the terminal after a foreground program");
            }
            state.dirty = true;
            queue.push_back(Msg::ForegroundDone { id, error });
        }
    }
}

/// Forwards process-level interrupts (ctrl-c outside raw mode, console control events) as a
/// message. In raw mode ctrl-c is an ordinary key event and is handled by the keymap.
///
/// Interrupts that arrive while a foreground program owns the console are meant for that
/// program (the console is in cooked mode then, so the control event reaches us too) and are
/// ignored.
fn spawn_interrupt_listener(tx: mpsc::Sender<Msg>) {
    tokio::spawn(async move {
        while tokio::signal::ctrl_c().await.is_ok() {
            if FOREGROUND_ACTIVE.load(Ordering::SeqCst) {
                continue;
            }
            let _ = tx.send(Msg::Interrupt).await;
            break;
        }
    });
}

/// True while a foreground program runs; see [`spawn_interrupt_listener`].
static FOREGROUND_ACTIVE: AtomicBool = AtomicBool::new(false);

/// Marks a foreground run for as long as it lives, also when the host panics.
struct ForegroundGuard;

impl ForegroundGuard {
    fn enter() -> Self {
        FOREGROUND_ACTIVE.store(true, Ordering::SeqCst);
        Self
    }
}

impl Drop for ForegroundGuard {
    fn drop(&mut self) {
        FOREGROUND_ACTIVE.store(false, Ordering::SeqCst);
    }
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
            foreground: None,
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
        // `z` (press + release) then `q`. The release must be ignored, so exactly one
        // "not available" notice is produced and the loop still quits on `q`.
        let events = vec![
            key('z', KeyEventKind::Press),
            key('z', KeyEventKind::Release),
            key('q', KeyEventKind::Press),
            key('q', KeyEventKind::Release),
        ];
        let (result, _) = drive(events, &mut state, options()).await;
        assert_eq!(result.unwrap(), QuitKind::Exit);
        assert_eq!(
            state.status.as_ref().unwrap().text,
            "Frecency jump is not available yet"
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
            foreground: None,
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
            foreground: None,
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
        // Quitting with a running job asks first; accept.
        .chain(stream::once(async {
            Ok(Event::Key(KeyEvent {
                code: KeyCode::Enter,
                modifiers: KeyModifiers::NONE,
                kind: KeyEventKind::Press,
                state: KeyEventState::NONE,
            }))
        }))
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

    /// Records what it was asked to run and answers with a canned outcome.
    struct FakeHost {
        runs: std::sync::Arc<std::sync::Mutex<Vec<ks_core::fileops::ForegroundSpec>>>,
        outcome: Result<Option<i32>, String>,
    }

    impl Foreground for FakeHost {
        fn run(&mut self, spec: &ks_core::fileops::ForegroundSpec) -> Result<Option<i32>, String> {
            self.runs.lock().unwrap().push(spec.clone());
            self.outcome.clone()
        }
    }

    /// `e` on a file with a terminal editor: the loop suspends (host), runs the editor on
    /// the file, redraws, and reports a failing exit code.
    async fn edit_with_host(outcome: Result<Option<i32>, String>) -> (AppState, Vec<String>) {
        let dir = tempfile::tempdir().unwrap();
        let mut state = state();
        state.cwd = dir.path().to_owned();
        state.config.editor.open = "notepad".into();
        state.config.editor.terminal = ks_core::config::EditorTerminal::Always;
        let rows = state.viewport_height();
        state
            .view
            .push_chunk(vec![ks_core::Entry::file("a.txt", 1)], rows);
        state.view.finish_loading();

        let runs = std::sync::Arc::new(std::sync::Mutex::new(Vec::new()));
        let host = FakeHost {
            runs: runs.clone(),
            outcome,
        };
        let waiting = runs.clone();
        let events = stream::iter([key('e', KeyEventKind::Press)])
            .chain(stream::once(async move {
                for _ in 0..100 {
                    if !waiting.lock().unwrap().is_empty() {
                        break;
                    }
                    sleep(Duration::from_millis(50)).await;
                }
                // Give the message that follows the foreground run time to be processed.
                sleep(Duration::from_millis(200)).await;
                key('q', KeyEventKind::Press)
            }))
            .chain(stream::pending());
        let mut terminal = Terminal::new(TestBackend::new(80, 24)).unwrap();
        let theme = Theme::builtin("ember", ColorDepth::TrueColor).unwrap();
        let options = LoopOptions {
            startup: Vec::new(),
            panic_after_first_frame: false,
            foreground: Some(Box::new(host)),
        };
        let result = run(&mut terminal, events, &mut state, &theme, options).await;
        assert_eq!(result.unwrap(), QuitKind::Exit);
        let programs = runs
            .lock()
            .unwrap()
            .iter()
            .map(|spec| format!("{} {}", spec.program, spec.args.join(" ")))
            .collect();
        drop(dir);
        (state, programs)
    }

    #[tokio::test]
    async fn terminal_editor_runs_through_the_foreground_host() {
        let (state, runs) = edit_with_host(Ok(Some(0))).await;
        assert_eq!(runs.len(), 1, "{runs:?}");
        assert!(runs[0].contains("a.txt"), "{runs:?}");
        assert!(state.status.is_none(), "{:?}", state.status);
    }

    #[tokio::test]
    async fn foreground_failures_reach_the_status_line() {
        let (state, runs) = edit_with_host(Ok(Some(3))).await;
        assert_eq!(runs.len(), 1);
        let text = state.status.unwrap().text;
        assert!(
            text.contains("Editor failed") && text.contains("code 3"),
            "{text}"
        );

        let (state, _) = edit_with_host(Err("cannot run vim: not found".into())).await;
        assert!(state.status.unwrap().text.contains("not found"));
    }

    #[tokio::test(start_paused = true)]
    async fn without_a_host_foreground_programs_are_refused() {
        let mut state = state();
        let mut terminal = Terminal::new(TestBackend::new(80, 24)).unwrap();
        let mut runner = crate::effects::EffectRunner::new(tokio::sync::mpsc::channel(8).0);
        let mut host: Option<Box<dyn Foreground>> = None;
        let request = ks_core::fileops::ForegroundRequest {
            spec: ks_core::fileops::ForegroundSpec {
                program: "vim".into(),
                args: vec![],
            },
            purpose: ks_core::fileops::ForegroundPurpose::Editor,
        };
        apply(
            Msg::Foreground(request),
            &mut state,
            &mut runner,
            &mut host,
            &mut terminal,
        );
        assert!(state.status.unwrap().text.contains("no terminal available"));
    }

    #[tokio::test(start_paused = true)]
    #[should_panic(expected = "--debug-panic")]
    async fn debug_panic_fires_after_the_first_frame() {
        let mut state = state();
        let options = LoopOptions {
            startup: Vec::new(),
            panic_after_first_frame: true,
            foreground: None,
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
}
