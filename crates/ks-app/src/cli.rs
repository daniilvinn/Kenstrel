//! Command-line arguments.

use std::path::PathBuf;

use clap::Parser;

#[derive(Debug, Parser)]
#[command(
    name = "ks",
    version,
    about = "Kestrel: a keyboard-first terminal file explorer"
)]
pub struct Cli {
    /// Directory to open (default: the current directory). A file opens its parent.
    pub path: Option<PathBuf>,

    /// On `Q`, write the directory being browsed to FILE so a shell function can `cd` to it.
    #[arg(long, value_name = "FILE")]
    pub cwd_file: Option<PathBuf>,

    /// Print the shell function that makes `Q` change the shell's directory, then exit.
    /// Add its output to your profile (see the README).
    #[arg(long, value_name = "SHELL", value_enum)]
    pub print_shell_init: Option<crate::shell_init::Shell>,

    /// Panic right after the first frame, to test that the terminal is restored.
    #[arg(long, hide = true)]
    pub debug_panic: bool,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_all_flags() {
        let cli =
            Cli::try_parse_from(["ks", "--cwd-file", "out.txt", "--debug-panic", "src"]).unwrap();
        assert_eq!(cli.path, Some(PathBuf::from("src")));
        assert_eq!(cli.cwd_file, Some(PathBuf::from("out.txt")));
        assert!(cli.debug_panic);
    }

    #[test]
    fn defaults_are_empty() {
        let cli = Cli::try_parse_from(["ks"]).unwrap();
        assert!(cli.path.is_none() && cli.cwd_file.is_none() && !cli.debug_panic);
        assert!(cli.print_shell_init.is_none());
    }

    #[test]
    fn print_shell_init_takes_a_shell() {
        use crate::shell_init::Shell;
        let cli = Cli::try_parse_from(["ks", "--print-shell-init", "powershell"]).unwrap();
        assert_eq!(cli.print_shell_init, Some(Shell::Powershell));
        let cli = Cli::try_parse_from(["ks", "--print-shell-init=bash"]).unwrap();
        assert_eq!(cli.print_shell_init, Some(Shell::Bash));
        assert!(Cli::try_parse_from(["ks", "--print-shell-init", "cmd"]).is_err());
        assert!(Cli::try_parse_from(["ks", "--print-shell-init"]).is_err());
    }

    #[test]
    fn version_flag_is_supported() {
        let err = Cli::try_parse_from(["ks", "--version"]).unwrap_err();
        assert_eq!(err.kind(), clap::error::ErrorKind::DisplayVersion);
    }

    #[test]
    fn debug_panic_is_hidden_from_help() {
        let err = Cli::try_parse_from(["ks", "--help"]).unwrap_err();
        let help = err.to_string();
        assert!(help.contains("--cwd-file"));
        assert!(!help.contains("debug-panic"));
    }
}
