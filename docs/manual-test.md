# Manual test checklist (Windows Terminal)

Automated tests cannot drive a real terminal, so this list covers what only a human can check:
real keyboards, real resizing, real graphics, real drives, a real `claude`. Keys are the
defaults from [keymap.md](keymap.md). Run it in **Windows Terminal** with a Nerd Font, once with
PowerShell 7 and, for section 8, once with Windows PowerShell 5.1. Tick a box when the
**Expect** line holds; write down anything that does not.

Setup:

```powershell
cargo build --release -p ks-app
$env:Path = "$PWD\target\release;$env:Path"      # so that `ks` runs this build
$log = "$env:LOCALAPPDATA\kestrel\state\logs"    # newest kestrel.<date>.log is the log file
```

Do not script a Claude session: every Claude step below is done by hand (the checklist never
sends a prompt for you), and most of section 9 only uses Claude's own `/status` and `/mcp` commands.

---

## 1. Basics

- [ ] **Starts.** `ks C:\`. Expect: the list of `C:\` renders with header, list, prompt line and hint
  line; no stray characters.
- [ ] **Resize.** Drag the window edge larger and smaller about 20 times, also very narrow and very
  short. Expect: the screen redraws cleanly every time, no leftover fragments, no crash.
- [ ] **One key, one move.** Press `j` once. Expect: the cursor moves exactly one row (not two).
  Hold `j`: it repeats smoothly.
- [ ] **Quit.** Press `q`. Expect: the shell prompt returns cleanly with the old screen content
  restored (no alternate-screen residue), and the cursor is visible.
- [ ] **Ctrl-C.** Start `ks`, press `ctrl-c`. Expect: it quits and the terminal is restored.
- [ ] **Panic restore.** `ks --debug-panic` (hidden flag). Expect: the program panics right after
  the first frame, the terminal is restored first, and the panic message is readable on the
  normal screen; typing afterwards echoes normally.

## 2. Navigation

Steps in order, starting from `ks C:\`.

- [ ] **Walk down.** `l` (or `enter`) into `Users`, into your user folder, into a repository. `h`
  (or `backspace`) goes back up with the cursor on the directory you left.
- [ ] **Jumps.** `gg` top, `G` bottom, `ctrl-d` / `ctrl-u` half pages. `ctrl-o` goes back in
  history, `tab` forward. `g h` home, `g d` Downloads, `g D` Desktop, `g c` the Kestrel config
  directory (`%APPDATA%\kestrel`), `g /` This PC.
- [ ] **This PC.** Expect: all drives with label, file system, a usage bar and `free of total`; the
  bar turns warning-coloured from 85 % full. `l` on a drive enters it; `h` at a drive root returns
  here.
- [ ] **Removable drives.** With the This PC view open, plug in a USB drive, eject it, and open an
  empty card reader. Expect: no Windows "insert a disk" dialog at any point, and Kestrel keeps
  responding.
- [ ] **Git header.** In a repository the header shows the branch and counts, and rows carry glyphs
  (`M + R D ? ! U`). Edit a tracked file in another program and save. Expect: its `M` glyph and the
  counts update within about half a second.
- [ ] **External changes.** Create and delete files in the displayed directory from another
  window. Expect: the list updates within about 200 ms and the cursor stays on the same entry.
- [ ] **Icons.** Expect: file-type icons render as glyphs, not boxes. In the palette (`:`) run
  **Switch icon set** and pick `unicode`, then `ascii`: the icons change at once. **Reload config**
  returns to the file's value.
- [ ] **Huge directory.** Open a directory with 100,000 files (create one with
  `1..100000 | % { New-Item "$env:TEMP\big\f$_.txt" -Force }`, slow, or use an existing one).
  Expect: first rows immediately, `j`/`G` stay instant. Delete the directory afterwards.

## 3. Layout

- [ ] **80 x 24.** Resize to 80 columns by 24 rows. Expect: the preview pane is shown (the preview
  appears from 80 columns), the list has about 40 % of the width and shows no size column. At 79
  columns the preview disappears and the list uses the full width with sizes.
- [ ] **`w` forces.** At 70 columns press `w`: Expect: the preview appears (or a status message says
  there is no room); `w` again hides it. The choice survives resizes.
- [ ] **Sizes vanish.** Make the list pane narrower than 60 columns (hide the preview with `w`, then
  narrow the window to 59). Expect: the size column is gone, names remain.
- [ ] **Parent column.** At 100 columns or more press `P`. Expect: a column with the parent
  directory appears to the left, the current directory highlighted. Below 100 columns it does not
  show.

## 4. Preview

- [ ] **Hold `j`** through a directory of big text files (several MB each). Expect: no stutter; the
  preview of the final file appears when you stop.
- [ ] **Images.** Select a PNG and a JPG. Expect: the image is drawn in the pane (Sixel in Windows
  Terminal 1.22+, otherwise half blocks). Look in the newest log for the line with
  `image protocol` and a `detected=` field.
- [ ] **Forced protocol.** Set `image_protocol = "sixel"` (and then `"halfblocks"`) in
  `config.toml`, run **Reload config** (the protocol is read at start-up, so restart `ks`).
  Expect: the corresponding rendering.
- [ ] **No remnants.** Move from an image to a text file, then to a directory, then resize the
  window while an image is shown. Expect: no image pixels left behind anywhere.
- [ ] **Kinds.** A `.rs` file (highlighted, line numbers), a `.md` file (headings), a `.zip` (entry
  list), an `.exe` (hex dump), a UTF-16 text file (readable text), a junction or symlink (metadata
  with its target). Expect: each renders sensibly.
- [ ] **Metadata.** `i` toggles content and metadata; `J` / `K` scroll long previews.
- [ ] **OneDrive.** Select an online-only placeholder (cloud glyph in the list). Expect: the pane
  says why it is not shown and displays metadata; the file does **not** start downloading (check
  the OneDrive status icon).

## 5. Find file and content search

Use a repository with a few thousand files.

- [ ] **Find file.** `ctrl-p`. Type part of a path. Expect: results stream in while the walk runs,
  matched characters highlighted, a counter `matched / total`. At 100+ columns a live preview of
  the highlighted result is shown.
- [ ] **Scopes and toggles.** `ctrl-r` cycles the scope (title changes: current directory, git root,
  home, drive); `alt-.` toggles hidden files; `alt-g` toggles gitignored files (footer reflects it).
- [ ] **Mark, open.** `tab` marks the result and moves down (a `●` appears). `enter` navigates to the
  file with the cursor on it. With several marks in one directory, `enter` goes there and selects
  them; marks in different directories open a results list.
- [ ] **Open with the default app.** In the finder press `ctrl-enter`. Expect: the file opens with
  its default app. If nothing happens, note it (Windows Terminal may not deliver the key) and check
  `ctrl-o`, which must work.
- [ ] **Reopen is instant.** Close the finder (`esc`) and press `ctrl-p` again. Expect: the full list
  at once (cached walk).
- [ ] **New deep file.** Create a file several directories below the root from another window, then
  open the finder. Expect: it is found (a new file in an existing directory; brand-new directories
  may need a re-walk, which is a known limit).
- [ ] **Content search.** `ctrl-f`, type quickly. Expect: no lag while typing, results grouped by
  file with `42│` line numbers, matches highlighted.
- [ ] **Toggles and glob.** `ctrl-r` regex (chip appears), `ctrl-w` whole word, `tab` to the glob
  field and enter `*.rs` or `!target/**`. Expect: the results follow.
- [ ] **Open at line.** `enter` on a hit. Expect: VS Code opens at that line (`code -g file:line`).
  Then set `[editor] open_at_line = "nvim +{line} {file}"` (and again with `hx`), reload the config,
  repeat. Expect: Kestrel suspends, the editor shows the line, and after quitting the editor
  Kestrel is back, redrawn, with the search still open.
- [ ] **Results tab.** `ctrl-l` in the search. Expect: a new tab listing the files with matches; `esc`
  turns it into the real listing.

## 6. Palette, help, which-key, config

- [ ] **Palette.** `:` then type `sort`. Expect: matching commands with their keys on the right;
  `enter` runs one; `esc` closes.
- [ ] **Help.** `?`. Expect: all commands grouped by category; `/` filters; `esc` closes.
- [ ] **Which-key.** Press `g` and wait. Expect: after about 300 ms a popup lists `g h`, `g d`, ...,
  `g r`, `g w`, `g a`, `g X`, `g p`, `g j`. Press a second key before 300 ms: no popup flashes.
- [ ] **Custom keymap.** Add to `config.toml`: `[keymap.normal]` with `"H" = "dir.parent"`. Run
  **Reload config** from the palette. Expect: `H` goes to the parent. Add an invalid line
  (`"x" = "no.such.command"`) and reload: the status line shows the error and the rest still works.
- [ ] **Themes.** Palette, **Switch theme**, `paper`, then back to `ember`. Expect: all colours change
  at once, including the syntax colours in the preview of a `.rs` file.

## 7. File operations

Work in a scratch directory.

- [ ] **Big copy.** Copy a file of about 10 GB (or the largest you have) to another directory:
  `y`, move, `p`. Expect: progress with percentage, throughput and ETA in the prompt line; the UI stays
  responsive.
- [ ] **Jobs view.** `g j` during the copy. Expect: the job with a progress bar; `x` cancels it, and
  the partial `.part` file disappears. `esc` closes the view.
- [ ] **Conflicts.** Copy files into a directory that already has the same names. Expect: a dialog
  with `o` overwrite, `s` skip, `r` rename, `k` keep both, `a` apply to all, `c` cancel; `enter`
  keeps both, `esc` cancels the job. Try each key once.
- [ ] **Delete and undo.** `d` on a file: it goes to the Recycle Bin (check it there). `u` restores
  it.
- [ ] **Permanent delete.** `D`. Expect: a dialog that requires typing `yes` (for two or more items
  the count); `enter` on an empty or wrong answer does nothing destructive.
- [ ] **Rename and create.** `r` renames (cursor before the extension). `a` creates a file; `a`
  with `sub/dir/` creates a nested directory. Expect: the cursor lands on the new item.
- [ ] **Clipboard path.** `Y`, then `a`, `r`, `n` in turn; paste elsewhere. Expect: absolute path,
  relative path, name.
- [ ] **Open.** `o` opens a file with its default app; `O` shows a menu of apps (`1`..`9`).
- [ ] **Shell command.** `!` and run `dir` or `git status`. Expect: output streams into a pager;
  `ctrl-c` stops a long command (`ping -t localhost`); `esc` closes.
- [ ] **Bulk rename (GUI editor).** Mark three files, `R`, with `code` as editor. Change names, save,
  close the tab. Expect: a preview `old -> new`; `enter` applies it; `u` undoes the whole batch.
- [ ] **Bulk rename (terminal editor).** Set `[editor] open = "nvim"` (or `hx`), repeat. Expect:
  Kestrel suspends while the editor runs and returns redrawn.
- [ ] **Editor.** `e` opens the file in the configured editor (GUI detached; terminal suspends and
  resumes).
- [ ] **Quit with a job running.** Start a big copy, press `q`. Expect: a confirmation instead of an
  immediate exit; cancelling it keeps the job going; confirming ends the job and quits.

## 8. Tabs, bookmarks, frecency, shell integration

- [ ] **Tabs.** `t` opens a tab in the same directory (the header shows `1 ▏2`); navigate in each;
  `1` / `2` switch and each keeps its directory; `ctrl-w` closes; the last tab refuses to close.
- [ ] **Bookmarks.** `m` then `a` in a directory; elsewhere `'` then `a` jumps back. `M` lists
  bookmarks, `enter` jumps, `d` deletes. Restart `ks`: the bookmark is still there.
- [ ] **Frecency.** Visit several directories, press `z`, type part of a name, `enter`. Expect: it jumps.
  Quit and restart: the history persists (`frecency.toml` appears in the state folder).
- [ ] **Shell init, PowerShell 7.** Add `ks --print-shell-init pwsh | Out-String | Invoke-Expression`
  to `$PROFILE`, restart the shell. Run `k`, navigate somewhere, press `Q`. Expect: the shell is now
  in that directory. Run `k` again and quit with `q`: the shell stays where it was.
- [ ] **Shell init, Windows PowerShell 5.1.** Repeat in `powershell.exe`. Do it once in a directory
  with spaces and once with non-ASCII characters (for example `C:\temp\Zażółć gęślą jaźń`).
  Expect: `Q` changes into it correctly.
- [ ] **Odd names.** Navigate, preview, rename and copy within directories with spaces and
  non-ASCII names. Expect: no garbled names, no errors.

## 9. Claude Code

Needs a real `claude` logged in with a Pro/Max account (`claude auth status` shows
`"authMethod": "claude.ai"`). Remember: you type everything into Claude yourself, and every
exchange counts against your plan. Keep it to the minimum below.

- [ ] **Subscription with an API key set.** In the shell: `$env:ANTHROPIC_API_KEY = "sk-ant-bogus"`,
  then `ks` in a scratch directory, press `c`. Expect: no billing warning, the pane starts. Type
  `/status` in the pane by hand. Expect: it shows the subscription login, not an API key. Close with
  `g X`. (Kestrel stripped the variable from the child.)
- [ ] **Billing dialog.** Create a fake `claude.cmd` in an empty directory, say `C:\fake\claude.cmd`:

  ```bat
  @echo off
  if "%1"=="auth" goto auth
  echo fake claude started
  echo ANTHROPIC_API_KEY=[%ANTHROPIC_API_KEY%]
  pause
  exit /b 0
  :auth
  echo {"loggedIn":true,"authMethod":"api_key"}
  exit /b 0
  ```

  Set `[claude] binary = 'C:\fake\claude.cmd'`, reload, press `c`. Expect: a dialog "Claude would
  bill your API key; aborting". `enter` does nothing, `esc` closes it and nothing starts. Press `c`
  again and press `y`: the fake starts and prints `ANTHROPIC_API_KEY=[]` even though the variable is
  set in your shell. Remove the `binary` line afterwards.
- [ ] **Not installed / not logged in.** Set `binary = 'C:\nowhere\claude.exe'`, reload, press `c`.
  Expect: install instructions. (Optional: `CLAUDE_CONFIG_DIR` pointing at an empty folder before
  starting `ks` makes the real `claude` report a logged-out state; expect the "not logged in"
  dialog.)
- [ ] **Pane use.** With the real `claude` in the pane: type a character, press `esc`, press
  `shift-tab`. Expect: they reach Claude (its mode line changes on `shift-tab`), not Kestrel.
- [ ] **Focus.** `ctrl-\` returns to the explorer (the pane border shows `ctrl-\ focus`); `j` moves the
  list again; `ctrl-\` gives the pane focus back. On a layout where `\` needs AltGr, set
  `focus_key` and repeat.
- [ ] **Size and position.** `alt-=` and `alt--` grow and shrink the pane; `g p` moves it to the
  right and back. Drag-resize the window. Expect: Claude's screen re-wraps correctly each time.
- [ ] **Scrollback.** After some output, `shift-pageup` / `shift-pagedown` in the focused pane scroll
  its history.
- [ ] **Windows Terminal split.** Close the pane (`g X`), press `g w`. Expect: a new vertical split
  in the same Windows Terminal window starts Claude in the current directory. Type `/status`: still
  the subscription login even with `ANTHROPIC_API_KEY` set in the shell that started `ks`.
- [ ] **Mentions.** Mark two files (`space`), one in a directory whose name contains a space, then
  `C`. Expect: the pane gets focus and shows `@relative/path` mentions separated by spaces (the
  quoted `@"dir name/file"` form for the spaced one). Do not press enter. **Check:** does Claude show
  them as file mentions? If not, note it: the quoting is an unverified guess.
- [ ] **MCP tools.** In the pane, type `/mcp` by hand. Expect: a `kestrel` server, connected, with
  the tools `get_selection`, `search_files`, `search_contents`, `list_dir`, `reveal`. If you
  choose to use one (this sends a prompt, your decision), ask Claude to reveal a file and watch the
  explorer cursor move.
- [ ] **Edits are marked.** Ask Claude (by hand) to make a small edit in a scratch file. Expect:
  the pane border shows `◆ Edit <file>` at its right end, the file gets a `◆` in the list (a
  directory above it a `◇`). `g a` opens the Claude changes view listing the file; `d` shows the
  diff; `enter` reveals the file; `X` clears the list. (Kestrel does not log hook payloads; the
  marker and the tool name in the border are the evidence that `ks hook` received it. With
  `KESTREL_LOG=debug`, a `hook forward failed` line appears if it did not.)
- [ ] **Exit and resume.** `g X`, then `g r`. Expect: Claude restarts with the same session (its
  previous conversation is there). `g r` while one is running says to close it first.
- [ ] **Paste.** Paste a multi-line text into the focused pane. Expect: it arrives as one paste,
  nothing is submitted until you press `enter` yourself.
- [ ] **Quit with a session.** `q` while Claude runs. Expect: a confirmation.
- [ ] **No leaked credentials.** Open the newest log file:
  `Select-String -Path "$log\kestrel.*.log" -Pattern 'credentials','sk-ant','token'`.
  Expect: no secrets (Kestrel never reads credentials; a stray word match in a message is fine, an
  actual value is not). Also check `%TEMP%\kestrel-<pid>\` while running: only `mcp.json` and
  `settings.json`, no keys. Check that the project directory has no `.mcp.json` and that
  `claude mcp list` (run outside Kestrel) shows no `kestrel` entry.

## 10. Performance

- [ ] **Start-up.** Run `ks` from a warm shell. Expect: the first frame appears with no noticeable delay
  (budget: under 60 ms from launch). Kestrel does not log a first-frame line; compare the
  `starting` and `image protocol ... took_ms=` lines in the newest log (the second shows how long
  the terminal query took, at most 250 ms in a terminal that never answers) and judge the launch
  by eye or with `Measure-Command { ks --version }` for the bare process cost.
- [ ] **Keypress latency.** `cargo test --release -p ks-ui keypress_to_frame_stays_within_budget -- --nocapture`.
  Expect: p99 under 16 ms (measured about 0.5 ms).
- [ ] **Ignored performance tests.** Run each in release mode, read the numbers it prints:

  ```powershell
  cargo test -p ks-app --release --test navigation a_directory_of_100k -- --ignored --nocapture
  cargo test -p ks-app --release --test search -- --ignored finder_keystroke --nocapture
  cargo test -p ks-search --release -- --ignored finder_keystroke_latency --nocapture
  cargo test -p ks-core --release -- --ignored --nocapture z_ranking_time
  cargo test -p ks-app --release --lib -- --ignored --nocapture perf
  cargo test -p ks-claude --release --lib -- --ignored --nocapture perf ipc_latency
  cargo test -p ks-fs --release --test ops_bench -- --ignored --nocapture      # copies ~1 GiB; KS_BENCH_DIR / KS_BENCH_MIB choose volume and size
  cargo bench -p ks-search                                                       # walk, fuzzy and grep throughput; KS_BENCH_FILES / KS_BENCH_PATHS scale it
  ```

  Expect: 100k-entry listing complete in under 1 s (first rows under 150 ms), finder keystroke to
  results under 25 ms on 500k paths, all asserts passing.
- [ ] **Grep versus ripgrep.** If `rg` is installed, in a large repository time
  `Measure-Command { rg --count "TODO" . }` and compare with the time until the footer of
  `ctrl-f` for `TODO` stops spinning. Expect: the same order of magnitude. Record both numbers in
  `docs/progress/` (this comparison has not been measured yet).
