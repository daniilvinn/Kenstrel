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

    /// Print the default keymap as Markdown (the source of docs/keymap.md) and exit.
    #[arg(long)]
    pub dump_keymap: bool,

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
        assert!(!cli.dump_keymap);
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
