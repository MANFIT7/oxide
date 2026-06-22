# Oxide Total Codebase Audit - 2026-06-22

Audit dilakukan dari workspace `/Volumes/Data/oxide` pada Rust toolchain:

- `rustc 1.96.0 (ac68faa20 2026-05-25)`
- `cargo 1.96.0 (30a34c682 2026-05-25)`
- `clippy 0.1.96`
- `rustfmt 1.9.0-stable`

## Executive Summary

Oxide sudah punya fondasi arsitektur yang kuat: satu protocol (`Op`/`Event`), engine frontend-agnostic, provider abstraction, tool router terpusat, checkpoint/diff, MCP, GUI Dioxus, TUI, desktop command center, browser smoke, automation, memory, board/worktree, dan release packaging.

Risiko terbesar awal bukan compile failure, karena baseline `cargo test --workspace --all-targets`, `cargo check -p oxide-cli`, dan browser smoke lulus. Risiko terbesar ada di mismatch desain vs implementasi dan hardening operasional:

1. Sub-agent diklaim parallel di README, tetapi implementasi saat ini sequential.
2. Worker/sub-agent berbasis CLI masih bisa hang lama karena tidak punya idle timeout realistis di engine.
3. Board runner mengklaim worktree isolation, tetapi fallback ke root workspace jika `git worktree add` gagal.
4. Reviewer/tester sub-agent masih diberi `shell`, sehingga read-only role bergantung pada prompt.
5. Release hygiene belum enforce fmt/clippy/test di semua jalur.
6. GUI motion sudah banyak dipoles, tetapi CSS punya beberapa layer motion yang saling override dan belum ada visual regression test.

Status remediation pass dengan sub-agent (2026-06-22):

- Sub-agent fan-out sudah dibuat parallel/concurrent, dengan state worker terisolasi dan hasil dikumpulkan deterministik.
- Provider CLI sekarang punya wall-clock timeout configurable dan process cleanup saat timeout.
- Reviewer/tester profile tidak lagi diberi `shell` atau tool mutatif di whitelist.
- Review gate tidak lagi pass bila output `DONE` masih mengandung blocker marker seperti gaps/issues/bugs/missing.
- Board runner tidak lagi fallback ke root workspace saat worktree gagal dibuat.
- MCP initialized notification, HTTP notify, stdio notify timeout, dan SSE multiline parsing sudah dikeraskan.
- CI/release sekarang enforce `cargo fmt --check`, `cargo clippy --workspace --all-targets -- -D warnings`, check, dan test.
- `cargo fmt --check`, `cargo clippy --workspace --all-targets -- -D warnings`, `cargo test --workspace --all-targets`, dan browser smoke ignored semuanya lulus setelah remediation.

Follow-up audit pass dengan sub-agent khusus Cursor/Synara/OpenCode parity (2026-06-22):

