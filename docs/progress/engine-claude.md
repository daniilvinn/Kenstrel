# Engine: claude (phase P10 backend) - crate `ks-claude`

## Done
- preflight (`claude auth status` parse/classify, install text, env stripping), 10a WT split
  builder, 10b spike (verdict: embed with vt100) + `ClaudePane` engine, key encoder, session
  persistence, 10c mention formatting, 10d IPC (interprocess named pipe, versioned NDJSON),
  rmcp stdio MCP server, hook parser/entry point, `--mcp-config`/`--settings` generators.
- 50 unit tests + 1 `#[ignore]` real-claude PTY test (passes locally: `cargo test -p ks-claude -- --ignored`).
- Gates clean: `cargo fmt --all`, `cargo clippy --workspace --all-targets -- -D warnings`,
  `cargo test --workspace`. No edits to ks-core/ks-ui/ks-app/PROGRESS.md.

## Public API overview (how wiring should use it)
All long-running pieces take `(generation: u64, sink: EventSink, cancel: CancellationToken)`; the
`EventSink = Arc<dyn Fn(ClaudeEvent) + Send + Sync>` should just do a channel send and wake the loop.
Everything runs on std threads (IPC server: own thread + private current-thread tokio runtime);
no tokio runtime is required in the host.

| Msg to add | Engine source |
|---|---|
| `ClaudePreflight{gen,result}` | `preflight::spawn_preflight(gen, PreflightOptions, cancel, sink)` -> `ClaudeEvent::Preflight`; `PreflightResult::{Ok,BillingWarning,NotLoggedIn,NotInstalled{instructions},Failed}` |
| `ClaudePaneUpdated{gen}` / `ClaudePaneExited{gen,code}` | `pane::ClaudePane::spawn(PaneConfig, gen, sink, cancel)`; on Updated call `pane.snapshot()` (coalesced: one pending event until next snapshot); render `ScreenSnapshot` cells (renderer must apply `inverse`) |
| `ClaudeIpc{gen,request,reply}` | `ipc::IpcServer::start(&ipc_name_for_pid(pid), gen, sink, cancel, opts)`; answer with `reply.send(Response::..)` |

- Launch (10a): `wt::launch_wt_split(&WtSplit::new(cwd))` (`build_wt_args` is what the tests cover).
- Launch (10b): `let files = gen::write_launch_files(&gen::default_launch_dir(), &ks_exe, &ipc_name, Default::default())?;`
  then `cfg.args = session_mode.args() + files.claude_args()` and `cfg.env = [("KESTREL_IPC", name)]`;
  session ids via `session::SessionStore::mode_for(root, resume)`.
- Input: convert crossterm keys to `keys::KeyEvent`, `pane.send_key(..)`; `pane.paste(text)`
  (bracketed only if the app enabled it); `pane.resize(PaneSize)` on layout change; the focus
  toggle key ctrl-\ must be intercepted by the app.
- 10c: `mention::format_mentions(&paths, cwd)` or `format_mention(path, cwd, Some((a,b)))`, then `pane.paste(..)`.
- `ks mcp` -> `block_on(ks_claude::mcp::run_mcp_stdio())`; `ks hook` ->
  `std::process::exit(ks_claude::hook::run_hook(std::io::stdin()))`. Neither may print anything else to stdout.
- Tracing only; the binary decides where logs go.

## Deviations (with reasons)
- ConPTY needs the `CSI 6 n` reply (found in the spike); the pane answers it itself.
- Hook command default is the plan's shell string with a forward-slash, double-quoted exe path
  (Git Bash safe). `HookCommandStyle::Exec` (command + args, no shell) exists; the docs confirm
  `args` support in current Claude Code but older versions would run the bare exe, so it is opt-in.
- `--strict-mcp-config` is intentionally not passed so the user's own MCP servers still load.
- `AuthStatus` deliberately omits email/org fields (privacy).
- Hook attribution is `session_id, tool_name, file_path` (file_path also from `notebook_path`).

## VERIFY results
- `claude auth status`: JSON by default (`--json`/`--text` flags), exit 0 when logged in. Fields:
  loggedIn, authMethod ("claude.ai"), apiProvider ("firstParty"), analyticsDisabled,
  projectsDirectory, configDirectory, email, orgId, orgName, subscriptionType. Logged-out exit
  code NOT verified (would require logging the user out).
