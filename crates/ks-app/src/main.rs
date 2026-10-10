use std::process::ExitCode;

use clap::Parser;
use ks_app::cli::{Cli, Command};

// By the time an error reaches here the terminal has been restored, so stderr is safe.
#[allow(clippy::print_stderr, clippy::print_stdout)]
fn main() -> ExitCode {
    let cli = Cli::parse();
    if let Some(shell) = cli.print_shell_init {
        // Plain stdout: this is the program's output, consumed by the shell profile.
        print!("{}", ks_app::shell_init::script(shell));
        return ExitCode::SUCCESS;
    }
    match cli.command {
        Some(Command::Hook) => {
            // Must be fast and must never fail the tool call: the hook always exits 0.
            std::process::exit(ks_claude::hook::run_hook(std::io::stdin()));
        }
        Some(Command::Mcp) => return run_mcp(),
        None => {}
    }
    match ks_app::run(&cli) {
        Ok(()) => ExitCode::SUCCESS,
        Err(e) => {
            eprintln!("ks: {e:#}");
            ExitCode::FAILURE
        }
    }
}

/// `ks mcp`: stdout belongs to the MCP protocol, so nothing else may print there.
#[allow(clippy::print_stderr)]
fn run_mcp() -> ExitCode {
    let runtime = match tokio::runtime::Builder::new_multi_thread()
        .enable_all()
        .build()
    {
        Ok(runtime) => runtime,
        Err(e) => {
            eprintln!("ks mcp: cannot start the async runtime: {e}");
            return ExitCode::FAILURE;
        }
    };
    match runtime.block_on(ks_claude::mcp::run_mcp_stdio()) {
        Ok(()) => ExitCode::SUCCESS,
        Err(e) => {
            eprintln!("ks mcp: {e}");
            ExitCode::FAILURE
        }
    }
}