- GUI/motion audit menemukan pre-first-token shimmer masih dead code: empty live agent row dipush saat submit, tetapi `Message` merender kosong. Remediation: live empty agent row sekarang merender `.typing` shimmer hanya ketika message itu live; stray empty row setelah turn tetap hidden.
- GUI/motion audit menemukan `Event::Info` untuk `⚙ Running ...` masuk sebagai activity done. Remediation: activity row tersebut sekarang `running=true` sampai turn sweep/error sweep settle.
- GUI/motion audit menemukan reduced-motion masih mempertahankan spinner/shimmer infinite. Remediation: `status-spinner`, `activity-spin`, `syn-spinner`, `status-shimmer`, dan `.typing` dimatikan animasinya pada `prefers-reduced-motion: reduce` sambil tetap mempertahankan indikator statis.
- ChatGPT subscription audit menemukan OAuth refresh belum sesuai dokumen OpenCode/opencode karena memakai JSON body. Remediation: refresh token sekarang memakai form body `client_id`, `grant_type=refresh_token`, `refresh_token`.
- ChatGPT subscription audit menemukan account id hanya dibaca dari `tokens.account_id`. Remediation: provider sekarang fallback decode JWT claims `chatgpt_account_id`, namespaced `https://api.openai.com/auth.chatgpt_account_id`, lalu `organizations[0].id`.
- ChatGPT subscription audit menemukan access token kosong langsung gagal meski refresh token ada. Remediation: provider sekarang refresh access token sebelum request pertama jika access kosong dan refresh token tersedia.
- ChatGPT subscription audit menemukan request body mengabaikan `TurnRequest.temperature`. Remediation: ChatGPT body sekarang mengirim `temperature`.
- ChatGPT subscription audit menemukan `response.incomplete` tidak emit usage dan error code/type tidak surfaced. Remediation: usage dikirim untuk completed dan incomplete; `error.code`/`error.type` masuk ke error text supaya context-length retry lebih reliable.
- ChatGPT subscription audit menemukan 429 tidak mengirim rate-limit snapshot. Remediation: provider sekarang parse `x-codex-*` dan fallback `x-ratelimit-*` headers sebelum body response dikonsumsi.
- Harness/wrapper audit menemukan GUI bisa menampilkan `coding/debug/reviewer` dari `<workspace>/harnesses`, tetapi core/CLI hanya load `harness_dir` eksplisit. Remediation: `oxide-harness::manifest_dirs` menjadi resolver bersama; core, CLI, dan GUI memakai source-of-truth registry yang sama.
- Board runner memakai `cfg.harness = "coding"`; dengan resolver baru, workspace default `harnesses/coding.toml` bisa ditemukan tanpa `harness_dir` eksplisit. Regression test ditambahkan.
- Session replay metadata audit menemukan DB session hanya menyimpan provider/CLI id. Remediation: session row sekarang menyimpan `model`, `harness`, dan `reasoning_effort`; `SessionStore` menulis runtime config pada append/rewrite; GUI tab hydrate dan switch tab mengembalikan provider/model/harness/effort.
- Harness route audit menemukan `skill_routes` masih metadata mati. Remediation: engine sekarang memilih route berdasarkan trigger user text dan menginjeksi instruksi/template workflow ke system prompt.
- Review UI audit menemukan inspector `Accept` menghapus row dari review surface, tidak konsisten dengan chat card `Keep`. Remediation: inspector `Accept` sekarang menandai checkpoint sebagai kept dan mempertahankan row dengan state `✓ Kept`.
- OpenCode parity audit menemukan ChatGPT tool-call argument streaming hanya dibuffer sampai final call. Remediation: provider sekarang mengirim `StreamItem::ToolInputDelta`; engine mem-forward `Event::ToolCallDelta`; GUI menampilkan live `Preparing <tool>` preview dan settle row yang sama saat tool execution dimulai.
- Cursor motion audit menemukan live reasoning dirender di luar transcript lalu disisipkan saat finish. Remediation: live thinking box sekarang dirender di dalam current turn tepat di atas live agent row, sehingga tidak melompat saat selesai.
- Visual QA audit menemukan belum ada checklist state motion/review/session. Remediation: checklist manual dibuat di `docs/gui-visual-qa-checklist.md`.
- Visual QA follow-up menemukan checklist manual belum punya guard otomatis. Remediation: `scripts/gui-visual-qa.py` sekarang memverifikasi source-level contracts untuk pre-token shimmer, streamed tool arguments, live reasoning placement, reduced motion, edit shimmer, review Accept state, session runtime replay, dan membuat fixture HTML di `target/gui-visual-qa/fixture.html`.
- CI/release follow-up menambahkan `python3 scripts/gui-visual-qa.py` ke workflow, sehingga visual-state contract ikut menjadi gate ringan sebelum clippy/check/test/release packaging.
- Runtime visual QA follow-up menambahkan ignored CDP smoke `gui_visual_fixture_screenshot`: fixture dibuka lewat `chromiumoxide`, selector/layout order dicek, PNG ditulis ke `target/gui-visual-qa/fixture-cdp.png`, dan pixel sanity memastikan capture tidak blank.
- Browser harness follow-up memberi setiap `BrowserSession` user-data-dir unik agar CDP smoke paralel/berdekatan tidak gagal karena Chromium `SingletonLock`.
- Native visual QA follow-up menambahkan `scripts/gui-native-visual-smoke.py`: harness opsional macOS yang meluncurkan `./target/debug/oxide gui` dengan `OXIDE_GUI_VISUAL_FIXTURE=streaming`, mencari bounds window Oxide via System Events, mengambil screenshot region window dengan `screencapture`, dan menjalankan PNG pixel sanity.

Remaining high-impact gaps after this follow-up pass:

- Visual QA sekarang punya static contract harness, fixture-level screenshot/pixel smoke, dan seeded native app/window smoke opsional untuk state streaming/waiting/reasoning/activity/live-edit. Yang masih belum otomatis adalah golden/pixel-diff native dan state playback untuk reduced motion, tab switch, dan edited-card review di aplikasi asli.

## Scope dan Evidence

Yang diperiksa:

- 107 tracked files (`git ls-files`)
- 39,636 Rust LOC (`find crates -name '*.rs' ... wc -l`)
- Crates workspace, scripts, docs, harness TOML, `.oxide/commands`, `.oxide/hooks.toml`, assets GUI
- Core engine, sub-agent/orchestration, providers, MCP, tools/sandbox, GUI event binding, animation CSS, board/worktree, automation, update/release pipeline

Validasi yang dijalankan:

