//! Command-line arguments.

use std::path::PathBuf;

use clap::{Parser, Subcommand};

#[derive(Debug, Parser)]
#[command(
    name = "ks",
    version,
    about = "Kestrel: a keyboard-first terminal file explorer",
    args_conflicts_with_subcommands = true
)]
pub struct Cli {
    #[command(subcommand)]
    pub command: Option<Command>,

    /// Directory to open (default: the current directory). A file opens its parent.
    pub path: Option<PathBuf>,

    /// On `Q`, write the directory being browsed to FILE so a shell function can `cd` to it.
    #[arg(long, value_name = "FILE")]
    pub cwd_file: Option<PathBuf>,

    /// Panic right after the first frame, to test that the terminal is restored.
    #[arg(long, hide = true)]
    pub debug_panic: bool,
}

/// Helper processes started by Claude Code, not by the user.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Subcommand)]
pub enum Command {
    /// Serve Kestrel's tools to Claude Code over MCP (stdio). Reads KESTREL_IPC.
    #[command(hide = true)]
    Mcp,
    /// Claude Code PostToolUse hook: read the hook JSON on stdin, tell the running Kestrel,
    /// always exit 0. Reads KESTREL_IPC.
    #[command(hide = true)]
    Hook,
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
    fn helper_subcommands_parse() {
        assert_eq!(
            Cli::try_parse_from(["ks", "mcp"]).unwrap().command,
            Some(Command::Mcp)
        );
        assert_eq!(
            Cli::try_parse_from(["ks", "hook"]).unwrap().command,
            Some(Command::Hook)
        );
        // A directory with that name stays reachable.
        let cli = Cli::try_parse_from(["ks", "./mcp"]).unwrap();
        assert_eq!(cli.command, None);
        assert_eq!(cli.path, Some(PathBuf::from("./mcp")));
    }

    #[test]
    fn helper_subcommands_are_hidden_from_help() {
        let help = Cli::try_parse_from(["ks", "--help"])
            .unwrap_err()
            .to_string();
        assert!(!help.contains("mcp") && !help.contains("hook"), "{help}");
    }

    #[test]
    fn defaults_are_empty() {
        let cli = Cli::try_parse_from(["ks"]).unwrap();
        assert!(cli.path.is_none() && cli.cwd_file.is_none() && !cli.debug_panic);
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
