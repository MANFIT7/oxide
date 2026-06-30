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

## Status — Milestone 1 (this commit)
- ✅ window + wgpu (Metal) + glyphon text + PTY + alacritty_terminal grid
- ✅ keyboard → PTY (basic keys + arrows); resize re-grids PTY + Term
- ⬜ **per-cell color + bold/italic** (M1.5 — currently monochrome)
- ⬜ **cursor + selection** (M2)
- ⬜ **scrollback, mouse, bracketed paste** (M2)
- ⬜ **integration into the Oxide window** (M3 — coexist with the Dioxus/wry event loop, or a sibling window/process)

## Known tuning points (report what you see)
- **Font:** loaded by data + referenced as `JetBrainsMono Nerd Font Mono`. If glyphs
  fall back to a different face, the internal family name differs → adjust `FONT_FAMILY`.
- **Cell width** (`CELL_W`) is an approximation; if columns drift, derive it from the
  font's real advance metric.
- **Grid region:** M1 indexes `grid[Line(0..screen_lines)]`. If the visible region looks
  offset, switch to `term.grid().display_iter()` (handles the display offset).