| Command | Result |
|---|---|
| `cargo fmt --check` | PASS |
| `cargo clippy --workspace --all-targets -- -D warnings` | PASS |
| `cargo check -p oxide-gui` | PASS |
| `cargo test -p oxide-harness manifest_dirs` | PASS: 2 passed |
| `cargo test -p oxide-providers --lib` | PASS: 28 passed |
| `cargo test -p oxide-core registry_loads_workspace_harnesses_by_default` | PASS: 1 passed |
| `cargo test -p oxide-core session_meta_preserves_runtime_config` | PASS: 1 passed |
| `cargo test -p oxide-core harness_skill_routes_match_user_intent_without_short_false_positives` | PASS: 1 passed |
| `python3 scripts/gui-visual-qa.py` | PASS: visual-state contracts passed; fixture written to `target/gui-visual-qa/fixture.html` |
| `cargo test -p oxide-core gui_visual_fixture_screenshot -- --ignored --nocapture` | PASS: CDP fixture screenshot and pixel sanity passed; PNG written to `target/gui-visual-qa/fixture-cdp.png` |
| `python3 scripts/gui-visual-qa.py --runtime` | PASS: static contracts plus ignored CDP screenshot smoke passed |
| `python3 scripts/gui-native-visual-smoke.py --no-build --strict` | PASS: native Oxide window captured to `target/gui-native-visual-smoke/oxide-gui-native.png`; pixel sanity passed |
| `cargo test --workspace --all-targets` | PASS: 204 passed, 2 ignored |
| `cargo test -p oxide-core smoke_navigate_read -- --ignored` | PASS: browser smoke passed |
| `git diff --check` | PASS |

Not verified in this audit:

- Real OpenAI/Anthropic/ChatGPT subscription API calls, because that would require live credentials and quota.
- Native golden/pixel diff and full state playback for reduced motion, tab switch, and edited-card review, because the native smoke currently seeds and verifies the streaming/waiting path only.
- `scripts/make-dmg.sh`, because it is a release build and rewrites `dist/`; existing `dist/Oxide.dmg` is present.

## Inventory Existing

### Crates

| Crate | Inventory |
|---|---|
| `oxide-protocol` | Wire contract: `Op`, `Event`, `ApprovalPolicy`, `SandboxPolicy`, `ToolSpec`. |
| `oxide-config` | Layered TOML config, MCP import support, provider/model config, GUI persisted dimensions. |
| `oxide-harness` | Built-in harness registry and external TOML harness support. Includes default, hermes, coding/debug/reviewer examples. |
| `oxide-providers` | Provider abstraction, OpenAI-compatible API, Anthropic API, ChatGPT subscription, Codex CLI, Claude CLI, Claude interactive PTY, mock providers. |
| `oxide-mcp` | MCP stdio and HTTP/SSE transport, tool namespacing as `mcp__server__tool`. |
| `oxide-core` | Engine loop, tool routing, sandbox, approval, orchestration, sub-agents, hooks, commands, memory, browser automation, compaction, session DB. |
| `oxide-frontend` | Frontend trait boundary. |
| `oxide-tui` | Ratatui/crossterm terminal UI. |
| `oxide-gui` | Dioxus GUI, chat panes, diff cards, subagent cards, model/settings UI, preview proxy, board, OTA updater, style system. |
| `oxide-desktop` | Egui/eframe desktop command center, terminal, Git controls, automations, appshots, memory, Hermes profiles, global search. |
| `oxide-cli` | `oxide` binary dispatch for GUI/TUI/exec/harnesses. |

### Feature Inventory

- Agent engine: async `Op` in / `Event` out, frontend-agnostic.
- Tooling: `read_file`, `write_file`, `edit`, `shell`, browser tools, web fetch/search, codebase search, todo, memory, MCP tools.
- Safety: approval policy, sandbox policy, macOS seatbelt for shell, checkpoint/rewind, guard for dangerous shell hooks.
- Providers: Echo, mock, OpenAI, Gemini/XAI/DeepSeek/Mistral via OpenAI-compatible path, Anthropic, ChatGPT subscription, Codex CLI, Claude CLI, Claude interactive PTY.
- Subsystems: compaction, followups, lifecycle hooks, session persistence, Codex/Claude session import, Turso/Rusqlite DB, memory/self-improvement.
- GUI: multi-tab chat, streaming markdown, diff review, approvals, subagent cards, browser preview proxy, settings, skills, automations, board, usage/rate-limit display, xterm assets.
- Desktop: egui command center, terminal, Git staging/commit/push helpers, appshots, browser actions, Hermes profiles, automations, global search.
- Harnesses: `.oxide/commands/*.md`, `.oxide/hooks.toml`, `harnesses/coding.toml`, `debug.toml`, `reviewer.toml`.
- Assets: logo, provider icons, file icons, fonts, xterm JS/CSS, Mermaid bundle, sound asset, packaged OTA binary.
- Release: CI workflow, release workflow, `make-dmg.sh`, dev signing script, cert creation script, OTA update code.

## Findings by Priority

