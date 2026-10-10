# Keymap

> Generated from the built-in keymap and command registry. Do not edit by hand:
> run `cargo run -p ks-app -- --dump-keymap > docs/keymap.md` (or
> `KESTREL_UPDATE_DOCS=1 cargo test -p ks-core docs`) after changing a default binding.

Every command is also available in the command palette (`:`) and listed in the help
overlay (`?`), both of which show your own bindings after rebinding.

## Rebinding

Add `[keymap.<context>]` tables to `config.toml`. The key is a space-separated key
sequence, the value a command id from the tables below:

```toml
[keymap.normal]
"ctrl-t" = "tab.new"          # add a binding
"d" = "file.delete"           # rebind an existing key
"q" = "none"                  # unbind
"m <char>" = "bookmark.set"   # <char> passes the next typed key to the command
```

Contexts: `normal`, `visual` (inherits `normal`), `prompt`, `filter`, `finder`, `grep`,
`palette`, `help`, `confirm`, `jobs`, `claude`. Binding a prefix (`g`) removes the longer
defaults that start with it (`g g`, `g h`, ...); the status line tells you when that
happens. Key names: single characters, `space`, `enter`, `esc`, `tab`, `backspace`,
`delete`, `up`, `down`, `left`, `right`, `home`, `end`, `pageup`, `pagedown`, `f1`..`f12`,
with `ctrl-`, `alt-` prefixes (`ctrl-p`, `alt-.`). Reload the file without restarting from
the palette (`Reload config`).

## Normal mode

### Navigation

| Keys | Command | Id | What it does |
|---|---|---|---|
| `j`, `down` | Move cursor down | `cursor.down` | Move the cursor one row down. |
| `k`, `up` | Move cursor up | `cursor.up` | Move the cursor one row up. |
| `h`, `left`, `backspace` | Go to parent directory | `dir.parent` | Go to the parent directory, keeping the cursor on the directory you left. |
| `l`, `right`, `enter` | Enter directory / open file | `dir.enter` | Enter the directory under the cursor, or open the file. |
| `g g`, `home` | Jump to top | `cursor.top` | Jump to the first entry. |
| `G`, `end` | Jump to bottom | `cursor.bottom` | Jump to the last entry. |
| `ctrl-d`, `pagedown` | Scroll half a page down | `page.half_down` | Scroll half a page down. |
| `ctrl-u`, `pageup` | Scroll half a page up | `page.half_up` | Scroll half a page up. |
| `ctrl-o` | History: back | `history.back` | Go back to the previous directory. |
| `tab` | History: forward | `history.forward` | Go forward again after going back. |
| `~`, `g h` | Go to home directory | `go.home` | Jump to your home directory. |
| `g d` | Go to Downloads | `go.downloads` | Jump to the Downloads folder. |
| `g D` | Go to Desktop | `go.desktop` | Jump to the Desktop folder. |
| `g c` | Go to config directory | `go.config` | Jump to Kestrel's own config directory. |
| `g /` | Go to This PC (drives) | `go.drives` | Show all drives with free space. |

### Search

| Keys | Command | Id | What it does |
|---|---|---|---|
| `/` | Filter current directory | `filter.open` | Fuzzy-filter the current directory as you type. |
| `ctrl-p`, `f` | Find file (recursive fuzzy) | `find.file` | Find a file by fuzzy path match, recursively. |
| `ctrl-f`, `s` | Search file contents | `search.contents` | Search inside files (live grep). |

### Selection

| Keys | Command | Id | What it does |
|---|---|---|---|
| `space` | Toggle mark | `mark.toggle` | Mark or unmark the entry under the cursor. |
| `v` | Visual (range select) mode | `mode.visual` | Mark a range of entries while moving the cursor. |
| `ctrl-a` | Mark all | `mark.all` | Mark every visible entry. |

### File operations

