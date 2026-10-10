# Kestrel

A keyboard-first terminal file explorer for Windows (binary: `ks`), built in Rust with
[ratatui](https://ratatui.rs). Calm chrome, one accent colour, a prompt line at the bottom.

> Status: early development (phase 1, navigation). See [PLAN.md](PLAN.md) for the design and
> [PROGRESS.md](PROGRESS.md) for what exists so far.

## Build and run

```powershell
cargo run -p ks-app            # opens the current directory
cargo run -p ks-app -- --help
```

A Nerd Font and Windows Terminal are recommended. `q` quits.

## Keys so far

| Keys | Action |
|---|---|
| `j` `k` / arrows, `gg` `G`, `ctrl-d` `ctrl-u` | move, top / bottom, half page |
| `h` `l` / `backspace` `enter` | parent / enter a directory (above a drive root: This PC) |
| `ctrl-o` `tab` | history back / forward |
| `~` `g h` `g d` `g D` `g c` `g /` | home, Downloads, Desktop, config directory, This PC |
| `space` `v` `ctrl-a` `esc` | mark, select a range, mark all, clear |
| `.` | show / hide hidden files |
| `S` then `n` `s` `m` `e` | sort by name / size / modified time / extension (again: reverse) |

The header shows the git branch and change counts when the directory is in a repository, and
rows carry a git glyph (`M` modified, `+` added, `R` renamed, `D` deleted, `?` untracked,
`!` ignored, `U` conflict).

## Configuration

On first run Kestrel writes a commented `%APPDATA%\kestrel\config.toml`. Themes live in
`%APPDATA%\kestrel\themes\*.toml`; logs in `%LOCALAPPDATA%\kestrel\state\logs`.

## Tabs, bookmarks and jumping

| Keys | Action |
|---|---|
| `t` | new tab at the current directory |
| `1`..`9` | switch to that tab |
| `ctrl-w` | close the tab (the last one stays open; `q` quits) |
| `m` then a key | bookmark the current directory under that key |
| `'` then a key | jump to that bookmark |
| `M` | list bookmarks (`enter` jumps, `d` deletes, `esc` closes) |
| `z` | jump to a frequently / recently visited directory (type to filter, `enter` jumps) |

Every action is also in the command palette ("New tab", "Set bookmark", "Jump to bookmark",
"List bookmarks", "Frecency jump", ...). The tab strip (`1 ▏2 ▏3`) appears at the right of the
header once a second tab exists. Bookmarks and the visit history behind `z` live in
`%LOCALAPPDATA%\kestrel\state\bookmarks.toml` and `frecency.toml`.

## Shell integration (`Q` changes your shell's directory)

`q` quits; `Q` quits and asks your shell to `cd` to the directory you ended in. That needs a
small function in your shell profile, because a program cannot change its parent's directory.
`ks --print-shell-init <shell>` prints it:

```powershell
# PowerShell (Windows PowerShell 5.1 or 7): add to the profile, `notepad $PROFILE`
ks --print-shell-init powershell | Out-String | Invoke-Expression
```

or paste the output of `ks --print-shell-init powershell` into the profile as a plain function
(faster start-up). Then run `k` instead of `ks`. The function reads the temporary file with
`-Encoding UTF8` because Windows PowerShell 5.1 would otherwise decode it as the ANSI code page
and break non-ASCII directory names.

```sh
# bash or zsh (including Git Bash on Windows): add to ~/.bashrc or ~/.zshrc
eval "$(ks --print-shell-init bash)"
```

Windows Terminal: no special profile is needed, but to start a profile in Kestrel set its
command line to `pwsh -NoLogo -NoExit -Command k` (or `powershell.exe -NoLogo -NoExit -Command k`)
after adding the function to your PowerShell profile; `Q` then leaves you in that shell at the
directory you chose.