### P1 - README claims parallel sub-agents, implementation runs them sequentially

Original audit evidence (before Worker E remediation):

- README advertises "parallel sub-agents": `README.md:21-22`, `README.md:101-102`.
- Implementation loops through subtasks and awaits each worker before starting the next: `crates/oxide-core/src/lib.rs:3033-3051`.
- Existing test only proves sub-agent tool calls work, not concurrency: `crates/oxide-core/tests/turn_loop.rs:96-144`.

Impact:

- User-facing feature claim is inaccurate.
- Slow subtasks block later subtasks.
- A single hung worker can wedge the whole orchestrated turn.
- No speedup from fan-out.

Recommendation:

- Short-term: update README/UI copy to say sequential sub-agents if parallel is not ready.
- Proper fix: implement a bounded worker supervisor with `JoinSet`/worker tasks, isolated worker state, per-worker event forwarding, and deterministic result merge.
- Add a regression test with two delayed mock providers that proves wall-clock overlap or event interleaving.
- Decide edit model before parallel writes: either per-worker worktrees and merge queue, or make parallel workers read-only/review-only.

### P1 - Worker/sub-agent stream can hang for a long time on CLI providers

Evidence:

- `stream_agentic_collect` waits on `stream_rx.recv()` without the idle timeout pattern used by `stream_collect`: `crates/oxide-core/src/lib.rs:2558-2663`.
- `idle_timeout_for` gives CLI providers a 30-day idle timeout: `crates/oxide-core/src/lib.rs:1044-1049`.
- `run_jsonl` for Codex/Claude CLI has no explicit timeout around child stdout/exit: `crates/oxide-providers/src/cli.rs:203-329`.
- Claude interactive has a hard 45-minute timeout: `crates/oxide-providers/src/cli.rs:53-57`, `crates/oxide-providers/src/cli.rs:1030-1034`.

Impact:

- A stuck CLI/sub-agent can make the turn appear permanently running.
- Sequential sub-agents make this worse: later workers never start.
- Automated board/workflow runs can stall without surfacing a crisp failure.

Recommendation:

- Add provider-specific idle timeout for `stream_agentic_collect`.
- Treat CLI providers as long-running but not infinite: reset idle on `CommandOutput`, `TextDelta`, `FileChanged`, `Notice`.
- Expose timeout as config for local long builds.
- On timeout, abort process group and emit `SubagentFinished { ok: false }`.
- Add a mock provider that never sends `Done` and assert engine emits timeout and finishes.

### P1 - Board worktree isolation has unsafe fallback to root workspace

Evidence:

- `run_card` creates a worktree, but if the worktree path does not exist it falls back to `root`: `crates/oxide-gui/src/board.rs:202-220`.
- It then runs with `ApprovalPolicy::Never`: `crates/oxide-gui/src/board.rs:222-229`.

Impact:

- A feature described as isolated worktree execution can mutate the main workspace if `git worktree add` fails.
- This is especially risky on dirty worktrees and automated issue runs.

Recommendation:

- Fail the card run if worktree creation fails; do not fallback to root.
- Surface the git stderr in the card result.
- Add tests for worktree creation failure and ensure no engine is spawned on `root`.
- Consider setting a distinct branch/worktree cleanup policy and conflict reporting.

### P1 - CLI providers intentionally bypass native CLI permissions

Evidence:

- Codex CLI is invoked with `--dangerously-bypass-approvals-and-sandbox`: `crates/oxide-providers/src/cli.rs:382-384`.
- Claude CLI is invoked with `--dangerously-skip-permissions`: `crates/oxide-providers/src/cli.rs:607-612`, `crates/oxide-providers/src/cli.rs:939`.
- README also notes GUI permissions are bypassed by default: `README.md:82-83`.

Impact:

- Oxide's `ToolRouter` approval/sandbox boundary does not control tools executed inside black-box CLI providers.
- The risk is acceptable only if the UI and docs make this explicit and the default mode is intentional.

Recommendation:

- Add a visible provider-mode warning: API providers use Oxide ToolRouter; CLI providers use provider-native bypass.
- For sub-agents/board runs, prefer API/mock/native tools when enforcing capability boundaries.
- Add a "safe CLI" mode that omits dangerous flags and routes approvals through the provider where possible.

### P2 - Reviewer/tester sub-agent profiles are not enforceably read-only

Evidence:

- Tester profile includes `shell`: `crates/oxide-core/src/lib.rs:1200-1203`.
- Reviewer profile includes `shell`: `crates/oxide-core/src/lib.rs:1211-1219`.
- Test only asserts reviewer lacks `edit`; it does not assert shell is removed: `crates/oxide-core/src/lib.rs:4858-4866`.
- `shell` is mutating by `ToolSpec`: `crates/oxide-core/src/tools.rs:154-156`, `crates/oxide-core/src/tools.rs:314-388`.

Impact:

- "Do not edit files" is prompt-only for reviewer/tester.
- Under `ApprovalPolicy::Never` or full access, a reviewer/tester can mutate the workspace via shell.

Recommendation:

- Remove `shell` from reviewer/tester profiles, or introduce a read-only command tool with allowlisted commands.
- Add tests asserting reviewer/tester tool names exclude all mutating tools.
- If command execution is required for tester, enforce a read-only shell wrapper and block writes/network as applicable.

### P2 - Sub-agent profile router is English-centric

Evidence:

- Profile routing checks English terms: `test`, `verify`, `lint`, `build`, `review`, `audit`, `risk`, `inspect`, `explore`, `find`, `research`: `crates/oxide-core/src/lib.rs:1188-1238`.
- Only Indonesian term present is `risiko`: `crates/oxide-core/src/lib.rs:1206-1209`.

Impact:

- Indonesian tasks like "uji", "tes", "verifikasi", "cek", "cari", "audit" can route to the wrong worker profile.
- The user primarily works in Indonesian, so this is not theoretical.

Recommendation:

- Add localized keyword sets for Indonesian and common repo terms.
- Prefer explicit planner labels if available, e.g. `role: tester/reviewer/explorer/implementer`.
- Add tests for Indonesian task strings.

### P2 - Review gate can pass contradictory reviews

Evidence:

- Review loop asks for first line `DONE` or `GAPS`: `crates/oxide-core/src/lib.rs:3193-3199`.
- Detection passes if output starts with `DONE`, even if it later contains "gap": `crates/oxide-core/src/lib.rs:3200-3203`.

Impact:

- A response like `DONE, but gaps remain...` passes.
- A model formatting mistake can skip auto-fix.

Recommendation:

- Parse only the first non-empty line and require exact `DONE` or `GAPS`.
- Treat malformed first line as `GAPS`.
- Add tests for `DONE but gap`, lowercase, whitespace, markdown bullets, and malformed responses.

### P2 - Auto-verify silently ignores spawn errors/timeouts and misses fmt

Evidence:

- `run_verify` returns `None` for spawn error or timeout: `crates/oxide-core/src/lib.rs:3861-3864`.
- Auto-detected Rust verification uses `cargo check --message-format short`, not fmt/clippy: `crates/oxide-core/src/lib.rs:3832-3836`.
- Current repo fails `cargo fmt --check`.

Impact:

- A broken or missing verifier is treated like success/no-op.
- Formatting drift can ship unless caught manually.

Recommendation:

- Return a failure report for timeout/spawn errors unless explicitly configured as non-blocking.
- For Rust edits, run `cargo fmt --check` before `cargo check`, or make `verify_command` default configurable per repo.
- Emit a distinct "verify skipped" audit event when no relevant command is selected.

### P2 - CI/release gates do not enforce current quality checks

Worker E + integration status (2026-06-22): workflow config remediated. CI now blocks `cargo fmt --check` and runs blocking `cargo clippy --workspace --all-targets -- -D warnings`; release now runs fmt, clippy, workspace check, and workspace tests before signing/package steps. Follow-up integration also fixed the Rust fmt/clippy debt opened by the new gate, and the full workspace clippy command now passes.

Original audit evidence before Worker E:

- CI installed clippy but never ran it.
- CI formatting used `continue-on-error: true`.
- Release workflow built and packaged assets without tests/fmt/clippy in the release job.

Impact before remediation:

- Existing `cargo fmt --check` and `clippy -D warnings` failures could still pass CI/release paths.
- Release tags could ship code that main-branch PR CI would have caught, depending on trigger path.

Recommendation:

- Keep `cargo fmt --check` blocking in CI.
- Keep `cargo clippy --workspace --all-targets -- -D warnings` blocking in CI.
- Keep release preflight aligned with CI for fmt, clippy, check, and tests.
- Optional next hardening: make release depend on an already-green CI run for the tag SHA.

### P2 - MCP initialized notification and notify transport handling are weak

Evidence:

- Client ignores failure of `notifications/initialized`: `crates/oxide-mcp/src/lib.rs:115-119`.
- HTTP `notify` ignores `post` errors and returns `Ok(())`: `crates/oxide-mcp/src/http.rs:146-149`.
- Stdio `notify` writes without timeout while holding the mutex: `crates/oxide-mcp/src/stdio.rs:125-129`.
- HTTP SSE parsing consumes single `data:` lines only: `crates/oxide-mcp/src/http.rs:96-104`.

Impact:

- Some MCP servers require initialized notification; failure can become a later, less obvious tool/list failure.
- A blocked stdio notification can wedge a server transport.
- Multi-line SSE JSON-RPC payloads may be missed.

Recommendation:

- Propagate initialized notification failures during connect, or emit degraded health with exact reason.
- Wrap stdio notify send in the same timeout as calls.
- Return HTTP notify post errors.
- Implement proper SSE event data concatenation.

