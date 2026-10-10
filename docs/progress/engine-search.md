# Engine: search (P3 + P4 backend), crate `ks-search`

Self-contained library; no dependency on `ks-core`/`ks-ui`/`ks-app`, none of which were touched.

## Done
- `walk`: `ignore::WalkParallel` walker (`walk()`, `WalkOptions`, `WalkSink`, `WalkSummary`), hidden / gitignore toggles, no symlink/junction following, skip list (System Volume Information, $Recycle.Bin, pagefile/hiberfil/swapfile.sys, plus always `.git`), access-denied counted quietly, cancellable. Glob overrides builder (`build_overrides`). Cloud-placeholder attribute helpers.
- `scope`: `Scope {Cwd, GitRoot, Home, Drive}` with `.next()` cycling, `find_git_root`, `home_dir`, `drive_root`, `resolve_scope`.
- `finder`: `FinderSession` over `nucleo::Nucleo<FinderItem>` (path matching config, smart case, append optimisation), streaming injection while walking, `set_query` / `tick` / `results(offset, limit)` with highlight indices, `matched_count` / `total_count` / `walking`, `from_items` for tests/benches.
- `cache`: `WalkCache` (last 4 walks keyed by root + toggles), completed walks only; `apply_changes(&[PathChange])` for watcher events (add / remove subtree / rename subtree), `invalidate`, `clear`.
- `filter`: `ListFilter` / `filter_list` for the `/` mode (indices, score, stable sort).
- `grep`: `spawn_search` pipeline (walker -> bounded channel -> N workers -> collector with 30 ms batching), `GrepEvent` {Matches, Progress, Done, Cancelled, Error}, smart case, literal/regex/whole-word, glob field, binary detection, size limit, cloud skip, 10k cap with `capped` flag, cancellation, generation id, line sanitising/truncation with remapped ranges.
- `debounce`: pure `Debouncer` (120 ms default).
- `testgen`: deterministic synthetic path/tree generators (used by benches).
- Review round: walk cache matches paths the Windows way (case-insensitive, `/` = `\`, verbatim prefix ignored) with LRU recency and no filesystem calls or O(n) scans under its lock; finder keeps exact non-Unicode names (`FinderItem::raw`); cache-hit feed runs on a helper thread (`start_with_token` for a caller token); grep highlights are computed on the original bytes and mapped onto the lossy text; the sanitizer also replaces bidi/format/invisible characters; collector spawn failure emits `GrepEvent::Error`; `GrepHandle` cancels on drop (`detach()` opts out); optional `skip_cloud_dirs` for walker/finder/grep.
- Tests: 46 unit (+1 ignored perf guard) + 23 integration (`tests/grep.rs`: junction via `mklink /J`, offline attribute via `attrib +O`, access denied via `icacls /deny`, deterministic cancellation) + 2 doctests; criterion bench `benches/search.rs`.

## Public API overview (how wiring should use it)
Threading: all plain `std::thread`s, no tokio needed. Callbacks (`Notify`, `EventSink<E>`) run on engine threads and must be non-blocking (forward into the app's unbounded mpsc and map to `Msg`).

**Finder (ctrl-p)**
1. Keep one `Arc<WalkCache>` for the app lifetime.
2. On open: `FinderSession::start(FinderOptions{root,..}, generation, Some(cache), notify)`. `notify` -> post `Msg::FinderWake{generation}`; on that msg (and on each frame while `status.busy()`), call `session.tick(10)`; when `changed`, call `results(0, visible_rows)` and read `matched_count()/total_count()/walking()`.
3. On each keystroke: `set_query(&text)` then `tick`. Scope change (`ctrl-r`) / toggle change (`alt-.`, `alt-g`): drop the session and start a new one with a new generation (the cache makes revisits instant; `resolve_scope(scope.next(), cwd)` gives the root).
4. Dropping the session cancels the walk. `FinderResult { rel, path, kind, indices }` -> rows; Enter uses `path` (exact, also for non-Unicode names; `rel` is lossy display text). Carry the generation in the `notify` closure (`FinderSession::generation()` for the live session) so wake-ups of a replaced session are ignored.
5. Watcher: translate events to `PathChange` and call `cache.apply_changes`. Open sessions keep their snapshot (nucleo cannot remove items); reopen/restart to see changes.

**Filter (`/`)**: `ListFilter::filter(&names, query)` -> `Vec<FilterMatch{index, score, indices}>`; call synchronously on every keystroke.

**Content search (ctrl-f)**
1. Debounce with `Debouncer` (120 ms). On fire: cancel the previous search (`handle.cancel()` or its token; **dropping a `GrepHandle` also cancels**, call `detach()` to keep a search running without a handle), bump the generation, build `GrepQuery::new(root, text)` (set `regex`, `whole_word`, `glob`, `show_hidden`, `respect_gitignore` from toggles), `spawn_search(query, generation, token, sink)`.
2. Sink maps 1:1: `GrepEvent::Matches` -> append `FileMatches`; `Progress` -> status line (files searched, `skipped_cloud`, `skipped_large`, `denied`); `Done{capped}` -> show "refine your search" if capped; `Cancelled`/`Error` -> finish. Drop events whose `generation()` is stale: a cancelled search still delivers its `Cancelled` event later, after the UI has moved on to the next generation. A search always ends with exactly one terminal event (`is_terminal()`).
3. `LineMatch.ranges` are byte ranges into `LineMatch.text` (already sanitised), ready for highlighting.

## Deviations (with reasons)
- No `Msg`-like event enum for the finder: it is polled (`tick`) with a `Notify` wake-up, because nucleo's snapshot model is pull-based; grep is push-based with `GrepEvent`.
- Binary files: ripgrep semantics (`BinaryDetection::quit(0)`): matches before the first NUL are kept, the rest of the file is skipped and counted in `stats.binary`.
- Gitignore rules are applied even outside git repositories (`require_git(false)`) and from parent directories.
- Directory junctions are detected through `FileType::is_symlink()`, which std reports as true for mount-point reparse points on Windows. Covered by `directory_junction_not_followed` (creates a junction via `cmd /C mklink /J`); the symlink-dir tests print a `SKIPPED` line and pass where symlinks cannot be created (non-developer-mode Windows), so the junction test is the one that always runs.
- `WalkCache::apply_changes` cannot evaluate gitignore rules for added paths (documented); it is linear per change batch and works on a private copy outside the cache lock. Non-Unicode names that arrive through the watcher (`Added`) are stored lossily, and a renamed entry loses its exact name (falls back to the lossy `rel`).
- Memory mapping disabled (`unsafe` is forbidden workspace-wide; `MmapChoice::auto` is unsafe).
- Dependencies were added to the crate manifest only (not `[workspace.dependencies]`) to avoid root Cargo.toml churn; `Cargo.lock` updated.

- `.git` is always skipped, even with `show_hidden`; this is now stated on `WalkOptions::show_hidden` and `GrepQuery::show_hidden`. No toggle to include it was added (not in the spec).
- Cloud-placeholder directories are only skipped when `skip_cloud_dirs` is set (default off): OneDrive folders commonly carry recall attributes while still being useful to list, and the finder only reads names. Placeholder files are always skipped by grep. The per-file `metadata()` call in grep is not an extra stat on Windows (the enumeration data is reused).

## VERIFY results
- nucleo 0.5.0 (nucleo-matcher 0.3.1): `Nucleo::new(Config::DEFAULT.match_paths(), notify: Arc<dyn Fn()+Send+Sync>, threads: Option<usize>, columns: u32)`; `injector()` -> `Injector<T>` (`push(value, |v, cols| cols[0] = ..)`, cheap `Clone`, usable from walker threads); `pattern.reparse(col, text, CaseMatching::Smart, Normalization::Smart, append)` where `append` may only be true if the old text is a prefix of the new; `tick(timeout_ms) -> Status{changed, running}`; `snapshot()`: `matched_item_count()`, `item_count()`, `matched_items(range)` (ranked), `pattern().column_pattern(0)`. Highlight indices come from `Pattern::indices(item.matcher_columns[0].slice(..), &mut Matcher, &mut Vec<u32>)` (unsorted, may contain duplicates: sorted and deduped by the engine). `match_paths()` uses `/` and `\` delimiters on Windows, giving the file-name bonus. Scores are not exposed by `matched_items`.
- grep-searcher 0.1.17 / grep-regex 0.1.14 / grep-matcher 0.1.9: `RegexMatcherBuilder::new().case_smart(true).word(bool).build(pattern)`; literal mode = `regex_syntax::escape`; `SearcherBuilder::new().binary_detection(BinaryDetection::quit(0)).line_number(true).build()`; `searcher.search_path(&matcher, path, sink)`; custom `Sink` with `matched` (returning `Ok(false)` stops the file) and `binary_data`; `Matcher::find_iter` on the line bytes gives the highlight ranges. `SinkMatch::bytes()` includes the line terminator (trimmed by the engine).
- ignore 0.4.33 `WalkParallel` cancellation: there is no external handle, so each visitor callback checks the `CancellationToken` (and a shared stop flag) and returns `WalkState::Quit`; all walker threads then exit at their next entry. A sink returning `false` (for example a closed grep channel) uses the same mechanism. Tests: a pre-cancelled walk delivers 0 entries; grep cancellation test on 2000 files finishes well under 5 s.
- `OverrideBuilder` semantics confirmed by tests: `*.rs` whitelists, `!target/**` excludes, combinable.

## Added dependencies (crate `ks-search`)
ignore 0.4.33, nucleo 0.5.0, grep-searcher 0.1.17, grep-regex 0.1.14, grep-matcher 0.1.9, regex-syntax 0.8.11, crossbeam-channel 0.5.17, parking_lot 0.12.5, tokio-util and tracing (workspace); dev: criterion 0.8.2, tempfile (workspace).

## Benchmarks/measurements
Machine: Windows 11, 32 cores, warm OS file cache, `cargo bench -p ks-search` (env `KS_BENCH_FILES`, `KS_BENCH_PATHS` scale it down).
- Fuzzy, 500k synthetic paths, time from `set_query` to settled snapshot plus fetching the top 30 rows with indices: `h` 14.2 ms; `ha` 9.4; `han` 7.0; `hand` 5.1; `handl` 5.5; `handle` 5.5; `handler` 3.7; `handler c` 2.7; `handler ca` 2.5; `handler cac` 1.6; backspace to empty 0.33 ms. Budget 25 ms: met (worst case, the first keystroke, 14.7 ms upper bound).
- Walk, generated tree of 200k files (4k dirs; 2.06 GB; generation took 82 s): 58 ms (about 3.4M entries/s, warm cache).
- Grep over the same 2.06 GB tree, default workers: rare literal 1.21 s (1.59 GiB/s); absent literal 1.18 s (1.63 GiB/s); regex `hand\w+ cache` 1.13 s (1.70 GiB/s).
- `rg` is not on PATH on this machine, so the 1.5x-of-rg budget could not be compared. TODO: run `rg -c NEEDLE_MARKER <tree>` on a tree from `ks_search::testgen::generate_tree` once rg is installed.
- Memory at 1M paths (`cargo build --release -p ks-search --example mem`, peak working set of the process, 45-char average synthetic paths; holds one cache copy plus nucleo's own copy, 147k matches for `handler`): peak 312 MB including the transient generation of the input strings. The earlier 150 MB estimate was too low by about 2x; steady state is roughly one cache copy plus one nucleo copy, each about 100+ MB per million paths. MAX_ROOTS was lowered from 8 to 4 for this reason. With the cache limited to 4 roots, a 1M-path root costs about 150 MB cached plus about 150 MB per open session..

## Open issues / wiring TODO
- Wire `Msg`/state: finder overlay (query, selection, marks), `GrepEvent` accumulation, results-list tab, status line counters.
- Watcher -> `PathChange` translation (ks-fs) and `cache.apply_changes`.
- Compare against `rg` once installed (not on PATH here; 1.5x budget unchecked, still open).
- Finder keystroke guard: `cargo test -p ks-search --release -- --ignored finder_keystroke_latency` (measured worst 23 ms including the very first keystroke on 500k paths, guard 25 ms; the criterion numbers above are the reference).
- Cache-hit start still duplicates items into nucleo (nucleo owns its copy); sharing via `Arc<FinderItem>` would halve memory but needs a wider API change. Deferred.
- Walk cache `Added` of a directory does not scan it; watcher must report children or call `invalidate`.
- (done) Junction test via mklink /J.
- Finder tie order is discovery order, nondeterministic during a parallel walk; grep result order across files is nondeterministic too (UI may sort by `rel` on `Done`).
