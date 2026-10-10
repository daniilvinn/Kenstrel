# Engine: preview (phase P2 backend)

Crate `crates/ks-preview`. Self-contained, terminal-agnostic, does not depend on ks-core/ks-ui/ks-app/ks-fs. Gates (fmt, clippy `-D warnings`, `cargo test --workspace`) are clean. Nothing committed.

## Review follow-ups (this round)

- Worker panics are caught (`catch_unwind`) and become `Unavailable{Io}`; a panicking sink is caught too. Transient failures (Locked / PermissionDenied / Io) are never cached. Cache has a byte budget. Junction/symlink targets lose the `\?\` prefix; only real symlinks/junctions count as links (OneDrive/AppExec reparse points no longer produce "broken link"). Directory cloud check, `scan_capped`, `total_lines`, lone `` line splitting, UTF-32 BOM / BOM-less UTF-16 / lone surrogate handling, overlong-line parser sync, unterminated front matter, tab indent in markdown lists. New tests cover all of these plus stale-generation and cancel emitting nothing.

## Done

- `Previewer` service: `request(path, generation, opts)`; cancels the previous in-flight request; LRU cache (64) keyed by (path, mtime ns, size, raw attributes, hash of all options).
- Kinds: Text (256 KB / 2,000 lines, UTF-8 / UTF-8 BOM / UTF-16 LE+BE BOM / lossy fallback, syntect highlighting via two-face syntaxes, theme built from a caller `Palette`), Markdown (line-based light renderer, fenced code highlighted), Directory, Image (decoded `DynamicImage`, downscaled to <= 2048 px), Archive (zip listing via `by_index_raw`; tar/gz/bz2/xz/zst/7z/rar metadata-only), Binary (hex rows of first 4 KB + magic type hint), Metadata, Empty, Unavailable.
- Unavailable reasons: TooLarge (text/md/binary > 100 MiB, image file > 64 MiB or > 100 MP), CloudPlaceholder (attrs 0x400000 / 0x1000 / 0x40000, never opened), PermissionDenied, Locked (os error 32/33), NotFound, Io.
- Fallbacks: corrupt image / corrupt zip become the hex view with `BinaryPreview::note` set.
- All text from file content (lines, names in dir listings, archive entry names, link targets) is sanitized: tabs expanded to stops, C0 controls become control pictures (U+2400..), DEL/C1/bidi controls/U+2028-9 become U+FFFD. ESC can never reach the terminal.
- Tests: 17 unit + 20 integration (UTF-16 file, binary, zip built in-test, markdown, over-limit file, cache hit / miss / invalidate / bound of 64, cancellation, stale-generation, escape sanitisation, images incl. downscale + pixel guard + corrupt, hidden/readonly attrs on Windows, dir sorting/cap, timing).

## Public API overview (how the wiring should use it)

```rust
use ks_preview::*;

// once, at startup (any thread): loads the syntax set so the first preview is fast
std::thread::spawn(ks_preview::highlight::warm_up);

// sink runs on a worker thread; forward to the app's Msg channel
let previewer = Previewer::new(move |ev: PreviewEvent| { let _ = tx.send(Msg::Preview(ev)); });

// on every cursor move (never blocks, never touches the FS on the caller thread):
previewer.request(path, generation, PreviewOptions { palette, ..Default::default() });

