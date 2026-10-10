# Spike: embedding `claude` (Claude Code 2.1.296) in a ConPTY with vt100

Harness: `cargo run -p ks-claude --example pty_spike [secs] [cwd]` (sends no input except the
terminal cursor-position reply; no prompt is ever sent). Terminal 120x40, env stripped.

## Findings
- ConPTY emits `CSI 6 n` (cursor position request) at startup and produces **no further output
  until it is answered**. Without the reply the first run showed 4 bytes only. The reader thread
  must answer `ESC [ row ; col R`. Implemented in `ks_claude::pane`.
- After the reply, Claude Code renders its first screen (a "trust this folder" safety prompt for
  untrusted directories; both a temp dir and `E:\dev\Kestrel` were untrusted). `vt100` rendered it
  correctly: full-width rule, wrapped paragraph, `> No, exit / Yes, I trust this folder` list,
  footer. Cursor hidden (`?25l`). The post-trust main UI was not observed: reaching it needs
  keyboard input in a trusted directory (a launch in the vault dir was declined by the
  permission classifier), so it remains to be eyeballed manually.
- Sequences seen (counts in ~10 s): `?2026h/l` synchronized output (vt100 ignores it safely),
  `?2004h` bracketed paste (tracked by vt100: `screen().bracketed_paste()`), `?1004h` focus
  events x2, `?2031h` colour-scheme notifications, `?25l/h`, `ESC[?u` kitty keyboard query (x1,
  unanswered; Claude falls back to legacy keys), `ESC[>4;2m` modifyOtherKeys, `ESC[>0q` XTVERSION
  query, OSC 0 title x2, OSC 8 hyperlinks x2, OSC 7501 query, truecolor `38;2;r;g;b` x4
  (187 cells with RGB colours in the vt100 screen). No alternate screen (`?1049`), no mouse
  modes, no DECSCUSR cursor shapes, no OSC 9/777 notifications at startup.
- vt100 0.16.2 parsed everything cleanly: no garbage, no stray escape text, truecolor preserved.

## Verdict: **embed with vt100**
No need for `alacritty_terminal`. Caveats: vt100 never replies to queries (only DSR is answered by
us), does not expose the OSC title, and does not implement synchronized-output buffering (we
snapshot on a coalesced notification, so partial frames are possible but short-lived).

## CLI flags (VERIFY items from PLAN 10.2), from `claude --help`
- `--session-id <uuid>`: "Use a specific session ID for the conversation (must be a valid UUID)"
- `-r, --resume [value]`: "Resume a conversation by session ID, or open interactive picker"
- `--mcp-config <configs...>`: "Load MCP servers from JSON files or strings (space-separated)"
- `--settings <file-or-json>`: "Path to a settings JSON file or a JSON string to load additional settings from"
- `--strict-mcp-config`: "Only use MCP servers from --mcp-config, ignoring all other MCP configurations" (not used)
- `--bare` exists and ignores OAuth/keychain: never passed (the pane rejects it).
- `claude auth status [--json|--text]`: JSON default, exit 0 when logged in.

## ctrl-\
The interactive-mode shortcut reference (code.claude.com/docs/en/interactive-mode) lists no
`Ctrl+\` binding, so it is free as the pane focus toggle (on Windows ConPTY it arrives as 0x1C).
Kestrel must intercept it before forwarding; Claude itself does not use it.

## @-mentions
Docs: `@src/utils/auth.js` (files), `@src/components` (dirs), relative or absolute, several per
message. Paths with spaces and `#L10-20` ranges are not documented; see `ks_claude::mention`.
