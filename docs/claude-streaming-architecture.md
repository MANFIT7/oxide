# Claude driver streaming — architecture & roadmap

How Oxide streams the **Claude Code** CLI today, why it already mirrors the
Anthropic Managed-Agents (CMA) streaming model, and the design for the one thing
it still lacks: **true mid-turn steering** via a persistent `stream-json` driver.

Written after auditing the CMA "Agent Overrides" + `roadtrip_planner` cookbook
(`github.com/anthropics/claude-cookbooks/tree/main/managed_agents/roadtrip_planner`)
against `crates/oxide-providers/src/cli.rs`.

---

## TL;DR — Agent Overrides / the cookbook do NOT make this easier

"Agent Overrides" (`agent_with_overrides`) and the `roadtrip_planner` cookbook are
the **Managed Agents** surface: Anthropic-hosted agent loop, per-session container,
SSE event stream, **API key billed per token**. That is a *different provider*, not
a way to simplify the existing `claude -p` subscription driver. Switching to it
would drop the Claude-subscription model and move tool execution off the user's
machine.

The value is the **streaming patterns** — and Oxide's `claude -p --output-format
stream-json` driver already implements them independently:

| CMA pattern | Oxide `cli.rs` equivalent |
|---|---|
| Live preview (`event_start`/`event_delta`, `content_delta`) | `stream_event` → `content_block_delta` `text_delta`/`thinking_delta` (`--include-partial-messages`) |
| Preview retires when buffered event arrives (reconcile by id) | `message_start` resets `saw_partial`; final `assistant` text skipped when `saw_partial` |
| Dedup / pair tool calls by id | `tool_use` ids tracked in `command_ids`, matched to the later `user` `tool_result` |
| Idle/terminal gate (`status_idle && stop_reason != requires_action`) | `StreamItem::Done` emitted **once, last**, by `run_jsonl` after all content flushed |
| Stream-first ordering | process stdout is the stream; events read as they arrive |
| `model`/`system`/`tools` overridable per session | `--model`, `--effort`, `--append-system-prompt` (see Item 3 below) |

Conclusion: **don't adopt CMA for the claude driver.** Harden the patterns Oxide
already has and build the one missing capability.

---

## Current architecture: resume-per-turn

`Provider::stream(req, sink)` is called **once per turn** (`oxide-core` `run_turn`).
The claude driver:

1. spawns a **fresh** `claude -p --output-format stream-json --verbose
   --include-partial-messages --dangerously-skip-permissions [--resume <id>]
   [--model ..] [--effort ..]` process,
2. writes the prompt as the `-p` arg,
3. maps each JSONL line → `StreamItem`s inside the `run_jsonl` `on_line` closure,
4. `run_jsonl` emits the terminal `StreamItem::Done` **once, after the loop** —
   never inside the closure path that the engine reads to completion.

Multi-turn context carries via claude's own `--resume <session_id>` (the
`system` event reports `session_id`; Oxide persists it and resumes next turn).

**Steering today is engine-level, between turns** (`run_turn`, the `steered`
flag): a message sent mid-turn is folded into `self.session` and applied on the
*next* round. It cannot interrupt or redirect claude *while it is generating* —
each turn is a sealed subprocess.

### The `Done`-ordering invariant (regression class v0.0.107)

The engine consumer **stops reading at `Done`** — anything a provider sends after
`Done` is silently dropped. So every provider MUST emit all text/tool/error
`StreamItem`s **before** `Done`. `run_jsonl` is the single emitter of `Done`
(`cli.rs` ~443, and the timeout path ~389). The codex driver's v0.0.109/110 fix
flushes buffered answer text at `turn.completed`/`error` *inside* the closure for
exactly this reason. A unit test now locks "Done is strictly last, nothing
dropped" (`run_jsonl_emits_done_last_after_all_content`).

---

## Item 2 — persistent `stream-json` driver (true mid-turn steering)

The only thing the per-turn model can't do: **interrupt + inject while claude is
mid-generation** (the CMA `user.message` queue + `user.interrupt`). The CLI
supports it as of **2.1.197**:

```
--input-format stream-json    realtime streaming INPUT  (with --print)
--output-format stream-json   realtime streaming OUTPUT
```

### Design

One **long-lived** `claude --print --input-format stream-json --output-format
stream-json --verbose --include-partial-messages` process per `conversation_id`,
held by the provider (keyed map), instead of one process per turn.

```
engine Op::UserTurn ─┐                         ┌─> StreamItem::* (stdout JSONL)
                     ▼                         │
   provider.feed(conversation_id, msg) ──> child.stdin (JSONL user message)
                     ▲                         │
   Op::UserTurn (mid-turn) / Op::Interrupt ───┘   per-turn Done gated on the
                                                  child's terminal `result` event
