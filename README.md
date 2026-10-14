# Shelf

A fast, fully offline screenshot tool for Windows 10/11: a Lightshot replacement with a capture shelf, on-screen drawing for calls, OCR, dictation and area recording. No network access, no accounts, no telemetry.

- **Capture** — `PrtScrn` freezes all monitors in ~40 ms; select, mark up, then copy, save, copy the path, drag the file out, recognize text or record the area.
- **Shelf** — a grid of recent captures and copied images; drag files out, copy images, paths or text, pin.
- **Screen ink** — draw over everything while holding `Caps Lock`; the ink fades and is visible in screen sharing. Laser pointer, spotlight, zoom, click rings, pressed-keys overlay.
- **Board** — a translucent canvas for sketching; saved to the shelf.
- **Dictation** — hold a key, speak, get text (local Whisper model).
- **Recording** — any area to MP4.
- **Long screenshots** — scroll and stitch any area (fixed headers and footers appear once).
- **Clipboard chip** — whatever you copy anywhere (color, link, file, JSON, image, text, code) briefly shows next to the cursor; secrets are masked.
- **Text history** — every copied text, searchable from a Win+V-style picker; pinned entries can be templates with `{fields}`. Stored encrypted (DPAPI).

## Install

Shelf is a single portable executable with no dependencies. Download the latest `shelf-mN.exe` from [Releases](https://github.com/ZhulinskiiDanil/shelf/releases) and run it; it lives in the system tray. Starting a newer build replaces the running one.

- Settings: tray → **Settings…** (or edit `%APPDATA%\Shelf\config.toml`; changes apply on save).
- History: `%APPDATA%\Shelf\history\`. Log with timing metrics: `%APPDATA%\Shelf\logs\shelf.log`.
- Dictation needs a ggml Whisper model you download yourself (the tray menu explains where to get it and where to put it).

## Hotkeys

All hotkeys are defaults and can be changed in Settings, which also reports conflicts.

### Global

| Keys | Action | Config |
|---|---|---|
| `PrtScrn` | Capture screen | `hotkeys.capture` |
| `Ctrl+Shift+Space` | Open / close the shelf | `hotkeys.shelf` |
| Hold `Caps Lock` (or a mouse side button) | Draw on screen; strokes fade after 2.5 s | `ink.hold` |
| Hold `Ctrl` + `Caps Lock` | Laser pointer | `ink.hold` |
| `Ctrl+Alt+D` | Sticky draw mode (left button draws, `Esc` exits) | `ink.sticky` |
| `Ctrl+Alt+Backspace` | Clear screen ink and markers | `ink.clear` |
| `Ctrl+Alt+S` | Spotlight (wheel: radius, `Esc` exits) | `ink.spotlight` |
| `Ctrl+Alt+Z` | Zoom (wheel: level, `Esc` or right click exits) | `ink.zoom` |
| `Ctrl+Alt+C` | Toggle click rings | `ink.click_rings_toggle` |
| `Ctrl+Alt+K` | Toggle pressed-keys overlay | `ink.keys_toggle` |
| `Ctrl+Alt+B` | Thinking board | `board.hotkey` |
| Hold `Ctrl+Alt+V` | Dictation: record while held, text to the clipboard on release | `dictation.hold` |
| `Ctrl+Alt+R` | Stop area recording | `record.stop` |
| `Ctrl+Alt+X` | Toggle the clipboard chip (what was just copied, shown next to the cursor) | `clipboard_chip.toggle` |
| `Ctrl+Shift+V` | Text history picker (type to filter, `Enter` pastes, `Shift+Enter` copies, `Ctrl+P` pins, `Del` deletes) | `text_history.picker` |
| `Ctrl+Alt+P` | Pause / resume text history | `text_history.pause` |

### Capture mode (after `PrtScrn`)

| Keys | Action |
|---|---|
| Drag · click | Select area · select the window under the cursor |
| Drag edges, corners, inside | Resize / move the selection |
| `←` `↑` `→` `↓` (`Shift` ×10) | Nudge the selection by 1 px |
| `Ctrl+C` | Copy image and close |
| `Ctrl+Shift+C` | Save to history and copy the quoted path |
| `Ctrl+S` · `Ctrl+Shift+S` | Quick save to the configured folder · save as… |
| `Ctrl+D` | Attach the file to the cursor; click to drop (`Esc` cancels) |
| `Ctrl+T` | Recognize text (OCR) and copy it |
| `Ctrl+R` | Record the selected area to MP4 |
| `Ctrl+L` | Scrolling (long) screenshot of the selection; `Enter`/`Esc` stops early (`scroll_capture.key`) |
| `Esc`, right click | Cancel |
| `P` `L` `A` `R` `E` `M` `T` `B` `N` | Pen, line, arrow, rectangle, ellipse, marker, text, pixelate, numbered marker (`[tools]`) |
| `1`–`8` · `[` `]`, mouse wheel | Color · stroke width |
| `Shift` while drawing | Snap to 45° / square / circle |
| `Ctrl+Z` · `Ctrl+Y` | Undo · redo markup |
| `Enter`, `Esc` · `Shift+Enter` (text tool) | Finish text · new line |

### Shelf

| Keys | Action |
|---|---|
| Click, `Ctrl`/`Shift`+click, `Ctrl+A`, arrows | Select |
| Drag | Drag files out (the shelf closes after a drop) |
| `Ctrl+C` · `Ctrl+Shift+C` · `T` | Copy image · copy quoted paths · copy text (OCR) |
| `P` · `Del` · `O` · `Enter`, double click | Pin · delete · show in folder · open |
| `Esc`, click outside | Close |
| `Tab`, click the header | Switch between Captures and Text (text: type to search, `Enter` / `Ctrl+C` copy, `Ctrl+P` pin, `Del` delete, drag a row out) |

### Screen ink

| Keys | Action |
|---|---|
| Hold `Shift` while drawing | Keep the stroke until cleared |
| `1`–`8` while drawing | Color |
| Click while holding `Caps Lock` (sticky mode: `Ctrl`+click) | Numbered marker |

### Board

| Keys | Action |
|---|---|
| Same tool, color and width keys as capture mode; `Ctrl+Z` · `Ctrl+Y` | Draw · undo · redo |
| Middle button drag, `Space`+drag | Pan |
| `Esc` | Save to the shelf (if not empty) and close |

## Build

Requirements: Rust (MSVC toolchain), CMake and LLVM (`LIBCLANG_PATH` pointing to its `bin`) for whisper.cpp.

```
cargo build --release
cargo test
```

Development tools: `tools/bench.ps1` compares capture-path latency between builds; `examples/dda_grab.rs` captures the screen via Desktop Duplication (what screen sharing sees); `examples/transcribe.rs` runs a Whisper model on a WAV file.