// in update(): if ev.generation != state.preview_generation { drop } else store ev.result (Arc<Preview>)
```

- `PreviewEvent { generation, path, result: Arc<Preview>, from_cache }` maps 1:1 to a `Msg::Preview(PreviewEvent)`.
- Cancelled / superseded requests emit nothing. A request still queued when a newer one arrives is replaced and never runs. Only 3 `std::thread` workers exist (no tokio needed); the sink is called from them.
- `previewer.cancel()` (pane hidden), `invalidate(&path)` (watcher event or after file op), `clear_cache()` (theme change is already part of the key, so not needed for that), `cache_len()`.
- `PreviewOptions { kind: Auto | Metadata, palette, limits, tab_width, show_hidden, highlight }`. `PreviewOptions::metadata()` for the `i` toggle. All fields are part of the cache key. Build `Palette` from the active theme tokens (`fg, keyword, string, comment, function, type_, number, constant, operator, punctuation`); `Palette::dark()/light()` are provided.
- `Preview { path, meta: Option<FileMetadata>, body: PreviewBody }`: `meta` is always filled when the path could be stat'ed, so it serves the footer even for Text etc. `FileMetadata::to_lines()` renders the `i` view; `human_size`, `format_utc` helpers exist (UI should prefer its own local-time formatting from the `SystemTime` fields).
- `PreviewBody`: `Text(TextPreview)` / `Markdown(TextPreview)` with `lines: Vec<StyledLine>`, `line_count` (gutter width), `truncated`, `encoding`, `syntax`; `Directory(DirPreview)`; `Image(ImagePreview { image: Arc<DynamicImage>, width, height, format, downscaled })`; `Archive`; `Binary` (`HexRow::hex()` / `ascii()`); `Metadata`; `Empty`; `Unavailable { reason, message }`.
- Styling: `SpanStyle { fg: Option<Color>, bold, italic, underline, dim }`, `Color::Rgb(Rgb) | Color::Role(Role)`. `fg == None` means pane default foreground (highlighter folds spans equal to `palette.fg` to `None`). Roles: Fg, FgDim, FgMuted, Accent, Dir, Exec, Symlink, Hidden, Success, Warning, Error; map each to the theme token of the same name.
- Image on the UI side (ratatui-image 11.1.0, ratatui ^0.30.1 compatible): `let mut picker = Picker::from_query_stdio()?` once after entering the alternate screen and before reading events (falls back with `Picker::halfblocks()` on error); then `picker.new_resize_protocol((*img.image).clone())` gives a `StatefulProtocol` rendered with `StatefulImage` (resize+encode is blocking at render time, so offload with `thread::ThreadProtocol` as in the crate's `examples/thread.rs`), or `picker.new_protocol(img, Size, Resize::Fit(None))` for the stateless `Image` widget. Keep the Picker in app state; the protocol objects are created in the UI layer, never here.
- Separate overlays (finder preview) should use their own `Previewer` instance, otherwise they cancel the main pane's request.
- `build_preview(path, &opts, &CancellationToken) -> Option<Preview>` is the synchronous core (blocking, only call from your own thread).

## Deviations (with reasons)

- No `ThemeSet`: the spec's `ThemeSet` is unnecessary because the theme is built from the palette; only the `SyntaxSet` is loaded lazily once (`OnceLock`).
- Markdown renderer is hand-written (line based) instead of pulldown-cmark: "light rendering" only, avoids a dependency. Tables, nested lists, HTML blocks, reference links are shown as plain text.
- `ratatui-image` is NOT a dependency of ks-preview: the model stays terminal-agnostic and returns `Arc<DynamicImage>`; the UI crate adds ratatui-image (with `default-features = false, features = ["image-defaults", "crossterm"]` to avoid the default `chafa-dyn`, which needs libchafa).
- `owner` is always `None` on all platforms: resolving a SID to a name needs `unsafe` Win32 (`GetNamedSecurityInfoW` + `LookupAccountSidW`), and the workspace has `unsafe_code = "forbid"`. A safe crate (e.g. `windows` with unsafe in a dedicated crate) could be added later.
- Cloud placeholder check applies to files and directories (checked before listing); a placeholder directory yields `Unavailable{CloudPlaceholder}`.
- `docx/xlsx/pptx/odt/epub/jar/...` are listed as ZIP; `svg` is treated as text (xml).
- Extra syntax aliases in `highlight::find_syntax`: jsx/mjs/cjs -> JS, cmd -> Batch, ps1/psm1/psd1 -> Bash (no PowerShell grammar in either set), mts/cts -> TS, jsonc/json5 -> JSON, `Cargo.lock` -> TOML.
- Test uses `attrib` for hidden/readonly (Windows only); cloud-placeholder handling is tested by injecting the attribute bits into `FileMetadata` (no real OneDrive placeholder), see Open issues.
- Edits outside the crate: only `Cargo.lock` (new deps). Workspace `Cargo.toml` was not touched; new deps are declared crate-locally (versions from `cargo add`), `thiserror`, `tokio-util`, `tracing`, `unicode-width`, `tempfile` use workspace entries.

## VERIFY results

- syntect default syntaxes vs two-face (measured with `load_defaults_newlines` vs `two_face::syntax::extra_newlines`, fancy-regex backend): syntect 75 syntaxes, two-face 213. Missing in syntect: toml, ts, tsx, jsx, ps1, kt, swift, dart, scss, zig, ini, dockerfile, cmake, tf, proto, nix, vue, svelte, gitignore, csv, log, lock, vim, ex, jl. Missing in two-face: jsx, ps1/psm1, `.lock` (handled with aliases). Decision: two-face (`syntect-fancy`, pure Rust, no oniguruma C build). Embedded dump: syntect ~368 KB `default_newlines.packdump`; two-face's `syntaxes-fancy-newlines.bin` is ~959 KB (about 2.6x; release binary size not measured). Lazy load time (SyntaxSet deserialisation) 1.5 ms in release / 37 ms in a debug test run; syntaxes themselves are compiled lazily per language on first use.
- ratatui-image: latest stable is 11.1.0 (12.0.0-rc.2 exists as prerelease); depends on `ratatui ^0.30.1` (matches workspace 0.30.2) and `image ^0.25.6`. API: `Picker::from_query_stdio() -> Result<Picker>` (queries font size + protocol over stdio, must run after alt-screen and before the event reader), `Picker::halfblocks()`, `Picker::from_fontsize`, `picker.protocol_type()/set_protocol_type()` (cycle with `ProtocolType::next()`), `picker.new_protocol(DynamicImage, Size, Resize) -> Protocol` with widget `Image`, `picker.new_resize_protocol(DynamicImage) -> StatefulProtocol` with widget `StatefulImage`, and `thread::ThreadProtocol` for off-thread resize/encode. Default feature `chafa-dyn` needs libchafa: disable it.
- Windows Terminal Sixel: user's installed WT is 1.24.12741.0 (Microsoft.WindowsTerminal appx), which is newer than 1.22 where Sixel landed, so Sixel should be available. ratatui-image's compatibility matrix does not list Windows Terminal; its detection is DA1-query based (Sixel capability via device attributes) plus env guesses, so `from_query_stdio()` should pick Sixel. NOT visually verified (no interactive terminal here); needs a manual check in the real app. Windows Terminal supports neither the Kitty nor the iTerm2 graphics protocol, so Sixel or halfblocks are the only options there.

## Added dependencies

crates/ks-preview: `syntect 5.3.0` (default-features off; `parsing`, `regex-fancy`), `two-face 0.5.2` (`syntect-fancy`), `image 0.25.10` (png, jpeg, gif, bmp, webp, ico, tiff; no defaults), `zip 9.0.1` (deflate only; stored+deflate entries are listed raw so other codecs are fine), `lru 0.18.5`, plus workspace `thiserror`, `tokio-util` (CancellationToken only), `tracing`, `unicode-width`; dev: `tempfile`.

## Benchmarks / measurements

Release build, this machine (32 cores), 256 KB Rust file (14,839 lines of synthetic code):
- Syntax set load: 3.7 ms (first call, release); 37 ms in a debug test.
- Highlight all 14,839 lines: 670 ms. Default cap (first 2,000 lines): 83 ms. So a typical preview is well under a frame budget of perceived latency and is off-thread anyway.
- Debug build is ~8x slower (5.3 s for all lines, 734 ms for 2,000); the 1.5 s `highlight_budget` makes the rest of the file plain if exceeded, so debug builds never stall. Recommend `[profile.dev.package."*"] opt-level = 3` (or at least for syntect/fancy-regex) in the root Cargo.toml later (not edited here: out of allowed scope).
- Run: `cargo test --release -p ks-preview --test engine highlight_timing -- --nocapture`.

## Open issues / wiring TODO

- Wire `Msg::Preview(PreviewEvent)`, `generation` bump on cursor move, pane rendering of `StyledLine` -> ratatui `Line` (map `Role` to theme tokens; `dim` -> `Modifier::DIM`), line-number gutter from `line_count`, footer from `Preview::meta`, `i` toggle (re-request with `PreviewOptions::metadata()`), scroll offset kept by the UI.
- Image: build `Picker` once in ks-app, thread protocol encoding; handle `Unavailable`/`Binary{note}` messages. Verify Sixel in the user's Windows Terminal 1.24.
- Directory preview "same renderer as the list pane, dimmed": entries carry name/kind/size/hidden only; icons come from the UI's icon map.
- Call `invalidate(path)` on watcher events/file ops for the viewed file (mtime-based keys can miss sub-timestamp-granularity rewrites).
- Cloud placeholder path is only tested with injected attribute bits, not a real OneDrive placeholder.
- Owner on Windows is omitted (see Deviations).
- Overlong lines (> `max_highlight_line`, 4,000 bytes): the head is now fed to the parser (state stays in sync) and the tail is plain. A single in-limit pathological line can still run long in fancy-regex; it cannot be interrupted (the 1.5 s budget is checked between lines only).
- `TextPreview::total_lines` is `Some` only when the whole file was rendered; for larger files the real line count is unknown (`None`), `line_count` is the number of rendered lines. Counting lines of a huge file would need a full read.
- Large directories: the scan stops at 20,000 entries (`dir_scan_limit`) in OS order, then sorts; `DirPreview::scan_capped` tells the UI the listing is a subset and `total` a lower bound.
- Image decode and `ZipArchive::new` are not interruptible mid-call. Mitigations: the file is read once into memory (bounded by `max_image_file_size`, 64 MiB), the decoder gets an allocation limit, `max_image_pixels` lowered to 50 Mpx, and cancellation is checked before/after each stage. Still open: stale decodes occupy a worker until they finish (3 workers); a dedicated "latest request" worker or a cancellable decoder would fix that. Archives skip `max_file_size`; central-directory parse cost is bounded by the file but not cancellable.
- Cache memory: 64 entries AND a 96 MiB byte budget (`CACHE_BYTE_BUDGET`, estimates via `Preview::approx_bytes`); larger single previews are delivered uncached. Peak transient memory while decoding is still up to ~3 x 200 MB for three simultaneous max-size images.
- Cancelled/superseded requests emit nothing, and a single `Previewer` has one request slot: the UI must clear "loading" itself on cancel, always filter by `generation` (a result can race the cancel between the last check and the sink call), and use a second `Previewer` if `i`-metadata and auto previews must be live at once (they cancel each other).
- Not done: criterion bench (a timed test with recorded numbers satisfies the spec and avoids a dev-dependency); PermissionDenied is covered by classification unit tests, not a real ACL-denied file; BOM-less UTF-16 detection only handles mostly-ASCII content; UTF-32 is shown as hex.
