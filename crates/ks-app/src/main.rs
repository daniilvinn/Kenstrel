use std::process::ExitCode;

use clap::Parser;
use ks_app::cli::Cli;

// By the time an error reaches here the terminal has been restored, so stderr is safe.
#[allow(clippy::print_stderr, clippy::print_stdout)]
fn main() -> ExitCode {
    let cli = Cli::parse();
    if let Some(shell) = cli.print_shell_init {
        // Plain stdout: this is the program's output, consumed by the shell profile.
        print!("{}", ks_app::shell_init::script(shell));
        return ExitCode::SUCCESS;
    }
    match ks_app::run(&cli) {
        Ok(()) => ExitCode::SUCCESS,
        Err(e) => {
            eprintln!("ks: {e:#}");
            ExitCode::FAILURE
        }
    }
}