- Flags `--session-id`, `--resume`, `--mcp-config`, `--settings`, `--strict-mcp-config` exist (see docs/spikes/claude-pty.md).
- Hook payload fields, hook settings schema and MCP config schema confirmed from the docs; a live hook payload was not captured (needs a real Claude turn).
- ctrl-\ is not documented as bound in Claude Code: free.
- `@"path with spaces"` quoting and `@path#L1-5` are NOT verified (needs typing into a live session); fallback = plain paths per plan 10c.
- rmcp 3.5.1: `#[tool_router]` / `#[tool_handler(router = self.tool_router)]`, `Parameters<T: JsonSchema>`, `serve(stdio())`; tested in-process over a duplex pipe (initialize, tools/list, tools/call).

## Added dependencies (crates/ks-claude)
interprocess 2.4.5 (tokio), portable-pty 0.9.0, vt100 0.16.2, rmcp 3.5.1 (server, transport-io, macros, schemars), schemars 1.2.2, serde_json 1.0.152, uuid 1.28.0 (v4), which 8.0.6, plus workspace serde/thiserror/tokio/tokio-util/tracing; dev: tempfile. Root `Cargo.toml` only gained the workspace member. `alacritty_terminal` was not needed.

## Benchmarks/measurements
- Real claude first screen available about 1-2 s after spawn; about 1.3 KB of output for the trust screen.
- Unit test suite 0.1 s; ignored PTY test 4.2 s.

## Open issues / wiring TODO (original list; wiring status in the section below)
- Wire the three events into Msg/update; map keys (crossterm to `keys::KeyEvent`), ctrl-\ focus toggle, pane layout and resizing, render `ScreenSnapshot` into ratatui.
- App-side IPC handler must implement all `Request` variants from its own state/index (answer `Response::Error` for unsupported ones).
- Implement `ks mcp` / `ks hook` subcommands in ks-app; pass `KESTREL_IPC`.
- UI for `INSTALL_INSTRUCTIONS` and the billing-warning confirmation.
- vt100 limitations: no OSC title, no sync-output buffering, kitty keyboard queries unanswered. Eyeball the post-trust Claude UI in the pane once wired.
- `launch_wt_split` is untested against a real `wt` (by design).
- Named pipe uses the default `interprocess` security (current user); not hardened further.