| Keys | Command | Id | What it does |
|---|---|---|---|
| `y` | Copy | `clipboard.copy` | Copy the marked entries (or the one under the cursor). |
| `x` | Cut | `clipboard.cut` | Cut the marked entries to move them. |
| `p` | Paste | `clipboard.paste` | Paste copied or cut entries into this directory. |
| `Y` | Copy path to clipboard | `path.copy` | Copy absolute path(s) to the clipboard. |
| `r` | Rename | `file.rename` | Rename the entry under the cursor. |
| `R` | Bulk rename in editor | `file.bulk_rename` | Rename many entries by editing their names in your editor. |
| `a` | Create file or directory | `file.create` | Create a file; a trailing slash creates a directory. |
| `d` | Move to Recycle Bin | `file.trash` | Move the entries to the Recycle Bin. |
| `D` | Delete permanently | `file.delete` | Delete the entries for good after typed confirmation. |
| `u` | Undo last file operation | `file.undo` | Undo the last rename, move, create or trash. |
| `o` | Open with default app | `open.default` | Open with the default application. |
| `O` | Open with... | `open.with` | Choose which application opens the file. |
| `e` | Open in editor | `open.editor` | Open in your editor. |
| `!` | Run shell command | `shell.run` | Run a shell command in this directory. |

### View

| Keys | Command | Id | What it does |
|---|---|---|---|
| `.` | Toggle hidden files | `view.hidden` | Show or hide hidden files. |
| `S n` | Sort by name | `sort.name` | Sort by name; again to reverse. |
| `S s` | Sort by size | `sort.size` | Sort by size; again to reverse. |
| `S m` | Sort by modified time | `sort.mtime` | Sort by modified time; again to reverse. |
| `S e` | Sort by extension | `sort.ext` | Sort by extension; again to reverse. |
| `w` | Toggle preview | `preview.toggle` | Show or hide the preview pane. |
| `P` | Toggle parent column | `preview.parent_column` | Show or hide the parent directory column. |
| `i` | Toggle metadata view | `preview.metadata` | Switch the preview between content and metadata. |
| `J` | Scroll preview down | `preview.scroll_down` | Scroll the preview down. |
| `K` | Scroll preview up | `preview.scroll_up` | Scroll the preview up. |

### Tabs and bookmarks

| Keys | Command | Id | What it does |
|---|---|---|---|
| `m <char>` | Set bookmark | `bookmark.set` | Remember this directory under a key. |
| `' <char>` | Jump to bookmark | `bookmark.jump` | Jump to the directory saved under a key. |
| `M` | List bookmarks | `bookmark.list` | List the saved bookmarks and jump to one. |
| — | Delete bookmark | `bookmark.delete` | Delete the selected bookmark in the bookmark list. |
| `z` | Frecency jump | `frecency.jump` | Jump to a frequently or recently used directory. |
| `t` | New tab | `tab.new` | Open a new tab in this directory. |
| `1-9` | Switch to tab | `tab.goto` | Switch to a numbered tab. |
| `ctrl-w` | Close tab | `tab.close` | Close the current tab. |

### Claude Code

| Keys | Command | Id | What it does |
|---|---|---|---|
| `c` | Open Claude | `claude.open` | Open Claude Code beside the explorer. |
| `C` | Send marked files to Claude | `claude.send_marked` | Send the marked files to Claude as @ mentions. |

### Application

| Keys | Command | Id | What it does |
|---|---|---|---|
| `:` | Command palette | `palette.open` | Open the command palette. |
| `?` | Help | `help.open` | Show all key bindings and commands. |
| `g j` | Jobs overlay | `jobs.open` | Show running background jobs. |
| — | Cancel selected job | `job.cancel` | Cancel the selected job in the jobs overlay. |
| — | Clear finished jobs | `jobs.clear` | Drop finished jobs from the jobs overlay. |
| `esc` | Cancel / close | `cancel` | Close the top overlay, leave the mode, clear marks. |
| — | Accept / confirm | `accept` | Confirm in prompts and overlays. |
| — | Reload config | `config.reload` | Re-read config.toml, the keymap and the theme. |
| — | Switch theme | `theme.switch` | Choose a colour theme for this session. |
| — | Switch icon set | `icons.switch` | Choose nerd, unicode or ascii icons for this session. |
| `q`, `ctrl-c` | Quit | `quit` | Quit Kestrel. |
| `Q` | Quit and cd shell to current directory | `quit.cd` | Quit and make the shell wrapper cd to this directory. |

## Other contexts

### `visual` (also uses every `normal` binding it does not override)

