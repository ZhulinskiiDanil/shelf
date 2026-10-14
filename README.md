# Shelf

A fast, fully offline screenshot tool for Windows 10/11: a Lightshot replacement with a capture shelf and on-screen drawing. No network access, no accounts, no telemetry.

## Install

Shelf is a single portable executable. Download the latest `shelf-mN.exe` from [Releases](https://github.com/ZhulinskiiDanil/shelf/releases) and run it; it lives in the system tray. Starting a newer build replaces the running one.

- Config: `%APPDATA%\Shelf\config.toml` (tray → Settings), applied on save.
- Log with timing metrics: `%APPDATA%\Shelf\logs\shelf.log`.
- History: `%APPDATA%\Shelf\history\`.

## Build

```
cargo build --release
cargo test
```

`tools/bench.ps1` compares capture-path latency between builds (it drives the mouse and keyboard).

## Hotkeys

| Context | Keys | Action |
|---|---|---|
| Global | `PrtScrn` | Capture screen |
| Capture | Drag | Select area |
| Capture | Click | Select window under cursor |
| Capture | Drag edges / corners / inside | Resize / move selection |
| Capture | `←↑→↓` (`Shift` ×10) | Nudge selection by 1 px |
| Capture | `Ctrl+C` | Copy image and close |
| Capture | `Ctrl+Shift+C` | Save to history, copy quoted path |
| Capture | `Ctrl+S` | Quick save to configured folder |
| Capture | `Ctrl+Shift+S` | Save as… |
| Capture | `Ctrl+D` | Attach file to cursor, click to drop (`Esc` cancels) |
| Capture | `Esc`, right click | Cancel |
| Capture | `P` `L` `A` `R` `E` `M` `T` `B` `N` | Pen, line, arrow, rectangle, ellipse, marker, text, pixelate, numbered marker |
| Capture | `1`–`8` | Color |
| Capture | `[` `]`, mouse wheel | Stroke width |
| Capture | `Shift` while drawing | Snap to 45° / square / circle |
| Capture | `Ctrl+Z` / `Ctrl+Y` | Undo / redo markup |
| Capture, text | `Enter` / `Esc` · `Shift+Enter` | Finish text · new line |
| Global | `Ctrl+Shift+Space` | Open / close the shelf |
| Shelf | Click, `Ctrl`/`Shift`+click, `Ctrl+A`, arrows | Select |
| Shelf | Drag | Drag files out (closes the shelf on drop) |
| Shelf | `Ctrl+C` · `Ctrl+Shift+C` | Copy image · copy quoted paths |
| Shelf | `P` · `Del` · `O` · `Enter` | Pin · delete · show in folder · open |
| Shelf | `Esc`, click outside | Close |
| Global | Hold `Caps Lock` (or a mouse side button) | Draw on screen; strokes fade after 2.5 s |
| Ink | Hold `Shift` while drawing | Keep the stroke until cleared |
| Ink | `1`–`8` while drawing | Color |
| Global | `Ctrl+Alt+Backspace` | Clear screen ink |
| Global | `Ctrl+Alt+D` | Sticky draw mode (left button draws, `Esc` exits) |
| Global | Hold `Ctrl`+`Caps Lock` | Laser pointer |
| Ink | Click while holding `Caps Lock` (sticky mode: `Ctrl`+click) | Numbered marker |
| Global | `Ctrl+Alt+S` | Spotlight (wheel: radius, `Esc` exits) |
| Global | `Ctrl+Alt+Z` | Zoom (wheel: level, `Esc` or right click exits) |
| Global | `Ctrl+Alt+C` | Toggle click rings |
| Global | `Ctrl+Alt+K` | Toggle pressed-keys overlay |
| Global | `Ctrl+Alt+B` | Thinking board (`Esc` saves to the shelf and closes) |
| Board | Same tool, color and width keys as capture markup; `Ctrl+Z` / `Ctrl+Y` | Draw |
| Board | Middle button drag, `Space`+drag | Pan |
