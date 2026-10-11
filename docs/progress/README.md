# Progress notes (detailed history)

[PROGRESS.md](../../PROGRESS.md) is the consolidated, current record (per phase: Done, Deviations,
VERIFY results, Open issues, plus the audit rounds, the P8 measurements table and the consolidated open
issues). The files here are the per-track working notes it was built from. They are history: they are not
edited when a later track fixes something they list as open, so where a note and PROGRESS.md differ,
PROGRESS.md (and the code) win.

| Note | Track | Covers |
|---|---|---|
| [engine-preview.md](engine-preview.md) | engine/preview | `ks-preview` crate: preview kinds, cache, API, measurements (P2 backend) |
| [engine-search.md](engine-search.md) | engine/search | `ks-search` crate: walker, finder, cache, grep, benchmarks (P3 + P4 backend) |
| [engine-ops.md](engine-ops.md) | engine/ops | `ks-fs` job queue, conflicts, undo, bulk rename, launch helpers (P6 backend) |
| [engine-claude.md](engine-claude.md) | engine/claude | `ks-claude` crate: preflight, WT split, ConPTY pane, IPC, MCP, hooks (P10 backend) |
| [p2.md](p2.md) | phase/p2 | preview pane, parent column, images, theme syntax tokens |
| [p34.md](p34.md) | phase/p34 | filter, find file, content search wiring |
| [p5.md](p5.md) | phase/p5 | palette, help, which-key, keymap config, themes, `docs/keymap.md` generation |
| [p6.md](p6.md) | phase/p6 | file operations wiring, conflict/jobs/pager overlays, terminal suspend/resume |
| [p7.md](p7.md) | phase/p7 | tabs, bookmarks, frecency, persistence, shell init |
| [p10.md](p10.md) | phase/p10 | Claude Code wiring (10a to 10e) |
| [f1.md](f1.md) | fix/f1 | grep send/marks, preview line selection, link header, owner, deferred terminal query |
| [f2.md](f2.md) | fix/f2 | finder cache freshness, multi-tab watching, results stats, memory and rg measurements |
| [f3.md](f3.md) | fix/f3 | cut dimming, registry open-with, bulk re-edit, sweep by pid, help/hint-line contexts |
| [f4.md](f4.md) | fix/f4 | README, manual test checklist, example config/theme, CI and release workflows |
| [a1.md](a1.md) | audit/a1 | file ops, conflict dialog arming, foreground and event-loop fixes (round 1) |
| [a2.md](a2.md) | audit/a2 | routing, listing/git, search, preview/render fixes (round 1) |
| [a3.md](a3.md) | audit/a3 | persistence merge, Claude session/diff fixes, CI, flaky tests (round 1) |
| [b1.md](b1.md) | r2/b1 | bulk re-edit, parked tabs, windowed preview, pending unhide (round 2) |
| [b2.md](b2.md) | r2/b2 | bidi sanitising, finder case rename, columns, grapheme truncation (round 2) |
| [b3.md](b3.md) | r2/b3 | Claude session keying, git hardening, hook quoting, static CRT, watcher (round 2) |
| [b4.md](b4.md) | r2/b4 | deterministic flaky tests, persistence robustness (round 2) |
| [claude-wiring-ref.patch](claude-wiring-ref.patch) | reference | the P10 wiring patch written against an early base; reference only, never applied |

Related: [../spikes/claude-pty.md](../spikes/claude-pty.md) (the 10b embedding spike),
[../manual-test.md](../manual-test.md) (manual verification checklist),
[../keymap.md](../keymap.md) (generated, drift-tested), [../config.example.toml](../config.example.toml),
[../theme.example.toml](../theme.example.toml).

Notes for the round-3 track (`r3/c1`, timing flakes) will appear here when that branch merges.