| Keys | Command |
|---|---|
| `esc` | `cancel` |
| `v` | `mode.visual` |

### `prompt`

| Keys | Command |
|---|---|
| `esc` | `cancel` |
| `ctrl-c` | `cancel` |
| `enter` | `accept` |

### `filter`

| Keys | Command |
|---|---|
| `esc` | `cancel` |
| `ctrl-c` | `cancel` |
| `enter` | `accept` |

### `finder`

| Keys | Command |
|---|---|
| `esc` | `cancel` |
| `ctrl-c` | `cancel` |
| `enter` | `accept` |

### `grep`

| Keys | Command |
|---|---|
| `esc` | `cancel` |
| `ctrl-c` | `cancel` |
| `enter` | `accept` |

### `palette`

| Keys | Command |
|---|---|
| `esc` | `cancel` |
| `ctrl-c` | `cancel` |
| `enter` | `accept` |
| `down` | `cursor.down` |
| `ctrl-n` | `cursor.down` |
| `up` | `cursor.up` |
| `ctrl-p` | `cursor.up` |
| `pagedown` | `page.half_down` |
| `pageup` | `page.half_up` |

### `help`

| Keys | Command |
|---|---|
| `esc` | `cancel` |
| `q` | `cancel` |
| `?` | `cancel` |
| `j` | `cursor.down` |
| `down` | `cursor.down` |
| `k` | `cursor.up` |
| `up` | `cursor.up` |
| `ctrl-d` | `page.half_down` |
| `pagedown` | `page.half_down` |
| `ctrl-u` | `page.half_up` |
| `pageup` | `page.half_up` |
| `g g` | `cursor.top` |
| `home` | `cursor.top` |
| `G` | `cursor.bottom` |
| `end` | `cursor.bottom` |
| `/` | `filter.open` |

### `confirm`

| Keys | Command |
|---|---|
| `esc` | `cancel` |
| `ctrl-c` | `cancel` |
| `enter` | `accept` |

### `jobs`

| Keys | Command |
|---|---|
| `esc` | `cancel` |
| `q` | `cancel` |
| `x` | `job.cancel` |
| `c` | `jobs.clear` |
| `j` | `cursor.down` |
| `k` | `cursor.up` |
| `down` | `cursor.down` |
| `up` | `cursor.up` |
| `g g` | `cursor.top` |
| `G` | `cursor.bottom` |

### `bookmarks`

| Keys | Command |
|---|---|
| `esc` | `cancel` |
| `ctrl-c` | `cancel` |
| `q` | `cancel` |
| `enter` | `accept` |
| `j` | `cursor.down` |
| `down` | `cursor.down` |
| `k` | `cursor.up` |
| `up` | `cursor.up` |
| `d` | `bookmark.delete` |
| `delete` | `bookmark.delete` |

### `jump`

| Keys | Command |
|---|---|
| `esc` | `cancel` |
| `ctrl-c` | `cancel` |
| `enter` | `accept` |
| `down` | `cursor.down` |
| `ctrl-n` | `cursor.down` |
| `ctrl-j` | `cursor.down` |
| `up` | `cursor.up` |
| `ctrl-p` | `cursor.up` |
| `ctrl-k` | `cursor.up` |

### `conflict`

| Keys | Command |
|---|---|
| `esc` | `cancel` |
| `ctrl-c` | `cancel` |
| `enter` | `accept` |

### `pager`

| Keys | Command |
|---|---|
| `esc` | `cancel` |
| `q` | `cancel` |
| `ctrl-c` | `job.cancel` |
| `j` | `cursor.down` |
| `k` | `cursor.up` |
| `down` | `cursor.down` |
| `up` | `cursor.up` |
| `g g` | `cursor.top` |
| `G` | `cursor.bottom` |
| `ctrl-d` | `page.half_down` |
| `ctrl-u` | `page.half_up` |
| `pagedown` | `page.half_down` |
| `pageup` | `page.half_up` |

### `menu`

| Keys | Command |
|---|---|
| `esc` | `cancel` |
| `q` | `cancel` |
| `ctrl-c` | `cancel` |
| `enter` | `accept` |
| `j` | `cursor.down` |
| `k` | `cursor.up` |
| `down` | `cursor.down` |
| `up` | `cursor.up` |