## Wiring (done in a second step; ks-core, ks-ui, ks-app now touched)
- ks-core: `claude.rs` (state, `ClaudeScreen`/cell types, `pane_area` layout: right split, 45% width, min 40 cols, full width under 80 cols), new `Msg::Claude*` (Preflight, Started, LaunchFailed, Screen, PaneExited, Reveal, Edited, SendPaths), `Effect::Claude*` (Preflight, Launch, SendPaths, WtSplit), actions `claude.resume` (`g r`), `claude.split` (`g w`, WT split 10a), `claude.focus` (`ctrl-\` in `normal` and `claude` contexts, rebindable). Focus = `Overlay::Claude` on top of the overlay stack. Billing / install / login dialogs use `Overlay::Confirm` + `ClaudeState::dialog` (enter = launch anyway for billing, esc = cancel).
- ks-ui: `claude.rs` renders the pane (left border accent when focused, inverse handled, hardware cursor when focused) and the dialogs. Only the 160x48 which-key snapshots changed (extra `g r`/`g w` entries, ellipsis).
- ks-app: `claude.rs` (`ClaudeHost`: owns pane + IPC server, runs Claude effects, converts engine events to Msgs, key/paste forwarding, resize sync, shutdown removes generated launch files), `ipc_handler.rs` (answers every `ipc::Request`; file-system requests run via `spawn_blocking`), event loop routing (while the pane has focus every key except the `claude.focus` binding goes verbatim to the pane), `ks mcp` / `ks hook` hidden subcommands (`args_conflicts_with_subcommands`, so a directory literally named `mcp` must be written `./mcp`), `ctrl-\` also accepted as raw 0x1c. Session ids persist in `<state dir>/claude-sessions.json`; KESTREL_IPC and generated `--mcp-config`/`--settings` are passed per spawn.
- New ks-app deps: `regex` 1.13.1 (content search), `thiserror`, dev `serde_json`; `ks-claude` path dep.
- Tests added: core flow (11), ui (4), app ipc_handler (7), host conversions + real IPC round trip through the host (4), `ks hook`/`ks mcp` black-box (2), cli parse (2), input (1). `cargo test -p ks-app real_claude -- --ignored` launches the real claude in a temp dir (no input sent) and renders its first screen (trust prompt) through ks-ui: passes.

## Wiring limitations / TODO for later phases
- P0 has no marks, cursor entry or index: `GetSelection` returns an empty selection (cwd only), `claude.send_marked` says nothing is marked, `Reveal` only records `state.claude.reveal` (navigation must move the cursor), `SearchFiles/SearchContents` use a bounded stand-in walker in `ipc_handler.rs` (skips .git/node_modules/target, no .gitignore) to be replaced by `ks-search`. The `touched` set exists but the `◆` glyph and changes view need the file list.
- `Msg::ClaudeSendPaths` is the entry point for `C`/content-search/visual line ranges once marks exist; `mention::format_mention(.., Some((a,b)))` is not yet called.
- Terminal bracketed paste is not enabled in `terminal.rs`, so `Event::Paste` never arrives while the pane is focused (terminal pastes arrive as key events, which already work).
- Pane scrollback (`set_scrollback`) has no key binding yet; mouse is off by design.
- Still unverified (needs a live Claude turn): `@"path with spaces"` / `@path#L1-5` mention resolution, live PostToolUse payload, `claude auth status` exit code when logged out, post-trust UI look in the pane, `launch_wt_split` against a real `wt`.

## Status of the remaining-items pass
Gates re-run clean (fmt, clippy -D warnings, test --workspace). The leftover items were NOT implemented
here because they depend on code absent from this worktree (P0 only): ks-search is an empty stub (P3/P4),
and there are no marks, cursor entry or file list (P1/P2). They stay as wiring TODOs: reveal/marks/GetSelection,
ks-search in ipc_handler, touched-glyph + changes view + activity feed, bracketed paste in terminal.rs,
scrollback key binding. Unverified live items (mention syntax, live hook payload, logged-out exit code,
real wt, post-trust UI, dialog flow in a terminal) still need a human/live Claude turn.

## Final pass (this session)
- Done: bracketed paste enabled/disabled in `terminal.rs` (Event::Paste now reaches the pane); pane scrollback bound
  to Shift+PageUp / Shift+PageDown while the pane has focus (`ClaudePane::scroll_by`, `ClaudeHost::scroll`).
- VERIFIED: `claude auth status` when logged out exits with code 1 and prints JSON with `loggedIn:false`,
  `authMethod:"none"` (checked safely with `CLAUDE_CONFIG_DIR` pointing at an empty temp dir; real credentials untouched).
- Gates clean (fmt, clippy -D warnings, test --workspace).
- Not doable in this worktree (P0 only: no file list, marks, cursor entry, ks-search): reveal navigation, real
  GetSelection/marks, `C` line-range mentions, ks-search in ipc_handler, touched glyph / changes view / activity feed.
- Not run on purpose (need a live prompt or an interactive window): mention syntax, live PostToolUse payload,
  post-trust UI, `launch_wt_split` against real wt, dialog look in a terminal.

## Review-fix pass (engine hardening)
Fixed in `ks-claude` (each with tests): pane paste sanitising (all control chars except tab/CR/LF dropped; no
single-pass bypass), resize now emits a coalesced `PaneUpdated`, terminal-query handling rewritten as a streaming
`QueryScanner` (split-safe; cursor reply uses the position at the request point; also answers XTVERSION and
OSC 10/11), exit drain by output-idle instead of a fixed sleep, deterministic pane tests (powershell child: truecolor,
`?2004h`, coalescing, `PaneExited{generation,code}`, cancel kill, resize event, `snapshot_screen` cell mapping),
session store (resume only when a transcript exists, locked + atomic + unique temp names, corrupt file moved to
`.corrupt`), preflight (non-JSON non-zero exit is `Failed`, not `NotLoggedIn`; reader no longer joined after kill;
exactly one event per `spawn_preflight`, cancel gives `Failed("cancelled")`; fake-claude tests for env stripping,
timeout, cancel, output cap), env (case-insensitive removal tested on the real child environment; `EnvPolicy::extended()`),
mention (verbatim-prefix and `..` normalisation, `MentionOptions{style: Quoted|Plain}` switch, edge-case tests, docs
no longer claim the quoted form is verified), MCP (tool failures are `isError` results; success path tested for all
5 tools through a real `IpcServer`), IPC (per-request cancellation token via `ReplyHandle::cancelled()`, cloneable
`SharedReply`, tests for reply timeout, dropped handle, oversize line, stop/drop, generation, concurrency), hook
(MultiEdit fixture, stdin cap, hang/unreachable timing), `gen::sweep_stale_launch_dirs`.

### Verdicts on findings not fixed in the engine
- Rejected: DA1 and kitty `CSI ? u` replies. ConPTY answers DA1 itself (the spike never saw it forwarded) so a second
  reply would be typed into Claude; answering the kitty query would make Claude switch to kitty key encoding, which
  `keys` does not emit. Left unanswered on purpose (documented on `TerminalQuery`).
- Rejected: `Cell.text` as `String` is kept (public type already used by ks-ui/ks-app). Blank cells do not allocate,
  and the measured cost is small (see Benchmarks), so a compact type is not worth the API break.
- Rejected: env strip default stays exactly the 6 spec'd names (plan 10.2 fixes the WT command text); stripping a user's
  `ANTHROPIC_BASE_URL` is a policy choice. `EnvPolicy::extended()` adds `ANTHROPIC_BASE_URL`, `ANTHROPIC_FOUNDRY_API_KEY`,
  `AWS_BEARER_TOKEN_BEDROCK` for the app to opt in.
- Rejected: unauthenticated IPC / predictable pipe name. Same-user processes can already read the generated files and
  run commands as the user (Claude itself has Bash); the pipe uses the default per-user security. Not a boundary.
- Rejected: restrictive file permissions on launch files; they contain no secrets and live in the per-user `%TEMP%`.
- Partially fixed: stale launch dirs are cleaned by `sweep_stale_launch_dirs` (age based; no pid liveness without OS
  bindings). The app must call it at startup.
- Deferred (app/core code, out of scope for this engine task; see Open issues): WT split skips preflight (ks-core
  `update.rs` / ks-app `claude.rs`), ks-app `ipc_handler` cancellation, project-root-relative resolution and
  `list_dir` truncation flag.
- Not observed: `ctrl-\` free in Claude Code is still "not documented as bound", never observed live.

## Benchmarks (release build, this machine)
- `snapshot_screen` at 120x40 (fully coloured screen): about 170 microseconds; 200x60 blank: about 190 microseconds.
  Cheap enough to snapshot on every `PaneUpdated`.
- vt100 parse throughput: 1.1 MiB of dense truecolor output in 4.6 ms (about 240 MB/s).
- IPC round trip over the named pipe including connect and a throwaway runtime (what `ks hook` pays): about 0.1 ms.
- `ks hook` process start to exit (release, no server reachable, payload on stdin): median 36 ms, of which about 8 ms is
  the `cmd` baseline. Faster than Claude's own tool latency; no change needed.
- Reproduce: `cargo test -p ks-claude --release --lib -- --ignored --nocapture perf ipc_latency`.

## Open issues / wiring TODO (added by the review pass)
- ks-core/ks-app: run `claude auth status` (`spawn_preflight`) before `Effect::ClaudeWtSplit` too, so the billing
  warning and install instructions also apply to the WT split.
- ks-app `ipc_handler`: poll `reply.cancelled()` (new) inside the walks; resolve relative paths against the Claude
  project root (`ClaudeHost.running.cwd`) rather than `state.cwd`; add a `truncated` flag to `ListDir`. A `root` field on
  `Request::SearchContents` would be a protocol/API change that breaks ks-app pattern matches, so it was not added here.
- ks-app: call `gen::sweep_stale_launch_dirs(&std::env::temp_dir(), std::process::id(), 24h)` once at startup; call
  `SessionStore::forget` if a resumed session still fails to start; use `SessionStore::mode_for` as before (it now falls
  back to `New(stored id)` when no transcript exists yet).
- Wiring messages that must be `Clone` should carry `ReplyHandle::into_shared()` (`SharedReply`).
- Mention quoting (`@"..."`) remains an unverified guess; flip `MentionOptions.style` to `Plain` if it misbehaves.
