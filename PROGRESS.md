# Kestrel progress

Consolidated record of the implementation of [PLAN.md](PLAN.md), one section per phase, each with
**Done / Deviations (with reasons) / VERIFY results / Open issues**, as PLAN section 0 requires.

This file is the summary. The per-track working notes stay as the detailed history in
[docs/progress/](docs/progress/README.md) (index there); each section links the notes it was built
from. When a later note contradicts an earlier one, the later one wins, and statements that the code
has since overtaken were re-checked against the tree at `06b00a8` before they were kept or dropped
(the round-3 merges `93121d6` and `5205290` and the final P8 measurements are folded in on top).

State: main at `5205290` (1299 tests, all gates clean; the P8 measurement commit follows it) contains P0 to P8 and P10 plus three audit/fix rounds
(section [Audit and fix rounds](#audit-and-fix-rounds)). P9 is skipped by decision. Everything that
needs a real Windows Terminal, a live Claude turn or GitHub is collected in
[Manual verification needed](#manual-verification-needed); no agent could run the TUI interactively.

| Phase | Scope | Status | Detail notes |
|---|---|---|---|
| P0 | Skeleton | done | this file |
| P1 | Navigation | done | this file, [engine notes](docs/progress/README.md) |
| P2 | Preview | done | [engine-preview](docs/progress/engine-preview.md), [p2](docs/progress/p2.md) |
| P3 + P4 | Filter, find file, content search | done | [engine-search](docs/progress/engine-search.md), [p34](docs/progress/p34.md) |
| P5 | Palette, help, which-key, keymap config, themes | done | [p5](docs/progress/p5.md) |
| P6 | File operations | done | [engine-ops](docs/progress/engine-ops.md), [p6](docs/progress/p6.md) |
| P7 | Tabs, bookmarks, frecency, shell integration | done | [p7](docs/progress/p7.md) |
| P8 | Polish, performance, docs, release | done in code and docs; budgets measured on the final release build (2026-10-11), all met headless except the end-to-end cold start (not demonstrated, see open issue 24); real-terminal numbers and GIFs pending | [f1](docs/progress/f1.md) to [f4](docs/progress/f4.md) |
| P9 | Whole-disk search | skipped (user decision) | |
| P10 | Claude Code integration | done (10a, 10b, 10c, 10d, 10e); 10f not wanted | [engine-claude](docs/progress/engine-claude.md), [p10](docs/progress/p10.md), [spike](docs/spikes/claude-pty.md) |

## Decisions (confirmed by the user)

- Scope: phases P0 to P8, then P10. **P9 (whole-disk search) is skipped.**
- P10: 10a (Windows Terminal split) always; 10b (embedded pane) only if the `vt100` /
  `alacritty_terminal` spike renders cleanly (it did, with `vt100`); MCP config and hooks are
  injected **per spawn** (`--mcp-config` / `--settings`), never at user scope; **10f headless
  actions are not wanted.**
- Mouse capture stays off.
- Local git only, never pushed.

## Phase history in one table

Phases were built on parallel branches (engines first, then wiring) and merged into main in this
order. The per-merge decisions are summarised in each phase's "Integration" bullets below.

| Step | Merged |
|---|---|
| P0, P1 | direct on main (`409e62a`, `eb5129c`) |
| Engines | `engine/preview`, `engine/search`, `engine/ops`, `engine/claude` (`886b106`) |
| Wiring | `phase/p5`, `phase/p7`, `phase/p6`, `phase/p2`, `phase/p34`, `phase/p10` (to `5b58d42`) |
| Fix wave | `fix/f4`, `fix/f1`, `fix/f2`, `fix/f3` (to `2208fef`) |
| Audit round 1 | `audit/a1`, `a2`, `a3` (to `eab9211`) |
| Audit round 2 | `r2/b4`, `b3`, `b1`, `b2` (to `06b00a8`) |
| Round 3 | `r3/d1` docs consolidation, then `r3/c1` watcher cancellation gate, arming catch-up and deterministic flakes (`93121d6`); notes merged to `5205290` |
| Final P8 measurements | `P8: final measurements and progress update` (release build of `5205290`) |

---

## P0: Skeleton

### Done

- Cargo workspace per PLAN section 3.2: `ks-core`, `ks-fs`, `ks-search`, `ks-preview`, `ks-ui`,
  `ks-app` (binary `ks`); `ks-claude` arrived with P10. Workspace-level `[workspace.dependencies]`,
  `[workspace.lints]` (`unsafe_code = forbid`, `dbg_macro = deny`, `print_stdout`/`print_stderr`
  warn), edition 2021, release profile `lto = "thin"`, `codegen-units = 1`, `strip = true`.
- **ks-core** (no terminal, fs or async deps):
  - `Key`/`Modifiers`/`KeyChord` with a text form (`ctrl-p`, `alt-.`, `space`) and normalisation
    (shift folded into the character; `ctrl-A` equals `ctrl-a`).
  - `Action`: every action as data with a stable id (`cursor.down`) and a palette title, generated
    by a macro so palette, help and docs iterate `Action::ALL`.
  - Keymap-as-data: `Binding` = key sequence of `KeyPattern`s (`g g`, `ctrl-p`, `m <char>`) to an
    action plus optional argument (`tab.goto:3`). `Keymap::resolve(ctx, pending, key)` returns
    `Action | Pending | NoMatch`; `<char>` captures the next key as the argument (bookmarks);
    `Visual` falls back to `Normal`; `continuations()` and `keys_for()` feed which-key, hints,
    help and the palette. User overrides (`[keymap.<context>]`, value `"none"` unbinds) replace
    conflicting defaults; invalid entries are skipped and reported, never fatal.
  - Modes and overlay stack (`Mode`, `Overlay`, `KeyContext`); the topmost overlay decides the
    keymap context.
  - `AppState`, `Msg`, `Effect`, `update(state, msg) -> Vec<Effect>`; quit (`q`, `ctrl-c`, `Q` = quit
    and cd), cancel/`esc` unwinding, multi-key prefixes with a which-key timer, status line with
    expiring toasts, job registry driving the spinner, resize, interrupt. Actions not built yet
    reported "<title> is not available yet" instead of failing silently.
  - Generation ids (`Gen`, `GenCounter`): stale timer results are dropped in `update`.
  - Config model (PLAN section 9) with serde defaults and validation; unknown keys are rejected so
    typos are caught (the whole file then falls back to defaults with the error shown). The
    commented default file is `crates/ks-core/assets/default_config.toml`; a test proves that
    uncommenting every setting reproduces `Config::default()`.
- **ks-ui**: all 22 theme tokens of section 4.4 from TOML. `ember` (dark, terracotta accent
  `#d97757`) and `paper` (light) are embedded; users add or override themes in
  `%APPDATA%\kestrel\themes\*.toml` (optional `base = "<builtin>"`, a file named like a built-in
  shadows it); `bg = "reset"` in both built-ins. Truecolor when `COLORTERM` is `truecolor`/`24bit`
  or `WT_SESSION` is set, else the xterm 256 palette. Main screen: header, separators, prompt line,
  hint line (hidden under 20 rows). `insta` snapshots at 80x24 and 160x48 in both themes, with a
  style legend after the text so colour regressions are caught.
- **ks-app**: crossterm raw mode + alternate screen through an RAII `TerminalSession` (idempotent
  restore from `Drop`, the panic hook and error paths; mouse capture never enabled); panic hook that
  restores the terminal, logs a backtrace, then chains to the default hook; `tracing` file logging in
  `%LOCALAPPDATA%\kestrel\state\logs` (daily, 7 kept, `KESTREL_LOG=debug`; an unusable directory
  disables logging rather than failing); event loop generic over backend and event stream (tested
  headlessly) with a 16 ms redraw cap and a 10 Hz spinner tick only while jobs run; effect runner
  with timers as tokio tasks under child `CancellationToken`s; `translate_key` accepting only
  `Press`/`Repeat` and mapping AltGr (Ctrl+Alt on Windows) to a plain character; config loading from
  `%APPDATA%\kestrel\config.toml` (first run writes the commented default; a bad file keeps defaults
  and shows one error line for 8 s; paths injectable via `AppPaths::under`); CLI `ks [PATH]`,
  `--version`, `--cwd-file <FILE>` (written only on `Q`), hidden `--debug-panic`; `ks` refuses to
  start when stdin/stdout is not a terminal.
- CI `.github/workflows/ci.yml` on `windows-latest` and `ubuntu-latest`; `.gitignore`, README stub.

### Acceptance (PLAN section 12, P0)

| Criterion | Evidence |
|---|---|
| `ks` opens, `q` exits | Headless event-loop tests (`q` -> `QuitKind::Exit`, `Q` -> `ChangeDir`). Real console: `ks.exe --debug-panic` in a fresh console window entered raw mode + alternate screen, drew the first frame and panicked as designed. A real keypress `q` could not be sent (no interactive console). |
| Resizes cleanly | `Msg::Resize` tests in `update` and the loop; render tests at 0x0 .. 200x2 never panic. |
| Forced panic restores the terminal | `--debug-panic` in a real console: exit code 101, panic + backtrace in the log, console returned cleanly; `tests/panic_hook.rs` proves cleanup runs before the panic is reported. |
| Keys do not double-fire | `input::tests` (release dropped), `event_loop::tests::key_releases_do_not_double_fire`. |

### Deviations

1. **`clap` added** (not in the section 2 table) for the CLI. Also added: `futures-util` (event
   stream), `tokio-util`, `tracing-subscriber` (routes `tracing` to the file appender), `dunce`,
   `unicode-width`; dev: `tempfile`, `insta`.
2. **Paths use `directories::BaseDirs`, not `ProjectDirs`**: `ProjectDirs` on Windows inserts an
   extra `config` folder; the plan wants `%APPDATA%\kestrel\config.toml`.
3. **Breadcrumb separator is `/` on Windows too**, matching the mock-up in PLAN 4.2.
4. **Config `[files]`/`[layout]` field names are mine** where the plan lists only topics
   (`files.show_hidden`, `files.show_ignored`, `files.size_base`, `layout.list_percent`,
   `layout.parent_column`, `editor.open`, `editor.open_at_line`, `ui.which_key_delay_ms`).
5. **`ctrl-i` is not bound**: terminals deliver it as `tab` (history forward), which is bound.
6. **Hint line content in P0 was just `‹q› quit`**: hints for features that did not exist would have
   misled. The hint set grew phase by phase (see P5, f3).
7. **`docs/keymap.md` generation** was deferred to P5 (done there).
8. **`ConfigReload` has no default key**; it is palette-only (and bindable via `[keymap.*]`).
9. **`q` does not write the `--cwd-file`**, only `Q` does, so the PowerShell wrapper does not `cd` on `q`.
10. The panic hook restores the terminal and chains to the default hook only for panics on the UI
    (main) thread; panics in background tasks are logged only (completed by audit a1).
11. **Error toasts last 8 s, not 3 s** (PLAN 4.1): config errors need time to be read. (Referenced
    from `ks-core/src/update.rs` as "PROGRESS.md deviation 11"; keep this number.)

### VERIFY results

- **crossterm Windows Press/Release**: confirmed in the 0.29.0 source (`event/sys/windows/parse.rs`):
  key-down is `Press`, key-up `Release`, `Repeat` is never produced; auto-repeat arrives as repeated
  `Press`. Filtering to Press/Repeat is correct and sufficient.
- **ctrl-c in raw mode**: `enable_raw_mode` clears `ENABLE_PROCESSED_INPUT`, so ctrl-c is an ordinary
  key event (bound to quit). `tokio::signal::ctrl_c()` is also wired to `Msg::Interrupt` for the
  non-raw window.
- **One crossterm version** (`cargo tree -d`): a single 0.29.0 shared with `ratatui-crossterm`.
- **Locations on Windows**: `BaseDirs::config_dir()` = `%APPDATA%`, `data_local_dir()` =
  `%LOCALAPPDATA%`, confirmed by a real run (files removed afterwards). They ignore the `APPDATA` env
  var (Known Folder API), hence the injectable `AppPaths`.
- **Windows Terminal key conflicts**: nothing in the default keymap uses `ctrl-shift-*`, `alt-enter`,
  `ctrl-,` or `ctrl-tab`.
- Crate versions were the latest at implementation time (ratatui 0.30.2, crossterm 0.29.0, tokio
  1.53.2, clap 4.6.7, toml 1.1.8, and so on; `Cargo.lock` is committed).

### Open issues

- A keypress-driven session (`q`, `Q`, drag-resizing) was never exercised in a real console; see
  [Manual verification needed](#manual-verification-needed).
- Start path is made absolute and lexically normalised, not canonicalised (junctions/symlinks stay as
  typed); `--cwd-file` fails with an error for non-Unicode directory names.
- Resolved since P0: text input widget (`ks_core::text_input`, P5), `docs/keymap.md` generation and
  drift test (P5), the `m <char>` argument test (P7), PowerShell 5.1 encoding (P7 shell init uses
  `-Encoding UTF8`).

### Dependencies added in P0

Workspace: `ks-core`, `ks-ui`. External: `anyhow`, `clap`, `crossterm` (event-stream), `directories`,
`dunce`, `futures-util`, `ratatui`, `serde`, `thiserror`, `tokio`, `tokio-util`, `toml`, `tracing`,
`tracing-appender`, `tracing-subscriber`, `unicode-width`; dev: `insta`, `tempfile`.

---

## P1: Navigation

### Done

- **Module boundaries.** `ks-core` owns data and rules without I/O (`entry`, `view` = `DirView`,
  `cursor`, `drives`, `git`, `layout`, `nav`, `error`); `ks-fs` does the I/O (listing, attributes,
  drives, watcher, git, child counts); `ks-app`'s effect runner connects them.
- **Listing** (`ks-fs::listing`): `read_dir` + `DirEntry::metadata` in `spawn_blocking` under a
  cancellation token; results as `Msg::ListingChunk`/`ListingFinished` tagged with a generation.
  First chunk after 1000 entries (or 60 ms), later chunks of 5000. Entries keep `OsString` names,
  kind (file/dir/symlink/junction; junctions told apart by reparse tag), attributes (hidden, system,
  read-only, cloud placeholder, exec), lazily filled child count. Nothing in a listing opens a file,
  so cloud placeholders are never hydrated.
- **Sorting**: precomputed natural key (digit runs by value, case-insensitive) with the exact name as
  tie-break; keys name/size/mtime/ext, `reverse`, `dirs_first`. `S` opens a which-key menu
  (`S n/s/m/e`); choosing the active key again reverses.
- **Navigation as a transaction**: the old view stays until the first chunk of the new listing
  arrives, then history, per-directory cursor memory, watcher and git are committed. A failed move
  changes only the status line. A vanished current directory climbs to the nearest existing parent.
  `h` at a drive root opens "This PC"; the cursor lands on the directory you came from.
- Keys: `j k`, `gg G`, `ctrl-d/u`, `pageup/down`, `home/end`, `h l enter backspace`, `ctrl-o`/`tab`
  history, `~ g h g d g D g c g /`, `.` hidden toggle, `space` mark, `v` visual range, `ctrl-a`
  mark all/none, `esc` unwinding.
- **Drives view** (`ks-fs::platform`): `GetLogicalDrives`, `GetDriveTypeW`, `GetVolumeInformationW`,
  `GetDiskFreeSpaceExW` with `SEM_FAILCRITICALERRORS`; label, file system, usage bar (warning from
  85 %, error from 95 %), `free of total`. Non-Windows hosts list `/`.
- **Watcher** (`notify` 8 + `notify-debouncer-full` 0.7): any event on a direct child re-lists; the
  refresh is swapped in atomically keeping the cursor by name and pruning marks of vanished names.
  Also watches the parent directory (see deviation 8).
- **Git** (`ks-fs::git`): `git --no-optional-locks -C <dir> status --porcelain=v2 -z --branch
  --ignored=traditional` plus `rev-parse --show-prefix`, concurrent, cancellable, `CREATE_NO_WINDOW`.
  Glyphs `M + R D ? ! U`; directories show the highest-priority state below them; header
  `main ↑1 ↓2 +staged ~modified ?untracked Uconflicts` or a check mark. Triggered on entering a
  directory and (debounced 300 ms) on watcher events.
- **UI**: virtualized list (a test renders 100k entries), columns icon / name (middle truncation,
  CJK-correct) / size or child count / modified time / git glyph, `▌` cursor, `●` marks, nerd /
  unicode / ascii icon sets, `format.rs` (`human_size`, `relative_time`), `Msg::Clock` every 30 s so
  relative times age without I/O in `update`.
- Tests at the time: `ks-core` 126, `ks-fs` 40 (real temp dirs, a real git repo, a real junction),
  `ks-ui` 63, `ks-app` 53 + `tests/navigation.rs`; snapshots at 80x24 and 160x48 in both themes.
- Review fixes after the first pass: quitting no longer waits for a blocking listing; a superseded
  history move puts its entry back; control characters sanitised from names; `\\?\` prefix for
  `FindFirstFileW` over 240 characters; more snapshots (visual, loading, empty, error status).

### Deviations

1. **Data model lives in `ks-core`** (`Msg` carries entries); `ks-fs` depends on `ks-core`. PLAN 3.2
   put "listing, metadata, sorting" in `ks-fs`; the I/O is there, the pure parts are in core.
2. **`ks-fs` does not inherit `[workspace.lints]`**: Win32 calls need `unsafe`, and a workspace
   `forbid` cannot be relaxed for one module. Its manifest repeats the other lints with
   `unsafe_code = "deny"`; only `platform/win32.rs` allows it (the only `unsafe` in the workspace).
3. **Streaming threshold**: first chunk after 1000 entries, then 5000 (plan: "over 5k"); starting
   earlier only helps.
4. **Sort details**: natural sort is case-insensitive on every platform; size and mtime sort
   largest/newest first; `SortMenu` became `SortName/SortSize/SortMtime/SortExt` so each is
   bindable and palette-friendly; pressing the active key again reverses (instead of `S` twice).
5. **No `..` row**; `h` goes up. Child counts are computed lazily for the first 256 real directories
   (links and cloud placeholders skipped) and include hidden entries.
6. **Marks are per directory view** (kept by name, cleared when you leave); `space` moves down after
   toggling; `esc` leaves visual mode (marks stay), then clears marks.
7. **Watcher also watches the parent directory**: `ReadDirectoryChangesW` never reports removal of
   the watched directory (verified). The debouncer's file-id cache is disabled (`NoCache`) because
   the app re-lists anyway.
8. **Git**: status is recomputed per directory change (no cross-directory cache) with
   `--ignored=traditional`; header counts use `+ ~ ? U`.
9. **Time zone**: UTC offset from `GetTimeZoneInformation` (current offset incl. DST for all dates).
10. **Perf test is an `#[ignore]`d integration test**, not a criterion bench; extra bindings
    `home/end/pageup/pagedown`.
11. No preview pane or 60/40 split in P1 (P2); the hint line was reduced to what fits 80 columns (P5
    and f3 later rebuilt it, see P5).

### VERIFY results

- **`DirEntry::metadata` on Windows needs no extra syscall** (std docs): listing and sorting 100k
  entries takes 83 ms (release). A link's kind costs one `FindFirstFileW`, only for links.
- **`FileType::is_symlink()` is true for junctions too** (a real `mklink /J` junction is classified
  `Junction` through its reparse tag).
- **notify on Windows, non-recursive**: create/delete/rename of direct children are reported within
  about 125 ms (100 ms debounce); a burst of 20 files is one notification; writes inside a
  subdirectory are not reported; removal of the watched directory is not reported (parent watch).
- **git porcelain v2 `-z`** checked against `git status --help` and a real repository (git
  2.52.0.windows.1): header records are NUL-terminated, renames carry an extra path record, collapsed
  `? dir/` / `! dir/`, repo-root-relative paths, so `rev-parse --show-prefix` gives the prefix.
- **Drives**: `C:\` reports NTFS, fixed, size/free via the Win32 calls; empty label falls back to
  "Local Disk".
- **Linux**: `cargo check` and `clippy -D warnings` pass for `x86_64-unknown-linux-gnu` (not run).
- **Real console smoke**: `ks.exe <dir>` stayed alive for 3 s without a panic.

### Measurements (budget: first rows < 150 ms, complete < 1 s for 100k entries)

`cargo test -p ks-app [--release] --test navigation a_directory_of_100k -- --ignored --nocapture`
(100,000 empty files, warm cache, NTFS SSD, real runner): release 10 ms first rows / 83 ms complete,
debug 28 ms / 479 ms. Re-run later in f2: 12.4 / 98 ms and 10.7 / 86 ms (release). External file
creation visible in the list in 110 ms; tracked-file edit to updated git glyph 446 ms.

### Open issues

- Nerd Font code points (Font Awesome / Devicons / Seti ranges) need a look in a real Nerd Font.
- Git runs once per directory change; very large repositories with ignored trees may be slow
  (a repo-wide cache keyed by root is the follow-up; f2 only made repeated requests for the
  directory being scanned coalesce instead of restarting).
- Windows symbolic links (need developer mode or elevation) are not exercised on Windows.
- A test for one unreadable entry in the middle of a listing needs a fault-injection seam.
- Resolved since P1: dead network drives (f2: per-drive threads, 1.5 s deadline, "not responding"
  row), the preview split, `ks_fs::link_target` consumer (f1 link header), hint line (f3).

### Dependencies added in P1

`notify` 8.2.0, `notify-debouncer-full` 0.7.0, `windows` 0.62.2 (Windows only), `tokio` `process`
feature, `dunce`/`tracing`/`tokio-util` in `ks-fs`, `ks-fs` as a dependency of `ks-app`.

---

## P2: Preview

Engine: `crates/ks-preview` ([engine-preview](docs/progress/engine-preview.md)). Wiring:
[p2](docs/progress/p2.md). Later polish: f1, a2, b1.

### Done

- **Engine** (`Previewer`): background workers (3 std threads), request replaces queued/cancels
  running, LRU cache of 64 plus a 96 MiB byte budget keyed by path, mtime, size, attributes and an
  options hash (so a theme change re-highlights without clearing). Kinds: Text (256 KB / 2,000 lines;
  UTF-8, UTF-8 BOM, UTF-16 LE/BE, UTF-32 BOM added in f1; syntect with two-face syntaxes, theme built
  from a caller `Palette`), Markdown (hand-written light renderer), Directory (capped at 20,000),
  Image (decoded, downscaled to <= 2048 px), Archive (zip listing; other archive formats are
  metadata only), Binary (hex of 4 KB + magic hint), Metadata, Empty, Unavailable (TooLarge,
  CloudPlaceholder, PermissionDenied, Locked, NotFound, Io). Corrupt images/zips fall back to hex.
  All text from file content is sanitised (ESC can never reach the terminal). Cloud placeholders are
  never opened.
- **Layout and pane** (`ks-core::preview`, `ks-ui::preview`): optional parent column | list |
  preview divided by `│`. Preview shows from 80 columns (`w` forces it either way and the choice
  survives resizes); list gets `[layout] list_percent` (40); a pane narrower than 20 is dropped
  (preview first); parent column (`P`) needs 100+ columns. Header (name, syntax / WxH / entries, size,
  mtime; least important segments dropped), rule, content: text with line numbers, markdown, directory
  (icons, dimmed), zip listing, hex (16 bytes per row from 76 columns), metadata view (`i`, exact
  bytes, local-time timestamps, attributes, link target, owner), unavailable reason, empty, images.
- **Wiring**: `PreviewState` in `AppState.preview`; after every `update`, `preview::sync` compares
  what the pane should show with what was requested and, on a difference, bumps the generation and
  emits one request, so every way of moving the cursor is covered without touching `nav.rs`. Stale
  results are dropped by generation. `J`/`K` scroll half a page, clamped. Two lazily created
  `Previewer`s (main pane, parent column).
- **Theme syntax tokens**: optional `[syntax]` table in a theme (`keyword string comment function
  type number constant operator punctuation`); unset keys derive from existing tokens; a user theme
  inherits its base's explicit colours. A theme switch re-requests the current file.
- **Images** (`ks-ui::images`): `ratatui-image` 11.1.0 without libchafa. `[preview] image_protocol =
  auto | halfblocks | sixel | kitty | iterm2`. Resize/encode on worker threads (`ThreadProtocol`);
  when graphics leave the screen the loop clears the terminal and redraws (Sixel pixels are not
  erased by overwriting cells).
- **Polish (f1)**: link target in the header (`name → target`, error colour when broken); owner in
  the `i` view via an injected `OwnerResolver` (`ks_fs::file_owner`, `GetNamedSecurityInfoW` +
  `LookupAccountSidW`, 1.5 s helper-thread timeout); preview invalidation after file jobs
  (`Effect::InvalidatePreview`); UTF-32; the terminal graphics query moved off the start-up path
  (deferred until after the first frame on a `ks-terminal-query` thread, so a silent terminal delays
  input by at most 250 ms once but never the UI); image clone and protocol construction off the UI
  thread.
- **Audit/round-2 fixes**: truncated flag relative to the text limit (a2), hex wide threshold 76 (a2),
  image drawn once under overlays (a2), markdown linear-time inline scan (a2), a true windowed read
  for content-search hits beyond the head limit (`focus_line`, 1,000-line buckets; b1).
- Tests: `ks-core::preview` (15), `ks-ui::preview_tests` (21 tests, 46 snapshot files at 80x24 and
  160x48 in both themes), `ks-app/tests/preview.rs` (real engine and files), engine unit and
  integration tests including cancellation, stale generations, escape sanitisation, timing.

### Deviations

1. **`ks-core` depends on `ks-preview`** (`Msg` carries `Arc<Preview>`); `ks-preview` stays
   terminal-agnostic and independent of core. `PreviewMsg` has a manual `PartialEq`.
2. `Gen::raw()/from_raw()` added to carry generations through the engine (`u64`).
3. **Preview state is global to `AppState`**, not per tab; it follows the active tab's cursor.
4. **The syntax palette lives in the effect runner**, not in `Effect` (which derives `Eq`);
   `set_preview_palette` is called on every theme change.
5. **`[syntax]` is a separate optional theme table**, not part of the 22 required tokens.
6. **No request debounce** (the engine replaces queued requests; hand-off costs microseconds) and no
   "loading…" flash on cache hits (later a `loading…` line while a result is pending).
7. Parent column and directory preview use the engine's directory listing (dirs first, capped at 500
   entries, no git glyphs or child counts) rather than the list's row builder.
8. `J`/`K` scroll half a page; the `i` view uses the UI's local-time formatting.
9. Engine: no `ThemeSet` (theme built from the palette); markdown is hand-written (no
   pulldown-cmark): tables, nested lists, HTML blocks and reference links show as plain text;
   `ratatui-image` is a `ks-ui` dependency, not an engine one; docx/xlsx/pptx/odt/epub/jar list as
   ZIP; `svg` is text; PowerShell files use the Bash grammar (neither syntax set has PowerShell).
10. The extra `‹w› preview ‹P› columns ‹i› info` hint was dropped at integration (it truncated every
    other snapshot); the keys stay discoverable through help, palette and which-key.

### VERIFY results

- **syntect vs two-face**: syntect defaults carry 75 syntaxes, two-face 213 (missing in syntect: toml,
  ts, tsx, ps1, kotlin, swift, zig, dockerfile and more). Decision: two-face with the pure-Rust
  `syntect-fancy` backend. Syntax set load 1.5 to 3.7 ms in release.
- **ratatui-image**: 11.1.0 is the latest stable and matches ratatui 0.30; the default `chafa-dyn`
  feature needs libchafa, so it is disabled. `Picker::from_query_stdio_with_options` with a 250 ms
  timeout; picks kitty > sixel > iTerm2 > halfblocks.
- **Windows Terminal Sixel**: installed WT is 1.24.12741.0 (Sixel since 1.22; no kitty or iTerm2
  protocol), so Sixel is expected. **Not visually verified.** In a plain conhost window the query
  produced halfblocks in 0 ms and the app ran normally. Workaround if detection picks halfblocks:
  `[preview] image_protocol = "sixel"`.
- `ThreadProtocol` needs the full pane area every frame or it re-encodes every frame (found with a
  failing test, handled).
- Cloud placeholders are only exercised through injected attribute bits and the `unavailable_*`
  snapshots, not a real OneDrive file.

### Measurements

Highlight 14,839 lines (256 KB Rust) 670 ms; default cap (2,000 lines) 83 ms (release; debug is about
8x slower and a 1.5 s highlight budget keeps debug builds from stalling). Keypress to frame
(`update` + render at 160x48, 1000 entries, 38-line highlighted preview): release mean 0.23 ms, p99
0.31 ms; debug mean 2.3 ms. Holding `j` through 60 files of 240 KB: 59 presses processed in 53 µs
(release); cache hit delivered 0.1 to 0.9 ms after the request. Cold-start headless: 0.97 ms
(release) / 2.3 ms (debug).

### Open issues

- Real Windows Terminal image detection, Sixel clear-on-leave and clear when an overlay closes are
  unverified (manual).
- Directory preview has no git glyphs or child counts (the git snapshot only covers the current
  directory; per-move `git status` is too costly). Header shows the item count.
- Windowed content-search previews cover plain UTF-8 only; markdown and UTF-16/32 hits beyond the head
  limit keep the head preview with a "beyond the preview limit" status. The windowed read scans
  sequentially to the window start (no line index; cancellable, off thread); highlighting starts
  mid-file without parser state.
- Engine: image decode and `ZipArchive::new` are not interruptible (a stale decode occupies one of 3
  workers until it finishes); one pathological in-limit line can run long in fancy-regex (the 1.5 s
  budget is checked between lines); BOM-less UTF-16 detection only handles mostly-ASCII content;
  peak transient memory can reach about 3 x 200 MB with three max-size images decoding at once; no
  horizontal scrolling (lines are clipped).
- Debug builds highlight slowly; `[profile.dev.package."*"] opt-level = 3` was recommended and not
  applied.
- The Claude changes view keeps its own inline diff instead of feeding the preview pane.

### Dependencies added in P2

`ks-preview`: `syntect` 5.3.0 (default features off, `regex-fancy`), `two-face` 0.5.2, `image` 0.25.10,
`zip` 9.0.1, `lru` 0.18.5. `ks-ui`: `ratatui-image` 11.1.0, `image`, `ks-preview`.

---

## P3 + P4: Filter, find file, content search

Engine: `crates/ks-search` ([engine-search](docs/progress/engine-search.md)). Wiring:
[p34](docs/progress/p34.md). Later: f2, a2, b1, b2.

### Done

- **Engine**: `walk` (parallel `ignore` walker, hidden/gitignore toggles, no symlink/junction
  following, skip list, quiet access-denied counting, cancellable), `scope` (cwd, git root, home,
  drive), `finder` (`FinderSession` over `nucleo`, streaming injection, match indices), `cache`
  (`WalkCache`: last 4 roots, incremental `apply_changes`), `filter`, `grep` (walker -> bounded
  channel -> workers -> 30 ms batches, smart case, literal/regex/whole word/glob, binary detection,
  size limit, cloud skip, 10k cap, cancellation, generations), `debounce` (120 ms), `testgen`.
- **Filter (`/`)**: `DirView` holds the query and highlights; rows are the nucleo matches, best
  first, recomputed on every keystroke, chunk and watcher refresh. `enter` keeps the filter (chip on
  the prompt), `esc` clears it.
- **Find file (`ctrl-p` / `f`)**: centred overlay per PLAN 4.3 with counter and spinner, ranked rows
  with accent matches, marks, footer. `ctrl-r` cycles cwd -> git root -> home -> drive (skipping
  scopes that resolve to the same root, f2), `alt-.` hidden, `alt-g` gitignored; `tab` marks, `enter`
  navigates (all marks in one directory: cursor on the first, all marks applied; several directories:
  a results list), `ctrl-enter`/`ctrl-o` open. A worker thread (`FinderWorker`) ticks the session and
  posts throttled snapshots. The shared walk cache refreshes incrementally from watcher diffs, file
  jobs and Claude hook events (`PathChange::Touched`), and reopening a cached root also re-walks in
  the background (f2).
- **Content search (`ctrl-f` / `s`)**: query + glob fields, chips, `ctrl-r` regex, `ctrl-w` word,
  120 ms debounce with cancel-on-change and stale-generation drop, results grouped by file with match
  highlighting, virtualised scrolling, footer counters (matches / files / searched / skipped cloud /
  denied / binary / large), `10,000+ matches: refine your search`. `enter` opens in the editor at the
  line; `ctrl-l` opens the hits as a results list, which is its own tab.
- **Live preview in both overlays** at width >= 100 (P2 integration): `preview_target` drives the
  preview pane; a grep hit scrolls the file so its line sits a third down.
- **Later additions**: grep marks and send-to-Claude (`alt-m`, `alt-c`, f1); results-list size/mtime
  stats (f2); denied counter in the finder footer (b2); `config [files] show_hidden/show_ignored` seed
  the finder toggles (b2); hidden find-file targets unhide via the listing instead of a synchronous
  `stat` in `update` (b1); filter vs visual mode fixed (a2); brace-alternation globs, multi-line regex
  errors, grep panic safety (a2); case-only rename in the finder re-walk (b2); grapheme-aware
  highlighting (b2).

### Deviations

1. **`ks-core` depends on `ks-search`** (the filter runs synchronously in `update`; engine types
   travel in `Msg`/`Effect`).
2. **The finder is driven by a worker thread, not a 16 to 33 ms timer `Msg`** (render is never
   blocked; no work while idle). Scope resolution (`find_git_root`) happens on that thread.
3. Filtered rows are ordered by match score (dirs-first is not kept while filtering).
4. `ctrl-enter`/`ctrl-o` open, `tab` marks (not `space`, which is text; fzf convention, it also moves
   down).
5. P6 owns opening: the grep hit's `enter` emits `Effect::OpenInEditor { path, line }`, turned into a
   detached GUI editor or a suspend/resume foreground run for terminal editors.
6. **Results list opens in a new tab** (`nav::show_results_list` -> `tabs::open_in_new_tab`), with `esc`
   returning to the real listing of the root; at the 9-tab limit it replaces the pane.
7. Engine: ripgrep binary semantics (matches before the first NUL kept); gitignore applied outside git
   repositories; no memory mapping (`unsafe` forbidden); `.git` always skipped; cloud-placeholder
   directories skipped only with `skip_cloud_dirs` (default off), placeholder files always skipped by
   grep; dependencies declared crate-locally.
8. The Normal-mode hint additions (`/ filter`, `ctrl-p find`, `ctrl-f search`) were dropped at
   integration for snapshot churn and came back in f3's width-aware hint line.
9. **Grep root is always the cwd**: PLAN 6.3 gives `ctrl-r` to the regex toggle, so there is no scope
   cycle (recorded in f2).

### VERIFY results

- nucleo 0.5.0 / nucleo-matcher 0.3.1 and grep-* 0.1.x API details confirmed (`match_paths()` uses
  `/` and `\` delimiters on Windows; `append` only valid when the old query is a prefix; scores not
  exposed; highlight indices unsorted, deduped by the engine).
- `ignore` `WalkParallel` has no external cancel handle: visitors check the token and return
  `Quit`. `OverrideBuilder` semantics confirmed (`*.rs` whitelists, `!target/**` excludes).
- Reopening the finder on the same root is served from the cache; 13 keystrokes 10 ms apart start
  exactly one search.
- `ctrl-enter` arrives as `Enter + CONTROL` through crossterm's console backend; **not tested in a
  real Windows Terminal** (`ctrl-o` is the dependable key).

### Measurements (budgets in P8)

Finder keystroke on 500k paths: criterion worst 14.2 ms (`h`), ks-app worker test 15.8 to 19.3 ms
worst, ks-search release guard 21.9 to 24 ms worst under load (budget 25 ms). Walk of 200k files
58 ms. Grep over 2.06 GB: 1.13 to 1.21 s (1.6 to 1.7 GiB/s). Content search vs `rg` and memory at 1M
paths: see P8.

### Open issues

- `ctrl-enter` delivery and `alt-.` / `alt-g` / `alt-m` / `alt-c` chords in Windows Terminal are
  unverified (rebind through `[keymap.finder]` / `[keymap.grep]` if a terminal eats them).
- Finder: result tie order is discovery order (nondeterministic during a parallel walk); grep file
  order is nondeterministic until `Done` (sorted by path then); nucleo cannot remove items, so a
  refresh that finds removals rebuilds the matcher (one feed on a helper thread); `scan_new_dir` is
  capped at 200k entries and not cancellable; a cursor inside a grapheme cluster in an input field
  highlights the whole cluster, and `input_spans` can overflow by a few cells for a very wide cluster.
- `OpenInEditor` from the grep overlay leaves the overlay open under a terminal editor.
- File operations in a results tab prune or rename rows in place (a1) but skip cursor reveal and git.
- The filter highlight is skipped for names shortened with `…`.

### Dependencies added

`ks-search`: `ignore` 0.4.33, `nucleo` 0.5.0, `grep-searcher` 0.1.17, `grep-regex` 0.1.14,
`grep-matcher` 0.1.9, `regex-syntax`, `crossbeam-channel`, `parking_lot`; dev `criterion` 0.8.2.
`ks-core`: `nucleo-matcher` 0.3.1 (P5), `ks-search`. `ks-ui`: `unicode-segmentation` (b2).

---

## P5: Command palette, help, which-key, keymap config, themes

Notes: [p5](docs/progress/p5.md).

### Done

- **Command registry** (`ks-core/src/command.rs`): a `COMMANDS` table (category + description per
  action); a test fails when an action has no row. New palette-only actions: `theme.switch`,
  `icons.switch`; `config.reload` has no default key.
- **Palette** (`:`): centred rounded box, nucleo-matcher over titles (falls back to the stable id),
  bindings right-aligned, recents first, `Switch to tab` expanded to nine entries, `Set/Jump to
  bookmark` arm the `m`/`'` prefix, theme and icon pages (esc goes back one page first). Palette
  entries are filtered through `claude::palette_visible`.
- **Help** (`?`): generated from registry + live keymap, grouped by category, filterable, shows every
  command (unbound as `—`); f3 added contextual bindings (`ctrl-r (finder)`) and context sub headings.
- **Which-key**: boxed popup after `ui.which_key_delay_ms`; falls back to the hint line below a
  3-row body; no longer armed while an overlay is on top (f3).
- **Keymap config**: palette/help default bindings; override reporting (shadowed bindings, unbinding
  something unbound); `settings::build_keymap` shared by start-up and reload.
- **`docs/keymap.md`** generated by `ks-core/src/docs.rs` with a drift test
  (`docs::tests::docs_keymap_md_is_up_to_date`); regenerate with `KESTREL_UPDATE_DOCS=1 cargo test -p
  ks-core docs` or `ks --dump-keymap`. This is the only docs-related test and it checks
  `docs/keymap.md` (not PROGRESS.md).
- **Themes / reload**: `Effect::ReloadConfig` / `SetTheme`; `ks-app/src/reload.rs` does the file work
  off the UI thread; a broken `config.toml` keeps the running settings; a broken theme keeps the old
  one; `ThemeLoader::available()` lists built-ins then user themes.
- **Hint line (f3)**: width-aware priority (`: commands`, `? help`, `q quit` never dropped; optional
  hints give way lowest priority first; logic in `ks-ui/src/hints.rs`). At 80 columns:
  `h parent  l open  / filter  : commands  ? help  q quit`; `space mark`, `. hidden`, `g / drives` fit
  from about 160 columns.
- Tests: update()-level tests, event-loop tests (palette -> reload config, switch theme, broken
  theme), insta snapshots for palette, help and which-key at 80x24 and 160x48 in both themes.

### Deviations

1. Theme/icon choice is session-only (not written to `config.toml`).
2. Config/theme file reads run on `spawn_blocking`; results return through a private side channel
   (`Side::Msg` / `Side::Theme`) because `Msg` cannot carry a `Theme`.
3. `event_loop::run_with` was added instead of a new `LoopOptions` field so other tracks' literals
   kept compiling; every `runner.run(update(..))` site goes through `dispatch`.
4. `Keymap` derives `PartialEq, Eq`; which-key shows `any key` for `<char>` continuations.
5. Palette sort entries are the four `sort.*` actions; bookmarks use prefix arming.
6. Help and the palette's binding column cover the `normal` context; since f3 commands without a
   Normal binding show their own-context bindings.

### VERIFY results

- `indexed_256_colours_never_use_rgb` renders the palette with an Indexed256 theme and asserts no
  `Color::Rgb`.
- A wide character at the left edge of a popup swallowed the border in a TestBackend dump;
  `popup::framed` now blanks such a cell first.

### Integration

Merged with P7 first (event loop: P5's `dispatch`/`Side` kept as the only path to `update`; `bookmark.list`
and `bookmark.delete` got registry rows after the registry test caught them). Later merges followed
the same rule: every new `Action` needs a `COMMANDS` row, every new `Effect` an arm in
`EffectRunner::run`, and palette/help snapshots plus `docs/keymap.md` are regenerated, not hand-merged.

### Open issues

- Palette queries are literal fuzzy words (nucleo's `! ^ $ '` operators are not interpreted); the
  palette hint line is generic; `Reload config` discards session theme/icon picks by design.
- Bookmark commands from the palette need the `m`/`'` binding to exist.
- The bookmark list is on `M`, which is not in PLAN section 5 (a `g b` binding collided with another
  track's which-key snapshot).
- The README key tables are prose and have no drift test (only `docs/keymap.md` and the example
  config/theme are test-checked).
- Feel of palette, help and which-key in a real Windows Terminal is unchecked (manual).

### Dependencies added

`nucleo-matcher` 0.3.1 in `ks-core`.

---

## P6: File operations

Engine: `ks-fs` `ops/` and `launch/` ([engine-ops](docs/progress/engine-ops.md)). Wiring:
[p6](docs/progress/p6.md). Later: f3, a1, b1.

### Done

- **Engine**: `JobManager` (a thread per job, 2 concurrent, FIFO), kinds Copy / Move / Trash /
  DeletePermanent / Create / Rename / BulkRename / Restore; progress with 3 s windowed throughput and
  ETA; cancellation between 4 MiB chunks and entries; files copy to `.ks-<job>-<n>.part` and rename
  into place; conflict prompts (overwrite / skip / rename / keep both, apply to all) and
  non-interactive policies; links never followed; cross-device move = copy + delete; Recycle Bin via
  `trash` with ids found by diffing `os_limited::list()`; permanent delete with typed token;
  validated create input; bulk rename planning (duplicates, collisions, cycles via temp names);
  session undo for rename, move, create, trash, copy, bulk rename with size/mtime fingerprints;
  long-path and Unicode handling. `launch/`: clipboard formats, open-default, open-with, editor
  resolution and templates, shell runner with streaming and `taskkill /T /F` cancel.
- **Wiring**: `y x p d D u r R a Y o O e !` and `g j` as listed in the keymap. Register = marks else
  cursor entry; `p` into the cwd (same-directory pastes use keep-both); trash without a modal (toast
  with `u to undo`); typed `yes`/count confirmation for `D`; undo with errors to the status line;
  rename prompt with the cursor before the extension; `R` bulk rename in `$EDITOR` with preview and
  confirm (invalid plans are listed and cannot be applied); create with trailing `/` for directories;
  `Y` menu (absolute / relative / name); `!` pager overlay streaming stdout/stderr (20,000 line cap,
  `ctrl-c` kills the tree); Jobs overlay (`g j`, `x` cancel, `c` clear finished); conflict overlay
  (`O` overwrite since a1, `s` skip, `r` rename, `k` keep both, `a` apply to all, `enter` = keep
  both, `esc` = cancel job); progress in the prompt line.
- **Terminal suspend/resume**: `Effect::RunForeground` handled by the event loop through the
  `Foreground` trait; the crossterm `EventStream` is dropped so the child gets console input; full
  redraw and resync of the terminal size afterwards (a1). Terminal vs GUI editors by a known list +
  `[editor] terminal = auto|always|never`; GUI editors spawn detached (VS Code family gets `--wait`
  for bulk rename).
- **Later work**: cut entries dimmed and flagged `×` (f3); `O` honours marks and reads the registry
  `OpenWithList` / ProgIds (f3, mixed extensions offer common handlers only); bulk-rename re-edit keeps
  the temp file on an invalid plan (`enter` reopens, `esc`/`n`/interrupt/quit/delete discard it; f3,
  b1); `o` and `e` act on every mark (cap 8); `jobs.clear`; quit with running jobs or a running Claude
  session asks first; clipboard injectable; a cut register is consumed only for paths the move
  actually consumed (a1 `consumed_sources`); non-destructive overwrite via a hidden `.ks-old-*` sibling,
  with a failed overwrite restoring the old destination (a1).
- Cross-volume moves use `CopyFileExW` (alternate data streams, attributes, creation time preserved;
  progress and cancel via callback; a1).

### Deviations

1. Engine events are mirrored in `ks-core` (`JobMsg`, `JobDone`, ...) because `ks-fs` depends on core
   and `Msg` must be `Eq`.
2. Name validation and typed-confirm tokens run in the runner / are duplicated in core (three lines).
3. Prompt and dialog hot keys are hard-wired via `fileops::intercept_key` (enter/esc/scrolling still go
   through rebindable contexts), to avoid polluting the palette with one action per dialog key.
4. **Esc in the conflict dialog cancels the job**, enter keeps both (never a destructive default).
   The overwrite key is `O` (shift) and the dialog swallows keys for 600 ms after it appears (a1).
5. Pager leaves the header and prompt/hint rows visible; engine events use an unbounded channel
   because `submit`/`cancel` call the sink on the caller's thread.
6. Engine: `JobKind::Restore` added for undo; `Done` = no item errors, `Failed` = at least one;
   directory onto existing directory merges silently (conflicts asked per file); copies that overwrite
   are not recorded for undo; chunked copy instead of `std::fs::copy` (needed for cancellation; since
   a1 Windows uses `CopyFileExW`); custom quote-only shell tokenizer (shlex would corrupt `\` paths).
7. `Msg::JobStarted` / `JobFinished` (P0 placeholders) are still in `msg.rs` and unused by file jobs.

### VERIFY results

- **Trash restore via `trash::os_limited`: works** (non-ASCII directory and file restored with
  contents). Limitation: the shell restore API rejects originals longer than MAX_PATH
  (`E_INVALIDARG`); the undo fails cleanly and the item stays in the bin.
- Cross-volume move verified for real (C: to E:). `is_symlink()` is true for junctions; copy, delete
  and move never touch a junction target.
- Bulk rename end to end with a `.cmd` fake editor (preview, confirm, one-step undo); 200 MB copy
  through the runner (monotonic progress, no temp files); cancel of a 512 MiB copy leaves no target or
  `.part`; `!` streams and `ctrl-c` cancels in well under 10 s.
- Terminal editor suspend/resume is covered only by a fake `Foreground` host.

### Measurements

1 GiB copy (release, page-cache to page-cache): 0.30 s, about 3.4 GiB/s; 200 MB cancel in well under
100 ms. The PLAN acceptance example (10 GB copy with progress and cancel) was exercised at 200 MB and
512 MiB only.

### Integration

Event loop: P6's `apply` folded into `dispatch`; `Effect::RunForeground` handled there after flushing
earlier effects. `fileops` state is app-global in the tab guard (marks per tab, registers global). Key
routing order: help filter, `intercept_key` (acts only on its own overlays), keymap context; in
`run_action`: palette/help, search, `fileops::handle_action`, rest. `EffectRunner::configure` is called
after `ConfigReloaded` so `[editor]` changes apply.

### Open issues

- Real suspend/resume of nvim / hx in Windows Terminal for `e` and `R` (manual).
- Trash undo of originals over MAX_PATH fails (shell API); undo pressed while the trash job is still
  running is a silent no-op (f1 note, not re-verified in d1).
- Directory timestamps and ACLs are not preserved on copy; hard links are copied as independent files;
  recreating symlinks needs developer mode or elevation (else "link skipped").
- Bulk rename excludes names that are not valid Unicode (with a warning); GUI editors need a
  blocking flag (`code -w`), the VS Code family is handled automatically; a kept edit file is only
  leaked if the process is hard-killed.
- Conflict-dialog arming is timer-based, not tied to the render (a 600 ms quiet window; a draw-ack
  message would be needed).
- Trash batching (`trash::delete_all`) deferred; the bin is listed twice per Trash job (cost scales
  with the bin size; `JobOptions::track_trash_ids` can switch it off).
- Shell runs are not in the Jobs overlay; `is_terminal_editor` is a heuristic list.
- The real-Recycle-Bin test `trash_undo` runs in the default gate (a1 re-enabled it after f3 had
  ignored it) and depends on a usable Recycle Bin; the `ks-fs` bin tests skip with a `SKIP:` line when
  a probe (`recycle_bin_usable`) finds none.

### Dependencies added

`ks-fs`: `arboard` 3.6.1, `open` 5.4.4, `trash` 5.2.9, `winreg` 0.56 (f3, Windows only), `tempfile`
(normal dependency for the bulk-rename file).

---

## P7: Tabs, bookmarks, frecency, shell integration

Notes: [p7](docs/progress/p7.md). Later: f2, a3, b4.

### Done

- **Tabs** (`t`, `1`..`9`, `ctrl-w`, max 9): the active tab's state stays in the existing `AppState`
  fields and parked tabs are `TabSlot`s swapped in and out, so no existing code needed accessors.
  Background results of a parked tab are dropped through per-tab generation spaces. An exhaustive
  destructure of `AppState` (`classify_state_fields`) breaks the build until a new field is classified
  per-tab or shared. Tab strip `1 ▏2 ▏3` in the header, hidden with one tab. **All tabs are watched**
  (f2): parked directories get a watcher set, a changed tab is marked stale (dot / `*`) and re-listed
  in the background; two parked tabs on one directory share one listing (b1).
- **Bookmarks** (`m<char>` / `'<char>`, list overlay on `M`) in `bookmarks.toml`; **frecency** (`z`
  overlay, zoxide model with buckets and aging, capped at 5,000 entries; ranking = frecency x nucleo
  fuzzy score) in `frecency.toml`.
- **Persistence**: atomic writes (temp + rename) through one writer thread; loading off-thread and
  merged with what happened meanwhile; nothing is written before the load has merged; damaged files
  are renamed `.bad`. Since a3, concurrent instances **merge on write** under a lock file (three-way
  merge: own sets/deletes win, foreign additions survive, ranks add); b4 hardened temp names
  (`.{name}.{pid}.{seq}.tmp`), lock wait 8 s / stale 30 s.
- **Shell integration**: `ks --print-shell-init <powershell|pwsh|bash|zsh>` prints the `k` function
  (`Get-Content -LiteralPath -Encoding UTF8 -Raw`, ignores empty or vanished targets, removes the
  temp file in `finally`; bash/zsh convert with `cygpath -u`). README documents the profile line and a
  Windows Terminal profile command line. `Q` writes the active tab's directory.

### Deviations

1. History is not persisted separately (frecency is the persistent record; back/forward stays
   session-local).
2. `M` for the bookmark list, no `g b` (it cut off the `g` which-key group in a snapshot).
3. No `active()` accessor or `TabState` wrapper (swap design).
4. The jump overlay has no live preview pane (PLAN 4.3).
5. Frecency matches the displayed `~`-abbreviated text so highlights line up.
6. A new tab shows an empty list for one frame until the first chunk arrives.
7. The earlier "only the active tab is watched" deviation was overtaken by f2.

### VERIFY results

- **Windows PowerShell 5.1 `Get-Content`** misreads UTF-8 without BOM unless `-Encoding UTF8`
  (confirmed with `C:\Zażółć é`, 5.1.26100); the function always passes it.
- **End-to-end `Q` + function**: `tests/shell_init.rs` runs the printed function in a real
  `powershell.exe` 5.1 with a stand-in `ks.ps1` (directory with space and non-ASCII characters;
  empty file or vanished directory stays put; no temp file left). `pwsh.exe` is not installed here, so
  that run is skipped. The real TUI cannot run in tests.
- `std::fs::rename` over an existing file works on Windows (atomic replace).
- nucleo-matcher `Pattern::indices` are char offsets, used directly for highlighting.

### Measurements

`z` with 5,000 entries, whole `update` incl. ranking: open 2.3 ms, 2.7 ms per keystroke (release);
debug 9.9 / 23 ms. Ranking runs on the UI thread; move to a job if the cap is raised.

### Open issues

- A running instance does not pick up another instance's bookmark/frecency writes until restart (the
  file itself is always merged correctly). The stale-lock break has a theoretical race after a crashed
  holder, and the lock fall-through after 8 s can lose a merge (best effort by design).
- Orphan `.*.tmp` files in the state directory are not swept at start-up.
- The `z` overlay has no live preview at width >= 100.
- Real keyboard use of `t`/`1`/`z` in Windows Terminal is unexercised.

### Dependencies added

`nucleo-matcher` in `ks-core` (shared with P5); nothing else.

---

## P8: Polish, performance, docs, release

PLAN scope: profile against the section 11 budgets, fix regressions, README with GIFs, a Windows
Terminal profile snippet, `cargo install` / release `.exe`. Notes: [f1](docs/progress/f1.md),
[f2](docs/progress/f2.md), [f4](docs/progress/f4.md), [a3](docs/progress/a3.md),
[b3](docs/progress/b3.md).

### Done

- **README rewrite** (it still said phase 1): features with real default keys, the annotated config,
  key-binding rules, theme format, shell init for pwsh / PowerShell 5.1 / bash, Windows Terminal
  profile JSON, icons, terminal graphics, Claude integration (subscription only, strip list,
  preflight, per-spawn config), file locations, logs, troubleshooting, development, License section.
- **`docs/manual-test.md`**: a ten-section Windows Terminal checklist corrected against the code.
- **`docs/config.example.toml`** and **`docs/theme.example.toml`**, each checked by a test
  (`config::tests::the_documented_example_config_is_valid`,
  `theme::tests::the_documented_example_theme_is_valid`).
- **CI** (`ci.yml`: fmt, clippy `-D warnings`, `cargo test --workspace --no-fail-fast`, release build,
  `ks.exe` artifact; Windows and Ubuntu; apt packages for arboard; a Windows step running the
  keypress latency test in release; a Windows step failing if the release exe imports vcruntime or
  `api-ms-win-crt` DLLs; 45 min timeout) and **`release.yml`** (tag `v*`: gated on fmt, clippy and
  tests with `--locked`, zips `ks.exe` + README + keymap + examples + SHA-256, creates a GitHub
  release). `LICENSE` (MIT) added in a3.
- **Static CRT** (`.cargo/config.toml` `+crt-static` for x86_64 and aarch64 MSVC; a byte scan of the
  release exe shows no vcruntime or `api-ms-win-crt` imports; `ks-app/tests/static_crt.rs` guards the
  setting).
- **Performance work**: finder memory (shared `Arc<str>` per path between cache and nucleo; 301 ->
  204 MB), deferred terminal query and off-thread image preparation (cold start), drive-query timeout,
  git status coalescing, `grepbench` example, ignored guards for finder latency and memory.
- **Test hardening**: round 2 removed the sleeps and shared-temp-dir assumptions behind most flaky
  tests (b4, a2, a3); see [Audit and fix rounds](#audit-and-fix-rounds).

### Deviations

1. The README GIFs/screenshots are placeholders (paths listed; the user records them).
2. The spec's `[theme]` / `[syntax]` tables in `config.toml` do not exist: `Config` denies unknown
   fields, `theme` is a name, `[colors]` / `[syntax]` live in theme files. README documents this.
3. No `rust-version` is declared: Rust 1.99 is the tested toolchain, no older minimum verified.
4. Windows 10 "1809 or newer" is an unverified reading of ConPTY's requirement.
5. Manual-test corrections found while writing it: the preview IS shown at 80x24; the size column
   hides on a list pane under 30 cells or a terminal under 60 columns (b2 rule); find-file marks with
   `tab`; the WT split always runs the literal `claude` (`[claude] binary` affects the embedded pane
   and preflight only).

### VERIFY results

- Release exe import scan: only advapi32, api-ms-win-core-synch, bcrypt, combase, conpty, dbghelp,
  kernel32, ntdll, ole32, oleaut32, propsys, shell32, user32, userenv, ws2_32 (b3).
- Final pass (2026-10-11, release build of `5205290`): `dumpbin /dependents` lists kernel32, advapi32,
  api-ms-win-core-synch-l1-2-0, shell32, combase, ole32, oleaut32, ntdll, userenv, propsys,
  bcryptprimitives, ws2_32, user32 and bcrypt; no vcruntime, msvcp, ucrtbase or api-ms-win-crt import
  (the b3 byte scan above also named dbghelp and conpty, which are not in the import table; they are
  presumably runtime-loaded names).
  Size 15,449,088 bytes. Budget measurements: see the table below.
- `ks --help`, `--version`, `--print-shell-init pwsh` run and match the README. Both workflow files
  parse as YAML but **have never run on GitHub**; the Windows CI job failed once (logs unavailable, a3)
  and was hardened blind.
- Linux: `cargo check`/`clippy -D warnings` clean for `x86_64-unknown-linux-gnu` (with clang for
  criterion's `alloca`); Linux tests were never executed, and the `ks-search` test and bench targets
  cannot be compiled from Windows without a Linux C compiler.

### Measurements

Final headless pass taken **2026-10-11 on a release build of `5205290`** (`cargo build --release -p ks-app`,
`cargo test --release` for the ignored perf tests, `cargo bench -p ks-search`). Budgets are PLAN
sections 11 and 12 (P8). Every metric was run at least 3 times (n is given); "median / worst" are over
those runs. Nothing else ran on the machine and no cargo job overlapped a measurement.

Machine: AMD Ryzen 9 9950X3D (16 cores, 32 threads), Windows 11 Pro 10.0.26200, rustc 1.99.0, release
profile `lto = "thin"`, `codegen-units = 1`, `strip = true`, `+crt-static`. Microsoft Defender real-time
protection was **on** (it matters for process start, see the first row). Disks (from `Get-PhysicalDisk`):
**E:** Samsung 990 EVO Plus 4 TB, NVMe SSD; **C:** Samsung 980 500 GB, NVMe SSD; **D:** WDC WD62PURZ, SATA
**HDD**. The performance corpora that touch the disk were generated on **E: (NVMe)** under
`target\bench-corpus` (100k-file directory, 200k and 20k file grep trees, the 300-file start-up
directory, the watcher test and the benchmark scratch dirs); the `~\.cargo\registry\src` grep corpus is
an existing tree on **C: (NVMe)**. `TEMP` was on D: (HDD) only for runs whose timed part does not
touch the disk (key-to-frame, finder and memory guards use synthetic in-memory data; the headless
cold-start fixture is 300 tiny files). All corpora created for this pass were deleted afterwards.
Files were created just before each run, so every directory/file number is a **warm cache** number;
a cold cache (after a reboot) was not measured.

| Metric | Budget | Result (median / worst, n) | Met? | Method and notes |
|---|---|---|---|---|
| Cold start to first frame, **in-process** | < 60 ms | `first frame elapsed_ms` log line: **5 ms / 6 ms** (n=10 real `ks.exe` launches); headless test (`cold_start`): **634 µs / 645 µs** (n=5, each a best-of-5) | yes (this part) | The log line counts from `startup::mark_process_start()` (first thing in `run`, after clap) to the first drawn frame, with the real raw mode, alternate screen and ConPTY terminal. The test builds the session and runs the loop to the first-frame hook on a `TestBackend`. |
| Cold start, **end to end** (process creation to a visible listing) | < 60 ms | `ks.exe` launched in a hidden ConPTY (Win32 `CreatePseudoConsole` harness, 120x32, 300-file dir): first bytes **73 ms / 250 ms** (the 250 ms is the first launch after a rebuild, 87 ms otherwise), listing visible **118 ms / 288 ms** (134 ms otherwise); n=10, plus a ConPTY floor of 23 ms for `cmd /c echo` | **not demonstrated; probably not met on this machine** | Real Windows Terminal number stays manual. The bare process is the cost: kernel-measured `ks --version` lifetime is **77 ms median (73 min, 98 max, n=20)** against 29 ms for a minimal static-CRT Rust exe, 30 ms for `cmd /c exit`, 43 ms for `rg --version`. A 13.9 MB dummy exe (random data, no code) costs 72 ms, so the overhead is proportional to image size (about 3 ms per MB, most likely Defender's scan on exec), not to Kestrel's start-up code or DLL imports (a minimal exe importing user32/shell32/ole32/propsys/ws2_32/userenv still takes 29 ms). Subtracting process creation, Kestrel's own work to the first frame is 5 to 6 ms. See open issue 24. |
| Key press to frame | < 16 ms p99 | p50 **214 µs / 216 µs**, p99 **278 µs / 406 µs**, mean 217 µs, max 461 µs (n=5 runs of 200 presses) | yes | `ks-ui` `keypress_to_frame_stays_within_budget`: `update` + full render at 160x48 with a 1000-entry list and a text preview. The test now prints p50 as well. Terminal and ConPTY latency is not included (manual). |
| Enter a directory with 100k entries | first rows < 150 ms, complete < 1 s | first rows **1.1 ms / 1.4 ms**; complete and sorted **61 ms / 67 ms** (n=3) | yes | `navigation::a_directory_of_100k_files_meets_the_budget` on E: (NVMe), 100,000 empty files created seconds before (4.2 to 5.8 s to create; the same step took over 5 min on the D: HDD), warm cache. Earlier: first rows 10.7 to 12.4 ms, complete 86 to 98 ms on a busy machine. Cold cache not measured. |
| Fuzzy finder keystroke to results (500k paths) | < 25 ms | `ks-app` worker path (real worker thread + channel): worst keystroke **12.8 ms / 13.0 ms** (n=5; `h` is the slowest, later keys 1 to 8 ms); `ks-search` guard: **17.1 ms / 19.1 ms** (n=5, 13.8 to 19.1); criterion `h`: **11.8 ms** (11.6 to 12.0), `backspace_to_empty` 0.34 ms | yes | The ks-search guard has the thinnest margin (19.1 of 25 ms); its figure includes `tick` polling and result materialisation on one thread. |
| Content search vs `rg` | within 1.5x | ratio of wall times (ks / rg), medians of 5: **0.52 to 0.80**; worst case `unsafe` on the cargo registry 0.80 (308 ms vs 386 ms) | yes | `grepbench` example vs `rg 15.2.0 -c -S [-F]` (built into `target\tools`), alternating runs, whole-process wall time of both. Identical match counts in all 9 cases. Table below. Engine time alone on the 200k-file, 2058 MB tree: 785 ms for `NEEDLE_MARKER` (about 2.4 GiB/s), 1314 ms for `handler` (7.8M matching lines). |
| Memory with a 1M-path finder cache | < 250 MB | peak working set **206 MB / 210 MB** (204, 204, 206, 209, 210; n=5) | yes | `ks-search` `memory` guard: the `mem` example holds a 1M-path cache plus one open session (147k matches), the test reads `PeakWorkingSet64` from outside. It measures the finder, not the whole app; a real `ks.exe` idle in a 300-file directory peaks at 20 to 21 MB (n=10), so adding the two (an estimate, the sum was not measured in one process) gives about 227 MB, still under the budget. |
| External change appears in the list (PLAN P1: about 200 ms) | ~200 ms | create to visible: **171 ms / 186 ms** (n=7; 155 to 186) | yes (thin) | `navigation::external_changes_appear_quickly_and_the_cursor_stays_on_its_entry` on E:, `TestBackend` (no terminal draw). Debounce plus atomic re-list; the test's own assertion allows 2.5 s because of load flakes. |
| `ks.exe` size and C runtime | no vcruntime import | **15,449,088 bytes (14.74 MiB)**; `dumpbin /dependents`: kernel32, advapi32, api-ms-win-core-synch-l1-2-0, shell32, combase, ole32, oleaut32, ntdll, userenv, propsys, bcryptprimitives, ws2_32, user32, bcrypt | yes | No `vcruntime*`, `msvcp*`, `ucrtbase` or `api-ms-win-crt-*` import (`dumpbin /imports` finds 0 lines); `static_crt` test passes. |
| Walk of 200k files (no budget) | n/a | 66 ms (criterion, 66.1 to 66.7) | n/a | `tree_200000_files/walk`. Criterion grep on the same tree: 838 ms rare literal, 838 ms no match, 862 ms common-word regex. |
| 1 GiB copy (no budget) | n/a | 0.20 / 0.21 s, 4.7 to 4.9 GiB/s (n=3) | n/a | `ops_bench` on E:, page cache to page cache. |
| `z` ranking at the 5,000 entry cap (no budget) | frame budget | open 2.4 ms (2.37 to 2.47), 2.75 to 2.84 ms per key (n=3) | n/a | `ks-core` `z_ranking_time`, runs on the UI thread. |
| IPC round trip, pane draw (no budget) | n/a | IPC 102 to 106 µs; `convert_screen` 200x60 241 to 300 µs; frame draw with pane 150 to 154 µs; vt100 1.1 MiB 4.0 to 4.7 ms (n=3) | n/a | `ks-claude` and `ks-app` ignored `perf` tests. |
| `ks hook` process start to exit (no budget) | fast | kernel lifetime median **77.6 ms** (74 to 92, n=15) | n/a | The earlier "36 ms" is not comparable (different method and exe size); it is the same image-size cost as the cold-start row. Runs once per Claude tool call. |
| Full `cargo test --workspace` wall time (information) | n/a | 71 s, 1299 tests (debug, incremental build already warm) | n/a | Gate run of this pass. |

Content search detail (median and worst of 5 alternating runs after one warm-up; `ks wall` and `rg wall` both
include process start; `ks engine` is the time the example reports for the search itself):

| Corpus | Pattern | rg wall (median / worst) | ks wall (median / worst) | ks engine (median / worst) | Ratio | Matching lines (ks = rg) |
|---|---|---|---|---|---|---|
| generated, 20k files, 209 MB (E:) | `NEEDLE_MARKER` | 218 / 249 ms | 129 / 135 ms | 86 / 88 ms | 0.59 | 7,543 |
| same | `handler` | 285 / 323 ms | 195 / 210 ms | 137 / 156 ms | 0.68 | 792,910 |
| same | `needle_marker` (smart case) | 235 / 251 ms | 135 / 143 ms | 87 / 92 ms | 0.57 | 7,543 |
| same | regex `NEEDLE_\w+ line1\d\d\d` | 213 / 217 ms | 139 / 148 ms | 89 / 95 ms | 0.65 | 941 |
| generated, 200k files, 2058 MB (E:) | `NEEDLE_MARKER` | 1646 / 1651 ms | 854 / 865 ms | 785 / 805 ms | 0.52 | 74,490 |
| same | `handler` | 2200 / 2279 ms | 1376 / 1438 ms | 1314 / 1367 ms | 0.63 | 7,800,310 |
| `~\.cargo\registry\src` (C:) | `unsafe` | 386 / 393 ms | 308 / 312 ms | 261 / 261 ms | 0.80 | 682,629 |
| same | `fn main` | 378 / 387 ms | 249 / 262 ms | 185 / 200 ms | 0.66 | 3,267 |
| same | `Windows` | 381 / 399 ms | 225 / 240 ms | 179 / 182 ms | 0.59 | 24,710 |

Not measurable headless (manual, real Windows Terminal; checklist section 10): the real cold start
(process start to a visible first frame in WT), terminal-side keypress latency, and the real
cold-cache directory entry.

Commands (set `TEMP`/`TMP` to a fast disk for the tests that create files; `rg` is installed with
`cargo install ripgrep --root target\tools`, which is gitignored):

```
cargo build --release -p ks-app
cargo test --release -p ks-ui keypress_to_frame_stays_within_budget -- --nocapture
cargo test --release -p ks-app --test cold_start -- --nocapture
cargo test -p ks-app --release --test navigation a_directory_of_100k -- --ignored --nocapture
cargo test -p ks-app --release --test navigation external_changes_appear_quickly -- --nocapture
cargo test -p ks-app --release --test search -- --ignored finder_keystroke --nocapture
cargo test -p ks-search --release --lib -- --ignored finder_keystroke_latency --nocapture
cargo test -p ks-search --release --test memory -- --ignored --nocapture
cargo bench -p ks-search --bench search
cargo run -p ks-search --release --example grepbench -- <root> <pattern> [--regex] [--gen <files>] [--runs <n>]
dumpbin /dependents target\release\ks.exe
```

The ConPTY start-up harness and the process-lifetime probes were throwaway scripts (a C# `CreatePseudoConsole`
wrapper compiled with `Add-Type`, and `Process.ExitTime - StartTime`); they are not in the repository.
Earlier numbers, kept for context (older builds, busy machine): key to frame mean 227 to 234 µs and p99
411 to 458 µs; 100k entries 10.7 to 12.4 ms / 86 to 98 ms; finder worst 14.2 to 24.0 ms; rg ratio 0.43 to
0.67 at 1.6 to 1.7 GiB/s; 1M-path memory 204 to 210 MB (301 MB before the shared-`Arc<str>` change).

### Open issues

- GIFs/screenshots are not recorded; `cargo install --path crates/ks-app --locked` and the release
  zip were not tried on a clean machine; the workflows have not run on GitHub.
- The cold-start and key-to-frame budgets are met only in headless measurements; the 60 ms budget
  needs a real run (manual). The end-to-end cold start is **not demonstrated and probably not met on
  the development machine**: Kestrel's own start-up is 5 to 6 ms, but the OS charges about 77 ms to
  create a process from the 14.7 MB `ks.exe` with Defender on (open issue 24). P8's "all budgets met or
  documented with the reason" is satisfied for this one by the documentation here, not by a pass.
- Debug-profile syntect speed (see P2); MSRV undeclared.
- Documentation drift: the README key tables were
  verified at `5b58d42` and have been spot-checked by later merges (no default key changed except
  the conflict overwrite `O`, which the README already states); no test guards the prose.

---

## P9: Whole-disk search (skipped)

Skipped by the user's decision (see Decisions). Nothing was built: no Everything integration, no
MFT/USN index, no content index. The content-search throughput measured in P8 (1.6 to 1.7 GiB/s,
faster than `rg`) is the evidence that a `tantivy` index is not needed yet (PLAN 6.4 item 3).

---

## P10: Claude Code integration

Engine: `crates/ks-claude` ([engine-claude](docs/progress/engine-claude.md)). Spike:
[docs/spikes/claude-pty.md](docs/spikes/claude-pty.md). Wiring: [p10](docs/progress/p10.md).
`docs/progress/claude-wiring-ref.patch` is the reference patch the wiring was rewritten from (not
applied). Later: f1, a3, b3, f3.

### Done

- **Preflight** (`claude auth status`, JSON): requires `authMethod == "claude.ai"`; `api_key` /
  `api_key_helper` / cloud providers give the billing warning (`Claude would bill your API key;
  aborting`), proceeding only with an explicit `y` (`claude.proceed`, enter does nothing); not
  logged in and not installed give instructions; any non-JSON failure is `Failed`. The WT split goes
  through the same preflight. Credentials are never read or stored; the child environment is stripped
  of the six configured variables (case-insensitive; `EnvPolicy::extended()` adds three more for
  opt-in); `--bare` is never passed.
- **10a** WT split (`g w`, palette): `wt -w 0 sp -V -d <cwd> pwsh|powershell -NoProfile -EncodedCommand <base64
  UTF-16LE>` (`pwsh` if on PATH, else Windows PowerShell; since a3 `-EncodedCommand` instead of the
  plan's `-Command` string, so no quoting rules apply); it strips
  the env inside the pane's command, sets `KESTREL_IPC` there and passes `--mcp-config` /
  `--settings`.
- **10b** Embedded pane (spike verdict: `vt100` renders cleanly; `alacritty_terminal` not needed):
  ConPTY via `portable-pty`, vt100 parse, ratatui render with inverse handled and a hardware cursor
  when focused. Right (45 %, min 40 cols) or bottom split, resizable (`alt-=` / `alt--`), movable
  (`g p`), closable (`g X`). Focus toggle `ctrl-\` (rebindable via `[claude] focus_key`; raw 0x1c also
  accepted); while focused every other key goes verbatim to the pane (esc, ctrl-c, shift-tab);
  `shift-pageup/down` scroll the history. Session continuity: ids per project root in
  `claude-sessions.json`, `g r` resumes (a transcript must exist; keyed by git root, started in the
  creation directory, b3).
- **10c** `C` sends marked files (or the cursor file) as `@` mentions; grep hits send `@path#L<n>`
  (`alt-c`, marks `alt-m`); `V` in a text preview selects lines and `C`/`enter` send `@path#La-b`
  (f1). On Windows, where crossterm never yields `Event::Paste`, runs of 4+ plain text keys that
  arrive together are coalesced into one paste.
- **10d** `ks mcp` (rmcp 3.5.1 stdio; tools `get_selection`, `search_files`, `search_contents`,
  `list_dir`, `reveal`) behind a versioned NDJSON protocol over an `interprocess` named pipe
  (`kestrel-<pid>`); the TUI answers from state and the cached walk. `--mcp-config` / `--settings`
  generated per spawn; `--strict-mcp-config` is intentionally not passed.
- **10e** `ks hook` (PostToolUse `Edit|Write|MultiEdit|NotebookEdit`; exits 0 always, median 36 ms)
  forwards `{session_id, tool_name, file_path}`; `◆` marks (and `◇` on parent directories) in the list;
  activity line in the pane border; changes view (`g a`) with an inline `git diff HEAD` area (`d`),
  hardened against repo-configured commands.
- **Hardening rounds**: unified Confirm overlay for the billing/information dialogs, quit asks when
  a session runs, pane paste sanitising, query scanner for terminal queries (CSI 6n, XTVERSION, OSC
  10/11), session store locking and `.corrupt` handling, exit status shows the pane's last output on
  a non-zero exit and a rejected `--resume` forgets the stale id (a3), stale launch dirs swept only
  when the owning pid is dead (f3), hook command single-quoted (b3), `git_command` as the one
  hardened git builder (b3, a2).

### Deviations

1. Billing confirmation needs `y`, not enter (PLAN: "explicit confirmation").
2. The WT split uses `-EncodedCommand` (a3) instead of the plan's `-Command` shape (the script is
   base64 UTF-16LE; `pwsh` is still preferred when installed, with the plan's `powershell` fallback).
3. `ctrl-\` is rebindable and unverified as free in Claude Code (not documented as bound, never
   observed live); layouts where `\` needs AltGr must set `[claude] focus_key`.
4. Pane has a top-only rounded border (a boxed border would cost two more PTY rows and columns).
5. Default colours of the embedded screen map to the theme `fg`/`bg`.
6. The explorer's list and preview split the area left of the pane (`pane_widths`/`preview_rows` use
   the list area).
7. Resume from a subdirectory starts the pane in the directory the session was created in (b3), while
   a plain `c` starts in the browsed directory. The WT split does not use the session store.
8. IPC `ListDir` has no `truncated` flag; the protocol has none, so a visible "listing truncated"
   entry is appended after 2,000 entries.
9. The git diff stays inside the changes view (not routed through the preview pane).
10. The Claude WalkCache is separate from the finder's on purpose (it is cleared on hook events and
    after 5 s).
11. 10f (headless actions) is not built, by decision; `claude -p` was never run.
12. Hook command default is a shell string (`'<path>' hook`); `HookCommandStyle::Exec` exists but is
    opt-in because older Claude Code ignores `args`.

### VERIFY results

- **`claude auth status`**: JSON by default; exit 0 when logged in; **exit 1 with `loggedIn:false`,
  `authMethod:"none"` when logged out** (checked safely with `CLAUDE_CONFIG_DIR` at an empty temp
  dir). Flags `--session-id`, `--resume`, `--mcp-config`, `--settings`, `--strict-mcp-config` exist.
- **Spike**: ConPTY emits `CSI 6 n` at start-up and sends nothing until answered (the pane answers it
  itself); first screen arrives 1 to 2 s after spawn; vt100 renders the trust prompt correctly and
  ignores synchronized output safely.
- **crossterm Windows ctrl-backslash** arrives as `Char('\\')` + CONTROL (source read); raw 0x1c handled too.
- rmcp tools tested in-process over a duplex pipe and through a real `IpcServer`; a fake `claude.cmd`
  through the real runner and ConPTY covers preflight, launch args (`--session-id`, `--mcp-config`,
  `--settings`, never `--bare`), generated files, env stripping, `KESTREL_IPC`, typing, focus, resize,
  `C` mention, hook to changes log, exit status and `g r`.
- `cargo test -p ks-app --test claude_flow -- --ignored` launches the real `claude` and renders its
  first screen (trust prompt) through ks-ui. No input was ever sent to the real claude.
- **Not verified (needs a live Claude turn or a real window)**: `@path` mention resolution when
  pasted (`@"path with spaces"` quoting, `@path#L1-5`), a live PostToolUse payload (fields confirmed
  from the docs only), `launch_wt_split` against a real `wt`, the post-trust UI inside the pane, how
  the dialogs and pane look in Windows Terminal, the paste-burst heuristic and `ctrl-\` decoding in a
  real console.

### Measurements

`snapshot_screen` 120x40 about 170 µs; vt100 parse 240 MB/s; IPC round trip about 0.1 ms; converting a
200x60 snapshot 0.25 ms and a full frame with the pane 0.16 ms; fake-claude end-to-end test 0.5 s.

### Open issues

- All items under "Not verified" above (manual); if `@"..."` quoting misbehaves, flip
  `MentionOptions.style` to `Plain`.
- vt100 limitations: no OSC title, no synchronized-output buffering, kitty keyboard queries
  unanswered on purpose. The named pipe uses default per-user security (not a boundary; same-user
  processes can already do the same).
- The paste-burst heuristic may mistake 4+ keys queued during a stall for a paste (the text arrives
  intact; an Enter inside is a newline rather than a submit).
- The Claude changes log lives for the process lifetime; the changes view keeps its own diff area.
- A project path whose encoding differs from Claude's would never offer Resume (b3 risk of the
  cwd-scoped transcript lookup); a resumed session starts in its creation directory.
- `[claude] binary` does not affect the WT split (always the literal `claude`).

### Dependencies added

`ks-claude`: `interprocess` 2.4.5, `portable-pty` 0.9.0, `vt100` 0.16.2, `rmcp` 3.5.1, `schemars`
1.2.2, `serde_json`, `uuid` (v4), `which`. `ks-app`: `ks-claude`, `regex`, `thiserror`; dev
`serde_json`.

---

## Audit and fix rounds

After the phase merges (`5b58d42`) three review rounds ran, each with independent bug finders and
verifiers, fixes on separate branches and a gate run after every merge. Per-track notes hold the
regression test names.

### Fix wave (f1 to f4)

Not a bug hunt but the polish list left by the phases: f1 preview polish and Claude senders, f2
search freshness / multi-tab watching / memory / rg ratio / drives timeout, f3 file-ops and UI polish
(cut dimming, registry open-with, bulk re-edit, pid-alive sweep, hint priority), f4 README / manual
test / examples / CI / release. Gates were clean at each merge (1179 tests at the f3 merge; 1293 at
`06b00a8`; 1299 at `5205290` after the r3/c1 tests).

### Round 1: audit a1 to a3 (29 confirmed findings, all fixed)

The finders reported 29 findings that were all reproduced on the base (`2208fef`): 10 in a1 (two
duplicate pairs, so 8 distinct defects), 13 in a2, 6 in a3.

| Track | Findings (where fixed) |
|---|---|
| a1 file ops, foreground, event loop | a failed overwrite left the old destination parked (`ks-fs ops/transfer.rs`: all-or-nothing restore); cross-volume move dropped alternate streams / attributes / creation time (`CopyFileExW`); the conflict dialog stole keystrokes typed before it appeared (disarm window, overwrite moved to `O`); finishing a move cleared a newer cut register (`consumed_sources`); file-op completion and editor return replaced a results list (`nav::can_refresh`, rows pruned/renamed in place; 2 findings); a resize during a foreground program was lost (resync before `ForegroundDone`; 2 findings); a worker-thread panic printed over the TUI (default hook only for UI-thread panics); error exits skipped flush and shutdown (`break Err`) |
| a2 routing, listing/git, search, preview/render | filter vs visual mode left a hidden range; `git status` ran repo-configured commands (`core.fsmonitor` etc. forced off); wrong long-path prefix (single backslash); a refresh during a streamed listing lost pending marks; stale child counts after refresh; `sanitize_line` panic on bad match ranges (`snap_ranges`, per-file `catch_unwind`); brace-alternation globs split wrongly; multi-line regex errors; image drawn twice under overlays; wrong `truncated` flag; hex wide threshold off by one; bidi/format characters in names (`ks_search::is_unsafe_char` shared); quadratic markdown inline scan |
| a3 state, Claude, CI, flakes | concurrent instances overwrote bookmarks/frecency (lock + three-way merge); frecency cap evicted the just-visited entry; typographic quotes in the PowerShell quoting; the changes view could show another file's diff; a failed or instantly-exiting pane left no trace and `forget()` was unused; a fresh session overwrote the resumable id; plus CI hardening (`--no-fail-fast`, `LICENSE`, Recycle-Bin probes, longer failure deadlines) and flake fixes (bulk-rename temp-dir assumptions, session lock retries, watcher burst assertion) |

Review rounds inside the tracks added: `-EncodedCommand` for `wt`, cwd-scoped transcript lookup keyed
on the project root, forgetting a session only on "No conversation found", merge-on-write retry
semantics, frecency merge protection, and 80x24 diff-guard snapshots.

### Round 2: r2/b1 to b4 (23 items, all fixed)

| Track | Items (5 + 7 + 6 + 5) |
|---|---|
| b1 core, file ops, tabs, preview | bulk re-edit `n` leaked the kept edit file (and quit / interrupt / delete / preflight dialogs replacing it); two parked tabs on one directory (one listing fans out); preview line-selection dead end message; synchronous `fs::metadata` in `update` for hidden find-file targets (now resolved from the listing); content-search hit beyond the preview cap (true windowed read) |
| b2 search config, UI text, list columns | Claude changes view and status toast bidi sanitising; finder re-walk case-only rename; size / child-count column rule (terminal >= 60 and list pane >= 30); grapheme-aware truncation and highlighting; `[files] show_hidden/show_ignored` seeding; find-file access-denied count |
| b3 Claude session, git, hooks, CRT, watcher | session keying by launch cwd (resume from a subdirectory); hardened `git_command` builder + `--no-textconv` on diffs; hook command quoting; static CRT; long-path watch; trailing-edge watcher (a flake that was also a product race) |
| b4 flaky tests, persistence | `many_threads_writing_at_once_lose_no_bookmark` modelled on a real instance; unique `atomic_write` temp names and lock timeout semantics; event-loop palette/theme tests without sleeps (plus a draw-before-quit fix); the fake-claude test waits for `prompt>`; bulk-rename fake editor in pure cmd; watcher tests that wrote before the watch existed (`change_until`); shell cancel test bounds |

### Round 3: bug finders and stress run

The round-3 finders reported **0 new bugs**. A stress run (tests run many copies wide at once) confirmed
**5 timing flakes**, one of which was a product race. All were fixed in track `r3/c1`, merged as `93121d6`
(notes: [c1](docs/progress/c1.md)); the docs consolidation `r3/d1` merged as `5205290` (1299 tests).

Fixed in c1:

| Area | Change |
|---|---|
| Watcher cancellation race (the product bug) | `spawn_watch` gates the OS callback on the cancellation token, so a cancelled watch is silent whatever the watcher handle or the blocking pool is doing; a watcher that finishes arming after the cancel is dropped at once. Test `a_watch_cancelled_before_it_is_armed_never_reports`. A stale `ParkedDirChanged` was already harmless (`tabs::on_parked_dir_changed` ignores a path no parked tab shows; ks-core test added). |
| Gap between listing and watcher arming | After arming, one catch-up event is delivered when the directory mtime read before and after `DirWatcher::new` differs, or arming took over 30 ms (`needs_catch_up`, a pure function with a unit test; also gated on the token and racing the cancel in a `select!`). Tests `a_change_between_listing_and_arming_is_caught_up`, `a_catch_up_is_not_delivered_after_the_cancel`. |
| Flaky tests made deterministic | `parked_directories_are_watched_as_a_set` (wait and drain after the shrinking `WatchParked`), `shutdown_stops_watching` (drains the legitimate catch-up), `shutdown_cancels_a_running_copy_and_cleans_up` (waits for the job queue to be idle; completed vs cancelled decided from the output file), `permanent_delete_requires_the_typed_word` (fixed by the catch-up, test unchanged), `ipc_work` search budgets made injectable (`search_files_with_budget` / `search_contents_with_budget`, new test `an_exhausted_budget_reports_truncation`). |
| Load robustness found along the way | `persist` `concurrent_instances_lose_no_bookmark` (test-only `with_lock_stale`: a starved machine kept the lock past the 30 s staleness threshold), `rename_with_retry` now retries about 2.4 s instead of 250 ms (product hardening on the persist writer thread), fileops test `until` uses an inactivity bound plus a hard limit (a progressing 200 MB copy no longer times out), navigation latency assertion 1 s -> 2.5 s (the measured latency is 171 ms median). |

Stress results from c1 (VERIFY): ks-app lib test exe, 64 runs 16-wide: **64/64 ok** (before: 19 of 48 failed);
fileops exe, 16 runs 16-wide: **16/16 ok** (before: 3 to 4 of 16 failed, from the fixed shutdown assumption and
disk starvation of 16 concurrent 200 MB copies); lib exe 40 copies 40-wide: **40/40 ok** (before: 10 failed in
persist); 9 full workspace runs plus 3 final ones, 0 failures after the last fix; fmt and clippy `-D warnings`
clean. The final gate run of the P8 measurement pass: 1299 tests, 0 failures.

Deferred / open from c1 (all Low, none is a known bug in a normal run):

- The mtime catch-up cannot see a change that lands between `ListDir` and the first mtime read unless arming
  takes over 30 ms, nor content-only writes. Closing it needs `WatchDir` before `ListDir` in
  `nav::commit_dir` (a hub file) or an unconditional catch-up (one extra re-list per navigation).
- Slow arming of a parked set can queue one extra `ParkedDirChanged` per directory (bounded, wasted I/O only).
- The runner test for "cancelled before armed" cannot tell the gate from the drop without a hook after arming;
  the gate is covered by `a_catch_up_is_not_delivered_after_the_cancel`.
- PLAN defines no manual refresh key or command, so none was added.
- Fileops stress at 16-wide on the HDD is I/O bound (215 to 245 s per run).
- Scratch directories from the c1 stress runs (`D:\kestrel-base`, `D:\kestrel-base-target`, logs under
  `D:\kestrel-tmp`) could not be deleted by the agent; delete them by hand.

Load-sensitive families seen in earlier notes (still the first suspects if a test fails on a loaded
machine): the watcher tests, `claude_flow` (fake claude over ConPTY), the PowerShell/cmd fake-editor
bulk-rename tests, persist concurrency tests, `navigation::a_deleted_current_directory_moves_to_its_parent`
and the event-loop palette tests (rewritten without sleeps in b2/b4).

---

## Open issues

Consolidated and deduplicated from all notes (updated for the r3/c1 merge and the final P8 measurements,
issues 6, 22, 23 and 24). Items that later notes or the code at `06b00a8` show as
fixed were removed (for example: cut dimming, `O` ignoring marks, registry open-with, results-list
stats, new directories in the walk cache, `ctrl-r` duplicate roots, parked-tab watching, owner
display, image clone on the UI thread, quit-with-jobs/Claude confirm, stale launch-dir sweep, the
`ClaudeSendRange` senders, rg throughput, 1M-path memory, concurrent-instance overwrite, hint line,
dead network drives). Severity: **High** = can lose data or break a core flow; **Medium** = a stated
requirement unverified or a user-visible limitation; **Low** = polish, rare, or documented limitation.

No High items are known. "Manual" = needs a human in a real Windows Terminal, a live Claude turn or
GitHub; the "Manual verification needed" section groups them.

| # | Issue | Severity | Manual? |
|---|---|---|---|
| 1 | Terminal editor suspend/resume (`e`, `R` with nvim / hx: screen restored, no stuck or doubled keys) only covered by a fake host | Medium | yes |
| 2 | Image protocol detection (expected Sixel in WT 1.24), Sixel clear-on-leave and clear when an overlay closes unverified; real OneDrive placeholders never exercised | Medium | yes |
| 3 | Claude: `@path` mention resolution (quoted paths, `#L` ranges), live PostToolUse payload, WT split against a real `wt`, post-trust UI in the pane, dialogs in WT, `ctrl-\` free in Claude and decoded in a real console, paste-burst heuristic | Medium | yes |
| 4 | Keys that terminals may eat or change: `ctrl-enter`, `alt-.`, `alt-g`, `alt-m`, `alt-c`, `V`, `ctrl-\` (AltGr layouts) | Medium | yes |
| 5 | GitHub workflows (CI, release) never ran; the Windows CI job failed once undiagnosed; Linux tests never executed (only check/clippy; `ks-search` tests and benches not compiled for Linux from Windows); aarch64 and MSRV unverified | Medium | yes (GitHub) |
| 6 | Cold start (60 ms) and terminal-side key latency can only be measured in a real Windows Terminal. Re-measured headless on the final release build (2026-10-11, P8 table): key-to-frame p99 0.28 ms, finder worst 13 to 19 ms, 100k entries 61 ms: all met. Cold start: Kestrel's own part is 5 to 6 ms, the end-to-end approximation is not met (issue 24) | Medium | yes |
| 7 | A real keypress-driven session (`q`, `Q`, resize), Nerd Font glyphs, `×` / `◆` / `●` rendering, palette / help / which-key feel not seen in a real terminal | Low | yes |
| 8 | README GIFs / screenshots are placeholders; `cargo install` and the release zip untried on a clean machine; Windows symlinks (developer mode) and dead network shares not exercised | Low | yes |
| 9 | Running instances do not see another instance's bookmark / frecency writes until restart; stale-lock break race; lock fall-through after 8 s can lose a merge; orphan `.*.tmp` in the state directory not swept | Low | no |
| 10 | Undo pressed while a trash job is still running is a silent no-op; trash undo of originals over MAX_PATH fails cleanly (shell API) | Low | no |
| 11 | Copy does not preserve directory timestamps or ACLs, hard links become independent files, symlink recreation needs developer mode; bulk rename skips non-Unicode names (with a warning) | Low | no |
| 12 | Conflict-dialog arming is a timer (600 ms), not render-coupled; Esc / enter / ctrl-c are swallowed while disarmed by design | Low | no |
| 13 | Trash batching deferred (bin listed twice per job); shell runs are not in the Jobs overlay; `is_terminal_editor` is a heuristic | Low | no |
| 14 | Directory preview and parent column have no git glyphs / child counts; Claude changes view keeps its own diff area; windowed preview limited to plain UTF-8; sequential scan to the window start; syntax highlighting starts mid-file | Low | no |
| 15 | Preview engine: image decode / zip parse not interruptible (stale decode holds one of 3 workers), a pathological single line can run long in fancy-regex, BOM-less UTF-16 heuristic is ASCII-oriented, no horizontal scroll, transient memory up to about 3 x 200 MB for three max-size images | Low | no |
| 16 | Finder: nondeterministic tie order, matcher rebuild when a refresh finds removals, `scan_new_dir` capped at 200k and not cancellable, healthy drives wait for a dead one's 1.5 s deadline (abandoned threads live until the OS call returns); grep result order nondeterministic until done | Low | no |
| 17 | Grep `enter` leaves the overlay open under a terminal editor; file ops in a results tab skip cursor reveal and git; filter highlight skipped for names shortened with `…`; cursor inside a grapheme cluster highlights the whole cluster | Low | no |
| 18 | `z` overlay has no live preview (PLAN 4.3) and ranks on the UI thread (2.7 ms at the cap); bookmark list is on `M` (not in PLAN 5); palette queries are literal fuzzy words; `Reload config` discards session theme / icon picks; time column keyed on pane width >= 90 only; hint line drops mark / hidden / drives below about 160 columns (deliberate) | Low | no |
| 19 | Git status is recomputed per directory change with no repo-wide cache (large repositories with big ignored trees may be slow) | Low | no |
| 20 | Claude: vt100 limits (no OSC title, no sync-output buffering, kitty keyboard queries unanswered); named pipe default security; changes log lives for the process; WT split has no resume and ignores `[claude] binary`; a project path whose encoding differs from Claude's never offers Resume; resumed session starts in its creation directory | Low | no |
| 21 | Debug builds highlight slowly (`[profile.dev.package."*"] opt-level = 3` recommended, not applied) | Low | no |
| 22 | Documentation drift: README key tables have no drift test; some per-track notes under docs/progress still list items that were fixed later (each marked RESOLVED where the owner noticed). (The `docs/manual-test.md` section 10 claim that Kestrel logs no first-frame line, and its stale latency and ripgrep statements, were corrected in the final P8 pass.) | Low | no |
| 23 | Timing-sensitive tests can still fail on a machine under heavy load or starved of disk I/O. The five flakes of the round-3 stress run (one a watcher product race) are fixed and merged (`r3/c1`, `93121d6`): 64/64 and 16/16 stress runs 16-wide, 40/40 at 40-wide, 1299 tests clean in every gate run since. What c1 deferred (Low): the mtime catch-up misses a change between `ListDir` and the first mtime read when arming is fast, and content-only writes; one extra `ParkedDirChanged` per directory on slow parked arming; the cancelled-before-armed runner test cannot tell the gate from the drop. About a dozen tests are `#[ignore]`d on purpose (perf and memory guards, real `claude`, real registry, 1 GiB copy) | Low (was Medium until c1 merged) | no |
| 24 | Process start cost: the bare `ks.exe` process (`ks --version`) lives 77 ms (median, n=20) on the development machine against 29 ms for a minimal Rust exe; a 13.9 MB dummy exe costs 72 ms, so the cost follows image size (about 3 ms per MB, most likely Defender's scan on exec), not Kestrel's start-up code (5 to 6 ms to the first frame) or its DLL imports. The PLAN cold-start budget of 60 ms to first frame is therefore probably not met end to end on a machine with real-time protection, and the same cost applies to every `ks hook` call. Possible levers: a smaller image (`opt-level = "s"` / fewer syntect or image codecs, `panic = "abort"`), a Defender exclusion for the install folder. Not tried | Medium | yes (real WT, ideally on a machine without real-time scanning) |

## Manual verification needed

Everything below needs a real Windows Terminal session (WT 1.24 was installed on the development
machine), a live Claude turn or GitHub. The ready-made checklist is
**[docs/manual-test.md](docs/manual-test.md)** (ten sections: basics, navigation, layout, preview,
find and search, palette / help / config, file operations, tabs / bookmarks / shell integration,
Claude Code, performance). Highest-value checks, by open-issue number:

1. Section 7: `e` and `R` with `nvim` / `hx`, then a few keys (issue 1).
2. Section 4: an image (Sixel in WT 1.24, else set `[preview] image_protocol = "sixel"`), leaving it,
   and a OneDrive placeholder that must not download (issue 2).
3. Section 9: mention resolution, hook payload, WT split, `ctrl-\`, paste of a multi-line text, quit
   with a session running (issues 3 and 4).
4. Section 5 and 6: `ctrl-enter`, `alt-*` chords, `V` (issue 4).
5. Section 10 and the P8 table: the real start-up number (read the `first frame elapsed_ms=` log line and
   time the launch by eye; headless it is 5 to 6 ms in-process but about 77 ms of process creation on the
   development machine, issue 24), terminal-side key latency (issue 6).
6. Push to a throwaway GitHub repository to see the CI and release workflows run (issue 5).
7. A plain `q` / `Q` session with the shell function from `ks --print-shell-init` (issue 7).

Record outcomes back in this file.
