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

    /// Print the default keymap as Markdown (the source of docs/keymap.md) and exit.
    #[arg(long)]
    pub dump_keymap: bool,

    /// Print the shell function that makes `Q` change the shell's directory, then exit.
    /// Add its output to your profile (see the README).
    #[arg(long, value_name = "SHELL", value_enum)]
    pub print_shell_init: Option<crate::shell_init::Shell>,

    /// Make Kestrel the default file manager for the current user (Windows): opening a
    /// folder or drive, and Win+E, start `ks` instead of File Explorer. Then exit.
    #[arg(long, conflicts_with = "unset_default")]
    pub set_default: bool,

    /// Give folders, drives and Win+E back to File Explorer, restoring what `--set-default`
    /// replaced. Then exit.
    #[arg(long)]
    pub unset_default: bool,

    /// Start PowerShell and run Kestrel there through the `k` function, so the window
    /// stays a shell after quitting. This is what `--set-default` registers.
    #[arg(long, hide = true)]
    pub via_powershell: bool,

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
        assert!(!cli.dump_keymap);
        assert!(cli.print_shell_init.is_none());
        assert!(!cli.set_default && !cli.unset_default);
    }

    #[test]
    fn default_manager_flags_parse_and_exclude_each_other() {
        assert!(
            Cli::try_parse_from(["ks", "--set-default"])
                .unwrap()
                .set_default
        );
        assert!(
            Cli::try_parse_from(["ks", "--unset-default"])
                .unwrap()
                .unset_default
        );
        assert!(Cli::try_parse_from(["ks", "--set-default", "--unset-default"]).is_err());
    }

    #[test]
    fn via_powershell_takes_the_path_and_is_hidden() {
        let cli = Cli::try_parse_from(["ks", "--via-powershell", r"C:\x"]).unwrap();
        assert!(cli.via_powershell);
        assert_eq!(cli.path, Some(PathBuf::from(r"C:\x")));
        let help = Cli::try_parse_from(["ks", "--help"])
            .unwrap_err()
            .to_string();
        assert!(!help.contains("via-powershell"), "{help}");
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
