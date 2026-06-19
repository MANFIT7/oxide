# ChatGPT/Codex subscription streaming — alignment notes (opencode → Oxide `chatgpt.rs`)

Reference for aligning `crates/oxide-providers/src/chatgpt.rs` (the no-API-key ChatGPT
Plus/Pro provider hitting `chatgpt.com/backend-api/codex/responses`) with how
[anomalyco/opencode](https://github.com/anomalyco/opencode/tree/dev) (TypeScript) drives the
same Codex-subscription backend. Studied from a `dev`-branch clone; line refs are a
point-in-time snapshot — re-verify before relying on them.

Everything here is **for the Rust implementation**. Nothing in this doc is implemented yet
unless the gap table says so.

---

## 1. What opencode does (reference spec)

### 1.1 Auth + token refresh — `packages/opencode/src/plugin/openai/codex.ts`
- Credential is an OAuth object: `{ type:"oauth", access, refresh, expires (ms), accountId? }`.
- `accountId` is pulled from the access-token JWT claims, in order: `chatgpt_account_id` →
  `https://api.openai.com/auth.chatgpt_account_id` → `organizations[0].id`.
- **Refresh** (this is the big thing Oxide lacks): when `access` is empty OR `expires < now`,
  `POST https://auth.openai.com/oauth/token` with form body
  `grant_type=refresh_token & refresh_token=<refresh> & client_id=app_EMoamEEZ73f0CkXaXp7hrann`.
  Response `{ id_token, access_token, refresh_token, expires_in? }`; new expiry =
  `now + (expires_in ?? 3600)*1000`. Refresh token may rotate — persist it back. Concurrent
  refreshes are de-duped behind one in-flight promise.
- Issuer/client constants: `ISSUER=https://auth.openai.com`, `CLIENT_ID=app_EMoamEEZ73f0CkXaXp7hrann`.

### 1.2 Endpoint + headers
- Endpoint: `https://chatgpt.com/backend-api/codex/responses` (it rewrites any `/v1/responses`
  or `/chat/completions` to this).
- Headers: `Authorization: Bearer <access>`, `ChatGPT-Account-Id: <accountId>`,
  `originator: opencode`, `User-Agent: opencode/<ver> (<os> <rel>; <arch>)`,
  `session-id: <conversation uuid>`. WebSocket variant adds `openai-beta: responses_websockets=2026-02-06`.

### 1.3 Request body (Responses API)
- `model`, `input[]`, `stream:true`, `store:false` (default — stateless), `instructions`,
  `include:["reasoning.encrypted_content"]`, `reasoning:{ effort, summary:"auto" }`,
  `tools:[{type:"function",name,description,parameters,strict?}]`, `tool_choice`,
  `prompt_cache_key`, `text:{verbosity}`, `max_output_tokens`, `temperature`, `top_p`,
  `service_tier`.
- `input[]` item shapes: user `{role:"user",content:[{type:"input_text",text}|{type:"input_image",image_url}]}`;
  assistant `{role:"assistant",content:[{type:"output_text",text}]}`;
  reasoning `{type:"reasoning",id,summary:[{type:"summary_text",text}],encrypted_content?}`
  (or `{type:"item_reference",id}` when `store:true`); tool call
  `{type:"function_call",call_id,name,arguments(JSON string)}`; tool result
  `{type:"function_call_output",call_id,output}`.
- Codex-model defaults: `store:false`, `include` encrypted reasoning, effort `medium`, no
  `text.verbosity:"low"` for codex models.

### 1.4 SSE event handling — `packages/llm/src/protocols/openai-responses.ts`
State-machine parser. Events handled (→ normalized event emitted):
- `response.created` / `response.in_progress` → response metadata / start.
- `response.output_text.delta` → `text-delta` (keyed by item_id; auto text-start on first).
- `response.reasoning_text.delta`, `response.reasoning_summary_text.delta` (+ `.done`,
  `reasoning_summary_part.added/done`) → `reasoning-start/delta/end`. Separate channel from text.
- `response.output_item.added` → start of an item. For `reasoning`, captures
  `encrypted_content` EARLY. For `function_call`, seeds a pending tool buffer
  (`item.id`, `call_id`, `name`, `arguments`).
- `response.function_call_arguments.delta` → **streams tool input** (`tool-input-delta`),
  appended to the per-item buffer.
- `response.output_item.done` → finalize. `function_call` → `tool-input-end` + `tool-call`
  (uses `item.arguments` if present, else the accumulated buffer). Hosted tools
  (`web_search_call`, `file_search_call`, `code_interpreter_call`, `local_shell_call`,
  `mcp_call`, …) → `tool-call` + `tool-result`. `reasoning` → close summary parts.
- `response.completed` / `response.incomplete` → `step-finish`+`finish`; usage from
  `response.usage` (`input_tokens`, `input_tokens_details.cached_tokens`, `output_tokens`,
  `output_tokens_details.reasoning_tokens`). Finish reason: null→`stop`/`tool-calls`,
  `max_output_tokens`→`length`, `content_filter`→`content-filter`.
- `response.failed` and top-level `error` → `provider-error` (classifies `context_length_exceeded`
  → context-overflow).
- IDs: `call_id` is the tool-call id (paired to `function_call_output`), `item_id` is the
  Responses output index (kept in provider metadata). Dedup by emitted-id set.

### 1.5 Retry — `packages/llm/src/route/executor.ts`
- Max 2 retries, base 500ms, exponential `base*2^attempt*rand(0.8..1.2)`, cap 10s; honor
  `retry-after` / `retry-after-ms`. Retryable: 5xx/503/504/529, network. 401→refresh,
  403→hard, 429→rate-limit (read `x-codex-*` + `x-ratelimit-*`), 400/413→invalid/context.

### 1.6 Desktop UI streaming + status — `packages/app` (confirms Oxide's GUI refactor)
- Transport: HTTP SSE `AsyncGenerator` from `/global/event`, coalesced ~16ms.
- Message → `Part[]`; **every part has a stable `id`**; updates matched by `Binary.search(parts, id)`,
  **never array index**. Text deltas append to `part.text`. `ToolPart.state` enum =
  `pending|running|completed|error`. Working indicator driven by a single `session.status`
  (`idle` vs working) with a 260ms fade. Granular events: `session.next.tool.input.started/
  delta/ended`, `.called/progress/success/failed`, `text.*`, `reasoning.*`,
  `message.part.delta/updated`.
- **Takeaway:** this is exactly the stable-id-keyed model Oxide adopted in
  `5c1cb19` ([[streaming-status-refactor]]). Oxide's per-row `key` ≈ opencode's `part.id`;
  Oxide's `streaming` flag ≈ `session.status`.

---

## 2. Current `chatgpt.rs` state (as of v0.0.82)
- Endpoint + `chatgpt-account-id` + `session_id` header + `originator: codex_cli_rs` +
  `OpenAI-Beta: responses=experimental`. ✓
- Auth: reads `~/.codex/auth.json` `tokens.access_token` + `account_id`. **No refresh** — on
  expiry it just errors ("run codex login").
- Body: model, instructions, input, stream, `store:false`, `include` encrypted reasoning,
  `reasoning.effort`, function tools, `tool_choice:auto`. Replays encrypted reasoning item
  inline (good).
- SSE handled: `output_text.delta`, `reasoning_summary_text.delta`/`reasoning_text.delta`,
  `output_item.done` (reasoning item replay; `function_call`/`shell_call` → tool),
  `function_call_arguments.done` (buffers args), `completed` (usage), `failed`, `incomplete`.
- Tool pairing by `call_id`+`item_id` with a `sent_tools` dedup set. ✓
- Errors 401/403/429/413 + `x-codex-*` rate-limit snapshot. ✓
- **No retry/backoff. No token refresh. No live tool-input streaming (only `.done`).**

---

## 2b. English replies + stuck/loop (studied opencode + Synara turn loops)

**English-by-default cause:** neither opencode nor Synara sends any language/locale
instruction — they rely on the system prompt + model default, which is English-first.
Oxide replied English because its (English, large) system prompt went out as `instructions`
with no language cue. **Fix:** Oxide now appends a "reply in the user's language" block to
the system prompt (oxide-core `run_turn`). Deliberate — neither reference has it, but it's
the lever.

**Stuck/loop fixes (from opencode's iterative loop):**
- `response.completed`/`response.incomplete` now BREAK the SSE loop (chatgpt.rs) instead of
  reading until the connection closes — opencode uses `takeUntil(terminal)`; not breaking
  could hang the turn.
- `response.incomplete` is a soft stop (emit a notice + end) instead of erroring the turn.
- Dangling tool calls: oxide-core `sanitize_tool_pairs` now synthesizes an "interrupted"
  tool result for an unanswered call (opencode's failUnsettledTools) instead of stripping it,
  so the model is told it failed and doesn't re-issue it in a loop.
- Stalls: covered by the http client `read_timeout(120s)` (API) + CLI driver timeouts;
  an engine-level idle watchdog (Synara's `AcpTurnIdleWatchdog`) is a possible future add.
- Max-steps + doom-loop guard already exist in oxide-core run_turn.

## 3. Gap analysis → prioritized Rust TODO

> **Implemented in v0.0.83:** gaps 1 (token refresh on 401, persisted back to auth.json),
> 2 (retry+backoff honoring retry-after), 3 (`output_item.added` seeds the tool buffer),
> 4 (`function_call_arguments.delta` accumulation), 5 (top-level `error` frame), and the
> gap-6 essentials (`reasoning.summary:"auto"`, `prompt_cache_key`). Remaining: gap-6
> `text.verbosity`/`max_output_tokens`, gap 7 (WebSocket), gap 8.

| # | Gap | Priority | Notes |
|---|-----|----------|-------|
| 1 | **OAuth token refresh** | HIGH | Read `tokens.refresh_token` + `expires`/`last_refresh` from auth.json; when expired or on 401, POST `https://auth.openai.com/oauth/token` (`grant_type=refresh_token`, `client_id=app_EMoamEEZ73f0CkXaXp7hrann`), write tokens back to auth.json (rotate refresh), retry once. De-dup concurrent refresh. Removes the "run codex login" dead-end. |
| 2 | **Retry + backoff** | HIGH | Wrap the POST: retry 5xx/timeout/`429` up to 2× with exp backoff + jitter, honor `retry-after`. |
| 3 | **`output_item.added` handling** | MED | Capture `function_call` start (+ `reasoning` `encrypted_content`) early, not only on `.done`. Lets a tool row appear at call-start. |
| 4 | **Live tool-input streaming** | MED | Handle `response.function_call_arguments.delta` (append to a per-item buffer) so tool args stream; today only `.done` is read. Needs a streaming-args path (Oxide could surface as the activity row's detail). |
| 5 | **Top-level `error` event** | MED | Handle a bare `{type:"error",...}` SSE frame (not just `response.failed`). |
| 6 | **Body extras** | LOW | `reasoning.summary:"auto"`, `prompt_cache_key` (stable per conversation → cache hits + cost), `text.verbosity`, `max_output_tokens`. |
| 7 | **Reasoning summary parts** | LOW | `reasoning_summary_part.added/done` for multi-part summaries (cosmetic). |
| 8 | **WebSocket transport** | LOW/maybe | opencode has a `responses_websockets` variant; only if HTTP SSE proves unreliable. |
| 9 | Header cosmetics | SKIP | `originator`/User-Agent differences are harmless; keep `codex_cli_rs` (it's what the CLI uses). |

GUI/status side: already aligned by the stable-id refactor ([[streaming-status-refactor]]); no
change needed there to match opencode.

---

## 4. Suggested order if implementing
1 (refresh) + 2 (retry) first — they fix real failures (expiry dead-end, transient 5xx/429).
Then 3+4 (live tool-call start/stream) for nicer UX. 5 for robustness. 6–8 optional.
