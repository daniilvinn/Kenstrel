# Kestrel: keyboard-first terminal file explorer for Windows

> Implementation plan for a coding agent. "Kestrel" (binary `ks`) is a working name, rename freely.
> Read this whole file before writing code. Work phase by phase. Each phase ends with acceptance criteria. Do not start the next phase until those pass.

---

## 0. Rules for the implementing agent

1. **One phase at a time.** At the end of each phase: `cargo fmt`, `cargo clippy --all-targets -- -D warnings`, `cargo test` must all be clean. Then commit with message `phase N: <summary>` and update `PROGRESS.md` (what was done, what deviates from this plan, open issues).
2. **Never require a mouse.** Every feature must be reachable from the keymap *and* from the command palette. Mouse capture is off by default.
3. **Never block the UI thread.** All filesystem, search, preview, git and process work runs off the render loop and reports back via messages. A slow disk must never freeze input.
4. **Keep the decisions in this file.** If you must deviate (a crate doesn't work, an API is missing), make the smallest change, record it in `PROGRESS.md` under "Deviations" with the reason, and continue. Stop and ask the user only for changes that alter the architecture or the user-facing design.
5. **Items marked `VERIFY`** are assumptions not confirmed against current docs or behaviour. Check each before relying on it (spike, `--help`, a dump to a log) and record the result in `PROGRESS.md`.
6. **Dependencies.** Use the crates listed in §2. Adding another one is fine when it clearly saves work. Note it in `PROGRESS.md`. Use the latest stable versions at implementation time and commit `Cargo.lock`.
7. **Primary target is Windows 10/11 + Windows Terminal.** Keep code cross-platform where it's free (CI also runs on Linux), but never trade Windows quality for portability.
8. **Logging goes to a file, never stdout/stderr** (it would corrupt the TUI). Install a panic hook that restores the terminal before printing the panic.

---

## 1. Product summary

A fast, beautiful, fully keyboard-driven file explorer that runs in the terminal and *feels* like Claude Code: calm minimal chrome, one accent colour, a prompt line at the bottom, instant feedback.

**Core pillars**
- **Navigate** at the speed of thought: vim-style keys, history, bookmarks, frecency jump, tabs.
- **Find anything:** instant fuzzy search over file names and fast streaming search over file contents.
- **Look before opening:** rich preview (syntax-highlighted text, images, directories, archives, metadata).
- **Act safely:** copy/move/rename/trash with progress, conflict prompts and undo.
- **Work with Claude Code** (last phase): open Claude next to the explorer, hand it files/lines, let it query the explorer's search, see what it changed. Uses the user's **Claude subscription only, never API billing** (see §10).

**Non-goals (for now):** a GUI, network filesystems/remote (SFTP, S3), plugin system, being a full editor.

---

## 2. Tech stack

| Concern | Choice | Notes |
|---|---|---|
| Language | Rust (stable, edition 2021+) | single static `.exe` |
| TUI | `ratatui` + `crossterm` | crossterm uses the Windows console API |
| Async | `tokio` (multi-thread) + `tokio::sync::mpsc`, `tokio_util::sync::CancellationToken` | blocking FS work in `spawn_blocking` or dedicated threads |
| Directory walk | `ignore` (parallel walker from ripgrep) | gitignore-aware, toggles for hidden/ignored |
| Fuzzy matching | `nucleo` (from Helix) | streaming injector, match indices for highlighting |
| Content search | `grep-searcher`, `grep-regex`, `grep-matcher` | ripgrep's engine, binary detection |
| FS watching | `notify` + `notify-debouncer-full` | refresh visible dirs |
| Syntax highlight | `syntect` (or `two-face` for extra syntaxes) | load syntax set lazily, once |
| Image preview | `ratatui-image` | Sixel / halfblocks fallback |
| Trash | `trash` | Recycle Bin on Windows |
| Clipboard | `arboard` | copy paths |
| Open with default app | `open` crate (ShellExecute) | |
| Config | `serde`, `toml`, `directories` | |
| Errors / logs | `anyhow`, `thiserror`, `tracing`, `tracing-appender` | |
| Windows APIs | `windows` crate | drives, attributes, later NTFS |
| Paths | `dunce` | strip `\\?\` for display |
| Text width | `unicode-width`, `unicode-segmentation` | correct CJK/emoji alignment |
| Git status | shell out to `git status --porcelain=v2 -z` | simpler and reliable; debounce |
| Testing | `insta` (snapshots), ratatui `TestBackend`, `tempfile`, `criterion` | |
| Phase 10 only | `portable-pty`, `vt100` or `alacritty_terminal`, `rmcp` (official Rust MCP SDK), `interprocess` | see §10 |

---

## 3. Architecture

### 3.1 Shape: unidirectional data flow (Elm-style)

```
 crossterm events ─┐
 fs watcher ───────┤                 ┌──────────────┐
 job results ──────┼──► Msg channel ─►│ update(state, │──► Vec<Effect> ──► effect runner (tokio tasks)
 timers/ticks ─────┘                 │      msg)     │                        │
                                     └──────┬───────┘                        │
                                            │ state                          │
                                            ▼                                │
                                   render(&state, frame)   ◄── results as Msg ┘
```

- `AppState` is the single source of truth. `update` is a pure-ish function that maps `(state, Msg)` → mutated state + a list of `Effect`s (e.g. `ReadDir(path)`, `StartFuzzy{..}`, `CopyFiles{..}`).
- Effects run on background tasks and post results back as `Msg`. Each long job carries a **generation id** and a `CancellationToken`. Stale results (old generation) are dropped in `update`.
- `render` is a pure function of state. Redraw only when state is dirty, with a cap of about 60 fps. Spinners tick at 10 Hz only while a job is running.
- This makes the core testable without a terminal: feed `Msg`s, assert state, snapshot the render.

### 3.2 Crate / module layout

```
kestrel/
  Cargo.toml                (workspace)
  crates/
    ks-core/                state, Msg, Effect, update(), keymap model, config model. No terminal deps
    ks-fs/                  listing, metadata, sorting, file ops + job queue, watcher, windows specifics
    ks-search/              walker, fuzzy (nucleo), content search (grep-*), optional indexes
    ks-preview/             text/syntax, image, dir, archive, binary/hex, metadata
    ks-ui/                  ratatui widgets, layout, theme, overlays
    ks-app/                 main.rs: terminal setup, event loop, effect runner, CLI args → binary `ks`
    ks-claude/              (phase 10) PTY pane, MCP server, hook receiver, IPC
  assets/themes/*.toml
  docs/keymap.md            (generated from the default keymap)
  PROGRESS.md
```

### 3.3 Input modes

`Normal` · `Visual` (range select) · `Prompt` (command line, rename, create) · `Filter` (in-dir filter) · overlays: `Finder`, `Grep`, `Palette`, `Help`, `Confirm`, `Jobs`, and later `Claude` (focus inside the PTY pane).

Key handling: overlay on top of the mode stack first, then the mode, then global keys. **Windows gotcha:** crossterm on Windows emits both `Press` and `Release` events, so handle `KeyEventKind::Press` (and `Repeat`) only, or every key fires twice.

---

## 4. Design system (the "CLI-like, beautiful" part)

### 4.1 Principles
- **Calm chrome.** No heavy box grids. Borders only around overlays and the focused pane, rounded (`╭╮╰╯`), in `border` colour. The focused pane uses `border_focus`.
- **One accent colour.** Used for the cursor row marker, matches, the prompt glyph and active tab. Everything else is a neutral fg, dim or muted.
- **Hierarchy through brightness, not boxes.** Primary text `fg`, metadata `fg_dim`, hints `fg_muted`.
- **Text-first feedback.** Errors and confirmations appear in the status line (and a small toast for 3 s), never as modal popups unless a decision is needed.
- **Key hints everywhere, quietly.** For example `‹/› filter  ‹ctrl-p› find  ‹:› commands  ‹?› help` in `fg_muted` in the status bar, adapting to the current mode.
- **Never colour-only meaning.** Git and selection states also have a glyph.

### 4.2 Main screen (target at 120×32)

```
 ~/projects/omniforce/src/renderer                                    main ●2 +1    1 ▏2 ▏3
 ────────────────────────────────────────────────────────────────────────────────────────────
   ..                                    │  vulkan_device.rs              14.2 KB · 3h ago
   shaders/                         12   │ ─────────────────────────────────────────────────
   passes/                           7   │   1  use ash::vk;
 ▌ vulkan_device.rs           14.2 KB  M │   2  use crate::core::{Result, Handle};
   swapchain.rs                9.8 KB    │   3
   descriptors.rs              6.1 KB  + │   4  pub struct VulkanDevice {
   pipeline_cache.rs           3.3 KB    │   5      instance: ash::Instance,
   mod.rs                        412 B   │   6      physical: vk::PhysicalDevice,
                                         │   7      logical: ash::Device,
                                         │  ...
 ────────────────────────────────────────────────────────────────────────────────────────────
 › _                                                                  4/8 · 1 selected · ⠹ 1 job
   ‹/› filter  ‹ctrl-p› find file  ‹ctrl-f› search text  ‹:› commands  ‹?› help
```

- **Header:** breadcrumb path (the home dir shown as `~`, middle segments elided with `…` when narrow), git branch + counts, tab strip.
- **List pane (about 40%):** icon, name, size/child count right-aligned, git glyph. Directories first, natural sort. Cursor row: `▌` accent bar + `selection_bg`. Marked (selected) items: `●` glyph + accent name.
- **Preview pane (about 60%):** header line (name, size, relative mtime), then content. An optional parent column (Miller columns) toggles with `P`/config.
- **Prompt line:** `›` in accent. It is the command line in `:`/rename/create modes, otherwise shows position, selection count and a job spinner.
- **Hint line:** context-sensitive key hints. Hidden when height < 20.
- **Responsive:** under 80 cols hide the preview (toggle with `w`). Under 60 cols hide sizes. Must look right at 80×24.

### 4.3 Overlays
Centered, rounded border, title in the top border (`╭─ Find file ─────╮`), input line at top with `›` prompt, results below, live preview on the right when width ≥ 100. Counter `1,234 / 58,910` with spinner while walking. Matched characters in accent + bold.

### 4.4 Theme tokens
`bg, fg, fg_dim, fg_muted, accent, accent_fg, selection_bg, border, border_focus, dir, exec, symlink, hidden, success, warning, error, git_added, git_modified, git_untracked, git_ignored, git_conflict, match`.
- Ship two themes: `ember` (dark, warm accent close to Claude Code's terracotta) and `paper` (light). Users add more in `themes/*.toml`.
- Truecolor when `COLORTERM=truecolor` or `WT_SESSION` is set, otherwise map to the 256-colour palette.
- `bg` defaults to *terminal default* (transparent) so it respects the user's Windows Terminal theme. Themes may override it.

### 4.5 Icons & glyphs
Config `icons = "nerd" | "unicode" | "ascii"`. The default is `nerd`; the README says a Nerd Font is required, and the palette offers "Switch icon set". Map by extension + special names (`Cargo.toml`, `.git`, `README`). All alignment uses `unicode-width`.

### 4.6 Formatting rules
Human sizes (`14.2 KB`, base-1000 or 1024 configurable). Relative times (`3m ago`, `yesterday`, then `2025-03-14`). Absolute timestamps go in the metadata view. Numbers right-aligned. Long names truncated in the middle with `…` (keep the extension visible).

---

## 5. Keymap (default; all rebindable in `config.toml`)

The keymap is data. The help overlay (`?`), the which-key popup (shown 300 ms after a prefix key) and `docs/keymap.md` are all generated from it.

| Keys | Action |
|---|---|
| `j/k`, `↓/↑` | down / up |
| `h`/`←`/`backspace`, `l`/`→`/`enter` | parent / enter dir or open file |
| `gg` / `G`, `ctrl-d` / `ctrl-u` | top / bottom, half page |
| `ctrl-o` / `ctrl-i` (`tab`) | history back / forward |
| `~`, `g h`, `g d`, `g D`, `g c`, `g /` | home, downloads, desktop, config dir, "This PC" (drives) |
| `/` | filter current directory (incremental, fuzzy). `esc` clears |
| `ctrl-p` or `f` | **find file** (recursive fuzzy) |
| `ctrl-f` or `s` | **search contents** |
| `:` | command palette (fuzzy over all commands, shows bindings) |
| `?` | help |
| `space` / `v` / `ctrl-a` | toggle mark / visual range / mark all |
| `y` / `x` / `p` | copy / cut / paste (into current dir) |
| `Y` | copy path(s) to clipboard (submenu: absolute / relative / name) |
| `r` / `R` | rename / bulk rename in `$EDITOR` |
| `a` | create (trailing `/` makes a directory) |
| `d` / `D` | move to Recycle Bin / delete permanently (typed confirm) |
| `u` | undo last file operation |
| `o` / `O` | open with default app / open with… (palette of handlers) |
| `e` | open in `$EDITOR` (`$VISUAL`/`$EDITOR`, fallback `code`, then `notepad`) |
| `!` | run shell command in current dir (output in a pager overlay) |
| `.` | toggle hidden files |
| `S` | sort menu (name / size / mtime / ext; `S` again reverses) |
| `m<char>` / `'<char>` | set / jump to bookmark |
| `z` | frecency jump (zoxide-style) |
| `t` / `1`..`9` / `ctrl-w` | new tab / switch tab / close tab |
| `w` / `P` / `i` | toggle preview / toggle parent column / toggle metadata view. `J/K` scroll preview |
| `g j` | jobs overlay (progress of copy/move jobs; cancel with `x`) |
| `c` / `C` | (phase 10) open Claude / send marked files to Claude as `@` mentions |
| `q` / `Q` | quit / quit and cd the shell to the current dir |

**Windows Terminal conflicts:** avoid `ctrl-shift-*`, `alt-enter`, `ctrl-,`, `ctrl-tab` (WT captures them). Don't depend on the kitty keyboard protocol. Everything must work with plain console key events.

---

## 6. Search specification

### 6.1 In-directory filter (`/`)
Run nucleo over the current listing (already in memory). Updates on every keystroke with no debounce. Non-matching rows are hidden and matched chars highlighted. `enter` keeps the filter, `esc` clears it.

### 6.2 Find file (`ctrl-p`)
- Scope: current directory, recursive (default). `ctrl-r` cycles scope: cwd → git root → home → drive.
- Walker: `ignore::WalkParallel`, streaming paths into a `nucleo::Injector` as they're found. Results appear while walking. Toggles: `alt-.` hidden, `alt-g` gitignored files. (Avoid `ctrl-h`: many terminals send it as backspace.) Never follow junctions/symlinks.
- Match on the relative path, with the file-name portion weighted higher (nucleo path matching config).
- Show the top N visible results only. Live preview of the highlighted item.
- `enter`: navigate to the file's dir with the cursor on it. `ctrl-enter`/`o`: open it. `space`: mark (multi-select), then act on marks.
- **Cache:** keep the last walk per root in memory and refresh it incrementally from watcher events. Reopening the finder on the same root is instant.

### 6.3 Search contents (`ctrl-f`)
- Smart case. Literal by default, `ctrl-r` toggles regex, `ctrl-w` whole word. An optional second field filters by glob (`*.rs`, `!target/**`).
- Debounce 120 ms. Each new query cancels the previous search (token) and bumps the generation.
- Pipeline: walker → bounded channel → N worker threads running `grep-searcher` (binary detection on, max file size 10 MB by default) → batched results (every 30 ms) → UI.
- Results grouped by file: `path` header + `  42│ matched line` with match highlight. Preview jumps to the line with context.
- `enter`: open in `$EDITOR` at the line (`code -g file:line`, `nvim +line file`; configurable template). `ctrl-l`: send results to a "results list" tab that behaves like a directory listing.
- Cap at 10k matches with a "refine your search" hint.

### 6.4 Whole-disk instant search (phase 9, optional)
1. **Everything integration (cheap win, no admin):** if voidtools Everything is running, query it through its SDK/IPC or `es.exe` for instant whole-disk filename search.
2. **Own NTFS index:** read the MFT via `FSCTL_ENUM_USN_DATA` and keep it fresh via the USN journal (`FSCTL_READ_USN_JOURNAL`). This needs admin, so ship it as an optional small Windows service or elevated helper writing an index file the TUI reads. Stretch goal.
3. Content index with `tantivy` only if streaming search proves too slow on the user's real trees. Measure first.

### 6.5 Windows-specific search hazards
- **OneDrive / cloud placeholders:** files with `FILE_ATTRIBUTE_RECALL_ON_DATA_ACCESS` or `FILE_ATTRIBUTE_OFFLINE` must **not** be read by preview or content search, because that triggers a download. Show them with a cloud glyph and skip them in grep, with a counter of skipped files.
- Skip `System Volume Information`, `$Recycle.Bin`, `pagefile.sys`, `hiberfil.sys`, `swapfile.sys`. Treat access-denied as a quiet skip, counted in the status line.

---

## 7. Filesystem & file operations

- **Listing:** `std::fs::read_dir`. On Windows, `DirEntry::metadata()` comes from `FindNextFileW` without extra syscalls, so it's cheap. Use it. For dirs over 5k entries, stream in chunks and render progressively.
- **Large dirs:** render only visible rows (virtualized list, don't build a `List` of 100k items). Sort with a precomputed natural-sort key. Case-insensitive on Windows.
- **Drives root ("This PC"):** a virtual directory listing drives (`GetLogicalDrives` + `GetVolumeInformationW`) with label, filesystem and free/total space bars.
- **Attributes:** hidden/system files follow the hidden toggle. Show junctions/symlinks with their target in preview. Long paths: use `\\?\` internally, `dunce` for display.
- **Watcher:** watch the current dir and the dirs visible in tabs only (not recursive). Debounce 100 ms, then re-list and preserve the cursor by name.
- **Job queue** for copy/move/delete: runs in the background with progress (bytes, files, ETA) in the status line and a Jobs overlay. Cancellable. Conflict prompt: overwrite / skip / rename / keep both, with an "apply to all" toggle.
- **Trash** by default (`trash` crate). Permanent delete requires typing `yes` (or the item count for multi).
- **Undo log** (session-scoped): rename, move, create, trash. Trash undo restores via the `trash` crate's restore API (`VERIFY` on Windows). Copy undo deletes the copies (to trash).
- **Bulk rename:** write names to a temp file, open `$EDITOR`, diff on save, show a preview of changes, then confirm.
- **Git status:** run `git status --porcelain=v2 -z` in the background when entering a repo or on watcher events (debounced 300 ms). Map results to glyphs per entry and aggregate them to dirs.
- **Shell integration (`Q`):** `ks --cwd-file <path>` writes the final dir on exit. Document a PowerShell profile function:
  ```powershell
  function k { $f = New-TemporaryFile; ks --cwd-file $f @args; $d = Get-Content $f; if ($d) { Set-Location $d }; Remove-Item $f }
  ```

---

## 8. Preview

Runs in the background, cancelled on cursor move, with an LRU cache (last 64 previews).
- **Text:** read at most 256 KB / the first 2,000 lines. Detect encoding (UTF-8, UTF-16 BOM). Highlight with syntect using a theme derived from the theme tokens. Show line numbers.
- **Directory:** its listing (same renderer as the list pane, dimmed).
- **Image:** `ratatui-image`, with protocol auto-detection and a fallback to halfblocks. `VERIFY` Sixel on the user's Windows Terminal version (Sixel landed in WT 1.22).
- **Markdown:** light rendering (headings bold/accent, code blocks highlighted).
- **Archives:** list entries for zip (`zip` crate). Others show metadata only.
- **Binary:** hex view of the first 4 KB.
- **Metadata footer/toggle (`i`):** size, created/modified/accessed, attributes, owner, link target.
- **Large/cloud/locked files:** metadata only, with a reason line.

---

## 9. Config & state

- `%APPDATA%\kestrel\config.toml`: theme, icons, keymap overrides, editor templates, default sort, ignore toggles, preview limits, layout ratios.
- `%APPDATA%\kestrel\themes\*.toml`: user themes.
- `%LOCALAPPDATA%\kestrel\state\`: history, bookmarks, frecency DB, finder caches, logs.
- On first run, write a commented default `config.toml`. Bad config: show the error in the status line and fall back to defaults (never crash).
- Palette command `Reload config`.

---

## 10. Claude Code integration (final phase)

### 10.1 Hard constraints (subscription only, no API billing)
- **Drive the user's installed, unmodified `claude` CLI.** It handles login with the user's Pro/Max subscription. **Do not** use the Claude Agent SDK or the Anthropic API: the SDK docs require an API key and say third-party apps may not offer claude.ai login.
- **Strip credentials from the child environment** so the subscription login is used. API keys take precedence over `/login`, and under `-p` a present key is always used. When spawning `claude`, `env_remove` these: `ANTHROPIC_API_KEY`, `ANTHROPIC_AUTH_TOKEN`, `CLAUDE_CODE_OAUTH_TOKEN`, `CLAUDE_CODE_USE_BEDROCK`, `CLAUDE_CODE_USE_VERTEX`, `CLAUDE_CODE_USE_FOUNDRY`. Make this list configurable.
- **Never pass `--bare`** (bare mode ignores the subscription login).
- **Never read, copy or store credentials** (`~/.claude/.credentials.json`, keychain, tokens). Kestrel only spawns the CLI.
- **Preflight:** run `claude auth status` (JSON; exit 0 = logged in). Require `authMethod == "claude.ai"`. If it reports `api_key`/`api_key_helper`, show a clear warning ("Claude would bill your API key; aborting") and offer to proceed only with explicit confirmation. If `claude` isn't found, show install instructions.
- **Personal use only.** Usage counts against the user's plan limits. Every background (`-p`) action asks for confirmation the first time.

### 10.2 Integration pieces (in order)

**10a. Launch Claude next to the explorer (minimal, robust).**
Palette command + key `c`: open Claude in a **Windows Terminal split pane** in the current dir:
`wt -w 0 sp -V -d "<cwd>" pwsh -NoProfile -Command "Remove-Item Env:ANTHROPIC_API_KEY,Env:ANTHROPIC_AUTH_TOKEN,Env:CLAUDE_CODE_OAUTH_TOKEN -ErrorAction SilentlyContinue; claude"`. Note: a WT pane does **not** inherit Kestrel's environment (WT builds a fresh one), so `env_remove` on our process isn't enough here. The stripping must happen inside the pane's command, as shown. Fall back to `powershell` if `pwsh` is missing. No embedding needed. This gives value immediately and is the fallback if 10b proves flaky.

**10b. Embedded Claude pane (spike first).**
- Spawn `claude --session-id <uuid>` in a ConPTY via `portable-pty`. Parse output with a terminal emulator crate and render it into a ratatui pane (bottom or right split, resizable).
- **Spike before committing:** Claude Code's TUI uses rich escape sequences. Test `vt100` first. If rendering glitches (cursor, colours, alternate screen, synchronized output), switch to `alacritty_terminal`. Record the result in `PROGRESS.md`.
- **Focus:** a dedicated toggle key (default `ctrl-\`, configurable; `VERIFY` it isn't used by Claude Code). While Claude has focus, forward *all* other keys verbatim, including `esc`, `ctrl-c` and `shift-tab`. Resize the PTY on pane resize.
- Session continuity: remember the session id per project root and offer "Resume last Claude session" (`claude --resume <id>`).

**10c. Hand context to Claude.**
- `C` on marked files: type `@relative/path` mentions (space-separated) into the Claude pane using **bracketed paste**.
- From a content-search hit or a preview line range in visual mode: `@path#L<start>-<end>`.
- `VERIFY` that pasted `@path` text resolves as a file mention the same way typed mentions do. If not, fall back to plain paths plus a short instruction line.

**10d. Kestrel as an MCP server (Claude can use the explorer).**
- `ks mcp` subcommand: a stdio MCP server built with `rmcp`. Tools:
  - `get_selection()`: marked files, cursor file, cwd.
  - `search_files(query, root?, limit=50)`: uses Kestrel's fuzzy index (cached walk).
  - `search_contents(pattern, glob?, regex=false, limit=200)`
  - `list_dir(path)`, `reveal(path)` (moves the explorer cursor to a path Claude mentions).
- **IPC:** the running TUI listens on a local socket (`interprocess`; a named pipe on Windows) named `kestrel-<pid>`. It passes `KESTREL_IPC=<name>` in the env of the spawned `claude`, which passes it on to the MCP server process. The MCP process is a thin proxy to the TUI's state.
- **Registration without touching the repo:** spawn with `--mcp-config <generated json>` (`VERIFY` the flag in `claude --help`). Fallback: a user-scope `claude mcp add` documented in the README. Never write `.mcp.json` into the user's projects automatically.

**10e. See what Claude changed.**
- The fs watcher already refreshes listings. Hooks add *attribution*.
- Spawn with `--settings <generated json>` (`VERIFY`) containing a `PostToolUse` hook with matcher `Edit|Write|MultiEdit|NotebookEdit` and command `ks hook`. `ks hook` reads the hook JSON from stdin and forwards `{session_id, tool_name, tool_input.file_path}` to `KESTREL_IPC`. It must exit fast and always exit 0.
- `VERIFY` the payload fields by logging one real hook input first.
- UI: files touched by Claude in this session get a `◆` glyph. A "Claude changes" view (palette) lists them with `git diff` previews. A one-line activity feed in the Claude pane's border shows recent edits.

**10f. Optional headless actions** (only after 10a–e are solid).
- For example "Explain this folder" or "Summarize file" in the preview pane, via `claude -p "<prompt>" --output-format stream-json --verbose` (env stripped), streaming into the preview. Confirm before each run type. Show "uses your Claude plan limits".

### 10.3 Decisions to confirm with the user before starting phase 10
1. Embedded pane (10b) vs WT split only (10a), after the spike result.
2. Whether hooks/MCP are injected per spawn (preferred) or installed at user scope.
3. Which headless actions (10f), if any, are wanted.

---

## 11. Quality, testing, performance

**Tests**
- `ks-core`: unit tests for `update()` (navigation, modes, keymap resolution, generation dropping).
- `ks-ui`: `insta` snapshots via `TestBackend` for every screen/overlay at 80×24 and 160×48, in both themes.
- `ks-fs`: file ops in `tempfile` dirs: conflicts, cancel mid-copy, undo, long paths (> 260 chars), Unicode names.
- `ks-search`: correctness tests plus `criterion` benchmarks on a generated tree (200k files, mixed sizes).
- CI: GitHub Actions on `windows-latest` (primary) and `ubuntu-latest`: fmt, clippy, test, release build artifact.

**Performance budgets** (measure; record in `PROGRESS.md` at phase 8)

| Metric | Budget |
|---|---|
| Cold start to first frame (dir < 5k entries) | < 60 ms |
| Key press → frame | < 16 ms p99 |
| Enter dir with 100k entries | first frame < 150 ms, fully listed < 1 s |
| Fuzzy finder: keystroke → updated results | < 25 ms (on 500k paths) |
| Content search throughput | within 1.5× of `rg` on the same tree and query |
| Memory with a 1M-path finder cache | < 250 MB |

**Robustness:** terminal restored on panic and on `ctrl-c`. Handle no-permission, vanished files and disconnected drives gracefully. Every error ends up in the log with context.

---

## 12. Phases & acceptance criteria

**P0: Skeleton**
Workspace layout, terminal setup/teardown, panic hook, file logging, event loop with `Msg`/`Effect`, config loading with defaults, theme tokens, CI.
- ✅ `ks` opens on Windows Terminal, resizes cleanly, `q` exits, a forced panic restores the terminal, and keys don't double-fire (Press-only filtering).

**P1: Navigation**
Listing, natural sort, icons, header breadcrumb, status + hint lines, cursor/scroll, parent/enter, history, hidden toggle, "This PC" drives view, watcher refresh, marks/visual mode, git glyphs.
- ✅ Navigate `C:\` → deep project dirs keyboard-only. A 100k-file dir meets the budget. External file changes appear within ~200 ms with the cursor preserved. Snapshots pass at 80×24 and 160×48.

**P2: Preview**
All preview types in §8, background + cancellation + cache, `w` toggle, preview scrolling.
- ✅ Holding `j` through a dir of large files never stutters. OneDrive placeholders are not downloaded. Images render (or fall back) in WT.

**P3: Filter & Find file**
§6.1 and §6.2, including streaming, scopes, toggles, cache, match highlighting, live preview.
- ✅ Results appear while walking, typing stays responsive during a 500k-file walk, and the budget is met.

**P4: Content search**
§6.3 complete.
- ✅ Cancellation works under fast typing, open-at-line works for VS Code and Neovim templates, and the throughput budget is met.

**P5: Command palette, help, which-key, keymap config, themes**
- ✅ Every action is listed in the palette with its binding. Rebinding in config works and shows in help. Both themes pass snapshots.

**P6: File operations**
§7 job queue, conflicts, trash, permanent delete, undo, bulk rename, clipboard paths, open/open-with/editor, `!` shell command.
- ✅ Copying 10 GB shows progress and can be cancelled. Undo works for each op type. No op is possible without a keyboard path.

**P7: Tabs, bookmarks, frecency, shell integration**
- ✅ `Q` + PowerShell function changes the shell dir. Bookmarks and frecency persist across runs.

**P8: Polish & performance**
Profile against §11 budgets, fix regressions, write the README with GIFs, a Windows Terminal profile snippet, and `cargo install` / release `.exe`.
- ✅ All budgets met or documented with the reason.

**P9 (optional): Whole-disk search**
Everything integration first. MFT/USN index as a stretch.

**P10: Claude Code** (§10, after confirming §10.3 with the user)
10a → 10b spike → 10c → 10d → 10e → 10f.
- ✅ `claude auth status` preflight blocks API-key billing. Claude opens beside the explorer, receives `@` mentions, can call `search_files` via MCP, and edited files get the `◆` marker. No credentials are ever read by Kestrel.

---

## 13. References
- Ratatui: https://ratatui.rs · Nucleo: https://github.com/helix-editor/nucleo · ripgrep crates: https://github.com/BurntSushi/ripgrep/tree/master/crates
- Prior art to study for UX (don't copy code blindly): Yazi, Broot, Superfile, lf, ranger, Helix pickers.
- Claude Code docs: authentication (precedence, `claude auth status`) https://code.claude.com/docs/en/authentication.md · CLI reference https://code.claude.com/docs/en/cli-reference.md · headless https://code.claude.com/docs/en/headless.md · MCP https://code.claude.com/docs/en/mcp.md · hooks https://code.claude.com/docs/en/hooks.md · legal/compliance (subscription use) https://code.claude.com/docs/en/legal-and-compliance.md
