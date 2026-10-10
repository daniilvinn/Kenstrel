# Engine: ops (phase P6 backend)

Branch `engine/ops`, worktree `E:\dev\Kestrel-wt\ops`. Lives entirely in `crates/ks-fs` (`src/ops/**`,
`src/launch/**`, `tests/ops_*.rs`). No dependency on ks-core; ks-core/ks-ui/ks-app untouched.
`crates/ks-fs/src/lib.rs` only has the two `pub mod` lines and a doc comment.

## Done

- **Job queue** (`ops::JobManager`): dedicated `std::thread` per job (`ks-job-N`), at most 2 concurrent by
  default (`with_concurrency`), FIFO queue for the rest. `JobId`, caller `Generation` echoed in every event.
  Kinds: Copy, Move, Trash, DeletePermanent, Create, Rename, BulkRename, plus Restore (inverse of Trash, used by undo).
- **Progress**: bytes/files done and total, current file, 3 s windowed throughput, ETA. Throttled to ~10/s; the
  first copied chunk always reports. Cancellable pre-scan with `Scanning` events. Move does not pre-scan
  (same-volume rename is O(1)); totals grow if an item falls back to copy+delete.
- **Cancel**: `tokio_util::CancellationToken`, checked between 4 MiB chunks and between entries. Files are
  copied to `.ks-<job>-<n>.part` next to the target and renamed into place, so cancel/failure never leaves a
  partial target and never destroys a file being overwritten. Queued jobs cancel instantly. Dropping the manager
  cancels everything.
- **Conflicts**: `ConflictPrompt{job, src, dst, src_meta, dst_meta}` pauses the job; answer with
  `JobManager::resolve(job, Resolution{choice, apply_to_all})`. Choices: Overwrite, Skip, Rename(name), KeepBoth
  (`name (1).ext`, `a.tar.gz` -> `a.tar (1).gz`), Cancel. Non-interactive `ConflictPolicy`: Ask, Overwrite, Skip,
  KeepBoth, Fail.
- **Links**: never followed (copy, scan, delete). Copy recreates the link as a symlink (error item
  "link skipped" if not permitted); move renames the link itself; delete removes only the link. Tested with real
  junctions (`mklink /J`).
- **Move**: `rename`; on cross-device error (Windows 17 / EXDEV) copy+delete (source deleted only if the copy had
  no errors). Tested for real across C: and E:.
- **Trash** via `trash` crate; trash ids are resolved by diffing `os_limited::list()` before/after.
  **Permanent delete**: recursive, read-only cleared (files and dirs), links removed not followed.
  `confirmation_token(n)` / `confirmation_matches(n, input)` for the typed confirm.
- **Create**: `parse_create_input(base, "a/b/c.txt" | "dir\\")`, nested creation, never clobbers, validation
  (`validate_name`: invalid chars, reserved names with/without extension, trailing dot/space, `.`/`..`, > 255).
- **Bulk rename**: `BulkRename::{new, edit_text, write_edit_file, plan, plan_from_file}` -> `RenamePlan`
  `{changes, steps, preview(), into_spec()}`. Validates line count, empty, duplicates (case-insensitive on
  Windows), invalid names, collisions with non-participating files; reports all problems at once. Swaps/cycles use
  `.ks-tmp-N` names; chains are ordered so no temp is needed.
- **Undo** (`UndoLog`, driven by the manager): rename, move, create, trash, copy, bulk rename (one entry).
  Validated on undo with size/mtime fingerprints; changed state gives `UndoError::Changed(reason)` and drops the entry.
