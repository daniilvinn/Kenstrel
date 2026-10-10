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
