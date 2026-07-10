<p align="center">
  <img src="logo.png" width="96" alt="Oxide logo">
</p>

<h1 align="center">Oxide</h1>

<p align="center">
  Rust-native AI coding agent — like Codex/Claude Code, but every feature owned and built in house.
  One reusable engine drives <b>both a terminal UI and a desktop GUI</b>; behavior is updated through
  <b>pluggable harnesses</b> without recompiling the engine.
</p>

---

## Highlights

- **One engine, two frontends** — a frontend-agnostic core (`Op` in / `Event` out) powers a Dioxus
  desktop GUI *and* a ratatui TUI. Adding a UI means writing one more `Frontend`, never touching the engine.
- **Bring your own login, no API key** — drive your already-authenticated `codex` / `claude` CLIs, or
  use your **ChatGPT subscription directly** (reads `~/.codex/auth.json`, hits the Responses backend — no CLI spawn, fast).
- **Orchestration** — two-stage *plan → implement → review* with an **auto-fix loop** (re-implements until the
  reviewer signs off), plus **parallel sub-agents** (fan a plan out to N backends, then synthesize).
- **Slash commands** — `/review`, `/test`, `/commit`, … as `.oxide/commands/*.md` templates with `$ARGUMENTS`.
- **Skills browser** — aggregates every skill from Codex (`~/.codex/plugins`), Claude Code (`~/.claude/skills`) and Oxide.
- **Lifecycle hooks** — `pre_tool` (can block), `post_tool`, `stop` shell commands via `.oxide/hooks.toml`.
- **Diff review** — every file write shown as a colored unified diff with one-click **Revert** (checkpoint rewind).
- **MCP client** — connect external tool servers; tools surface as `mcp__<server>__<tool>` through the same approval/sandbox path.
- **Persistent memory + self-improvement** — `remember` / `save_skill` tools write durable memory under `.oxide/memory`.
- **OTA self-update** — checks a GitHub repo's latest release, downloads the macOS asset, swaps the binary, restarts.
- **Packaged** — `scripts/make-dmg.sh` builds a signed `Oxide.app` + `Oxide.dmg`.

## Architecture

A frontend-agnostic engine that speaks only `Op` (in) and `Event` (out). Every UI is a thin shell.

```
  frontend ──Op──▶   [ oxide-core engine ]   ──Event──▶ frontend
  (GUI / TUI / RPC)        │      │
                    Harness │      │ Provider (streaming)
                 (prompt+tools)    │
                            ToolRouter ──▶ hooks ──▶ approval ──▶ sandbox
```

### Crates

| Crate | Role |
|---|---|
| `oxide-protocol` | Wire types: `Op`, `Event`, `ToolSpec`, policies. The contract. |
| `oxide-core` | The engine: async submit/event loop, `ToolRouter` chokepoint, orchestration, hooks, commands, memory. |
| `oxide-harness` | Pluggable behavior packs (prompt + tools + loop policy). Builtins `default`, `hermes`; external via TOML manifest. |
| `oxide-providers` | Streaming providers: OpenAI + Anthropic APIs, `codex`/`claude` CLI drivers, ChatGPT-subscription backend. |
| `oxide-mcp` | MCP client — consume external tool servers over stdio JSON-RPC. |
| `oxide-config` | Layered TOML config (defaults → `~/.config/oxide` → project `./oxide.toml`). |
| `oxide-frontend` | The `Frontend` trait every UI implements. |
| `oxide-tui` | Terminal UI (ratatui + crossterm). |
| `oxide-gui` | Desktop GUI (Dioxus) — Codex-style command center over the same engine. |
| `oxide-cli` | The `oxide` binary / subcommand dispatcher. |

## Providers

| `--provider` | Auth | Notes |
|---|---|---|
| `chatgpt` | `~/.codex/auth.json` (Sign in with ChatGPT) | Direct Responses backend — no CLI spawn, fastest. Uses your Plus/Pro plan. |
| `codex` | local `codex` login | Spawns Codex CLI (its own tools/sandbox). 272k context. |
| `claude` | local `claude` login | Spawns Claude Code. Opus, 1M context. |
| `openai` | `OPENAI_API_KEY` | Hand-rolled reqwest + SSE. |
| `anthropic` | `ANTHROPIC_API_KEY` | Hand-rolled reqwest + SSE. |

