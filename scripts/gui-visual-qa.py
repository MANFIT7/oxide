#!/usr/bin/env python3
"""Visual-state contract checks for the Oxide GUI.

This is intentionally lightweight: it catches regressions in the motion and
streaming hooks that are hard to prove with normal Rust unit tests. By default
it runs static checks and writes a fixture. With --runtime it also runs the
ignored chromiumoxide screenshot smoke against that fixture.
"""

from __future__ import annotations

import argparse
import re
import subprocess
import sys
from pathlib import Path


ROOT = Path(__file__).resolve().parents[1]
GUI = ROOT / "crates/oxide-gui/src/lib.rs"
CSS = ROOT / "crates/oxide-gui/assets/style.css"
PROTOCOL = ROOT / "crates/oxide-protocol/src/lib.rs"
PROVIDER = ROOT / "crates/oxide-providers/src/lib.rs"
CHATGPT = ROOT / "crates/oxide-providers/src/chatgpt.rs"
CORE = ROOT / "crates/oxide-core/src/lib.rs"
DB = ROOT / "crates/oxide-core/src/db.rs"
STORE = ROOT / "crates/oxide-core/src/store.rs"
CHECKLIST = ROOT / "docs/gui-visual-qa-checklist.md"
NATIVE_SMOKE = ROOT / "scripts/gui-native-visual-smoke.py"
NATIVE_RECORD = ROOT / "scripts/gui-native-visual-record.py"
UPDATE = ROOT / "crates/oxide-gui/src/update.rs"
HOOKS = ROOT / "crates/oxide-core/src/hooks.rs"
AUTOMATION = ROOT / "crates/oxide-core/src/automation.rs"
OUT_DIR = ROOT / "target/gui-visual-qa"
FIXTURE = OUT_DIR / "fixture.html"


failures: list[str] = []


def read(path: Path) -> str:
    try:
        return path.read_text(encoding="utf-8")
    except FileNotFoundError:
        failures.append(f"missing file: {path.relative_to(ROOT)}")
        return ""


def rel(path: Path) -> str:
    return str(path.relative_to(ROOT))


def require(name: str, ok: bool, evidence: str) -> None:
    status = "PASS" if ok else "FAIL"
    print(f"{status} {name}: {evidence}")
    if not ok:
        failures.append(name)


def contains_all(source: str, needles: list[str]) -> bool:
    return all(needle in source for needle in needles)


def nearby(source: str, first: str, second: str, window: int = 700) -> bool:
    start = source.find(first)
    if start < 0:
        return False
    return second in source[start : start + window]


def unicode_spinner_fixture(class_name: str) -> str:
    return f'<span class="unicode-spinner {class_name}" aria-hidden="true"></span>'


