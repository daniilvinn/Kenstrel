# Kestrel

A keyboard-first terminal file explorer for Windows (binary: `ks`), written in Rust with
[ratatui](https://ratatui.rs). Calm chrome, one accent colour, a prompt line at the bottom,
vim-style keys, fuzzy find and live grep built in, and an optional integration with your own
installed [Claude Code](https://claude.com/claude-code).

Kestrel is not affiliated with or endorsed by Anthropic; it launches your own installed Claude Code.

> Screenshots and a short GIF are not included yet. Record them once in Windows Terminal and
> put them in `docs/img/`:
>
> - `docs/img/main.png`: the file list with the preview pane and git glyphs
> - `docs/img/find.png`: the find-file overlay (`ctrl-p`)
> - `docs/img/claude.png`: the embedded Claude pane
> - `docs/img/tour.gif`: navigate, find, search, preview, copy

The design is in [PLAN.md](PLAN.md), the build log in [PROGRESS.md](PROGRESS.md), the complete
default key table in [docs/keymap.md](docs/keymap.md) (generated from the program, so it is
always current), and a manual test checklist in [docs/manual-test.md](docs/manual-test.md).

## Requirements

- Windows 10 (1809 or newer, which ConPTY and so the embedded Claude pane need) or Windows 11.
  The code also builds on Linux (CI does so), but Kestrel is designed and tested for Windows.
- [Windows Terminal](https://aka.ms/terminal) is recommended. Other terminals work if they
  deliver plain console key events; the Claude Windows Terminal split (`g w`) needs `wt`.
- A [Nerd Font](https://www.nerdfonts.com/) set as the terminal font for the default icons.
  Without one, use the `unicode` or `ascii` icon set (see [Icons](#icons)).
- Optional: `git` on `PATH` (branch and change glyphs), `code`, `nvim` or another editor, and
  the `claude` CLI logged in with a Pro/Max account for the Claude features.

## Install

From a clone of this repository, with a stable Rust toolchain (MSVC target). Kestrel is developed and tested with Rust 1.99; no older minimum version has been verified, so no `rust-version` is declared:

```powershell
cargo install --path crates/ks-app --locked    # installs ks.exe into %USERPROFILE%\.cargo\bin
ks                                              # open the current directory
ks C:\Users                                     # open a directory (a file opens its parent)
```

Or build without installing: `cargo build --release -p ks-app` produces `target\release\ks.exe`.
The CI workflow (`.github/workflows/ci.yml`) uploads that `ks.exe` as the `ks-windows-x86_64`
artifact of every run, and pushing a tag such as `v0.1.0` runs `.github/workflows/release.yml`,
which attaches `ks.exe` and a zip to a GitHub release.

`ks --help` lists the command line: `ks [PATH]`, `--cwd-file <FILE>`,
`--print-shell-init <powershell|pwsh|bash|zsh>`, `--dump-keymap`, `--version`. (`ks mcp` and
`ks hook` are helper processes started by Claude Code, see [Claude Code](#claude-code).)

## Quick start

| Keys | What |
|---|---|
| `j` `k` / `down` `up` | move the cursor |
| `l` `enter` / `h` `backspace` | enter a directory or open a file / go to the parent |
| `/` | filter this directory (fuzzy, as you type) |
| `ctrl-p` | find a file anywhere below here |
| `ctrl-f` | search file contents |
| `:` | command palette: every command, with its keys |
| `?` | help: every key binding, filterable with `/` |
| `q` | quit (`Q` quits and `cd`s your shell, see [Shell integration](#shell-integration)) |

After a prefix key (`g`, `S`, `m`, `'`) a which-key popup lists the possible next keys once
300 ms have passed (`[ui] which_key_delay_ms`).

## Feature tour

All keys below are the defaults and can be rebound ([Configuration](#configuration)). Every
command is also in the command palette (`:`). [docs/keymap.md](docs/keymap.md) has the complete
table.

### Navigation

| Keys | Action |
|---|---|
| `j` `k`, `gg` `G`, `ctrl-d` `ctrl-u` | move, top / bottom, half page (also `home` `end` `pagedown` `pageup`) |
| `h` `l`, `backspace` `enter` | parent / enter a directory, or open a file with its default app |
| `ctrl-o` / `tab` | history back / forward |
| `~` `g h` `g d` `g D` `g c` `g /` | home, Downloads, Desktop, Kestrel's config directory, This PC |
| `.` | show or hide hidden files |
| `S` then `n` `s` `m` `e` | sort by name / size / modified time / extension; choosing the active key again reverses |

Above a drive root, `h` shows **This PC**: drives with label, file system and a free-space bar.
Empty drives never raise a system dialog. Listings stream in chunks and only the visible rows
are drawn, so directories with 100,000 entries open instantly. The directory refreshes by itself
when files change on disk, keeping the cursor on its entry.

In a git repository the header shows the branch and counts (`↑` ahead, `↓` behind, `+` staged,
`~` modified, `?` untracked, `U` conflicts, or a check mark when clean) and each row carries a
glyph: `M` modified, `+` added, `R` renamed, `D` deleted, `?` untracked, `!` ignored,
`U` conflict. OneDrive placeholders get a cloud glyph and are never downloaded by preview or
search.

### Selection

`space` marks the entry and moves down, `v` starts a range (visual mode), `ctrl-a` marks all,
`esc` leaves visual mode and then clears the marks. Marked entries are what copy, cut, delete,
bulk rename and `C` (send to Claude) act on; with no marks they act on the entry under the
cursor.

### Layout and preview

The preview pane shows on terminals 80 columns wide or more; the list gets `[layout]
list_percent` (40 %) of the width. `w` forces the preview on or off at any width, `P` toggles the
parent-directory column (it needs 100 columns), `i` switches the preview between content and
metadata, `J` `K` scroll the preview. File sizes disappear below 60 columns.

The preview runs in the background and never blocks the keys; it is cancelled when the cursor
moves. It renders:

- text with line numbers and syntax highlighting (the first 256 KiB / 2000 lines by default;
  UTF-8 and UTF-16 with BOM are detected),
- Markdown (headings, code blocks),
- directories (their listing), zip archives (entry list), images (see
  [Terminal graphics](#terminal-graphics)),
- binary files as a hex dump,
- metadata (size, times, attributes, link target, owner) with `i`. That is also what you get for
  files that must not be read: OneDrive online-only placeholders and files that are too large
  or locked, with the reason shown.

### Filter, find file, search contents

| Keys | What |
|---|---|
| `/` | filter the current directory; `enter` keeps the filter (shown as a chip on the prompt line), `esc` clears it |
| `ctrl-p` or `f` | **find file**: recursive fuzzy match on the relative path, results stream in while walking |
| `ctrl-f` or `s` | **search contents**: live grep with smart case, literal by default |

In the find-file overlay: `up` `down` / `ctrl-n` `ctrl-p` move, `tab` marks a result and moves
down, `ctrl-r` cycles the scope (current directory, git root, home, drive), `alt-.` toggles
hidden files, `alt-g` toggles gitignored files, `enter` navigates to the file's directory with
the cursor on it (several marks in one directory select them all; marks in different directories
open a results list), `ctrl-enter` or `ctrl-o` open the marked or highlighted results with their
default app. The last walk of a root is cached and kept fresh from file watcher events, so
reopening the finder is instant. Junctions and symlinks are never followed.

In the content-search overlay: type the query; `tab` switches to the glob field (`*.rs`,
`!target/**`), `ctrl-r` toggles regex, `ctrl-w` toggles whole words, `enter` opens the hit in
your editor at that line, `ctrl-l` shows the files with matches as a results list in a new tab.
Results are grouped by file, capped at 10,000 matches, and skip binary files, files over 10 MB
and cloud placeholders (counted in the footer). On terminals 100 columns or wider both overlays
show a live preview of the highlighted result.
In the grep overlay `alt-m` marks the highlighted hit and `alt-c` sends the marked hits (or the highlighted one) to the running Claude pane as `@path#Lnn` mentions.

### File operations

| Keys | What |
|---|---|
| `y` `x` `p` | copy, cut, paste into the current directory |
| `d` | move to the Recycle Bin (`u` undoes) |
| `D` | delete permanently, after typing `yes` (or the item count for several) |
| `u` | undo the last rename, move, create or trash |
| `r` | rename (prompt prefilled, cursor before the extension) |
| `R` | bulk rename in your editor: marked entries, or all shown entries; the `old -> new` preview must be confirmed |
| `a` | create a file; a trailing `/` creates a directory (nested paths work) |
| `Y` | copy path(s) to the clipboard: menu with `a` absolute, `r` relative, `n` name |
| `o` / `O` | open with the default app / choose an app (menu, hot keys `1`..`9`) |
| `e` | open in your editor |
| `!` | run a shell command in this directory; output streams into a pager (`ctrl-c` stops it) |
| `g j` | jobs overlay: progress, throughput and ETA; `x` cancels the selected job, `c` clears finished ones |

Copy, move and delete run as background jobs with progress in the prompt line. A name conflict
asks: `o` overwrite, `s` skip, `r` rename, `k` keep both, `a` apply to all remaining, `c` cancel
(`enter` keeps both, `esc` cancels the job). Quitting while jobs run asks first.

The editor is `[editor] open` if set, else `$VISUAL`, then `$EDITOR`, then `code`, then
`notepad`. Terminal editors (vim, nvim, hx, nano, micro, ...) suspend Kestrel while they run and
bring it back afterwards; GUI editors (code, notepad) are started detached.
`[editor] terminal` overrides that guess.

### Tabs, bookmarks and frecency

| Keys | What |
|---|---|
| `t` / `1`..`9` / `ctrl-w` | new tab / switch to tab / close tab (up to 9; the last one cannot be closed) |
| `m` then a key / `'` then a key | set a bookmark / jump to it |
| `M` | list bookmarks (`enter` jumps, `d` deletes) |
| `z` | frecency jump: type to filter the directories you visit most and most recently, `enter` jumps |

Bookmarks (`bookmarks.toml`) and visit history (`frecency.toml`) live in
`%LOCALAPPDATA%\kestrel\state`. The tab strip appears in the header once there is a second tab.

### Shell integration

A program cannot change its parent shell's directory, so `Q` ("quit and cd") works through a
small function that passes `--cwd-file` to `ks`. `q` leaves the shell where it was. Print the
function with `ks --print-shell-init <shell>` and then run `k` instead of `ks`:

```powershell
# PowerShell 7 (pwsh) and Windows PowerShell 5.1: add this line to the profile (notepad $PROFILE)
ks --print-shell-init pwsh | Out-String | Invoke-Expression
```

`pwsh` and `powershell` print the same script. It reads the temporary file with
`-Encoding UTF8`, which matters in Windows PowerShell 5.1: its default would decode the file as
the ANSI code page and break directory names with non-ASCII characters. For a faster start-up,
paste the output of `ks --print-shell-init pwsh` into the profile as a plain function instead.

```sh
# bash or zsh, including Git Bash on Windows (it converts C:\ paths with cygpath)
eval "$(ks --print-shell-init bash)"
```

### Windows Terminal profile

Add a profile to the `profiles.list` array of Windows Terminal's `settings.json` to open
Kestrel in its own tab. Generate your own GUID (`[guid]::NewGuid()`); the one below is only an
example.

```json
{
    "guid": "{6b2f1d3e-84a0-4c57-9e1b-3d5a7c9f0e21}",
    "name": "Kestrel",
    "commandline": "ks.exe",
    "startingDirectory": "%USERPROFILE%",
    "hidden": false
}
```

`ks.exe` must be on `PATH` (the `cargo install` folder normally is); otherwise use the full path.
To make `Q` change the directory of that tab's shell, start PowerShell and run the `k` function
from your profile instead (see above):

```json
"commandline": "pwsh.exe -NoLogo -NoExit -Command k"
```

(Use `powershell.exe` for Windows PowerShell 5.1.)

## Icons

`icons = "nerd"` (default) needs a Nerd Font. If the icons show as boxes, either install one and
select it in Windows Terminal (Settings > Profiles > Appearance > Font face), or set
`icons = "unicode"` or `icons = "ascii"` in `config.toml`. To try a set without editing the file,
run **Switch icon set** from the palette; the choice lasts for the session (**Reload config**
returns to the file's value).

## Terminal graphics

Image previews use Sixel, Kitty or iTerm2 graphics when the terminal supports them and fall back
to half-block characters otherwise. At start-up Kestrel asks the terminal which protocol it
speaks (waiting at most 250 ms) and logs the result as `image protocol` with a `detected=` field.
Windows Terminal supports Sixel since version 1.22. If detection falls back to halfblocks
although your terminal can do better, force a protocol:

```toml
[preview]
image_protocol = "sixel"      # "auto" | "halfblocks" | "sixel" | "kitty" | "iterm2"
```

`"halfblocks"` skips the terminal query entirely. With `[preview] enabled = false` the query is
skipped as well.

## Claude Code

Kestrel can run your installed Claude Code beside the file list, hand it files as `@` mentions,
and show what it changed. It is built on three rules.

**Subscription only, never API billing.** Kestrel only starts your installed, unmodified `claude`
CLI. It does not use the Anthropic API or the Claude Agent SDK, never passes `--bare`, and never
reads, copies or stores credentials (not `.credentials.json`, not the keychain, not tokens).
Every `claude` it starts has `ANTHROPIC_API_KEY`, `ANTHROPIC_AUTH_TOKEN`,
`CLAUDE_CODE_OAUTH_TOKEN`, `CLAUDE_CODE_USE_BEDROCK`, `CLAUDE_CODE_USE_VERTEX` and
`CLAUDE_CODE_USE_FOUNDRY` removed from its environment (`[claude] strip_env` changes the list).
Usage counts against your own plan limits. Kestrel never sends a prompt itself: you type to
Claude, in its pane.

**Preflight.** Before each launch Kestrel runs `claude auth status` and requires the `claude.ai`
login method. If it reports an API key (`api_key`, `api_key_helper`) or a cloud provider, a
dialog says *"Claude would bill your API key; aborting"* and nothing starts unless you press
`y`; `enter` does nothing and `esc` aborts. If you are not logged in, the dialog tells you to run
`claude` once and use `/login` with your Pro/Max account. If `claude` is not found, it shows
install instructions. (`[claude] binary` sets an explicit path.)

**Nothing is installed behind your back.** The MCP server and the hook are injected per spawn
with `--mcp-config` and `--settings`, pointing at generated files in `%TEMP%\kestrel-<pid>`.
Nothing is added to your user-scope Claude configuration and no `.mcp.json` is ever written into
your projects.

### Keys

| Keys | Action |
|---|---|
| `c` | open Claude in an **embedded pane** in the current directory (focuses it if it already runs) |
| `g w` | open Claude in a **Windows Terminal split** instead (`wt -w 0 sp -V`). It goes through the same preflight, gets the same MCP server and hook, and strips the variables inside the pane's own command, because a WT pane does not inherit Kestrel's environment |
| `g r` | resume the last Claude session of this project (`claude --resume`) |
| `C` | send the marked files (or the file under the cursor) to the running pane as `@` mentions and focus it |
| `V` | in the text preview: select lines (`j`/`k` extend, `C` or `enter` sends `@path#Lstart-end`, `esc` cancels) |
| `ctrl-\` | move the keyboard between the explorer and the pane (`[claude] focus_key`) |
| `alt-=` / `alt--` | grow / shrink the pane (15 % to 85 %) |
| `g p` | move the pane between the bottom and the right |
| `shift-pageup` / `shift-pagedown` | scroll the pane's history (while it has focus) |
| `g X` | close the pane and end its session process |
| `g a` | **Claude changes** view |

While the pane has focus, every key except the focus, grow/shrink and scroll keys goes to Claude
untouched, including `esc`, `ctrl-c` and `shift-tab`. Press `ctrl-\` to get back to the explorer.
On keyboard layouts where `\` needs AltGr, set `focus_key` to something else (for example
`'ctrl-]'`). A burst of typed text (a paste) arrives in the pane as one paste instead of a
series of `enter` presses. Quitting Kestrel while a session runs asks first.

The embedded pane is a terminal emulator inside Kestrel (ConPTY plus a `vt100` screen), drawn
with a top border that shows the focus hint and, right-aligned, the latest edit Claude made. If
it ever renders badly for you, `g w` gives a real terminal pane.

### Mentions

`C` pastes `@relative/path` mentions, separated by spaces, into the pane. Paths under the
project use forward slashes; others are absolute. Paths that contain spaces are quoted as
`@"my dir/a b.md"`; that form is a best guess, not documented Claude Code behaviour, so check
that it resolves on your version (see [docs/manual-test.md](docs/manual-test.md)).

### Claude can use the explorer (MCP)

Claude Code started by Kestrel gets an MCP server named `kestrel` (its tools appear as
`mcp__kestrel__*`; the server is `ks mcp`, talking to the running Kestrel over a per-process
named pipe whose name is passed in `KESTREL_IPC`). Tools: `get_selection` (marked files, cursor
file, cwd), `search_files` (fuzzy, over Kestrel's cached index), `search_contents`, `list_dir`
and `reveal` (moves the explorer cursor to a path, so you see what Claude is talking about).

### Seeing what Claude changed

A `PostToolUse` hook for `Edit|Write|MultiEdit|NotebookEdit` runs `ks hook`, which tells the
running Kestrel which file was touched (it always exits 0). Touched files get a `◆` after their
name in the list (`◇` on directories that contain one; `*` / `+` with the ascii icon set). `g a`
opens the **Claude changes** view: `j` `k` move, `enter` reveals the file in the explorer, `d`
shows or hides its `git diff` (against `HEAD`), `X` forgets the list, `esc` closes. The list lasts
for the Kestrel process.

### Optional manual step: register the MCP server at user scope

Kestrel never does this for you, and you do not need it: sessions started from Kestrel already
have the server. If you still want the `kestrel` server listed in Claude Code sessions you start
yourself, you can add it by hand:

```powershell
claude mcp add --scope user kestrel -- ks mcp
```

Be aware of the limit: the server needs the `KESTREL_IPC` pipe name that only Kestrel-started
sessions get, so in a session started elsewhere its tools answer *"Kestrel is not reachable"*.
Remove it with `claude mcp remove kestrel`.

## Configuration

Kestrel writes a commented `config.toml` on first run. Files and folders:

| Path | Content |
|---|---|
| `%APPDATA%\kestrel\config.toml` | settings and key overrides (`g c` opens the folder) |
| `%APPDATA%\kestrel\themes\*.toml` | your themes |
| `%LOCALAPPDATA%\kestrel\state\bookmarks.toml`, `frecency.toml` | bookmarks and visit history |
| `%LOCALAPPDATA%\kestrel\state\claude-sessions.json` | Claude session id per project |
| `%LOCALAPPDATA%\kestrel\state\logs\kestrel.<date>.log` | logs (daily, 7 kept) |

A mistake in `config.toml` never stops Kestrel from starting: it falls back to the defaults and
shows the error on the status line. Unknown keys count as mistakes, so typos are caught. Edit the
file, then run **Reload config** from the palette (`:`); no restart is needed.

Below is every setting with its default value (the keymap lines are examples). The same text is
[docs/config.example.toml](docs/config.example.toml), which a test keeps valid.

```toml
theme = "ember"          # "ember" (dark), "paper" (light) or a file themes\<name>.toml
icons = "nerd"           # "nerd" | "unicode" | "ascii"

[sort]
key = "name"             # "name" | "size" | "mtime" | "ext"
reverse = false
dirs_first = true

[files]
show_hidden = false      # dotfiles and hidden/system files
show_ignored = false     # gitignored files in find/search
size_base = 1024         # 1024 (KiB-style) or 1000 (SI)

[preview]
enabled = true
image_protocol = "auto"  # "auto" | "halfblocks" | "sixel" | "kitty" | "iterm2"
max_bytes = 262144       # read at most this much of a text file
max_lines = 2000

[layout]
list_percent = 40        # width of the list pane, 10..=90
parent_column = false    # the parent-directory column (P), needs 100 columns

[editor]
open = ""                # empty: $VISUAL, then $EDITOR, then "code", then "notepad"
open_at_line = "code -g {file}:{line}"   # {file} and {line} are substituted
terminal = "auto"        # "auto" | "always" | "never": suspend Kestrel while the editor runs

[ui]
which_key_delay_ms = 300

[claude]
binary = ""              # empty: look `claude` up on PATH
strip_env = [            # removed from every claude process
    "ANTHROPIC_API_KEY", "ANTHROPIC_AUTH_TOKEN", "CLAUDE_CODE_OAUTH_TOKEN",
    "CLAUDE_CODE_USE_BEDROCK", "CLAUDE_CODE_USE_VERTEX", "CLAUDE_CODE_USE_FOUNDRY",
]
pane_position = "bottom" # "bottom" | "right"
pane_percent = 40        # share of the screen body, 15..=85
focus_key = 'ctrl-\'     # focus toggle between the explorer and the pane

[keymap.normal]          # examples; the contexts are listed below
"H" = "dir.parent"
"L" = "dir.enter"
"ctrl-t" = "tab.new"

[keymap.finder]
"ctrl-j" = "cursor.down"
"ctrl-k" = "cursor.up"
```

A few details:

- `[editor] open_at_line` is used when a content-search hit is opened. A value other than the
  default is taken as the whole command line, for example `"nvim +{line} {file}"`.
- There is no `[theme]` or `[syntax]` table in `config.toml`: colours, including syntax colours,
  belong to the theme file (see below). `theme = "name"` selects one, and **Switch theme** in the
  palette tries one for the session.
- Choices made from the palette (theme, icon set) last for the session only; write them into
  `config.toml` to keep them.

### Key bindings

Keys are `[keymap.<context>]` tables: the key is a space-separated sequence (`"g g"`,
`"ctrl-p"`, `"alt-."`, `"m <char>"` where `<char>` passes the next typed key to the command), the
value is a command id from [docs/keymap.md](docs/keymap.md) (optionally with an argument:
`"tab.goto:3"`), or `"none"` to remove a binding. Key names: single characters, `space`,
`enter`, `esc`, `tab`, `backspace`, `delete`, `up`, `down`, `left`, `right`, `home`, `end`,
`pageup`, `pagedown`, `f1`..`f12`, with `ctrl-` and `alt-` prefixes. Contexts: `normal`,
`visual` (inherits `normal`), `prompt`, `filter`, `finder`, `grep`, `palette`, `help`,
`confirm`, `jobs`, `claude`, `bookmarks`, `jump`, `conflict`, `pager`, `menu`,
`claude_changes`. Binding a prefix such as `g` removes the longer defaults that start with it
(`g g`, `g h`, ...); the status line tells you when that happens. Avoid `ctrl-shift-*`,
`alt-enter`, `ctrl-,` and `ctrl-tab`: Windows Terminal consumes them.

### Themes

A theme is a TOML file `%APPDATA%\kestrel\themes\<name>.toml` (letters, digits, `-` and `_` in
the name). It inherits every colour you leave out from `base` (`"ember"` or `"paper"`, default
`"ember"`), so a theme can be three lines long. A user file named like a built-in shadows it.
Colours are `"#rrggbb"`, `"#rgb"` or `"reset"` (the terminal's own colour; both built-ins use
`bg = "reset"` so your Windows Terminal background shows through). On terminals without
truecolor they are mapped to the 256-colour palette. A complete annotated file is
[docs/theme.example.toml](docs/theme.example.toml); its shape:

```toml
base = "ember"

[colors]            # 22 tokens: bg fg fg_dim fg_muted accent accent_fg selection_bg border
accent = "#6aa5d9"  #   border_focus dir exec symlink hidden success warning error git_added
border_focus = "#4f7aa0"  # git_modified git_untracked git_ignored git_conflict match

[syntax]            # preview highlighting; unset keys are derived from [colors]
keyword = "#6aa5d9" # keyword string comment function type number constant operator punctuation
comment = "#7a746c"
```

## Logs and troubleshooting

Logs go to `%LOCALAPPDATA%\kestrel\state\logs\kestrel.<date>.log`; nothing is ever printed over
the UI. Set `KESTREL_LOG=debug` in the environment for more detail. The log has the start-up
lines (`starting`, `image protocol`), config problems and the Claude integration's activity.
Kestrel does not read any credentials, so none can end up in it.

- **Icons show as boxes or question marks**: the terminal font is not a Nerd Font. Install one,
  or set `icons = "unicode"` / `"ascii"`.
- **A key does nothing in Windows Terminal** (`ctrl-enter` is the usual one): the terminal did
  not deliver it as a distinct key. Use the alternative (`ctrl-o` in the find-file overlay) or
  rebind. Kestrel does not rely on the Kitty keyboard protocol, only on plain console key
  events, and avoids the combinations Windows Terminal consumes itself.
- **Images look blocky**: detection chose halfblocks. Look for the `image protocol` line in the
  log; if your Windows Terminal is 1.22 or newer, set `image_protocol = "sixel"`.
- **Images leave remnants or flicker**: set `image_protocol = "halfblocks"`.
- **The first key press after start-up is lost in a terminal that never answers the graphics
  query**: set `image_protocol = "halfblocks"`, which skips the query.
- **`Q` does not change the directory**: the `k` function is not loaded (check with
  `Get-Command k`), or you quit with `q`, which never changes the directory.
- **Config error on the status line**: the whole file was ignored. Fix the line it names and run
  **Reload config**. Delete `config.toml` to get a fresh commented copy.
- **Claude: "not found"**: install Claude Code or set `[claude] binary`. **"would bill your API
  key"**: Claude Code is logged in with an API key or provider rather than your subscription; run
  `claude` and use `/login` to sign in with your Pro/Max account.
- **Terminal left in a strange state after a crash**: Kestrel restores the terminal on panics
  and errors. If it ever does not, close the tab and report it together with the log.

## Development

```powershell
cargo fmt --all
cargo clippy --workspace --all-targets -- -D warnings
cargo test --workspace
cargo test --release -p ks-ui keypress_to_frame_stays_within_budget   # latency budget, release only
```

The workspace has seven crates: `ks-core` (state, update, keymap, config; no I/O), `ks-fs`
(listing, watcher, git, file jobs, launching), `ks-search` (fuzzy find and grep), `ks-preview`
(preview engine), `ks-ui` (rendering, themes), `ks-claude` (Claude Code engine) and `ks-app` (the
`ks` binary). Snapshot tests use `insta`; after an intended UI change inspect the `.snap.new`
files and accept them with `cargo insta accept`. `docs/keymap.md` is generated:
`KESTREL_UPDATE_DOCS=1 cargo test -p ks-core docs`. The ignored performance tests are listed in
[docs/manual-test.md](docs/manual-test.md#10-performance). Tests never run `claude -p` or send a
prompt to a real Claude; they use fake binaries.

## License

MIT, see [LICENSE](LICENSE).
