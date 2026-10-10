use std::process::ExitCode;

use clap::Parser;
use ks_app::cli::Cli;

// By the time an error reaches here the terminal has been restored, so stderr is safe.
#[allow(clippy::print_stderr)]
fn main() -> ExitCode {
    let cli = Cli::parse();
    match ks_app::run(&cli) {
        Ok(()) => ExitCode::SUCCESS,
        Err(e) => {
            eprintln!("ks: {e:#}");
            ExitCode::FAILURE
        }
    }
}