- **Long paths / Unicode**: all paths absolutized at submit; `std::fs` adds `\\?\` itself over MAX_PATH;
  `ops::paths::{verbatim, display, path_key, relative_to}`.
- **launch/**: `clipboard` (formats + `Clipboard` trait, `SystemClipboard` (arboard), `MemoryClipboard`),
  `open` (`open_default` detached via `open` crate, `open_with_handlers`/`launch_handler`), `editor`
  (`resolve_editor`, templates, `build_open_command`, `spawn_detached`, `run_foreground`, `find_in_path` with
  PATHEXT), `shell` (`spawn_shell` streaming lines, exit code, cancel with `taskkill /T /F`, CREATE_NO_WINDOW).

## Public API overview (how wiring should use it)

```rust
use ks_fs::ops::*;
// 1. one manager; the sink must be cheap (forward into the app's Msg channel)
let mgr = JobManager::new(move |ev: JobEvent| { let _ = tx.send(Msg::Job(ev)); });  // or an mpsc::Sender<JobEvent>
// 2. submit (returns immediately); pass the current generation
let id = mgr.submit(JobSpec::copy_into(&sources, &dest_dir), Generation(gen));
//    other specs: move_into, rename(from,new_name)?, Trash{paths}, DeletePermanent{paths},
//    Create{entries: vec![parse_create_input(&cwd, "a/b.txt")?]}, plan.into_spec()
// 3. events -> Msg 1:1:
//    Queued/Started/Scanning/Progress -> job registry + status line; ConflictPrompt -> overlay;
//    Done/Failed/Cancelled(JobReport) -> toast + refresh; report.outcomes tells what to select/refresh
// 4. mgr.resolve(job, Resolution::once(ConflictChoice::KeepBoth)) / Resolution::all(..)
// 5. mgr.cancel(job); mgr.jobs() for the Jobs overlay; mgr.undo(gen)? ; mgr.undo_description()
```

- `u` (undo): `mgr.undo(generation)` returns `Err(UndoError::{Empty, Changed(msg), Unsupported(msg)})` for the status line.
- `D`: show `confirmation_token(count)`, accept with `confirmation_matches`.
- `R`: `BulkRename::new(cwd, names)`; `write_edit_file()`; app suspends the TUI, runs
  `launch::run_foreground(&build_open_command(&editor, &path, None, None, None))` (terminal editors) or
  `spawn_detached` + wait UX (GUI editors need `-w`/`--wait`; `resolve_editor` keeps args from `$EDITOR`);
  then `plan_from_file` -> show `plan.preview()` -> confirm -> `mgr.submit(plan.into_spec(), gen)`; delete the temp file.
- `y`-register paste: `Copy`/`Move` are built from the register with `copy_into`/`move_into`; same-directory
  paste: use `ConflictPolicy::KeepBoth` (tested) or Ask.
- `e` / `o` / `O` / `!`: see `launch` docs. Terminal editors (`Editor::terminal`) need the alternate screen left
  before `run_foreground` and a full redraw after. `!` uses `spawn_shell` (streaming) or `run_foreground` for TTY programs.
- `Y`: `launch::format_paths(&paths, PathFormat::..., &cwd)` + `SystemClipboard.set_text`.

## Deviations (with reasons)

- Extra `JobKind::Restore` / `JobSpec::Restore`: undo of trash needs a job that restores; it is the "inverse job".
- `Done` means no item errors; `Failed` means at least one item error (other items may have succeeded; report has
  both). Spec only said "Done/Failed/Cancelled with per-item errors".
- Rename and BulkRename are separate specs: `Rename{pairs}` uses the conflict machinery; `BulkRename{steps}` is an
  ordered, no-overwrite, stop-on-first-error sequence (so a cycle can never clobber data).
- Directory onto existing directory merges silently (conflicts are asked per file) instead of prompting for the directory.
- Copies that overwrite an existing file are not recorded for undo (the original is gone); merged directory copies
  record only the new leaves.
- No `std::fs::copy`/CopyFileEx: chunked read/write (needed for cancellation). Alternate data streams / ACLs are not copied;
  mtime and the read-only attribute are.
- `open_with_handlers` does not read the registry `OpenWithList`; it lists Notepad, VS Code (if on PATH), "Explorer (reveal)"
  and the system "Choose another app..." dialog (`rundll32 shell32.dll,OpenAs_RunDLL`). Registry handlers would need
  App Paths resolution; left as a later enhancement.
- Shell tokenizer for editor commands/templates is custom (quotes only, backslashes literal) instead of `shlex`, because
  `shlex` treats `\` as an escape and would corrupt Windows paths. `shlex` was therefore not added.
- `ops::paths::display` keeps the `\\?\` prefix for paths longer than MAX_PATH (that is dunce's rule: stripping it would
  produce an unusable path). `path_key` strips it unconditionally for comparisons.

## VERIFY results

- **Trash restore on Windows via `trash::os_limited`**: WORKS. `undo_trash_restores_from_recycle_bin` trashes a directory
  (with a non-ASCII name) and a file in a temp dir, then restores both through `os_limited::list()` + `restore_all`; contents verified.
  The trash crate does not return ids from `delete`, so ids are found by diffing the bin listing (before/after) and
  matching `original_path()` (compared case-insensitively, `\\?\`-insensitive).
- **Limitation found**: the shell restore API rejects originals longer than MAX_PATH (`E_INVALIDARG`, 0x80070057). Trash
  of such a file works, the undo then fails cleanly (error item, item stays in the bin). Covered by `trash_and_restore_deep_file`.
  The bin also reports long original paths with a `\\?\` prefix.
- `FileType::is_symlink()` is true for junctions as well as symlinks on this toolchain (verified with `mklink /J`
  tests: copy/delete/move never touch the junction target).
- Cross-volume move verified for real (C: temp dir -> E: build dir).
- Gates: `cargo fmt --all` clean, `cargo clippy --workspace --all-targets -- -D warnings` clean, `cargo test --workspace`
  clean (ks-fs: 28 unit + 5 paths + 24 transfer + 6 delete/create + 14 undo tests incl. `track_trash_ids=false`; 1 ignored bench).

## Added dependencies

crates/ks-fs: `arboard 3.6.1`, `open 5.4.4`, `trash 5.2.9`; workspace deps `dunce`, `thiserror`, `tokio-util`, `tracing`;
`tempfile` (also normal dep for the bulk-rename temp file). Root `Cargo.toml` unchanged (all workspace deps already existed); `Cargo.lock` updated.
`tokio-util` is used only for `CancellationToken` (no tokio runtime is required).

## Benchmarks/measurements

`cargo test -p ks-fs --release --test ops_bench -- --ignored --nocapture` (1 GiB incompressible file, 4 MiB chunks,
NVMe, source freshly written so it is in the page cache): **0.30 s, about 3.4 GiB/s** (debug build measured the same,
IO bound). 200 MB cancel test finishes in well under 100 ms.
Note: the cancel test cancels from inside the sink on the first progress event, because a 200 MB copy completes in
~70 ms here and a timing-based cancel would race.

## Open issues / wiring TODO

- Wire `JobManager` into ks-app (effects for y/x/p/D/d/a/r/R/u), a Jobs overlay from `mgr.jobs()`, and a conflict overlay
  from `ConflictPrompt` (keys: o/s/r/k, `A` for apply-to-all).
- Terminal suspend/resume around `run_foreground` for terminal editors and for `R` (bulk rename edit).
- GUI editors for bulk rename: caller must pass a wait flag (e.g. `code -w`) via `$EDITOR`, otherwise the file is read before saving.
- Directory timestamps and ACLs are not preserved on copy; hard links are copied as independent files.
- Symlink recreation on Windows needs Developer Mode/elevation; otherwise copied links are reported as skipped.
- Trash listing cost scales with the size of the Recycle Bin (two `list()` calls per Trash job). Added
  `JobOptions::track_trash_ids` (default true; only effective with `record_undo`). Set it false to skip the listings
  (such trashes then cannot be undone via restore).
- Registry-based "Open with" handlers (`OpenWithList`) not implemented.
- Tests use the real Recycle Bin and purge only their own entries (`purge_trash_under`).

## Review round (post-review fixes)

- Overwrite is now non-destructive: a conflicting destination (moves, dir/file mismatches) is renamed to a hidden
  sibling `.<name>.ks-old-*`, the transfer runs, then the sibling is deleted. Cancel restores the old item; a partly
  failed transfer keeps the old item beside the result and reports its path. Same-volume moves use the same path.
- `JobManager::resolve` returns false unless the job is awaiting a conflict; the runner also drains stale answers
  before each prompt. Queued is now always the first event of a job. `cancel()` true = request registered only.
- Job threads: concurrency slot is freed by a drop guard around a catch_unwind of the whole job (sink panics no longer
  stall the queue); 64 MiB virtual stack for deep trees (tested with 1500 levels). Drop cancels but does not join
  (documented; use `wait_idle`).
- Shell runner: output lines are sanitized (CSI/OSC/control chars removed), capped at 64 KiB, and no Line follows the
  terminal event (gate shared with the supervisor). `ShellId` vs `JobId` separation documented.
- Editor: unquoted path with spaces in `$EDITOR` is tried as a whole first; `emacs -nw` counts as terminal.
- Bulk rename temp file is created exclusively with an unpredictable name (tempfile is now a normal dependency).
- Copy emits a final `Scanning` event with exact totals. New tests in `tests/ops_review.rs`, trash tests no longer
  pass vacuously on Windows, long-path trash undo asserted to fail cleanly.
- Bench: `KS_BENCH_DIR` / `KS_BENCH_MIB` env vars; release 1 GiB: 0.31 s (3343 MiB/s) on C: temp, 0.29 s (3515 MiB/s)
  on E:; both page-cache to page-cache (see Open issues).

Rejected/deferred findings: see Open issues below.

## Open issues (review deferrals)

- Trash batching (`trash::delete_all`): deferred. Per-path calls give per-item errors and cancellation between items;
  the Recycle Bin is listed twice per job (not per path). Can be optimized later if profiling shows need.
- Bulk rename takes `String` names; items whose names are not valid Unicode cannot round-trip through the editor.
  The wiring should exclude them from the selection (an `OsString` API is a larger change, deferred).
- Cold-read benchmark / chunk-size comparison not automated (needs cache drop or another volume).
- Shell runs are not in `JobManager::jobs()`; wiring keeps its own table keyed by `ShellId`.
- Windows `is_terminal_editor` list is heuristic; configure via template for anything unusual.
