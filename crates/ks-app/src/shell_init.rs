//! Shell integration: the `k` function that runs `ks` and, after `Q`, changes the
//! shell's directory to the one Kestrel ended in. Printed by `ks --print-shell-init <shell>`.

use clap::ValueEnum;

#[derive(Clone, Copy, Debug, PartialEq, Eq, ValueEnum)]
pub enum Shell {
    /// Windows PowerShell 5.1 and PowerShell 7+.
    #[value(alias = "pwsh")]
    Powershell,
    Bash,
    Zsh,
}

// The cwd file is UTF-8 without a BOM. Windows PowerShell 5.1 reads text as the ANSI code
// page unless told otherwise, which would garble non-ASCII directory names, hence `-Encoding`.
const POWERSHELL: &str = r#"# Kestrel: run `k` instead of `ks`; quitting with Q changes to the directory you ended in.
function k {
    # Not New-TemporaryFile: a Windows PowerShell started from pwsh 7 inherits its PSModulePath
    # and then cannot load that cmdlet.
    $cwdFile = [IO.Path]::GetTempFileName()
    try {
        ks --cwd-file $cwdFile @args
        $dir = Get-Content -LiteralPath $cwdFile -Encoding UTF8 -Raw
        if ($dir) {
            $dir = $dir.TrimEnd("`r", "`n")
            if ($dir -and (Test-Path -LiteralPath $dir -PathType Container)) {
                Set-Location -LiteralPath $dir
            }
        }
    }
    finally {
        Remove-Item -LiteralPath $cwdFile -Force -ErrorAction SilentlyContinue
    }
}
"#;

const POSIX: &str = r#"# Kestrel: run `k` instead of `ks`; quitting with Q changes to the directory you ended in.
k() {
    local cwd_file dir
    cwd_file="$(mktemp)" || return
    ks --cwd-file "$cwd_file" "$@"
    dir="$(cat -- "$cwd_file")"
    rm -f -- "$cwd_file"
    if [ -n "$dir" ]; then
        # Git Bash and MSYS2 on Windows: turn C:\Users\me into /c/Users/me.
        if command -v cygpath >/dev/null 2>&1; then
            dir="$(cygpath -u -- "$dir")"
        fi
        if [ -d "$dir" ]; then
            cd -- "$dir"
        fi
    fi
}
"#;

/// The script to put in the shell's profile.
pub fn script(shell: Shell) -> &'static str {
    match shell {
        Shell::Powershell => POWERSHELL,
        Shell::Bash | Shell::Zsh => POSIX,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn scripts_call_ks_with_the_cwd_file() {
        for shell in [Shell::Powershell, Shell::Bash, Shell::Zsh] {
            let text = script(shell);
            assert!(text.contains("ks --cwd-file"), "{shell:?}");
            assert!(text.ends_with('\n'));
        }
    }

    #[test]
    fn powershell_reads_the_file_as_utf8() {
        assert!(
            script(Shell::Powershell).contains("Get-Content -LiteralPath $cwdFile -Encoding UTF8")
        );
    }

    #[test]
    fn shell_names_parse() {
        assert_eq!(Shell::from_str("pwsh", true).unwrap(), Shell::Powershell);
        assert_eq!(
            Shell::from_str("PowerShell", true).unwrap(),
            Shell::Powershell
        );
        assert_eq!(Shell::from_str("zsh", true).unwrap(), Shell::Zsh);
        assert!(Shell::from_str("cmd", true).is_err());
    }
}
