# Kestrel

A keyboard-first terminal file explorer for Windows (binary: `ks`), built in Rust with
[ratatui](https://ratatui.rs). Calm chrome, one accent colour, a prompt line at the bottom.

> Status: early development (phase 0, the skeleton). See [PLAN.md](PLAN.md) for the design and
> [PROGRESS.md](PROGRESS.md) for what exists so far.

## Build and run

```powershell
cargo run -p ks-app            # opens the current directory
cargo run -p ks-app -- --help
```

A Nerd Font and Windows Terminal are recommended. `q` quits.

## Configuration

On first run Kestrel writes a commented `%APPDATA%\kestrel\config.toml`. Themes live in
`%APPDATA%\kestrel\themes\*.toml`; logs in `%LOCALAPPDATA%\kestrel\state\logs`.
