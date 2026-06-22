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


def write_fixture(css: str) -> None:
    OUT_DIR.mkdir(parents=True, exist_ok=True)
    escaped_css = css.replace("</style", "<\\/style")
    body = """
<div class="app" data-theme="dark">
  <main class="chat">
    <section class="col streaming">
      <div class="row agent agent-waiting">
        <div class="avatar"></div>
        <div class="typing"><span></span><span></span><span></span></div>
      </div>
      <details class="thinking-box" open>
        <summary class="thinking-sum live"><span class="thinking-glow">Reasoning</span><span class="thinking-secs">3s</span></summary>
        <div class="thinking-body">Inspecting harness routes, streamed tool args, and session metadata.</div>
      </details>
      <div class="activity-card running activity-search">
        <div class="activity-spin"></div>
        <div class="activity-main">
          <div class="activity-verb">Preparing browser_search</div>
          <div class="activity-detail">{"query":"oxide gui visual qa"}</div>
        </div>
      </div>
      <div class="row agent">
        <div class="avatar"></div>
        <div class="agent-text agent-md live">Streaming answer text stays readable while the tail fades softly.</div>
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
      <div class="composer-live-changes">
        <div class="live-changes-head">
          <span class="live-changes-icon">~</span>
          <div class="live-changes-copy">
            <span class="live-changes-title">Changing 2 files</span>
            <span class="live-changes-sub">Streaming edits into the review surface</span>
          </div>
          <span class="live-changes-counts"><span class="diff-adds">+18</span><span class="diff-dels">-4</span></span>
        </div>
        <div class="live-changes-files">
          <div class="live-change-row"><span class="live-change-path">crates/oxide-gui/src/lib.rs</span><span class="live-change-state shimmer">editing...</span></div>
          <div class="live-change-row"><span class="live-change-path">scripts/gui-visual-qa.py</span><span class="live-change-state"><span class="diff-adds">+44</span></span></div>
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
            <button class="agents-session active"><span class="agents-session-logo">✦</span><span class="agents-session-copy"><span class="agents-session-title">Codex</span><span class="agents-session-sub">codex · default · medium</span></span><span class="agents-session-meta"><span class="agents-status running">running</span><span>8 msgs</span></span></button>
          </div>
        </div>
        <div class="agents-work-grid">
          <button class="agents-work-card"><span>⎇</span><span class="agents-work-title">Review queue</span><span class="agents-work-sub">2 file(s)</span></button>
          <button class="agents-work-card"><span>✎</span><span class="agents-work-title">Changes</span><span class="agents-work-sub">git diff + commit</span></button>
          <button class="agents-work-card"><span>⌁</span><span class="agents-work-title">Preview</span><span class="agents-work-sub">browser + design mode</span></button>
          <button class="agents-work-card"><span>◈</span><span class="agents-work-title">Bugbot review</span><span class="agents-work-sub">local git diff</span></button>
        </div>
        <div class="agents-worker running"><span class="agents-worker-status"><span class="syn-spinner"></span></span><span class="agents-worker-copy"><span class="agents-worker-title">reviewer · GUI parity</span><span class="agents-worker-sub">Auditing local non-cloud controls.</span></span></div>
      </div>
      <div class="status-pill"><span class="status-spinner"></span><span class="status-shimmer">Running validation</span></div>
    </section>
  </main>
</div>
"""
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
    result = subprocess.run(command, check=False, capture_output=True, text=True, timeout=180)
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

    require(
        "pre-token shimmer render",
        contains_all(gui, ['class: "row agent agent-waiting"', 'class: "typing"', "if live"]) and ".typing" in css,
        f"{rel(GUI)} renders .agent-waiting/.typing for live empty agent rows",
    )
    require(
        "typing shimmer css",
        contains_all(css, [".typing", "glass-sweep", "@keyframes glass-sweep"]),
        f"{rel(CSS)} defines glass-sweep typing skeleton",
    )
    require(
        "reduced-motion collapses decorative waiting row",
        contains_all(
            css,
            [
                "@media (prefers-reduced-motion: reduce)",
                ".row.agent.agent-waiting",
                "display: none;",
                ".status-pill",
                "margin-top: 2px;",
            ],
        ),
        f"{rel(CSS)} removes the empty pre-token skeleton and compacts the active status pill under Reduce Motion",
    )
    require(
        "reduced-motion uses static progress dots",
        contains_all(
            css,
            [
                ".status-spinner, .activity-spin,",
                ".syn-spinner, .status-shimmer, .typing, .typing span",
                "animation: none !important;",
                "background: var(--syn-accent);",
                "-webkit-mask: none;",
                "-webkit-text-fill-color: currentColor;",
            ],
        ),
        f"{rel(CSS)} converts spinner rings to static progress dots and disables shimmer text under Reduce Motion",
    )
    require(
        "reduced-motion freezes edit shimmer",
        contains_all(
            css,
            [
                ".live-changes-skeleton, .live-change-state.shimmer { animation: none;",
                ".edits-row.pending .edits-rowcounts.shimmer { animation: none; }",
                ".slot-char",
            ],
        )
        and contains_all(
            gui,
            [
                'class: "edits-rowcounts shimmer slot-status"',
                'class: "live-change-state shimmer slot-status"',
                'class: "composer-live-changes"',
            ],
        ),
        f"{rel(CSS)} and {rel(GUI)} cover live-edit and pending-edit shimmer states",
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
        contains_all(gui, ["fn SlotText", "SlotText { text: \"Reject\".to_string()", "SlotText { text: \"✓ Reverted\".to_string()"])
        and contains_all(css, ["@keyframes slot-roll-up", "@keyframes slot-roll-down", ".slot-char", ".slot-text.down"])
        and "v.remove(idx)" not in re.sub(r"fn SlotText[\\s\\S]*?\\n}\\n", "", gui),
        "edit/revert labels use native slot-roll motion and rejected review rows resolve visually",
    )
    require(
        "activity copy output uses awaited helper",
        "copy_text_to_clipboard(out.clone())" in gui
        and "fn copy_text_to_clipboard(text: String)" in gui
        and ".join::<bool>().await" in gui,
        f"{rel(GUI)} uses the async clipboard helper for activity output",
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
    checklist_needles = [
        "pre-first-token shimmer",
        "Reasoning",
        "Preparing <tool>",
        "Reduce Motion",
        "Accept",
        "provider/model/harness/effort",
        "Rust-native UI Spec",
        "Agents Window",
        "Bugbot review",
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
                "--strict",
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
