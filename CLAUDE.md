# CLAUDE.md

This file provides guidance to Claude Code (claude.ai/code) when working with code in this repository.

## Overview

antpad is a tiny Windows 95/98 Notepad-style terminal text editor. The whole program is a single file, `src/main.rs` (~1400 lines), with `crossterm` as the only dependency. There are no tests.

## Commands

- `cargo build --release` / `cargo run -- [FILE]` — build / run locally
- `./build.sh` — build inside a podman Rust container (for machines without Rust); output goes to `dist/antpad`. Cargo registry is cached in the `antpad-cargo` podman volume.
- `./install.sh` — install per-user (`~/.local/bin/antpad`, icon, `.desktop` launcher). It uses `dist/antpad`, then `target/release/antpad`, and otherwise builds with cargo or podman. `./install.sh --uninstall` removes everything.

`target/` and `dist/` are gitignored. The release profile is tuned for size (`opt-level = "s"`, LTO, `panic = "abort"`, stripped).

## Architecture (`src/main.rs`)

- **Event loop**: `main` sets up raw mode, the alternate screen, bracketed paste and mouse capture. `event_loop` redraws every iteration with `Editor::draw` and then dispatches key, paste, mouse and resize events. A panic hook calls `restore_terminal` so the terminal is not left in raw mode.
- **`Editor`** holds all state. The document is `lines: Vec<Vec<char>>`, and positions are `(row, col)` in chars. `trailing_nl` remembers whether the file ended in a newline.
- **`Mode`** is a state machine: `Edit`, `Menu`, `Prompt`, `Browser`, `Confirm`, `Overwrite`, `Message`. Input is routed by mode in `on_key` and `on_mouse` (`key_edit`, `key_menu`, `key_prompt`, `key_browser`). Add new dialogs as `Mode` variants.
- **Commands**: the menu bar is the const `MENUS` of `Cmd` items. `Editor::run(Cmd)` is the single entry point for both menu activation and keyboard shortcuts.
- **`Pending`** is an action deferred behind the unsaved-changes prompt (new, open, quit). It is run via `request` and `do_pending` after the user confirms or saves.
- **Undo** is whole-document snapshots (`push_undo`), capped at `UNDO_LIMIT` (100). Consecutive typing is grouped through the `typing` flag.
- **Clipboard**: `copy` stores text in the internal `clipboard` and also sets `osc`. The next `draw` emits an OSC 52 escape so the terminal copies it to the system clipboard. Paste uses the internal clipboard or bracketed paste events.
- **Rendering** is hand-rolled with `queue!` and the small helpers `put`, `style`, `draw_frame`, `draw_box`, using the `St` style constants. Mouse hit-testing helpers (`dropdown_geom`, `dropdown_hit`, `menu_at`, `browser_geom`) must stay in sync with the matching draw functions.
- **File browser** (`Browser`, `BrowseKind`) is a custom in-app Open/Save As dialog.