def write_fixture(css: str) -> None:
    OUT_DIR.mkdir(parents=True, exist_ok=True)
    escaped_css = css.replace("</style", "<\\/style")
    body = """
<div class="app" data-theme="dark">
  <main class="chat">
    <section class="col streaming">
      <div class="row agent agent-waiting streaming-message">
        <div class="avatar"></div>
        <div class="typing" role="status" aria-atomic="true"><!-- UNICODE_TYPING --><span class="typing-shimmer">Thinking…</span></div>
      </div>
      <details class="thinking-box" open>
        <summary class="thinking-sum live"><span class="thinking-glow">Reasoning</span><span class="thinking-secs">3s</span></summary>
        <div class="thinking-body">Inspecting harness routes, streamed tool args, and session metadata.</div>
      </details>
      <div class="row activity">
        <details class="activity-card running activity-search activity-preparing no-out">
          <summary class="activity-sum">
            <span class="activity-status" role="status" aria-atomic="true" aria-label="Running"><!-- UNICODE_ACTIVITY --><span class="activity-ic ok">✓</span><span class="activity-ic fail">×</span></span>
            <span class="activity-verb">Preparing</span>
            <span class="activity-text">ask_user · {"question":"This intentionally long streamed JSON argument must wrap inside the transcript instead of forcing a horizontal scrollbar across the entire chat surface."}</span>
          </summary>
        </details>
      </div>
      <div class="row activity">
        <details class="activity-card done activity-command has-out" open>
          <summary class="activity-sum">
            <span class="activity-status" role="status" aria-atomic="true" aria-label="Completed"><!-- UNICODE_ACTIVITY --><span class="activity-ic ok">✓</span><span class="activity-ic fail">×</span></span>
            <span class="activity-verb">Ran command</span>
            <span class="activity-text">cargo check -p oxide-gui</span>
            <span class="activity-out-n">2 lines</span><span class="activity-caret">⌃</span>
          </summary>
          <pre class="activity-out">Checking oxide-gui
Finished dev profile</pre>
        </details>
      </div>
      <div class="row agent streaming-message">
        <div class="avatar"></div>
        <div class="agent-text agent-md live"><div class="live-tail">Streaming answer text stays readable while the tail fades softly.</div></div>
      </div>
      <div class="review-item">
        <details class="review-diff-d" open>
          <summary class="review-file">
            <span class="review-path">crates/oxide-gui/src/lib.rs</span>
            <span class="diff-adds">+12</span>
            <span class="diff-dels">-3</span>
          </summary>
          <pre class="diff-code">+ live thinking stays in transcript order</pre>
        </details>
        <div class="review-actions"><span class="diff-kept">Kept</span></div>
      </div>
      <div class="row agent ui-spec-row">
        <div class="avatar"></div>
        <div class="ui-spec">
          <div class="ui-spec-title">Cursor-grade Visual QA</div>
          <div class="ui-node ui-card-spec">
            <div class="ui-card-title">Rust-native UI Spec</div>
            <div class="ui-card-caption">Rendered by Dioxus from a typed Oxide protocol spec.</div>
            <div class="ui-node ui-row-spec">
              <div class="ui-node ui-metric info">
                <div class="ui-metric-label">Native state</div>
                <div class="ui-metric-value">streaming</div>
              </div>
              <div class="ui-node ui-metric success">
                <div class="ui-metric-label">Visual QA</div>
                <div class="ui-metric-value">seeded</div>
              </div>
            </div>
            <div class="ui-node ui-table-wrap">
              <table class="ui-table">
                <thead><tr><th>Surface</th><th>Status</th></tr></thead>
                <tbody><tr><td>Protocol</td><td>typed</td></tr><tr><td>GUI</td><td>native</td></tr></tbody>
              </table>
            </div>
          </div>
        </div>
      </div>
      <div class="edits-card">
        <div class="edits-head"><span class="edits-title">Edited files</span></div>
        <div class="edits-row pending">
          <span class="edits-path">crates/oxide-providers/src/chatgpt.rs</span>
          <span class="edits-rowcounts shimmer">editing...</span>
        </div>
      </div>
      <details class="subagents-card run-disclosure">
        <summary class="subagents-head run-summary"><span class="workflow-ic">✦</span><span class="run-label">Subagents 1/1</span><span class="run-preview">reviewer · GUI performance audit</span><span class="run-caret">⌄</span></summary>
        <div class="subagent-row done"><span class="subagent-status">✓</span><div class="subagent-copy"><div class="subagent-title">reviewer · GUI performance audit</div><div class="subagent-summary">High-confidence findings stay available on demand.</div></div></div>
      </details>
      <details class="todo-card run-disclosure">
        <summary class="todo-head run-summary"><span class="todo-ic">☷</span><span class="run-label">Tasks 2/5</span><span class="run-preview">Implement compact orchestration layout</span><span class="run-caret">⌄</span></summary>
        <div class="todo-row in_progress"><span class="todo-box"></span><span class="todo-text">Implement compact orchestration layout</span></div>
      </details>
      <div class="composer-live-changes">
        <div class="live-changes-head">
          <span class="live-changes-icon">~</span>
          <div class="live-changes-copy">
            <span class="live-changes-title">Changing 2 files</span>
            <span class="live-changes-sub">Streaming edits into the review surface</span>
          </div>
          <span class="live-changes-counts"><span class="diff-adds">+18</span><span class="diff-dels">-4</span></span>
        </div>
      </div>
      <div class="agents-window">
        <div class="agents-hero">
          <div>
            <div class="agents-kicker">Local workspace</div>
            <div class="agents-title">Agents</div>
            <div class="agents-sub">Local agent sessions, sub-agents, review queue, browser context, and artifacts in one control surface.</div>
          </div>
          <div class="agents-hero-actions">
            <button class="agent-action primary">New Codex</button>
            <button class="agent-action on">Split on</button>
          </div>
        </div>
        <div class="agents-metrics">
          <div class="agents-metric"><span class="agents-metric-num">2</span><span class="agents-metric-label">open agents</span></div>
          <div class="agents-metric live"><span class="agents-metric-num">1</span><span class="agents-metric-label">running turns</span></div>
          <div class="agents-metric"><span class="agents-metric-num">1</span><span class="agents-metric-label">sub-agents</span></div>
          <div class="agents-metric"><span class="agents-metric-num">2</span><span class="agents-metric-label">review files</span></div>
        </div>
        <div class="agents-section">
          <div class="agents-section-head"><span>Agent sessions</span><span class="agents-section-meta">local</span></div>
          <div class="agents-session-list">
            <button class="agents-session active"><span class="agents-session-logo fixture-icon"></span><span class="agents-session-copy"><span class="agents-session-title">Codex</span><span class="agents-session-sub">codex · default · medium</span></span><span class="agents-session-meta"><span class="agents-status running">running</span><span>8 msgs</span></span></button>
          </div>
        </div>
        <div class="agents-work-grid">
          <button class="agents-work-card"><span class="fixture-icon"></span><span class="agents-work-title">Review queue</span><span class="agents-work-sub">2 file(s)</span></button>
          <button class="agents-work-card"><span class="fixture-icon"></span><span class="agents-work-title">Changes</span><span class="agents-work-sub">git diff + commit</span></button>
          <button class="agents-work-card"><span class="fixture-icon"></span><span class="agents-work-title">Preview</span><span class="agents-work-sub">browser + design mode</span></button>
          <button class="agents-work-card"><span class="fixture-icon"></span><span class="agents-work-title">Bugbot review</span><span class="agents-work-sub">local git diff</span></button>
        </div>
        <div class="agents-worker running"><span class="agents-worker-status"><span class="syn-spinner"></span></span><span class="agents-worker-copy"><span class="agents-worker-title">reviewer · GUI parity</span><span class="agents-worker-sub">Auditing local non-cloud controls.</span></span></div>
      </div>
      <div class="status-pill" role="status" aria-atomic="true"><!-- UNICODE_STATUS --><span class="status-shimmer">Running validation</span></div>
    </section>
  </main>
  <div class="toasts" aria-live="polite">
    <div class="toast ok compact" role="status">
      <span class="toast-icon"><svg viewBox="0 0 24 24" fill="none" stroke="currentColor"><circle cx="12" cy="12" r="9"></circle><polyline points="8 12 11 15 16 9"></polyline></svg></span>
      <div class="toast-copy"><div class="toast-title">Changes committed</div></div>
      <button class="toast-close compact" aria-label="Dismiss toast">×</button>
    </div>
    <div class="toast info expanded has-action" role="status">
      <span class="toast-icon"><svg viewBox="0 0 24 24" fill="none" stroke="currentColor"><circle cx="12" cy="12" r="9"></circle><line x1="12" y1="11" x2="12" y2="17"></line></svg></span>
      <div class="toast-copy"><div class="toast-title">Chat archived</div><div class="toast-actions"><button class="toast-action">Undo</button></div></div>
      <button class="toast-close expanded" aria-label="Dismiss toast">×</button>
    </div>
  </div>
</div>
"""
    body = body.replace("<!-- UNICODE_TYPING -->", unicode_spinner_fixture("typing-unicode"))
    body = body.replace("<!-- UNICODE_ACTIVITY -->", unicode_spinner_fixture("activity-spin"))
    body = body.replace("<!-- UNICODE_STATUS -->", unicode_spinner_fixture("status-spinner"))
    fixture = f"""<!doctype html>
<html lang="en">
<head>
  <meta charset="utf-8">
  <meta name="viewport" content="width=device-width, initial-scale=1">
  <title>Oxide GUI Visual QA Fixture</title>
  <style>
{escaped_css}
    body {{ margin: 0; min-height: 100vh; background: #0d0d0f; color: #f4f4f5; }}
    .chat {{ max-width: 920px; margin: 0 auto; padding: 40px 24px; }}
    .avatar {{ width: 28px; height: 28px; border-radius: 50%; background: #22242a; flex: none; }}
  </style>
</head>
<body>
{body}
</body>
</html>
"""
    FIXTURE.write_text(fixture, encoding="utf-8")


