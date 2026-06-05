# Oxide — agent instructions

Rust-native AI coding agent. One engine (`oxide-core`) drives a Dioxus GUI and a ratatui TUI.

## Build & test
- Build: `cargo build` · binary: `./target/debug/oxide`
- After editing `oxide-gui`, rebuild `-p oxide-cli` (the `oxide` binary relinks the GUI rlib).
- Test: `cargo test`. Browser smoke test (needs Chrome): `cargo test -p oxide-core smoke_navigate_read -- --ignored`.
- Package macOS app: `bash scripts/make-dmg.sh` → `dist/Oxide.dmg`.

## Conventions
- Hand-rolled providers (reqwest + SSE); no vendor SDKs.
- Frontend-agnostic engine: `Op` in / `Event` out. Never give the engine UI knowledge.
- All tool filesystem/shell access stays inside the workspace root.
- Keep changes minimal and matching surrounding style.

## Layout
- `oxide-core` — engine, tools, orchestration, hooks, commands, memory, browser, compaction.
- `oxide-providers` — openai/anthropic APIs, codex/claude CLI drivers, chatgpt-subscription backend.
- `oxide-gui` (Dioxus) · `oxide-tui` (ratatui) · `oxide-cli` (binary).