### P2 - Preview proxy does not forward normal request headers/cookies

Evidence:

- Normal proxied requests build a new reqwest request with method, URL, and body only: `crates/oxide-gui/src/preview_proxy.rs:131-139`.
- WebSocket upgrade replays raw bytes, but non-upgrade HTTP does not forward cookies, authorization, accept, content-type, or custom headers.

Impact:

- Authenticated localhost apps, CSRF-protected forms, API routes, and some dev-server features can behave differently through Oxide preview.

Recommendation:

- Parse and forward safe request headers: `Cookie`, `Authorization`, `Accept`, `Content-Type`, `User-Agent`, `Referer`, and framework-specific headers.
- Strip hop-by-hop headers.
- Add integration tests with a local server requiring a cookie/header.

### P2 - OTA integrity check is optional

Evidence:

- `apply` verifies SHA-256 only if `sha256_url` is present: `crates/oxide-gui/src/update.rs:155-160`.
- `expected_sha256` returns `Ok(None)` when no checksum URL exists: `crates/oxide-gui/src/update.rs:184-190`.
- Release workflow does generate checksums: `.github/workflows/release.yml:177-180`.

Impact:

- A custom manifest or malformed GitHub release can update without a checksum.
- HTTPS helps transport integrity, but it is not an artifact integrity policy.

Recommendation:

- Require checksum for OTA update unless an explicit unsafe override is enabled.
- Add signature verification or code-signing identity verification before replace.
- Add tests for missing checksum behavior.

### P2 - GUI output copy button is inconsistent with other async eval usage

Evidence:

- Activity output copy calls `dioxus::document::eval(&js)` but does not `await`, spawn, or join it: `crates/oxide-gui/src/lib.rs:10171-10178`.
- Other code paths spawn and await eval calls: examples around `crates/oxide-gui/src/lib.rs:5929-5937`, `crates/oxide-gui/src/lib.rs:6777`.

Impact:

- Copy output may be a no-op or fire inconsistently depending on Dioxus eval semantics.

Recommendation:

- Use the same pattern as other copy buttons: `spawn(async move { let _ = document::eval(&js).await; })`.
- Add a small UI/unit harness if possible, or manual GUI smoke checklist.

### P2 - Animation system is polished but hard to reason about and untested visually

Evidence:

- Multiple motion layers define shimmer, row entrance, details transitions, spinners, queue/toast animations: `crates/oxide-gui/assets/style.css:1476-1488`, `:2118-2148`, `:2495-2515`, `:2600-2784`.
- `details::details-content` is used as progressive enhancement: `crates/oxide-gui/assets/style.css:2122-2125`.
- Reduced motion intentionally keeps some functional spinners: `crates/oxide-gui/assets/style.css:2511-2515`, `:2774-2784`.

Impact:

- Visual quality is likely good in the happy path, but overlapping CSS layers make regressions hard to predict.
- Browser/WebView support for `details::details-content` and `interpolate-size` should be treated as progressive, not guaranteed.
- Static source-level visual-state contract checks now exist and run in CI/release; fixture-level CDP screenshot/pixel smoke exists as an ignored local test. Native app/window screenshot coverage is still missing.

Recommendation:

- Consolidate motion tokens and selectors into one canonical motion section.
- Keep the reduced-motion checklist and static visual contract script; use the fixture-level CDP smoke as Level 1 runtime proof, then add native app/window screenshot smoke.
- Keep transform/opacity-only animations for streaming paths; avoid `filter: blur` on large content if profiling shows jank.
- Add a Dioxus GUI smoke script or manual checklist for: streaming row, diff card open/close, subagent card, pending edit shimmer, reduced motion.

### P3 - DB worker can panic instead of surfacing recoverable errors

Evidence:

- Worker fallback to in-memory DB uses `expect`: `crates/oxide-core/src/db.rs:170-184`.
- Job send/receive use `expect`: `crates/oxide-core/src/db.rs:189-201`.

Impact:

- If the DB worker panics or channel closes, callers panic.
- For a desktop app, DB failure should degrade session persistence, not crash unrelated agent work.

Recommendation:

- Convert `with_db` to return `Result<T>`.
- Emit UI/audit degradation when persistence is unavailable.
- Add tests for simulated closed worker/channel if feasible.

### P3 - Clippy and formatting drift are small but real

Evidence:

- `cargo fmt --check` currently fails in:
  - `crates/oxide-core/src/lib.rs:308`
  - `crates/oxide-core/src/lib.rs:319`
  - `crates/oxide-core/src/lib.rs:1474`
  - `crates/oxide-providers/src/chatgpt.rs:284`
  - `crates/oxide-providers/src/chatgpt.rs:543`
  - `crates/oxide-providers/src/chatgpt.rs:783`
  - `crates/oxide-providers/src/chatgpt.rs:869`
