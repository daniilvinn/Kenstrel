//! Kestrel application: terminal setup, the event loop and the effect runner.
//!
//! The binary (`ks`) is a thin wrapper around [`run`]; everything else lives here so it can
//! be tested.

pub mod cli;
pub mod effects;
pub mod event_loop;
pub mod input;
pub mod logging;
pub mod paths;
pub mod settings;
pub mod startup;
pub mod terminal;

use std::io::IsTerminal;

use anyhow::{bail, Context, Result};
use crossterm::event::EventStream;
use ks_core::Size;
use ks_ui::ColorDepth;

use crate::cli::Cli;
use crate::event_loop::LoopOptions;
use crate::paths::AppPaths;
use crate::settings::{build_session, discover_places, SessionInputs};
use crate::terminal::TerminalSession;

/// Runs the TUI to completion.
pub fn run(cli: &Cli) -> Result<()> {
    // Checked first so a refused start has no side effects (no log file, no config).
    if !std::io::stdin().is_terminal() || !std::io::stdout().is_terminal() {
        bail!("ks is an interactive program and must be run in a terminal");
    }

    let paths = AppPaths::discover()?;
    // Held until exit so buffered log lines are flushed.
    let _log_guard = logging::init(&paths.log_dir());
    terminal::install_panic_hook(terminal::restore);

    let cwd = std::env::current_dir().context("cannot determine the current directory")?;
    let start_dir = startup::resolve_start_dir(cli.path.as_deref(), &cwd)?;
    let (width, height) = crossterm::terminal::size().context("cannot read the terminal size")?;

    let session = build_session(SessionInputs {
        paths: &paths,
        cwd: start_dir,
        home: paths::home_dir(),
        places: discover_places(&paths, paths::home_dir()),
        size: Size::new(width, height),
        depth: ColorDepth::detect_from_env(),
        now: std::time::SystemTime::now(),
        utc_offset_secs: ks_fs::utc_offset_secs(),
    });
    tracing::info!(cwd = %session.state.cwd.display(), theme = %session.theme, "starting");

    let runtime = tokio::runtime::Builder::new_multi_thread()
        .enable_all()
        .build()
        .context("cannot start the async runtime")?;

    let mut state = session.state;
    // Load the syntax set while the first listing is on its way.
    std::thread::spawn(ks_preview::highlight::warm_up);
    let outcome = {
        // Dropping the session restores the terminal on every path out of this block.
        let mut tui = TerminalSession::enter().context("cannot initialise the terminal")?;
        // With previews off there is nothing to draw images with: skip the query (up to
        // 250 ms at start-up). Pressing `w` later falls back to halfblocks.
        if state.config.preview.enabled {
            init_images(state.config.preview.image_protocol);
        }
        runtime.block_on(event_loop::run(
            tui.terminal(),
            EventStream::new(),
            &mut state,
            &session.theme,
            LoopOptions {
                startup: session.startup,
                panic_after_first_frame: cli.debug_panic,
            },
        ))
    };
    // A listing stuck in a blocking syscall (an offline network share) cannot be cancelled;
    // dropping the runtime would wait for it, so quitting would hang. Abandon it instead.
    runtime.shutdown_timeout(std::time::Duration::from_millis(200));
    let quit = outcome?;

    startup::finish(quit, cli.cwd_file.as_deref(), &state.cwd)?;
    tracing::info!("exiting");
    Ok(())
}

/// Asks the terminal which graphics protocol it speaks and installs the image renderer.
///
/// Runs right after the alternate screen is entered and before the first input is read (the
/// query's answer arrives on stdin). It waits at most `ks_ui::images::QUERY_TIMEOUT`; a
/// terminal that does not answer gets halfblocks, and `[preview] image_protocol` can force a
/// protocol (or skip the query with `"halfblocks"`).
fn init_images(preference: ks_core::config::ImageProtocol) {
    let started = std::time::Instant::now();
    let picker = ks_ui::images::detect_picker(preference);
    tracing::info!(
        detected = %ks_ui::images::describe(&picker),
        ?preference,
        took_ms = started.elapsed().as_millis() as u64,
        "image protocol"
    );
    ks_ui::images::install(picker);
}