def run_runtime_visual_qa() -> None:
    command = [
        "cargo",
        "test",
        "-p",
        "oxide-core",
        "gui_visual_fixture_screenshot",
        "--",
        "--ignored",
        "--nocapture",
    ]
    result = subprocess.run(command, check=False, capture_output=True, text=True, timeout=360)
    if result.stdout:
        print(result.stdout, end="")
    if result.stderr:
        print(result.stderr, end="", file=sys.stderr)
    require(
        "runtime CDP fixture smoke",
        result.returncode == 0,
        "`cargo test -p oxide-core gui_visual_fixture_screenshot -- --ignored --nocapture`",
    )


def main() -> int:
    parser = argparse.ArgumentParser(description="Run Oxide GUI visual-state QA contracts.")
    parser.add_argument(
        "--runtime",
        action="store_true",
        help="also run the ignored chromiumoxide fixture screenshot smoke",
    )
    args = parser.parse_args()

    gui = read(GUI)
    css = read(CSS)
    protocol = read(PROTOCOL)
    provider = read(PROVIDER)
    chatgpt = read(CHATGPT)
    core = read(CORE)
    db = read(DB)
    store = read(STORE)
    checklist = read(CHECKLIST)
    native_smoke = read(NATIVE_SMOKE)
    native_record = read(NATIVE_RECORD)
    update = read(UPDATE)
    hooks = read(HOOKS)
    automation = read(AUTOMATION)

    require(
        "pre-token shimmer render",
        contains_all(gui, ['class: "row agent agent-waiting streaming-message"', 'class: "typing"', "if live"]) and ".typing" in css,
        f"{rel(GUI)} renders .agent-waiting/.typing for live empty agent rows",
    )
    require(
        "typing shimmer css",
        contains_all(css, [".typing", "glass-sweep", "@keyframes glass-sweep"]),
        f"{rel(CSS)} defines glass-sweep typing skeleton",
    )
    require(
        "unicode activity micro-motion stays single-node",
        contains_all(
            gui,
            [
                "fn UnicodeSpinner",
                'rsx! { span { class: "unicode-spinner {class}", aria_hidden: "true" } }',
                'UnicodeSpinner { class: "status-spinner" }',
                'UnicodeSpinner { class: "typing-unicode" }',
                'UnicodeSpinner { class: "activity-spin" }',
                'role: "status"',
                'aria_atomic: "true"',
            ],
        )
        and contains_all(
            css,
            [
                ".unicode-spinner::after",
                "@keyframes oxide-unicode-frame",
                "steps(10, end)",
                "transform: translateY(-10em)",
                "will-change: transform",
                '.activity-status .activity-spin::after { animation: none; }',
                '.activity-card.running .activity-status .activity-spin::after {',
            ],
        )
        and "UNICODE_SPINNER_FRAMES" not in gui
        and "unicode-spinner-frame" not in gui
        and "unicode-spinner-frame" not in css,
        f"{rel(GUI)} and {rel(CSS)} render each Braille spinner as one DOM node and only animate active lifecycle states",
    )
    motion_override = "@media (prefers-reduced-motion: reduce) and (prefers-reduced-motion: no-preference)"
    require(
        "host motion preference does not disable Oxide motion",
        css.count("@media (prefers-reduced-motion: reduce)") == css.count(motion_override)
        and css.count(motion_override) > 0
        and "Oxide intentionally keeps interface motion enabled" in css,
        f"{rel(CSS)} makes every legacy reduced-motion fallback unreachable so host settings cannot freeze the UI",
    )
    require(
        "streaming lifecycle motion stays outside live HTML",
        contains_all(
            gui,
            [
                '"row agent streaming-message"',
                '"agent-text agent-md live"',
                'html.push_str("<div class=\\"live-tail\\">")',
            ],
        )
        and contains_all(
            css,
            [
                "@keyframes oxide-stream-first-token",
                "@keyframes oxide-stream-rail",
                ".row.agent.streaming-message::before",
                ".agent-md.live .live-tail",
                "will-change: opacity, transform;",
            ],
        ),
        f"{rel(GUI)} and {rel(CSS)} keep chunk-stable streaming motion on the keyed row instead of reanimating markdown children",
    )
    require(
        "detached pane streaming coalesces deltas",
        contains_all(
            gui,
            [
                "let mut reasoning_buf = String::new();",
                "macro_rules! flush_pane_streams",
                "if !agent_buf.is_empty() || !reasoning_buf.is_empty()",
                "agent_buf.len() + reasoning_buf.len() > 800",
                "std::time::Duration::from_millis(50)",
            ],
        )
        and nearby(gui, "fn ChatPane(", "macro_rules! flush_pane_streams", 9000),
        f"{rel(GUI)} batches pane answer/reasoning deltas at frame cadence and flushes before structural events",
    )
    require(
        "tool lifecycle uses stable status and disclosure slots",
        contains_all(
            gui,
            [
                "fn ActivityStatus",
                'class: "activity-status"',
                'UnicodeSpinner { class: "activity-spin" }',
                'span { class: "activity-ic ok"',
                'span { class: "activity-ic fail"',
                'if has_output { "has-out" } else { "no-out" }',
                'details { class: "{cls}", open: has_output && auto_open',
                'class: "activity-caret"',
            ],
        )
        and contains_all(
            css,
            [
                "@keyframes oxide-tool-enter",
                "@keyframes oxide-tool-halo",
                ".activity-status",
                ".activity-card.has-out::details-content",
                ".activity-card.has-out[open] .activity-caret",
            ],
        ),
        f"{rel(GUI)} and {rel(CSS)} cross-fade running/success/failure in a fixed slot and animate tool disclosure without layout measurement",
    )
    require(
        "motion policy keeps lifecycle polish active",
        contains_all(
            css,
            [
                "@keyframes oxide-stream-first-token",
                "@keyframes oxide-stream-rail",
                "@keyframes oxide-tool-halo",
                "@keyframes oxide-unicode-frame",
                motion_override,
            ],
        ),
        f"{rel(CSS)} keeps stream, tool, and Unicode lifecycle motion active under every host motion preference",
    )
    require(
        "pending edit shimmer remains active",
        contains_all(
            css,
            [
                ".edits-row.pending .edits-rowcounts.shimmer {",
                "animation: shimmer 1.7s linear infinite;",
                ".slot-char",
                motion_override,
            ],
        )
        and 'class: "edits-rowcounts shimmer slot-status"' in gui,
        f"{rel(CSS)} and {rel(GUI)} keep the transcript edit state animated regardless of host motion preference",
    )
    require(
        "composer orchestration surfaces stay compact",
        contains_all(
            gui,
            [
                'details { class: "subagents-card run-disclosure"',
                'details { class: "todo-card run-disclosure"',
                'class: "run-preview"',
                'class: "composer-live-changes"',
                'select_env_tab(env_tab, show_env, env_tab_by_tab, tabs, active_tab, "changes", false)',
            ],
        )
        and "live-changes-files" not in gui
        and contains_all(
            css,
            [
                ".run-disclosure { overflow: hidden; }",
                ".subagents-card { display: block; max-height: 40px;",
                ".todo-card { width: 100%; max-width: 760px; max-height: 40px;",
                ".live-changes-copy { min-width: 0; display: flex; align-items: baseline;",
            ],
        ),
        f"{rel(GUI)} and {rel(CSS)} collapse task/subagent detail and route file detail to the Diff environment tab",
    )
    require(
        "streamed tool arguments wrap within transcript",
        contains_all(
            gui,
            ['activity-preparing', 'view.verb == "Preparing"'],
        )
        and contains_all(
            css,
            [
                "overflow-x: hidden;",
                ".activity-card.activity-preparing .activity-text",
                "overflow-wrap: anywhere;",
                "word-break: break-word;",
            ],
        ),
        f"{rel(GUI)} and {rel(CSS)} keep long streamed JSON inside the transcript width",
    )
    require(
        "tool input delta protocol",
        contains_all(protocol, ["ToolCallDelta", "call_id", "accumulated"])
        and "ToolInputDelta" in provider
        and "StreamItem::ToolInputDelta" in core,
        "protocol/provider/core expose streamed tool-argument deltas",
    )
    require(
        "chatgpt streamed tool args",
        contains_all(
            chatgpt,
            [
                'Some("response.output_item.added")',
                'Some("response.function_call_arguments.delta")',
                'Some("response.function_call_arguments.done")',
                "StreamItem::ToolInputDelta",
            ],
        ),
        f"{rel(CHATGPT)} emits live tool input deltas before final ToolCall",
    )
    require(
        "gui tool input preview",
        contains_all(gui, ["Event::ToolCallDelta", "upsert_tool_input_preview", "Preparing"])
        and nearby(gui, "Event::ToolCallDelta", "upsert_tool_input_preview"),
        f"{rel(GUI)} handles ToolCallDelta as a live Preparing row",
    )
    require(
        "live thinking stays inside current turn",
        contains_all(
            gui,
            [
                "if is_live && !thinking.read().is_empty()",
                'details { class: "thinking-box"',
                "thinking-glow",
            ],
        ),
        f"{rel(GUI)} renders live reasoning above the live assistant row",
    )
    require(
        "settled thinking is gated outside streaming",
        "if !*streaming.read() && !thinking.read().is_empty()" in gui,
        f"{rel(GUI)} prevents duplicate live/global thinking blocks while streaming",
    )
    require(
        "stream follow respects reader intent",
        contains_all(
            gui,
            [
                "bottomDistance",
                "hasSelection",
                "typingTarget",
                # Direction-based unstick: an upward wheel releases the follow
                # IMMEDIATELY (a distance threshold was unreachable mid-stream).
                "ev.deltaY < 0",
                # Re-arm only at the true bottom, never by proximity.
                "if (d < 8) window.__oxstick = true;",
                "requestAnimationFrame(() =>",
                "window.__oxstick !== false",
            ],
        )
        and contains_all(css, [".scroll", "overflow-anchor: none"]),
        "streaming autoscroll stays smooth without pulling the reader away from scrollback",
    )
    accept_block = re.search(
        r'button \{ class: "review-accept"[\s\S]{0,320}accepted\.write\(\)\.insert\(cp\);[\s\S]{0,180}SlotText \{ text: "Accept"\.to_string\(\)',
        gui,
    )
    require(
        "review accept keeps row visible",
        accept_block is not None and "diff-kept" in gui and "is_accepted" in gui,
        f"{rel(GUI)} marks accepted checkpoints as kept instead of removing the row",
    )
    require(
        "slot-style edit/remove labels",
        contains_all(gui, ["fn SlotText", "SlotText { text: \"Reject\".to_string()", "SlotText { text: \"Reverted\".to_string()", "SlotText { text: \"Kept\".to_string()"])
        and contains_all(css, ["@keyframes slot-roll-up", "@keyframes slot-roll-down", ".slot-char", ".slot-text.down"])
        and contains_all(gui, ['Icon { name: "check" }', 'Icon { name: "undo" }'])
        and "v.remove(idx)" not in re.sub(r"fn SlotText[\\s\\S]*?\\n}\\n", "", gui),
        "edit/revert labels use native slot-roll motion and rejected review rows resolve visually",
    )
    banned_status_glyphs = (
        "\u2715\u21a9\u21aa\u2191\u2193\u25a0\u25b6\u21bb\u2753"
        "\U0001f4cd\U0001f6e0\u29d6\u232b\u276f\u229e\u229f"
        "\U0001f9ed\U0001f50d\U0001f916\U0001f9e9\U0001f501"
        "\u2699\u26a0\u23f3\u23f8\U0001f310\U0001f4f8\U0001fa9d"
        "\U0001f9ea\u2b06\u27f3\u2197"
    )
    require(
        "user-facing emoji glyphs use icons",
        re.search(rf'"[^"\\n]*(?:[{re.escape(banned_status_glyphs)}])[^"\\n]*"', gui) is None
        and contains_all(gui, ["fn StatusPill", "fn ToolNote", "is_stage_status", "prefixed_icon_text"])
        and contains_all(gui, ['Icon { name: "x" }', 'Icon { name: "arrow-up" }', 'Icon { name: "help" }']),
        "GUI user-facing controls render icons instead of hardcoded emoji/status glyph text",
    )
    require(
        "activity copy output uses awaited helper",
        "copy_text_to_clipboard(out.clone())" in gui
        and "fn copy_text_to_clipboard(text: String)" in gui
        and ".join::<bool>().await" in gui,
        f"{rel(GUI)} uses the async clipboard helper for activity output",
    )
    require(
        "message copy controls use icon",
        "\u29c9" not in gui
        and 'Icon { name: "copy" }' in gui
        and '"copy" => rsx!' in gui
        and contains_all(css, [".msg-copy svg", ".msg-act svg", ".copy-btn svg"]),
        "message/activity copy controls render the shared copy icon instead of a raw text glyph",
    )
    require(
        "done note uses icon and hides duplicate duration",
        contains_all(
            gui,
            [
                "fn DoneNote",
                "done_note_display_parts",
                "looks_like_done_duration",
                'span { class: "done-icon", Icon { name: "check" } }',
            ],
        )
        and contains_all(css, [".done-note", ".done-icon", ".done-label"])
        and '"check" => rsx!' in gui,
        "Done notes render with an SVG check icon and drop the already-shown turn duration",
    )
    require(
        "synara-style toast surface",
        contains_all(
            gui,
            [
                'class: "toast-icon"',
                'class: "toast-copy"',
                'class: "toast-actions"',
                'aria_label: "Dismiss toast"',
                '"circle-check" => rsx!',
                '"circle-alert" => rsx!',
                '"info" => rsx!',
                'ToastAction::OpenTab(ev_tid)',
                '"Open"',
                "switch_tab(tabs, active_tab, messages, cfg, engine, idx)",
            ],
        )
        and contains_all(
            css,
            [
                ".toasts {",
                "top: 16px; left: 50%;",
                "transform: translateX(-50%);",
                ".toast.expanded {",
                ".toast.compact .toast-title",
                ".toast-close",
                "backdrop-filter: blur(18px) saturate(140%);",
                "background: color-mix(in srgb, var(--syn-accent) 10%, transparent);",
            ],
        ),
        f"{rel(GUI)} and {rel(CSS)} render Synara-style centered compact/expanded toasts with semantic icons and explicit dismiss controls",
    )
    require(
        "session runtime metadata survives replay",
        contains_all(gui, ["struct AgentTab", "harness: String", "reasoning_effort: String"])
        and "oxide_core::db::SessionMeta" in gui
        and contains_all(db, ["pub struct SessionMeta", "pub model: String", "pub harness: String", "pub reasoning_effort: String"])
        and contains_all(db, ["ALTER TABLE sessions ADD COLUMN harness", "ALTER TABLE sessions ADD COLUMN reasoning_effort"])
        and contains_all(store, ["model: String", "harness: String", "reasoning_effort: String"]),
        "GUI tab plus core session store preserve model/harness/effort",
    )
    board = read(ROOT / "crates/oxide-gui/src/board.rs")
    require(
        "rust-native UI spec renderer",
        contains_all(protocol, ["pub struct UiSpec", "pub enum UiNodeKind", "pub enum UiTone"])
        and contains_all(core, ['ToolSpec::new("render_ui_spec"', 'store.append("ui_spec"', "Box::new(spec)"])
        and contains_all(gui, ["Author::UiSpec", "UiSpecView", "UiNodeView", "visual_fixture_ui_spec", '"ui_spec" => Author::UiSpec'])
        and "Event::UiSpec" in board
        and contains_all(css, [".ui-spec", ".ui-card-spec", ".ui-table"]),
        "protocol/core/gui/board/css expose, persist, and render constrained UiSpec artifacts",
    )
    require(
        "local agents window contract",
        contains_all(
            gui,
            [
                '("agents","Agents")',
                'class: "agents-window"',
                '"Local work"',
                '"Bugbot review"',
                "new_agent_tab(tabs, active_tab, messages, cfg, engine, next_tab_id",
                "switch_tab(tabs, active_tab, messages, cfg, engine, idx)",
                'display: "/review (Bugbot)".into()',
            ],
        )
        and contains_all(
            css,
            [
                ".agents-window",
                ".agents-hero",
                ".agents-session",
                ".agents-work-card",
                ".agents-worker",
            ],
        ),
        "GUI exposes a local-only Agents tab with session switching, review, changes, preview, and Bugbot actions",
    )
    require(
        "local server controls",
        contains_all(
            gui,
            [
                '"Local Servers"',
                'class: "local-server-row"',
                'title: "Stop dev server"',
                'Icon { name: "stop" }',
                "preview_proxy::set_target(port)",
                'title: "Open dev server"',
                '"agent-"',
            ],
        )
        and contains_all(
            css,
            [
                ".env-card-section-head",
                ".local-server-list",
                ".local-server-main",
                ".local-server-stop",
                ".local-server-empty",
            ],
        ),
        "Environment card exposes local server status/actions without internal agent ports",
    )
    require(
        "selected workflow surfaces",
        contains_all(
            gui,
            [
                '"verify" => rsx!',
                '"Fix feedback"',
                '"Compare solutions"',
                '"Hook Studio"',
                '"Next Work"',
                "fork_agent_tab",
                "review_comments",
            ],
        )
        and contains_all(css, [".verify-item", ".hunk-feedback", ".compare-modal", ".hook-editor"]),
        "GUI exposes verification, inline feedback, fork comparison, Hook Studio, and Next Work",
    )
    require(
        "automatic update and recovery hardening",
        contains_all(
            gui,
            [
                "15 * 60",
                "ToastAction::InstallUpdate",
                "show_native_notification",
                "oxide:draft:",
                "localStorage.setItem",
            ],
        )
        and contains_all(update, ["release is missing a SHA-256 checksum", "oxide-term checksum mismatch"]),
        "updates poll without reload, notify once, restore drafts, and require signed checksums",
    )
    require(
        "hook and thread automation contracts",
        contains_all(hooks, ["pub fn from_text", "pub fn commands_for"])
        and contains_all(automation, ["pub session_id: Option<String>", "Bound thread context"]),
        "Hook Studio validates real hook parsing and automations retain bound thread context",
    )
    require(
        "responsive board states",
        contains_all(
            gui,
            [
                'class: "board-col-count"',
                'class: "board-col-empty"',
                'aria_label: "New board task"',
                'aria_label: "Remove task"',
                'key: "{cid}-{col}"',
                "&& !*show_board.read()",
            ],
        )
        and contains_all(
            css,
            [
                ".board-cols.four { grid-template-columns: repeat(4, minmax(238px, 1fr)); }",
                "overflow-x: auto;",
                ".board-col-empty",
                "@media (max-width: 1180px)",
                ".board-card { position: relative;",
                "animation: oxide-rise var(--dur-med) var(--ease-enter) both;",
                "transition: border-color var(--dur-fast)",
            ],
        ),
        "Board keeps four usable lanes through horizontal overflow, explicit empty states, counts, and host-invariant card transitions",
    )
    require(
        "settings navigation scales to all destinations",
        contains_all(gui, ['("sessions", "Sessions")', '("updates", "Updates")'])
        and contains_all(
            css,
            [
                "grid-template-columns: 158px minmax(0, 1fr);",
                ".settings-modal .modal-body",
                "@media (max-width: 660px)",
                "overflow-x: auto;",
            ],
        ),
        "Settings uses a desktop side rail and a discoverable compact horizontal fallback instead of a clipped hidden scrollbar",
    )
    require(
        "accessible chrome and dialogs",
        contains_all(
            gui,
            [
                'class: "logo-btn"',
                'aria_label: "Collapse or expand sidebar"',
                'role: "dialog"',
                'aria_modal: "true"',
            ],
        )
        and contains_all(
            css,
            [
                "button:focus-visible",
                "input:focus-visible",
                "[tabindex]:focus-visible",
            ],
        ),
        "Primary icon controls are keyboard reachable, dialogs expose semantics, and interactive chrome has a shared focus ring",
    )
    require(
        "theme contrast and responsive dock tokens",
        contains_all(css, ["--faint: #858585;", "--faint: rgba(13, 13, 13, 0.58);", "--border: rgba(13, 13, 13, 0.13);"])
        and contains_all(css, ["@media (max-width: 900px)", "position: absolute; inset: 0 0 0 auto; z-index: 20;"]),
        "Small metadata text uses readable tokens and the Environment dock becomes an intentional compact drawer before it crushes content",
    )
    require(
        "native visual state recorder",
        contains_all(native_record, ["STATES =", '"streaming"', '"review"', '"verification"', '"board"', '"settings"', "compare_png", '"manifest.json"'])
        and "scripts/gui-native-visual-record.py" in checklist,
        f"{rel(NATIVE_RECORD)} records deterministic states and supports golden comparison",
    )
    checklist_needles = [
        "Braille spinner",
        "streaming rail",
        "Reasoning",
        "Preparing <tool>",
        "status slot",
        "Reduce Motion",
        "Accept",
        "provider/model/harness/effort",
        "Rust-native UI Spec",
        "Agents Window",
        "Bugbot review",
        "Local Servers",
        "Verification Center",
        "Fix feedback",
        "Board And Compact Layout",
        "Theme Contrast And Keyboard",
        "gui-native-visual-record.py",
    ]
    require(
        "manual checklist covers motion-critical states",
        contains_all(checklist, checklist_needles),
        f"{rel(CHECKLIST)} covers streaming, reduced motion, review, and replay checks",
    )
    require(
        "native GUI screenshot smoke is available",
        contains_all(
            native_smoke,
            [
                "oxide gui",
                "screencapture",
                "osascript",
                "decode_png",
                "window bounds",
                "OXIDE_GUI_VISUAL_FIXTURE",
                '"board"',
                '"settings"',
                "--strict",
                "--settle",
                "time.sleep(max(0.0, min(args.settle, 5.0)))",
            ],
        )
        and "scripts/gui-native-visual-smoke.py" in checklist,
        f"{rel(NATIVE_SMOKE)} launches the real GUI and {rel(CHECKLIST)} documents it",
    )

    if css:
        write_fixture(css)
        print(f"INFO fixture: {rel(FIXTURE)}")

    if args.runtime:
        run_runtime_visual_qa()

    if failures:
        print("\nVisual QA contract failed:")
        for failure in failures:
            print(f"- {failure}")
        return 1

    print("\nVisual QA contract passed.")
    return 0


if __name__ == "__main__":
    sys.exit(main())
