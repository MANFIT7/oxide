# oxide-term — native GPU terminal (PoC)

A standalone, GPU-rendered terminal for Oxide. **Not** an embedded web terminal
(xterm.js) — a real native terminal emulator in Rust.

## Stack
- **portable-pty** — spawns the shell, owns the PTY (same crate the Oxide GUI uses).
- **alacritty_terminal** — the VTE emulation core (the cell grid). The same crate Zed embeds.
- **glyphon** (cosmic-text + wgpu) — GPU text: glyph shaping + atlas + rendering.
- **wgpu** — on macOS this selects the **Metal** backend automatically.
- **winit** — window + input + event loop.
- **Nerd Font** — JetBrainsMono Nerd Font Mono is bundled (powerline / dev-icon glyphs render).

## Run
```sh
cargo run --manifest-path crates/oxide-term/Cargo.toml
# (release for smoothness)
cargo run --release --manifest-path crates/oxide-term/Cargo.toml
```
A window opens running your `$SHELL`. Type — it forwards to the PTY; output renders on the GPU.

This crate is **excluded from the main workspace** (`exclude` in the root `Cargo.toml`)
so its heavy wgpu/winit deps don't slow the Oxide build or gate releases.

## Status — Milestones 1 + 1.5 + 2 (compiles clean; visuals need a live GPU)
- ✅ window + wgpu (**Metal**) + glyphon text + PTY + alacritty_terminal grid
- ✅ **per-cell fg color + bold** (glyphon rich-text runs) + **per-cell bg color**
  (a small wgpu solid-quad pipeline) + xterm-256 palette fallback
- ✅ **inverse block cursor** (swaps fg/bg on the cursor cell)
- ✅ **scrollback** (mouse wheel → `scroll_display`; any keypress jumps to bottom)
- ✅ **keyboard incl. Ctrl-combos** (Ctrl-C/D/Z…, arrows, Home/End/Del, Esc, Tab)
- ✅ resize re-grids PTY + Term
- ⬜ **selection + copy/paste** (mouse drag → `Selection`, OSC 52 / clipboard)
- ⬜ **bold-italic faces, underline/strikethrough, true cursor shapes (beam/underline)**
- ⬜ **M3 — integration into Oxide**: launch this as a sibling native window/process
  from the GUI (a wgpu surface can't live inside the Dioxus/wry webview). Needs a
  packaging decision (bundle the binary) + your visual confirmation of M2 first.

## Known tuning points (report what you see)
- **Font:** loaded by data + referenced as `JetBrainsMono Nerd Font Mono`. If glyphs
  fall back to a different face, the internal family name differs → adjust `FONT_FAMILY`.
- **Cell width** (`CELL_W = FONT_SIZE * 0.6`) is an approximation; if columns drift or
  bg quads misalign with glyphs, derive it from the font's real advance metric.
- **Colors:** a fresh `Term` may not preload a full theme, so unset palette slots fall
  back to a built-in xterm-256 palette (`palette_256`). Default fg/bg match Oxide's.