> The `chatgpt` provider reuses the same OAuth token Codex stores and calls an **internal** endpoint
> (the same one Codex uses). It works but is not officially documented for third-party use — fine for
> personal use, can break if the endpoint changes. For the fully-sanctioned path use `--provider codex`.

CLI binaries are resolved even from a minimal `PATH` (Finder launch) by probing `~/.superconductor/bin`,
`~/.local/bin`, Homebrew, etc. — override with `OXIDE_CODEX_BIN` / `OXIDE_CLAUDE_BIN`.

## Build & run

```sh
cargo build
cargo test                                  # engine loop, providers, tools, sandbox, commands…

# Desktop GUI — permissions bypassed by default, opens the Open-folder welcome on first run:
./target/debug/oxide gui

# Interactive TUI:
./target/debug/oxide tui
./target/debug/oxide --harness hermes tui
./target/debug/oxide --safe --provider claude tui     # re-enable approval prompts

# Headless single turn:
./target/debug/oxide --provider chatgpt exec "summarize this repo"
./target/debug/oxide --provider codex   exec --yes "add a unit test for the parser"

./target/debug/oxide harness list
```

## Orchestration

Enable in **Settings → Orchestrate**. Each turn becomes:

1. **🧭 Plan** — front provider produces a numbered plan (shown in the thinking box).
2. **⚙ Implement** — backend provider executes it (or **🤖 sub-agents** fan the steps out in parallel, then **🧩 synthesize**).
3. **🔍 Review → 🔁 Fix** — reviewer replies `DONE` / `GAPS`; on gaps the backend re-implements, looping up to 3×.

Front/backend are any providers — e.g. plan with Opus, implement with Codex. Oxide drives the sequence,
so it works even with the black-box CLI providers.

## Extending

- **Slash commands** — drop `.oxide/commands/name.md` (YAML frontmatter `description:` + body with `$ARGUMENTS`). Type `/` in the composer.
- **Hooks** — `.oxide/hooks.toml`: `pre_tool = ["./guard.sh"]` (non-zero exit blocks), `post_tool = ["cargo fmt"]`, `stop = ["cargo test"]`. Payload JSON in `$OXIDE_HOOK_PAYLOAD`.
- **Harnesses** — drop a `*.toml` manifest into `harness_dir` to add/update behavior without recompiling.
- **MCP** — open **MCP servers** to trust servers discovered from Codex/Claude,
  or add an explicit stdio/HTTP server. Discovered servers are persisted as
  secret-free references and resolved from their source config at runtime.

```toml
[[mcp_servers]]
name = "filesystem"
command = "npx"
args = ["-y", "@modelcontextprotocol/server-filesystem", "."]
env_vars = ["FILESYSTEM_TOKEN"] # forwarded explicitly; parent secrets are not inherited
startup_timeout_sec = 15
tool_timeout_sec = 60
disabled_tools = ["delete_file"]
required = false

[[mcp_servers]]
name = "remote"
url = "https://example.com/mcp"
bearer_token_env_var = "REMOTE_MCP_TOKEN"
```

MCP tools require approval by default. Fully unrestricted access remains an
explicit per-process opt-in via `--dangerously-skip-permissions`.

## Install (macOS)

Download `Oxide.dmg` from [Releases](https://github.com/MANFIT7/oxide/releases), open it, and drag
**Oxide.app** to Applications.

The app is **ad-hoc signed, not notarized**, so on first launch macOS Gatekeeper shows
*"Apple could not verify Oxide…"*. Clear the quarantine flag once:

```sh
xattr -dr com.apple.quarantine /Applications/Oxide.app
```

…then open it normally. (Alternatively: **System Settings → Privacy & Security → Open Anyway**.)

## Packaging (macOS)

```sh
bash scripts/make-dmg.sh        # → dist/Oxide.dmg (release build + Oxide.app + icns + dmg)
```

Set a **GitHub repo** in *Settings → Updates* (`owner/name`). Each release the app reads
`releases/latest`, picks the macOS asset, and offers a one-click **Update & restart**. To publish:

```sh
bash scripts/make-dmg.sh
cp target/release/oxide oxide-macos-arm64
gh release create v0.0.2 dist/Oxide.dmg oxide-macos-arm64
```

> macOS-only for now (Linux/Windows later).

## License

MIT OR Apache-2.0.
