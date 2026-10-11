# Screenshots and the tour GIF

The images in [`docs/img`](../../docs/img) are recorded from the real `ks.exe`, not mocked up:

1. `make_demo.py` creates a small demo project (`aurora`) with git changes, an image and an archive.
2. `ks-recorder` (this crate) runs `ks.exe` in a ConPTY, plays a script of key presses and dumps
   the vt100 screen to JSON (`shot NAME`), or every change between `rec NAME` and `endrec`.
3. `render.py` draws the JSON as a PNG in a window frame, or a recording as an optimised GIF.

Kestrel runs with `KESTREL_CONFIG_DIR` / `KESTREL_STATE_DIR` pointing into the demo folder, so the
recordings never see your own config, bookmarks or frecency history.

## Requirements

- Windows, Rust, Python 3.9+ with Pillow (`pip install pillow`), git.
- JetBrains Mono Nerd Font Mono, unzipped into `tools/screenshots/fonts-dl/` (gitignored):
  `JetBrainsMonoNerdFontMono-{Regular,Bold,Italic,BoldItalic}.ttf` from the
  [Nerd Fonts releases](https://github.com/ryanoasis/nerd-fonts/releases) (`JetBrainsMono.zip`).

## Recording

```powershell
cargo build --release -p ks-app
cd tools\screenshots
cargo build --release
python make_demo.py D:\ks-demo
$env:KS_EXE = "$PWD\..\..\target\release\ks.exe"
$env:KS_DEMO = "D:\ks-demo"
.\target\release\ks-recorder.exe scripts\shots.ks out
.\target\release\ks-recorder.exe scripts\tour.ks out
$env:KS_TITLE = "ks"
foreach ($n in "main","preview","image","find","search","palette","help","drives") {
    python render.py shot "out\$n.json" "..\..\docs\img\$n.png"
}
$env:KS_FONT_SIZE = 16
python render.py gif out\tour.rec.json ..\..\docs\img\tour.gif 12
```

`scripts/claude.ks` records the Claude pane. It needs your own signed-in `claude`, answers Claude
Code's folder-trust question for the demo folder with "Yes", and types a question **without**
sending it.

Script commands: `size COLS ROWS`, `env K=V`, `unenv PREFIX`, `cwd DIR`, `start EXE ARGS`,
`wait MS`, `send TEXT` (escapes `\e` `\r` `\t` `\b` `\s`, `^X` for control keys), `type TEXT [MS]`,
`shot NAME`, `rec NAME` / `endrec`, `quit`. `{ks}` and `{demo}` expand to `$KS_EXE` and `$KS_DEMO`.
