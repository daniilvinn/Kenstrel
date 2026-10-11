# Performance

Kestrel has a set of performance budgets that are checked by tests and benchmarks in the
repository. This page lists the budgets, the numbers measured on a release build, how they were
taken, and how to reproduce them. Treat the numbers as indicative: they are from one fast
machine, with warm file caches.

## Method

- **Build**: release profile (`lto = "thin"`, `codegen-units = 1`, `strip = true`, static CRT),
  Rust 1.99.0, built from one commit; the ignored performance tests were run with
  `cargo test --release` and the benchmarks with `cargo bench -p ks-search`.
- **Machine**: AMD Ryzen 9 9950X3D (16 cores, 32 threads), Windows 11 Pro 10.0.26200, Microsoft
  Defender real-time protection on (it matters for process start, see the first rows). The disk
  corpora were generated on an NVMe SSD (Samsung 990 EVO Plus). Temporary files for tests whose
  timed part does not touch the disk were on a SATA hard disk.
- **Repetitions**: every metric was run at least three times; the table gives median and worst
  over those runs, with n. Nothing else ran on the machine during a measurement.
- **Caches**: the corpora were created just before each run, so all directory and file figures
  are **warm cache** numbers. A cold cache (after a reboot) was not measured.
- **Not measurable headless**: the real cold start in Windows Terminal, terminal-side keypress
  latency, and entering a directory with a cold cache. These are manual checks, see
  [manual-test.md](manual-test.md#10-performance).

## Budgets and results

| Metric | Budget | Result (median / worst, n) | Met? | Method and notes |
|---|---|---|---|---|
| Cold start to first frame, in-process | < 60 ms | `first frame elapsed_ms` log line: **5 ms / 6 ms** (n=10 real `ks.exe` launches); headless test: **0.63 ms / 0.65 ms** (n=5) | yes (this part) | The log line counts from the start of `run` (after argument parsing) to the first drawn frame, with the real raw mode, alternate screen and terminal. The test builds the session and runs the loop to the first frame on a test backend. |
| Cold start, end to end (process creation to a visible listing) | < 60 ms | Hidden ConPTY harness, 120x32, 300-file directory: first bytes **73 ms / 250 ms**, listing visible **118 ms / 288 ms** (n=10; the 250 / 288 ms are the first launch after a rebuild, about 87 / 134 ms otherwise) | not demonstrated; probably not met on this machine | The cost is the bare process: `ks --version` lives **77 ms** median (73 to 98 ms, n=20) against 29 ms for a minimal static-CRT Rust exe, 30 ms for `cmd /c exit` and 43 ms for `rg --version`. A 13.9 MB dummy exe with no code costs 72 ms, so the overhead scales with image size (about 3 ms per MB, most likely Defender scanning on exec), not with Kestrel's start-up code or imports. Kestrel's own work to the first frame is 5 to 6 ms. |
| Key press to frame | < 16 ms p99 | p50 **0.21 ms / 0.22 ms**, p99 **0.28 ms / 0.41 ms**, max 0.46 ms (n=5 runs of 200 presses) | yes | `ks-ui` test `keypress_to_frame_stays_within_budget`: `update` plus a full render at 160x48 with a 1000-entry list and a text preview. Terminal and ConPTY latency are not included. |
| Enter a directory with 100,000 entries | first rows < 150 ms, complete < 1 s | first rows **1.1 ms / 1.4 ms**; complete and sorted **61 ms / 67 ms** (n=3) | yes | `navigation` test `a_directory_of_100k_files_meets_the_budget`, on NVMe, warm cache. |
| Fuzzy finder keystroke to results (500k paths) | < 25 ms | worst keystroke **12.8 ms / 13.0 ms** (n=5, real worker thread and channel); `ks-search` guard **17.1 ms / 19.1 ms** (n=5); criterion `h` keystroke **11.8 ms** | yes | The `ks-search` guard has the thinnest margin (19.1 of 25 ms): its figure includes polling and result materialisation on one thread. |
| Content search vs `rg` | within 1.5x of `rg` wall time | ratio ks / rg: **0.52 to 0.80** (medians of 5) | yes | See the table below. Identical match counts in all 9 cases. |
| Memory with a 1M-path finder cache | < 250 MB | peak working set **206 MB / 210 MB** (n=5) | yes | `ks-search` memory guard: a 1M-path cache plus one open session (147k matches), peak read from outside the process. It measures the finder, not the whole app; an idle `ks.exe` in a 300-file directory peaks at 20 to 21 MB, so the sum (an estimate) is about 227 MB. |
| External change appears in the list | about 200 ms | create to visible **171 ms / 186 ms** (n=7) | yes (thin margin) | `navigation` test `external_changes_appear_quickly_and_the_cursor_stays_on_its_entry`: debounce plus atomic re-list, on a test backend. |
| `ks.exe` size and C runtime | no `vcruntime` import | **15,449,088 bytes (14.7 MiB)**; imports only system DLLs (kernel32, advapi32, api-ms-win-core-synch, shell32, combase, ole32, oleaut32, ntdll, userenv, propsys, bcryptprimitives, ws2_32, user32, bcrypt) | yes | `dumpbin /imports` finds no `vcruntime*`, `msvcp*`, `ucrtbase` or `api-ms-win-crt-*`; the `static_crt` test guards the setting. |

Measured for information, without a budget:

| Metric | Result | Notes |
|---|---|---|
| Walk of 200k files | 66 ms | criterion `tree_200000_files/walk` |
| Grep on the same 200k-file tree (criterion) | 838 ms rare literal, 838 ms no match, 862 ms common-word regex | |
| 1 GiB copy | 0.20 / 0.21 s (4.7 to 4.9 GiB/s, n=3) | `ops_bench`, NVMe, page cache to page cache |
| Frecency (`z`) ranking at the 5,000-entry cap | open 2.4 ms, 2.8 ms per key (n=3) | runs on the UI thread |
| IPC round trip (Claude MCP/hook to Kestrel) | 102 to 106 µs | ignored `perf` tests |
| Claude pane: screen conversion 200x60 / frame draw with the pane | 241 to 300 µs / 150 to 154 µs | ignored `perf` tests; `vt100` parsing 1.1 MiB takes 4.0 to 4.7 ms |
| `ks hook` process start to exit | 77.6 ms median (74 to 92, n=15) | the same image-size cost as the cold start; runs once per Claude tool call |
| `cargo test --workspace` | 71 s, 1299 tests | debug build, incremental build warm |

## Content search versus ripgrep

Median and worst of 5 alternating runs after one warm-up. Both wall times include process start.
"ks engine" is the time the `grepbench` example reports for the search itself. `rg` is 15.2.0
run as `rg -c -S` (with `-F` for literals). Ratio is ks wall / rg wall.

| Corpus | Pattern | rg wall | ks wall | ks engine | Ratio | Matching lines |
|---|---|---|---|---|---|---|
| generated, 20k files, 209 MB | `NEEDLE_MARKER` | 218 / 249 ms | 129 / 135 ms | 86 / 88 ms | 0.59 | 7,543 |
| same | `handler` | 285 / 323 ms | 195 / 210 ms | 137 / 156 ms | 0.68 | 792,910 |
| same | `needle_marker` (smart case) | 235 / 251 ms | 135 / 143 ms | 87 / 92 ms | 0.57 | 7,543 |
| same | regex `NEEDLE_\w+ line1\d\d\d` | 213 / 217 ms | 139 / 148 ms | 89 / 95 ms | 0.65 | 941 |
| generated, 200k files, 2058 MB | `NEEDLE_MARKER` | 1646 / 1651 ms | 854 / 865 ms | 785 / 805 ms | 0.52 | 74,490 |
| same | `handler` | 2200 / 2279 ms | 1376 / 1438 ms | 1314 / 1367 ms | 0.63 | 7,800,310 |
| `~\.cargo\registry\src` | `unsafe` | 386 / 393 ms | 308 / 312 ms | 261 / 261 ms | 0.80 | 682,629 |
| same | `fn main` | 378 / 387 ms | 249 / 262 ms | 185 / 200 ms | 0.66 | 3,267 |
| same | `Windows` | 381 / 399 ms | 225 / 240 ms | 179 / 182 ms | 0.59 | 24,710 |

The engine alone searches the 2058 MB tree for `NEEDLE_MARKER` at about 2.4 GiB/s.

## Reproducing

Build once, then run the tests in release mode. Set `TEMP` and `TMP` to a fast SSD for tests that
create files (creating the 100,000 files took 4 to 6 s on NVMe and over five minutes on a hard
disk). See [BUILD.md](../BUILD.md#ignored-tests) for details.

```powershell
cargo build --release -p ks-app
cargo test --release -p ks-ui keypress_to_frame_stays_within_budget -- --nocapture
cargo test --release -p ks-app --test cold_start -- --nocapture
cargo test -p ks-app --release --test navigation a_directory_of_100k -- --ignored --nocapture
cargo test -p ks-app --release --test navigation external_changes_appear_quickly -- --nocapture
cargo test -p ks-app --release --test search -- --ignored finder_keystroke --nocapture
cargo test -p ks-search --release --lib -- --ignored finder_keystroke_latency --nocapture
cargo build -p ks-search --release --examples
cargo test -p ks-search --release --test memory -- --ignored --nocapture
cargo test -p ks-fs --release --test ops_bench -- --ignored --nocapture
cargo bench -p ks-search --bench search
cargo run -p ks-search --release --example grepbench -- <root> <pattern> [--regex] [--gen <files>] [--runs <n>]
dumpbin /dependents target\release\ks.exe
```

For the comparison with `rg`, install ripgrep (for example `cargo install ripgrep --root target\tools`,
which is git-ignored via `target`) and time `rg -c -S <pattern> <root>` against `grepbench` on the
same tree; `grepbench --gen <files>` can generate a corpus.

The process start-up figures came from throwaway harnesses (a small program that creates a hidden
ConPTY and launches `ks.exe`, and kernel process-lifetime probes) that are not in the repository;
the simplest check is `Measure-Command { ks --version }`.