```

- **Lifecycle**: spawn on first turn; reuse across turns; reap on
  conversation close / idle TTL / app exit (process-group kill, as `run_jsonl`
  already does).
- **Input protocol**: write one JSONL `{"type":"user","message":{...}}` per
  turn to the child's stdin (kept open, not shut after the first write).
- **Per-turn terminal gate**: claude emits a `{"type":"result",...}` line at the
  end of each turn. Emit `StreamItem::Done` for *that* turn on `result` — the
  CMA `status_idle && stop_reason != requires_action` gate, applied per turn on
  one persistent process (Done is now per-turn, not per-process).
- **Mid-turn steering**: a second `user` JSONL written before `result` is the
  CMA message-queue behaviour; an interrupt maps to killing/cancelling the
  in-flight turn (or a future CLI interrupt control message). Echo/ack like CMA
  `processed_at` (queued vs processed) so the UI can show pending→accepted.
- **Reconnect/replay**: not needed (local pipe, no network drop) — the CMA
  "fetch event log then dedup by id" reconnect step has no analog here.

### Status — BUILT (process reuse), live-validated

Shipped as `ClaudePersistentProvider` in `crates/oxide-providers/src/cli.rs`,
gated behind **`OXIDE_CLAUDE_PERSISTENT`** (unset = the proven one-shot
`ClaudeCliProvider` stays default; zero regression). `build("claude")` swaps in
the persistent provider when the env var is set; its `name()` is still
`"claude"` so the engine treats it as a CLI driver unchanged.

What it does today:
- one long-lived `claude --print --input-format stream-json --output-format
  stream-json --verbose --include-partial-messages --dangerously-skip-permissions`
  per conversation (keyed by `session_key`), held in a `static` registry;
- each `stream()` writes one `{"type":"user",...}` JSONL line to the child's
  stdin (via an unbounded-channel writer task) and reads stdout until the
  child's `result` event, then emits `Done` and returns, **keeping the child
  warm** — context lives in-process, no respawn / `--resume` reload per turn;
- the line→`StreamItem` mapping is shared with the one-shot driver's intent via
  `claude_handle_line` (mirrors the `ClaudeCliProvider` closure; returns `true`
  on `result`);
- child death / read error / per-turn timeout → notice + `Done` + drop the
  registry entry (next turn respawns).

Validated by `persistent_driver_two_turns_one_process` (an `#[ignore]` live test
— `cargo test -p oxide-providers -- --ignored persistent_driver`): two turns run
through **one** warm process, each ends with `Done`, replies correct, ~6s total.

### Live mid-turn steering — WIRED (interrupt-based), live-validated

Probed Claude Code 2.1.197's stream-json input semantics first:
- a **second user line mid-turn is QUEUED**, not interrupted — claude finishes
  the current turn (full answer + `result`), then processes the queued line as
  the next turn (a second `result`). So a bare stdin write does NOT redirect a
  running generation.
- a **`control_request` interrupt DOES abort mid-flight**:
  `{"type":"control_request","request_id":"…","request":{"subtype":"interrupt"}}`
  → `control_response success` → the running turn ends within ~100ms with
  `result` `is_error:true`, `subtype:"error_during_execution"`. The next user
  line then runs as a fresh turn.

So live-steer is **interrupt-based**, and only the *interrupt* is "live"; the
steer text flows through the proven next-round path:

- `claude_persistent_interrupt(conversation_id, cwd) -> bool` (oxide-providers):
  computes the conversation's `session_key`, looks up the `STEER` registry, sets
  a per-conversation `interrupt: Arc<AtomicBool>`, and writes the interrupt
  control message to the live child's stdin. Returns false when no persistent
  child is running (every other provider / the one-shot driver) — so the engine
  hook is a no-op there, zero regression.
- Engine (`run_turn`, mid-stream `Op::UserTurn`): records the steer in the
  session (transcript/history) as before, calls
  `oxide_providers::claude_persistent_interrupt(&conv, &cwd)` to abort the
  in-flight generation, then sets `steered = true` so the steer is sent as the
  next round through the normal CLI-driver path. **No double-send** — the
  interrupt carries no text; the text comes only from the next round.
- The persistent read loop consumes the `interrupt` flag on the turn-ending
  `result` (`swap(false)`); when set, `claude_handle_line(..., suppress=true)`
  swallows the abort's `error_during_execution` notice so it isn't shown as a
  turn failure. The flag is also reset at each turn's start, so a boundary-race
  steer (fired exactly as a turn ends) degrades to a harmless no-op interrupt +
  normal next-round steering.

This sidesteps multi-turn draining and the off-by-one races of a queue-drain
design: the interrupt just makes the current turn end fast; everything else is
the existing engine round flow.

Validated by `persistent_driver_interrupt_aborts_turn` (`#[ignore]` live test):
a long turn is interrupted mid-stream and ends with `Done` and **no** error
notice, fast (≈does not wait for the full generation).

**Remaining (optional):** the interrupt aborts the current turn but the engine
still re-rounds; a `processed_at`-style queued→accepted ack to the UI (CMA
pattern) would make the steer feel acknowledged the instant it's sent. Minor
polish, not required for correctness.

---

## Item 3 — harness system override → `--append-system-prompt`

CMA Agent Overrides let a session swap `model`/`system`/`tools`/`mcp_servers`/
`skills`. The CLI analogs already used: `--model`, `--effort`. The missing one is
**`system`**: today the claude driver **drops** Oxide's harness system prompt
entirely (`extract_cli_images` keeps only the last user prompt + images), so
Claude Code runs with its own default prompt and zero harness influence.

`claude --append-system-prompt <text>` is the exact append-not-replace analog of
the override `system` field. Wiring:

- `Harness::cli_system_append(&self) -> Option<String>` (default `None`).
- `TurnRequest::system_append: Option<String>`, filled by the engine from the
  active harness on the claude turn path.
- The claude driver passes `--append-system-prompt <text>` when `Some`.

**Default `None` = zero behaviour change.** A harness opts in to layer its
persona/policy onto Claude Code. Deliberately NOT the full `system_prompt()`:
that carries the workspace file-tree + MCP instructions + skills, which Claude
Code gathers itself — injecting it would bloat every turn (busting claude's
prompt cache) and fight its tuned base prompt. The harness returns only the
short persona/policy slice it wants appended.