- `cargo clippy --workspace --all-targets -- -D warnings` fails in:
  - `crates/oxide-providers/src/cli.rs:900` (`derivable_impls`)
  - `crates/oxide-providers/src/cli.rs:918` (`too_many_arguments`)
  - `crates/oxide-providers/src/cli.rs:1475` (`collapsible_str_replace`)

Impact:

- These are not runtime bugs, but they make CI discipline weaker and hide future meaningful warnings.

Recommendation:

- Run `cargo fmt`.
- Derive `Default` for `ClaudeTranscriptTail`.
- Wrap `run_claude_interactive_turn` args into a struct or add a targeted allow if the call shape is intentional.
- Replace consecutive `replace` with `replace(['/', '.'], "-")`.

## Feature-by-Feature Notes

### Sub-agent and Orchestration

Strengths:

- Clear event model: `SubagentStarted`, `SubagentFinished`, worker-owned command events.
- Worker sessions are isolated from parent CLI resume state.
- Tool calls and CLI diffs can create checkpoints.
- High-agency default sub-agent prompt exists.

Gaps:

- Not parallel despite claim.
- Role capability not enforceable for reviewer/tester.
- English-centric profile routing.
- Timeout behavior is too weak for CLI workers.
- Review classifier is brittle.

### Providers

Strengths:

- Hand-rolled provider abstraction keeps engine provider-agnostic.
- ChatGPT subscription handles refresh, rate-limit snapshots, incomplete/failed SSE, and tool-call argument buffering.
- CLI providers capture command/file events and process group cleanup on interrupt.
- `cargo test` covers important parsing paths.

Gaps:

- CLI bypass flags need stronger UX disclosure and safe-mode path.
- `run_jsonl` does not enforce a turn timeout.
- Clippy errors are concentrated in Claude interactive provider.
- Live API contract was not exercised in this audit.

### Tools, Sandbox, Harness

Strengths:

- One central `ToolRouter` gate.
- Native read/write/search/shell have workspace and size guards.
- Shell timeout kills process group.
- Harness TOML is simple and useful.

Gaps:

- Non-macOS shell sandbox is explicitly not implemented (`crates/oxide-core/src/tools.rs:540-546`).
- Approval/session approval lifts sandbox for approved tool calls by design; docs/UI should make that very clear.
- Reviewer/tester profile issue described above.

### MCP

Strengths:

- Namespacing avoids tool name collisions.
- MCP tools are marked mutating for approval.
- HTTP transport supports bearer token/env headers and session id.
- Stdio calls have request timeout.

Gaps:

- Notification error handling/timeout.
- Single-line SSE parser.
- All MCP tools are mutating, which is safe but coarse; future improvement could respect tool annotations if available.

### GUI and Animation

Strengths:

- GUI has many production-grade touches: tabular numerals, reduced-motion handling, streaming markdown containment, no re-animation for live markdown blocks, command/subagent log truncation.
- Subagent UI maps worker command events into nested logs.
- Diff cards support keep/revert and checkpoint rewind.

Gaps:

- No automated visual verification.
- Motion CSS is layered and hard to audit.
- Activity copy output likely needs async eval consistency.
- Some event ordering fallbacks exist, but worker command logs can still be lost if command events precede card creation.

### Desktop/Egui

Strengths:

- Very broad command center: Git, terminal, automations, appshots, memory, Hermes profiles, settings, global search.
- Large unit test surface: 103 desktop tests passed.

Gaps:

- Desktop and Dioxus GUI appear to overlap in feature responsibility; risk of parity drift is high.
- Native UI was not visually smoke-tested in this audit.

### Browser/Preview

Strengths:

- `cargo test -p oxide-core smoke_navigate_read -- --ignored` passed.
- Preview proxy supports HTML injection and WebSocket/HMR tunneling.

Gaps:

- Preview proxy does not forward normal request headers/cookies.
- Browser snapshot/appshot paths are tested in desktop, but not end-to-end with actual capture in this audit.

### Release/Update

Strengths:

- `make-dmg.sh` signs when a stable identity exists, creates app bundle and DMG.
- Release workflow validates tag/version and attaches checksums.
- OTA updater can verify checksum when present and decompress `.gz`.

Gaps:

- Existing fmt/clippy debt now blocks the workflow gates until the Rust owners fix it.
- OTA checksum is optional.
- App is ad-hoc/not notarized per README, expected but should remain an explicit install caveat.

## Recommended Implementation Plan

### Phase 0 - Hygiene and gates (same day)

1. Run `cargo fmt` (done).
2. Fix clippy warnings opened by `-D warnings` across providers/core/gui/desktop (done).
3. Update CI (done):
   - remove `continue-on-error` from fmt
   - add `cargo clippy --workspace --all-targets -- -D warnings`
