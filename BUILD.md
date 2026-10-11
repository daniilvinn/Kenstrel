# Building Kestrel

Kestrel is a Cargo workspace; the binary is `ks` (crate `ks-app`). It is designed and tested for
Windows 10 and 11.

## Prerequisites

- Windows 10 or 11 (x86_64; the aarch64 MSVC target is configured too but untested).
- [Rust](https://rustup.rs) stable with the MSVC toolchain (`stable-x86_64-pc-windows-msvc`).
  Kestrel is developed and tested with Rust 1.99; no older minimum version has been verified, so
  no `rust-version` is declared.
- The MSVC linker and the Windows SDK: install the
  [Visual Studio Build Tools](https://visualstudio.microsoft.com/downloads/) with the
  "Desktop development with C++" workload (it includes the Windows SDK).
- `git`. Also useful at run time: [Windows Terminal](https://aka.ms/terminal), a
  [Nerd Font](https://www.nerdfonts.com/) and, for the Claude features, the `claude` CLI.

## Clone and build

```powershell
git clone https://github.com/daniilvinn/Kestrel.git
cd Kestrel

cargo build -p ks-app             # debug build: target\debug\ks.exe
cargo build --release -p ks-app   # release build: target\release\ks.exe
```

The release profile uses thin LTO, one codegen unit and stripped symbols. `Cargo.lock` is
committed; add `--locked` to build exactly what CI builds.

### Static C runtime

`.cargo/config.toml` sets `-C target-feature=+crt-static` for the Windows MSVC targets, so
`ks.exe` is one file that does not need the Visual C++ redistributable (CI checks that the
release exe imports no `vcruntime`, `msvcp`, `ucrtbase` or `api-ms-win-crt` DLL). Note that the
`RUSTFLAGS` environment variable replaces that setting entirely: if you set `RUSTFLAGS`, include
`-C target-feature=+crt-static` in it.

## Run and install

```powershell
cargo run -p ks-app -- C:\Users            # run from the source tree, opening a directory
.\target\release\ks.exe                    # run a release build

cargo install --path crates/ks-app --locked   # installs ks.exe into %USERPROFILE%\.cargo\bin
```

`ks --help` lists the command line; `ks --version` prints the version.

## Tests

```powershell
cargo test --workspace --no-fail-fast
```

This runs every non-ignored test (over a thousand) in a minute or two. Snapshot tests use
`insta`; see [CONTRIBUTING.md](CONTRIBUTING.md#snapshot-tests) for updating them. Tests never
send a prompt to a real Claude; they use fake binaries.

The gates CI runs are `cargo fmt --all --check`,
`cargo clippy --workspace --all-targets -- -D warnings` and `cargo test --workspace`.

### Ignored tests

Performance guards and tests that need the real `claude` are `#[ignore]`d. Run them in release
mode with `--ignored --nocapture` to see the numbers. The checklist in
[docs/manual-test.md](docs/manual-test.md#10-performance) has the full list; the main ones:

```powershell
cargo test --release -p ks-ui keypress_to_frame_stays_within_budget -- --nocapture   # not ignored
cargo test -p ks-app --release --test navigation a_directory_of_100k -- --ignored --nocapture
cargo test -p ks-app --release --test search -- --ignored finder_keystroke --nocapture
cargo test -p ks-search --release -- --ignored finder_keystroke_latency --nocapture
cargo test -p ks-search --release --test memory -- --ignored --nocapture
cargo test -p ks-fs --release --test ops_bench -- --ignored --nocapture
```

- The 100,000-file test creates its files under `%TEMP%`. Point `TEMP` and `TMP` at a fast SSD
  (`$env:TEMP = "E:\tmp"; $env:TMP = "E:\tmp"`): on a hard disk, creating the files alone takes
  minutes.
- The memory guard needs the `mem` example built first:
  `cargo build -p ks-search --release --examples`.
- `ops_bench` copies about 1 GiB; `KS_BENCH_DIR` and `KS_BENCH_MIB` choose the volume and size.
- Two tests launch the real `claude` CLI (`real_claude_first_screen_reaches_the_state` in
  `ks-app`'s `claude_flow` test and `real_claude_renders_startup_screen` in `ks-claude`). They need
  `claude` installed and logged in, start a session, and never send a prompt. Run one by name,
  for example `cargo test -p ks-app --test claude_flow -- --ignored real_claude`.

See [docs/performance.md](docs/performance.md) for measured numbers and how they were taken.

### Benchmarks

```powershell
cargo bench -p ks-search      # criterion: directory walk, fuzzy matching, grep throughput
```

`KS_BENCH_FILES` and `KS_BENCH_PATHS` scale the corpora. A head-to-head content-search timing
tool is also available:
`cargo run -p ks-search --release --example grepbench -- <root> <pattern>`.

## Linux and macOS

Kestrel targets Windows. The workspace does compile on Linux, and CI builds, lints and tests it
on `ubuntu-latest` to catch portability mistakes. On Debian or Ubuntu you need the X11/xcb
development packages for the clipboard crate (`libxcb1-dev libxcb-render0-dev libxcb-shape0-dev
libxcb-xfixes0-dev`). The Windows-specific parts (the Win32 platform module, drive listing,
file attributes, registry-based "open with", ConPTY and the Windows Terminal Claude split) are
replaced by fallbacks or inactive elsewhere, so it is not usable as a daily driver off Windows.
macOS is not tested.

## Logs

Logs go to `%LOCALAPPDATA%\kestrel\state\logs\kestrel.<date>.log` (daily files, the last 7 are
kept); nothing is printed over the UI. Set the filter with the `KESTREL_LOG` environment variable:

```powershell
$env:KESTREL_LOG = "debug"
$env:KESTREL_LOG = "ks_app::input=trace"    # log every key event the terminal delivers
```

## Releases

Releases are cut from `main` by pushing a version tag.

1. Set the new version in `[workspace.package]` in `Cargo.toml` (run `cargo check` to refresh
   `Cargo.lock`) and move the `[Unreleased]` entries of [CHANGELOG.md](CHANGELOG.md) under a
   heading for the new version.
2. Commit, then tag and push:

   ```powershell
   git tag v0.1.0
   git push origin main v0.1.0
   ```

3. `.github/workflows/release.yml` runs for any `v*` tag. On `windows-latest` it runs fmt, clippy
   and the tests with `--locked`, builds `ks.exe` in release mode and packages
   `ks-<tag>-windows-x86_64.zip` (the exe, README, `docs/keymap.md` and the example config and
   theme) with a `.sha256` file. Only if all of that passes does it create a GitHub release with
   the exe, the zip and the checksum attached, and generated release notes. Edit the notes
   afterwards if you like.

CI (`.github/workflows/ci.yml`) also uploads `ks.exe` from every run as the `ks-windows-x86_64`
workflow artifact.
