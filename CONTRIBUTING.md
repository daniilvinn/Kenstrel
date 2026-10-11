# Contributing to Kestrel

Thanks for your interest. Kestrel is a keyboard-first terminal file explorer for Windows
(binary `ks`), written in Rust. This page explains how to propose and land a change. For
building and running the tests see [BUILD.md](BUILD.md).

By taking part you agree to follow the [Code of Conduct](CODE_OF_CONDUCT.md). Contributions are
accepted under the project's [MIT license](LICENSE).

## Before you start

- **Bugs and small fixes**: search the [existing issues](https://github.com/daniilvinn/Kestrel/issues)
  first, then open a pull request directly (or an issue if you want to discuss it).
- **Features and larger changes**: open an issue first and wait for a reply before writing code.
  Kestrel is deliberately small and opinionated (calm chrome, vim-style keys, one accent colour),
  so not every feature fits, and it is better to find that out before you spend the time.
- **Security problems**: do not open a public issue, see [SECURITY.md](SECURITY.md).

## Workflow

1. Fork the repository and clone your fork.
2. Create a branch from `main`, named `<type>/<short-description>` where the type is one of the
   commit types below, for example `fix/rename-cursor-jump` or `feat/finder-scope-key`.
3. Make your change in small, focused commits.
4. Run the [local gates](#local-gates) until they pass.
5. Push to your fork and open a pull request against `main`. Fill in the pull request template.
   The title must follow Conventional Commits (a check enforces it); with squash merging it
   becomes the commit message on `main`.

## Commit messages and PR titles

Kestrel uses [Conventional Commits 1.0.0](https://www.conventionalcommits.org/en/v1.0.0/):

```
<type>(<scope>): <summary in the imperative, lower case, no trailing full stop>

<optional body: what and why, wrapped at about 100 columns>
```

Types used in this repository:

| Type | Use for |
|---|---|
| `feat` | a user-visible feature |
| `fix` | a bug fix |
| `perf` | a performance improvement |
| `refactor` | a change that neither fixes a bug nor adds a feature |
| `docs` | documentation only |
| `test` | adding or fixing tests |
| `build` | build system, `Cargo.toml`, dependencies |
| `ci` | GitHub Actions and other CI configuration |
| `chore` | anything else that does not touch behaviour |
| `style` | formatting only |
| `revert` | reverting an earlier commit |

The scope is optional. Use the crate name without the `ks-` prefix (`core`, `fs`, `search`,
`preview`, `ui`, `app`, `claude`) or an area such as `ci`, `docs` or `github`. Mark a breaking
change (a config key, a default key binding or a file format changing incompatibly) with `!`
after the type or scope, and explain it in a `BREAKING CHANGE:` footer:

```
feat(core)!: rename [files] show_hidden to [files] hidden

BREAKING CHANGE: config.toml files that set show_hidden must be updated.
```

## Local gates

CI runs exactly these on Windows and Linux, and a pull request cannot merge until they pass:

```powershell
cargo fmt --all --check
cargo clippy --workspace --all-targets -- -D warnings
cargo test --workspace
```

Run `cargo fmt --all` to fix formatting. There is no `rustfmt.toml`; the defaults apply.
`.editorconfig` records the whitespace conventions (LF, final newline).

If your change can affect rendering speed, also run:

```powershell
cargo test --release -p ks-ui keypress_to_frame_stays_within_budget -- --nocapture
```

### Snapshot tests

`ks-ui` renders screens into a test backend and compares them with [insta](https://insta.rs)
snapshots in `crates/ks-ui/src/snapshots/`. CI sets `INSTA_UPDATE=no`, so a changed rendering
fails the build instead of silently rewriting a snapshot.

After an **intended** UI change, run the tests locally; insta writes `*.snap.new` files next to
the snapshots (they are git-ignored). Review each one, then accept them with
[cargo-insta](https://insta.rs/docs/cli/) (`cargo install cargo-insta`, then `cargo insta review`
or `cargo insta accept`) and commit the updated `.snap` files. Describe the visible change in the
pull request and attach a screenshot.

### Generated keymap documentation

`docs/keymap.md` is generated from the built-in keymap and command registry; never edit it by
hand. A test fails when it is stale. After changing a default binding or adding a command,
regenerate it with either of:

```powershell
$env:KESTREL_UPDATE_DOCS = 1; cargo test -p ks-core docs; Remove-Item Env:KESTREL_UPDATE_DOCS
cargo run -p ks-app -- --dump-keymap > docs/keymap.md
```

(In Git Bash: `KESTREL_UPDATE_DOCS=1 cargo test -p ks-core docs`.)

`docs/config.example.toml` and `docs/theme.example.toml` are kept valid by tests: update them when
you add a setting or a theme token, and update the README's configuration section too.

## Architecture in a few paragraphs

Kestrel follows an Elm-style loop. `ks-core` owns the whole application state (`AppState`), the
messages that can change it (`Msg`), and a pure `update(state, msg)` function that returns the
new state together with a list of `Effect`s, which are descriptions of work to do. `ks-core`
performs no I/O at all: no file system, no clock, no threads. The keymap, commands, config
parsing, the palette and help data live there too, which makes most behaviour testable with
plain unit tests and a fake world (`testworld`).

`ks-app` is the binary and the only place where effects are executed. Its event loop reads
terminal events and results from background workers, turns them into `Msg`s, calls `update`, runs
the returned effects (spawning listing, preview, search, file-operation and Claude work), and asks
`ks-ui` to draw. Workers report back through channels as `Msg`s, tagged with generation numbers so
stale results can be dropped, and long-running work takes a cancellation token.

`ks-ui` is pure rendering: given a read-only view of the state and a theme it draws a frame with
ratatui, with no side effects, which is why it is snapshot-tested at several terminal sizes. The
engines sit beside it and know nothing about the UI: `ks-fs` (directory listing, file watcher,
git, copy/move/delete jobs, launching programs, the Windows platform layer), `ks-search` (fuzzy
file finder and content search, built on `nucleo` and the ripgrep libraries), `ks-preview`
(text, Markdown, image, archive and hex previews) and `ks-claude` (spawning the user's Claude Code
in a ConPTY pane, preflight, the MCP server and hook helpers, the IPC to the running Kestrel).

When adding a feature, the usual path is: add a `Msg` and any new state in `ks-core`, handle it in
`update` (returning an `Effect` if I/O is needed), execute the effect in `ks-app`, render the
result in `ks-ui`, and add tests at each layer.

## Coding conventions

- Stable Rust, edition 2021. Code must pass `clippy -D warnings`.
- `unsafe` is forbidden workspace-wide (`unsafe_code = "forbid"`). The one exception is the
  Windows platform module in `ks-fs` (`crates/ks-fs/src/platform/win32.rs`) for Win32 calls. Do
  not add `unsafe` anywhere else.
- Never write to stdout or stderr from library code or the running app: the terminal belongs to
  the UI. Use `tracing` (logs go to a file). `print_stdout` and `print_stderr` are lint warnings
  and `dbg!` is denied.
- `unused_must_use` is denied: handle or deliberately discard every result.
- Keep `ks-core` and `ks-ui` free of I/O; put it behind an `Effect` or in an engine crate.
- Anything that shows file content or file names must stay safe for the terminal: content goes
  through the `ks-preview` sanitiser so it can never inject escape sequences.
- Never follow junctions or symlinks while walking, and never download OneDrive placeholders.
- Do not block the UI thread. Work that can take noticeable time runs in the background and is
  cancellable.
- Tests must not depend on the real user profile or on sleeping for a fixed time, and must never
  run `claude -p` or send a prompt to a real Claude (use the fake binaries the existing tests
  use). Tests that need the real `claude` or measure performance are `#[ignore]`d.
- New dependencies need a reason in the pull request. Prefer the ones already in
  `[workspace.dependencies]`.
- User-visible changes go in [CHANGELOG.md](CHANGELOG.md) under `[Unreleased]`, and in the README
  when they change documented keys or settings.

## Claude Code integration rules

Kestrel can embed the user's own installed Claude Code. These rules are about trust and billing
and must keep holding; a pull request that breaks one will not be merged.

- **Subscription only.** Kestrel only starts the user's installed, unmodified `claude` CLI. It
  does not use the Anthropic API or an SDK.
- **Never pass `--bare`.** `ks-claude` rejects it (`PaneError::ForbiddenArg`); keep that check.
- **Strip API-billing environment variables** from every `claude` process (embedded pane,
  preflight, Windows Terminal split): `ANTHROPIC_API_KEY`, `ANTHROPIC_AUTH_TOKEN`,
  `CLAUDE_CODE_OAUTH_TOKEN`, `CLAUDE_CODE_USE_BEDROCK`, `CLAUDE_CODE_USE_VERTEX` and
  `CLAUDE_CODE_USE_FOUNDRY` (`ks_claude::env::DEFAULT_STRIP_ENV`, configurable with
  `[claude] strip_env`).
- **Never read, copy or store credentials**: not `.credentials.json`, not the keychain, not
  tokens. Preflight asks `claude auth status` and requires the `claude.ai` login method.
- **Never write `.mcp.json` into a user's project**, and never modify the user's own Claude
  configuration.
- **MCP server and hook are per spawn only**: they are injected with `--mcp-config` and
  `--settings`, pointing at generated files in `%TEMP%\kestrel-<pid>`, and are not installed
  anywhere permanent.
- Kestrel never sends a prompt itself; the user types to Claude in its pane.

## Reviews

A maintainer reviews every pull request. Small, focused pull requests are reviewed fastest. Keep
your branch up to date with `main` and respond to review comments by pushing further commits.