4. Add release preflight or require CI success for tag SHA (release preflight done).
5. Re-run:
   - `cargo fmt --check`
   - `cargo clippy --workspace --all-targets -- -D warnings`
   - `cargo test --workspace --all-targets`
   - `cargo test -p oxide-core smoke_navigate_read -- --ignored`
   - `cargo check -p oxide-cli`

### Phase 1 - Sub-agent correctness

1. Implement true parallel fan-out (done).
2. Isolate worker state and shared counters/checkpoints safely (done).
3. Add concurrency regression test proving overlap (done).
4. Remove mutating tools from reviewer/tester or provide read-only shell (done by removing mutating tools).
5. Remaining: add Indonesian routing tests and define smarter conflict resolution for parallel implementers that edit the same file.

### Phase 2 - Safety and orchestration hardening

1. Add provider-side CLI timeout and process cleanup (done).
2. Make review gate exact-first-line parsing with blocker marker detection (done).
3. Make auto-verify report timeout/spawn errors.
4. Fail board card if worktree creation fails (done).
5. Add tests:
   - worker timeout
   - board worktree failure does not mutate root
   - reviewer/tester cannot mutate
   - malformed review response triggers gaps

### Phase 3 - Integration hardening

1. MCP notify errors/timeouts (done).
2. Proper SSE data concatenation in MCP HTTP (done).
3. Preview proxy header/cookie forwarding.
4. OTA checksum required by default.
5. DB worker degradation instead of panic.

### Phase 4 - GUI and animation QA

1. Consolidate CSS motion layers.
2. Keep the manual checklist and static visual contract script green:
   - `python3 scripts/gui-visual-qa.py`
   - fixture review: `target/gui-visual-qa/fixture.html`
3. Add a native app/window screenshot/pixel visual smoke:
   - streaming first token
   - command row start/output/finish
   - subagent row and nested logs
   - diff open/close and keep/revert
   - reduced-motion mode
4. Fix ActivityRow copy output to await/spawn `document::eval`.
5. Profile long streaming replies for layout/paint jank.

## Verification Checklist

### Always run before claiming remediation complete

- [ ] `cargo fmt --check`
- [ ] `cargo clippy --workspace --all-targets -- -D warnings`
- [ ] `cargo test --workspace --all-targets`
- [ ] `cargo check -p oxide-cli`
- [ ] `cargo test -p oxide-core smoke_navigate_read -- --ignored`
- [ ] `python3 scripts/gui-visual-qa.py`
- [ ] `python3 scripts/gui-visual-qa.py --runtime` when a Chromium-compatible browser is available
- [ ] `git status --short` shows only intended files

### Sub-agent specific

- [ ] Sequential/parallel behavior matches README/UI text.
- [ ] At least one test proves parallel overlap if the feature is still advertised as parallel.
- [ ] Reviewer/tester profiles expose no mutating tools.
- [ ] Indonesian task routing works for tester/reviewer/explorer cases.
- [ ] Hung worker times out and emits failed `SubagentFinished`.
- [ ] Worker edit creates checkpoint and diff.

### Animation/UI specific

- [ ] Normal motion: no overlapping text, no flicker on streaming markdown, command rows settle.
- [ ] Reduced motion: decorative motion disabled, functional progress still understandable.
- [ ] Diff details open/close works in the target WebView.
- [ ] Activity copy buttons work.
- [ ] Subagent command logs render and truncate correctly.

### Harness/integration specific

- [ ] External harness TOML loads and tool list matches manifest.
- [ ] MCP stdio and HTTP connect failures show actionable health details.
- [ ] Preview proxy works for pages requiring cookies/headers.
- [ ] Browser smoke passes.
- [ ] Board runner refuses root fallback when worktree creation fails.

### Release specific

- [x] CI blocks fmt/clippy/test failures.
- [x] Release job runs or depends on the same gates.
- [ ] `scripts/make-dmg.sh` produces `dist/Oxide.dmg`.
- [ ] Release assets include `.sha256`.
- [ ] OTA refuses missing checksum unless explicitly overridden.

## Closing Assessment

After the remediation pass, Oxide is materially closer to a production-grade orchestration bar: sub-agent fan-out is now genuinely parallel, CLI workers have bounded runtime, MCP and board isolation failures are surfaced, and CI/release gates are enforceable and currently green.

Remaining high-impact work:

1. Add conflict-aware merge/reporting for parallel implementers that edit the same file.
2. Make auto-verify failures/timeouts first-class audit events.
3. Add native Dioxus app/window screenshot/pixel smoke coverage for motion-critical states.
4. Tighten OTA checksum policy.
5. Run release packaging smoke (`scripts/make-dmg.sh`) before the next tag.

After those are done, Oxide's architecture will be much closer to the quality implied by its README: one robust engine, multiple frontends, safe extensibility, and agent orchestration that behaves exactly as advertised.
