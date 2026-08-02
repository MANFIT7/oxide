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
PROVIDER_CATALOG = ROOT / "crates/oxide-providers/src/catalog.rs"
CHATGPT = ROOT / "crates/oxide-providers/src/chatgpt.rs"
CORE = ROOT / "crates/oxide-core/src/lib.rs"
BROWSER = ROOT / "crates/oxide-core/src/browser.rs"
PREVIEW_PROXY = ROOT / "crates/oxide-gui/src/preview_proxy.rs"
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
BRAIN_FIXTURE = OUT_DIR / "brain.html"
SIDEBAR_FIXTURE = OUT_DIR / "sidebar.html"
SIDEBAR_COMPACT_FIXTURE = OUT_DIR / "sidebar-compact.html"


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
      <details class="thinking-box" open>
        <summary class="thinking-sum live"><span class="thinking-glow">Reasoning</span><span class="thinking-secs">3s</span></summary>
        <div class="thinking-body">Inspecting harness routes, streamed tool args, and session metadata.</div>
      </details>
      <details class="thought-row settling">
        <summary class="thought-sum"><span class="thought-label-stack"><span class="thought-label-live">Reasoning</span><span class="thought-label-settled">Thought for 3s</span></span></summary>
        <div class="thought-body">The reasoning label settles before this body collapses.</div>
      </details>
      <details class="act-group">
        <summary class="act-group-head"><span class="diff-caret">›</span><span class="act-group-icon">⚙</span><span>Working… 93 actions</span></summary>
        <div class="row activity"><span>Hidden until explicitly expanded</span></div>
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
        <details class="activity-card running activity-command has-out live-output" open>
          <summary class="activity-sum">
            <span class="activity-status" role="status" aria-atomic="true" aria-label="Running"><!-- UNICODE_ACTIVITY --><span class="activity-ic ok">✓</span><span class="activity-ic fail">×</span></span>
            <span class="activity-verb">Running command</span>
            <span class="activity-text">cargo test -p oxide-gui</span>
            <span class="activity-out-n">5 lines</span><span class="activity-caret">⌃</span>
          </summary>
          <pre class="activity-out">Compiling oxide-gui
Checking motion contracts
Running visual fixture
Testing command output tail
Latest output remains visible</pre>
        </details>
      </div>
      <div class="row activity">
        <details class="activity-card waiting-approval activity-command no-out">
          <summary class="activity-sum">
            <span class="activity-status" role="status" aria-atomic="true" aria-label="Waiting for approval"><span class="unicode-spinner activity-spin" aria-hidden="true"></span><span class="activity-ic approval">◇</span><span class="activity-ic ok">✓</span><span class="activity-ic fail">×</span></span>
            <span class="activity-verb">Waiting for approval</span>
            <span class="activity-text">git push origin main</span>
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
        <div class="agent-text agent-md live"><div class="live-stable"><p>Streaming answer text stays readable.</p></div><div class="live-tail"><span class="live-word fresh">New </span><span class="live-word fresh">words </span><span class="live-word fresh">fade </span><span class="live-word fresh">in.</span></div></div>
      </div>
      <div class="row agent">
        <div class="avatar"></div>
        <div class="agent-content"><div class="agent-text agent-md"><p>GUI verification completed.</p></div><div class="artifact-grid"><button class="artifact-card"><svg class="artifact-image" viewBox="0 0 480 240"><rect width="480" height="240" fill="#16181d"></rect><rect x="28" y="28" width="424" height="184" rx="14" fill="#22252c"></rect><circle cx="54" cy="51" r="6" fill="#5c9cf5"></circle><path d="M55 92h250M55 122h340M55 152h210" stroke="#747b89" stroke-width="10" stroke-linecap="round"></path></svg><span class="artifact-caption"><span class="artifact-kind">▧ GUI evidence</span><span class="artifact-name">oxide-gui-native.png</span><span class="artifact-path">.oxide/screenshots/oxide-gui-native.png</span></span></button></div></div>
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
      <details class="todo-card run-disclosure">
        <summary class="todo-head run-summary"><span class="todo-ic">☷</span><span class="run-label">Tasks 2/5</span><span class="run-preview">Implement compact orchestration layout</span><span class="run-caret">⌄</span></summary>
        <div class="todo-row in_progress"><span class="todo-box"></span><span class="todo-text">Implement compact orchestration layout</span></div>
      </details>
      <div class="composer-dock">
        <div class="composer-stack">
        <div class="queue-bar">
          <span class="queue-label"><svg viewBox="0 0 24 24" fill="none" stroke="currentColor"><circle cx="12" cy="12" r="9"></circle><path d="M12 7v5l3 2"></path></svg><span>Queued (2)</span></span>
          <div class="queue-chip queue-primary"><button class="queue-prompt"><span class="queue-index">1</span><span class="queue-text">Polish the queued prompt controls above the composer</span></button><button class="queue-steer">↗</button><button class="queue-x">×</button></div>
          <details class="queue-more"><summary class="queue-more-trigger">+1 <span>⌃</span></summary><div class="queue-menu"><div class="queue-row"><button class="queue-prompt"><span class="queue-index">2</span><span class="queue-text">Run the targeted visual checks</span></button><button class="queue-steer">↑</button><button class="queue-steer">↗</button><button class="queue-x">×</button></div><button class="queue-clear">Clear all</button></div></details>
        </div>
        <div class="composer"><div class="mention-menu skill-menu fixture-skill-menu"><div class="menu-label">Skills</div><button class="menu-item sel"><span class="skill-mention-mark">$</span><span class="menu-name">audit-gui-motion</span><span class="menu-meta">Workspace skill</span></button><button class="menu-item"><span class="skill-mention-mark">$</span><span class="menu-name">oxide-release-tag</span><span class="menu-meta">Workspace skill</span></button></div><div class="input ce-input" data-empty="false">Use <span class="ce-chip" data-prefix="$">audit-gui-motion</span> for this change…</div></div>
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
      </div>
      <div class="env-card">
        <div class="env-card-head"><span class="env-card-title">Environment</span></div>
        <button class="env-card-row env-subagents-running">
          <span class="env-subagent-status"><span class="syn-spinner"></span></span>
          <span class="env-subagent-copy"><span class="env-subagent-label">Subagents</span><span class="env-subagent-preview">reviewer · Audit slash command interactions</span></span>
          <span class="env-card-badge nowrap">1 running</span>
        </button>
      </div>
      <div class="preview-panel preview-fixture">
        <div class="preview-bar">
          <input class="preview-addr" aria-label="Preview URL" value="http://127.0.0.1:4173/">
          <button class="preview-btn">Scan</button>
          <button class="preview-btn pick">Pick</button>
          <button class="preview-btn on">Done</button>
          <button class="preview-btn">Design</button>
          <button class="preview-btn">Reload</button>
        </div>
        <div class="preview-session" role="status" aria-live="polite">
          <span class="preview-session-state human_controlled">Your control</span>
          <span class="preview-session-detail">https://example.test/dashboard · Browser control handed to the user</span>
          <div class="preview-session-actions"><button class="preview-btn on">Resume agent</button><button class="preview-btn">Cancel browser</button></div>
        </div>
        <div class="annotation-bar" role="status" aria-live="polite">
          <span class="annotation-count">2 pinned</span>
          <span class="annotation-status">Numbered annotations survive navigation</span>
          <button class="preview-btn annotation-clear" disabled>Attached</button>
          <button class="preview-btn">Clear</button>
        </div>
        <div class="preview-frame fixture-preview-canvas">
          <div class="fixture-preview-card"><span class="fixture-annotation">1</span><strong>Revenue overview</strong><span>Responsive dashboard card</span></div>
          <button class="fixture-preview-action"><span class="fixture-annotation">2</span>Export report</button>
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
    html, body {{ margin: 0; min-height: 100%; height: auto; background: #0d0d0f; color: #f4f4f5; }}
    body > .app {{ width: 100%; height: auto; min-height: 100vh; overflow: visible; }}
    body > .app > .chat {{ width: min(920px, 100%); max-width: none; margin: 0 auto; padding: 40px 24px; box-sizing: border-box; }}
    .avatar {{ width: 28px; height: 28px; border-radius: 50%; background: #22242a; flex: none; }}
    .fixture-skill-menu {{ position: relative; inset: auto; width: 100%; margin-bottom: 10px; box-shadow: none; }}
    .preview-fixture {{ height: 390px; min-height: 390px; margin-top: 18px; border: 1px solid var(--border); border-radius: 12px; overflow: hidden; }}
    .fixture-preview-canvas {{ position: relative; display: grid; place-content: center; gap: 22px; min-height: 180px; padding: 30px; box-sizing: border-box; background: #f6f5f2; color: #202124; }}
    .fixture-preview-card, .fixture-preview-action {{ position: relative; border: 2px solid #6073cc; border-radius: 10px; background: white; color: #202124; box-shadow: 0 12px 28px rgba(23, 28, 45, .12); }}
    .fixture-preview-card {{ display: grid; gap: 4px; min-width: 260px; padding: 18px; }}
    .fixture-preview-action {{ justify-self: end; padding: 9px 14px; }}
    .fixture-annotation {{ position: absolute; left: -2px; top: -22px; min-width: 20px; height: 20px; padding: 0 5px; box-sizing: border-box; border-radius: 6px 6px 6px 0; background: #6073cc; color: white; font: 600 11px/20px sans-serif; text-align: center; }}
  </style>
</head>
<body>
{body}
</body>
</html>
"""
    FIXTURE.write_text(fixture, encoding="utf-8")


def write_brain_fixture(css: str) -> None:
    """Write an interactive browser preview of the workspace memory graph."""
    OUT_DIR.mkdir(parents=True, exist_ok=True)
    escaped_css = css.replace("</style", "<\\/style")
    fixture = f"""<!doctype html>
<html lang="en">
<head>
  <meta charset="utf-8">
  <meta name="viewport" content="width=device-width, initial-scale=1">
  <title>Oxide Brain Preview</title>
  <style>
{escaped_css}
    html, body {{ margin: 0; width: 100%; height: 100%; background: #111; }}
    .brain-preview-shell {{ flex: 1; min-width: 0; height: 100%; }}
    .brain-preview-logo {{ width: 24px; height: 24px; display: inline-flex; align-items: center; justify-content: center; border-radius: 7px; background: color-mix(in srgb, var(--syn-accent) 18%, transparent); color: var(--syn-accent); font-size: 9px; font-weight: 750; }}
    .nav-preview-icon {{ width: 17px; flex: 0 0 17px; color: var(--muted); text-align: center; }}
    .nav-preview-icon svg {{ width: 16px; height: 16px; fill: none; stroke: currentColor; stroke-width: 1.9; stroke-linecap: round; stroke-linejoin: round; }}
  </style>
</head>
<body>
<div class="app" data-theme="dark">
  <aside class="sidebar brain-mode">
    <div class="brand"><span class="brain-preview-logo">OX</span><span class="brand-name">Oxide</span></div>
    <div class="side-seg" role="group" aria-label="Sidebar view"><button aria-pressed="false">Threads</button><button aria-pressed="false">Workspace</button></div>
    <nav class="nav">
      <button class="nav-item"><span class="nav-preview-icon">＋</span><span>New chat</span></button>
      <button class="nav-item"><span class="nav-preview-icon">⌕</span><span>Search</span></button>
      <button class="nav-item brain-nav on"><span class="nav-preview-icon"><svg viewBox="0 0 24 24" aria-hidden="true"><path d="M4.22222 21.9948V18.4451C4.22222 17.1737 3.88927 16.5128 3.23482 15.4078C2.4503 14.0833 2 12.5375 2 10.8866C2 5.97866 5.97969 2 10.8889 2C15.7981 2 19.7778 5.97866 19.7778 10.8866C19.7778 11.4663 19.7778 11.7562 19.802 11.9187C19.8598 12.3072 20.0411 12.6414 20.2194 12.9873L22 16.4407L20.6006 17.1402C20.195 17.3429 19.9923 17.4443 19.851 17.6314C19.7097 17.8184 19.67 18.0296 19.5904 18.4519L19.5826 18.4931C19.4004 19.4606 19.1993 20.5286 18.6329 21.2024C18.4329 21.4403 18.1853 21.6336 17.9059 21.7699C17.4447 21.9948 16.8777 21.9948 15.7437 21.9948C15.219 21.9948 14.6928 22.0069 14.1682 21.9942C12.9247 21.9639 12 20.9184 12 19.7044"></path><path d="M14.388 10.5315C13.9617 10.5315 13.5729 10.3702 13.2784 10.1048M14.388 10.5315C14.388 11.6774 13.7241 12.7658 12.4461 12.7658C11.1681 12.7658 10.5043 13.8541 10.5043 15M14.388 10.5315C16.5373 10.5315 16.5373 7.18017 14.388 7.18017C14.1927 7.18017 14.0053 7.21403 13.8312 7.27624C13.9362 4.77819 10.3349 4.1 9.51923 6.44018M10.5043 8.29729C10.5043 7.52323 10.1133 6.8411 9.51923 6.44018M9.51923 6.44018C7.66742 5.19034 5.19883 7.4331 6.37324 9.43277C4.40226 9.72827 4.61299 12.7658 6.6205 12.7658C7.18344 12.7658 7.68111 12.4844 7.98234 12.0538"></path></svg></span><span>Brain</span></button>
    </nav>
  </aside>
  <main class="brain-preview-shell">
    <section class="brain-view" aria-label="Workspace memory graph">
      <div class="brain-head">
        <div><div class="brain-eyebrow">⌁ Workspace intelligence</div><h2>Brain</h2><p>Durable facts and reusable skills learned across your project folders.</p></div>
        <button class="brain-refresh">↻ Refresh</button>
      </div>
      <div class="brain-stats">
        <div class="brain-stat"><span>Projects</span><strong>4</strong></div>
        <div class="brain-stat facts"><span>Remembered facts</span><strong>18</strong></div>
        <div class="brain-stat skills"><span>Learned skills</span><strong>7</strong></div>
      </div>
      <div class="brain-layout">
        <div class="brain-map-card">
          <div class="brain-map-title"><span>Knowledge map</span><span>Click a workspace node to inspect what it learned</span></div>
          <svg class="brain-map" viewBox="0 0 900 520" role="group" aria-label="Knowledge map">
            <line class="brain-edge active" data-edge="oxide" x1="450" y1="260" x2="450" y2="82" style="--edge-width:1.96px"></line>
            <line class="brain-edge" data-edge="synara" x1="450" y1="260" x2="742" y2="260" style="--edge-width:1.60px"></line>
            <line class="brain-edge" data-edge="provider" x1="450" y1="260" x2="450" y2="438" style="--edge-width:1.48px"></line>
            <line class="brain-edge" data-edge="harness" x1="450" y1="260" x2="158" y2="260" style="--edge-width:1.36px"></line>
            <circle class="brain-core-halo" cx="450" cy="260" r="69"></circle>
            <circle class="brain-core" cx="450" cy="260" r="54"></circle>
            <text class="brain-core-mark" x="450" y="255">OX</text><text class="brain-core-label" x="450" y="278">Memory</text>
            <g class="brain-node active" role="button" aria-label="oxide, 12 memories, 8 facts, 4 skills" aria-pressed="true" data-node="oxide" tabindex="0"><rect x="362" y="46" width="176" height="72" rx="18"></rect><circle class="brain-current-dot" cx="522" cy="61"></circle><text class="brain-node-name" x="450" y="74">oxide</text><text class="brain-node-count" x="450" y="95">12 memories</text><text class="brain-node-kinds" x="450" y="111">8 facts · 4 skills</text></g>
            <g class="brain-node" role="button" aria-label="synara, 6 memories, 4 facts, 2 skills" aria-pressed="false" data-node="synara" tabindex="0"><rect x="654" y="224" width="176" height="72" rx="18"></rect><text class="brain-node-name" x="742" y="252">synara</text><text class="brain-node-count" x="742" y="273">6 memories</text><text class="brain-node-kinds" x="742" y="289">4 facts · 2 skills</text></g>
            <g class="brain-node" role="button" aria-label="providers, 4 memories, 3 facts, 1 skill" aria-pressed="false" data-node="provider" tabindex="0"><rect x="362" y="402" width="176" height="72" rx="18"></rect><text class="brain-node-name" x="450" y="430">providers</text><text class="brain-node-count" x="450" y="451">4 memories</text><text class="brain-node-kinds" x="450" y="467">3 facts · 1 skill</text></g>
            <g class="brain-node" role="button" aria-label="harnesses, 3 memories, 3 facts, 0 skills" aria-pressed="false" data-node="harness" tabindex="0"><rect x="70" y="224" width="176" height="72" rx="18"></rect><text class="brain-node-name" x="158" y="252">harnesses</text><text class="brain-node-count" x="158" y="273">3 memories</text><text class="brain-node-kinds" x="158" y="289">3 facts · 0 skills</text></g>
          </svg>
        </div>
        <aside class="brain-inspector">
          <div class="brain-project-head"><span class="brain-project-icon">⌘</span><div><h3 id="brain-project-name">oxide</h3><p>/Volumes/Data/oxide</p></div><span class="brain-current">Current</span></div>
          <div class="brain-memory-summary"><span id="brain-memory-total">12 memories</span><span>Stored in .oxide/memory</span></div>
          <div class="brain-memory-section"><div class="brain-memory-section-head"><span>Facts</span><span>8</span></div><div class="brain-memory-row fact"><span class="brain-memory-dot"></span><p>Tool-call activity is paired by stable call ID.</p></div><div class="brain-memory-row fact"><span class="brain-memory-dot"></span><p>Reasoning is coalesced into one Thought row per turn.</p></div><div class="brain-memory-row fact"><span class="brain-memory-dot"></span><p>Animations use the shared Oxide motion tokens.</p></div></div>
          <div class="brain-memory-section"><div class="brain-memory-section-head"><span>Skills</span><span>4</span></div><button class="brain-skill-row"><span class="brain-skill-icon">⌁</span><span class="brain-skill-copy"><strong>oxide-release-tag</strong><span>Build, verify, tag, and publish an Oxide release</span></span><span>›</span></button><button class="brain-skill-row"><span class="brain-skill-icon">⌁</span><span class="brain-skill-copy"><strong>audit-gui-motion</strong><span>Review motion contracts and visual regressions</span></span><span>›</span></button></div>
        </aside>
      </div>
    </section>
  </main>
</div>
<script>
  const labels = {{oxide:['oxide','12 memories'],synara:['synara','6 memories'],provider:['providers','4 memories'],harness:['harnesses','3 memories']}};
  const activateBrainNode = node => {{
    document.querySelectorAll('.brain-node').forEach(el => {{ el.classList.remove('active'); el.setAttribute('aria-pressed','false'); }});
    document.querySelectorAll('.brain-edge').forEach(el => el.classList.remove('active'));
    node.classList.add('active');
    node.setAttribute('aria-pressed','true');
    document.querySelector(`[data-edge="${{node.dataset.node}}"]`)?.classList.add('active');
    const [name, total] = labels[node.dataset.node];
    document.getElementById('brain-project-name').textContent = name;
    document.getElementById('brain-memory-total').textContent = total;
  }};
  document.querySelectorAll('.brain-node').forEach(node => {{
    node.addEventListener('click', () => activateBrainNode(node));
    node.addEventListener('keydown', event => {{
      if (event.key === 'Enter' || event.key === ' ') {{ event.preventDefault(); activateBrainNode(node); }}
    }});
  }});
</script>
</body>
</html>
"""
    BRAIN_FIXTURE.write_text(fixture, encoding="utf-8")


def write_sidebar_fixture(css: str) -> None:
    """Write an interactive browser preview of expanded, rail, and drawer states."""
    OUT_DIR.mkdir(parents=True, exist_ok=True)
    escaped_css = css.replace("</style", "<\\/style")
    fixture = f"""<!doctype html>
<html lang="en">
<head>
  <meta charset="utf-8">
  <meta name="viewport" content="width=device-width, initial-scale=1">
  <title>Oxide Sidebar Preview</title>
  <style>
{escaped_css}
    html, body {{ margin: 0; width: 100%; height: 100%; background: #111; }}
    .sidebar-preview-main {{ min-width: 0; padding: 28px 34px; background: var(--bg); }}
    .sidebar-preview-main h1 {{ margin: 0; font-size: 26px; }}
    .sidebar-preview-main p {{ max-width: 58ch; color: var(--muted); line-height: 1.55; }}
    .sidebar-preview-card {{ max-width: 620px; margin-top: 24px; padding: 22px; border: 1px solid var(--border); border-radius: 14px; background: var(--surface); }}
    .sidebar-preview-logo {{ width: 24px; height: 24px; display: grid; place-items: center; border-radius: 7px; background: color-mix(in srgb, var(--syn-accent) 18%, transparent); color: var(--syn-accent); font-size: 9px; font-weight: 750; }}
    .sidebar-preview-icon {{ width: 16px; height: 16px; flex: 0 0 16px; }}
  </style>
</head>
<body>
<div class="app" data-theme="dark">
  <aside id="primary-sidebar" class="sidebar" aria-label="Primary navigation" style="width:280px">
    <div class="brand">
      <button class="logo-btn desktop-sidebar-toggle" data-tooltip="Collapse sidebar" aria-label="Collapse sidebar"><span class="sidebar-preview-logo">OX</span></button>
      <button id="mobile-sidebar-toggle" class="logo-btn mobile-sidebar-toggle" data-tooltip="Open sidebar" aria-label="Open sidebar"><span class="sidebar-preview-logo">OX</span></button>
      <span class="brand-name">Oxide</span>
    </div>
    <div class="side-seg" role="group" aria-label="Sidebar view"><button class="on" aria-pressed="true">Threads</button><button aria-pressed="false">Workspace</button></div>
    <nav class="nav" aria-label="Workspace actions">
      <button class="nav-item" data-tooltip="New chat" aria-label="New chat"><svg class="sidebar-preview-icon" viewBox="0 0 24 24" fill="none" stroke="currentColor"><path d="M12 20h9M16.5 3.5a2.1 2.1 0 0 1 3 3L7 19l-4 1 1-4z"></path></svg><span class="nav-label">New chat</span></button>
      <button class="nav-item" data-tooltip="Search" aria-label="Search"><svg class="sidebar-preview-icon" viewBox="0 0 24 24" fill="none" stroke="currentColor"><circle cx="11" cy="11" r="7"></circle><path d="m16 16 5 5"></path></svg><span class="nav-label">Search</span></button>
      <button class="nav-item brain-nav" data-tooltip="Brain" aria-label="Brain"><svg class="sidebar-preview-icon" viewBox="0 0 24 24" fill="none" stroke="currentColor"><path d="M9 4a3 3 0 0 0-3 3v1a3 3 0 0 0-2 3 3 3 0 0 0 2 3v1a3 3 0 0 0 3 3M15 4a3 3 0 0 1 3 3v1a3 3 0 0 1 2 3 3 3 0 0 1-2 3v1a3 3 0 0 1-3 3M9 4v14M15 4v14"></path></svg><span class="nav-label">Brain</span></button>
    </nav>
    <div class="section-row"><span class="section-label">Projects</span><button class="section-add" aria-label="Open folder">+</button></div>
    <div class="projects">
      <div class="section-label">Pinned</div>
      <div class="thread-anchor"><button class="thread recent"><span class="thread-title">Refine release workflow</span></button><div class="row-actions"><button class="row-act-btn pinned" aria-label="Unpin">•</button></div></div>
      <div class="project-group">
        <div class="project current"><button class="proj-caret" aria-expanded="true">⌃</button><button class="project-main" aria-current="page"><svg viewBox="0 0 24 24" fill="none" stroke="currentColor"><path d="M3 6h7l2 3h9v10H3z"></path></svg><span class="project-name">oxide</span></button><button class="project-add" aria-label="New chat">+</button><button class="project-del" aria-label="Hide project">×</button></div>
        <div class="thread active" role="button" tabindex="0" aria-current="page"><span class="tab-prov">◈</span><span class="thread-title">Sidebar interaction polish</span><span class="unread-dot" aria-hidden="true"></span></div>
        <div class="thread-anchor"><div class="thread recent sub" role="button" tabindex="0"><span class="sess-logo">◇</span><span class="thread-title">Audit keyboard states</span><span class="thread-time">2m</span></div><div class="row-actions"><button class="row-act-btn" aria-label="Pin">•</button><button class="row-act-btn" aria-label="More">•••</button></div></div>
      </div>
    </div>
    <div class="usage-chip"><div class="usage-head">Usage remaining</div><div class="usage-row"><span class="usage-k">5h</span><span class="usage-bar"><span class="usage-fill" style="width:68%"></span></span><span class="usage-v">68%</span></div></div>
    <button class="update-row ready" data-tooltip="Update Oxide"><span class="update-row-ic">↑</span><span class="update-row-label">Update</span><span class="update-row-ver">v0.0.167</span></button>
    <button class="settings-btn" data-tooltip="Settings"><span>⚙</span><span>Settings</span></button>
  </aside>
  <button class="sidebar-drawer-backdrop" aria-label="Close sidebar"></button>
  <div class="panel-resizer" role="separator" tabindex="0" aria-label="Resize sidebar"></div>
  <main class="main sidebar-preview-main" tabindex="-1"><h1>Sidebar visual QA</h1><p>Use the Oxide logo to switch between the expanded sidebar and icon rail. Resize this page below 900px to exercise the explicit compact drawer and backdrop.</p><div class="sidebar-preview-card"><strong>Expected behavior</strong><p>The active thread has an accent rail, row actions keep a stable trailing slot, mixed icons share one optical box, and compact navigation never removes access to project history.</p></div></main>
</div>
<script>
  const sidebar = document.getElementById('primary-sidebar');
  const desktopToggle = document.querySelector('.desktop-sidebar-toggle');
  const mobileToggle = document.getElementById('mobile-sidebar-toggle');
  const backdrop = document.querySelector('.sidebar-drawer-backdrop');
  const initialState = new URLSearchParams(location.search).get('state');
  if (initialState === 'collapsed') sidebar.classList.add('collapsed');
  if (initialState === 'drawer') sidebar.classList.add('drawer-open');
  desktopToggle.addEventListener('click', () => sidebar.classList.toggle('collapsed'));
  mobileToggle.addEventListener('click', () => sidebar.classList.toggle('drawer-open'));
  backdrop.addEventListener('click', () => sidebar.classList.remove('drawer-open'));
  document.addEventListener('keydown', event => {{ if (event.key === 'Escape') sidebar.classList.remove('drawer-open'); }});
</script>
</body>
</html>
"""
    SIDEBAR_FIXTURE.write_text(fixture, encoding="utf-8")
    SIDEBAR_COMPACT_FIXTURE.write_text(
        """<!doctype html>
<html><head><meta charset="utf-8"><title>Oxide Compact Sidebar Preview</title>
<style>html,body{margin:0;width:100%;height:100%;background:#0d0d0f;display:grid;place-items:start center}iframe{width:760px;height:900px;border:0;background:#111}</style>
</head><body><iframe src="sidebar.html" title="Compact sidebar drawer" onload="this.contentDocument.getElementById('primary-sidebar')?.classList.add('drawer-open')"></iframe></body></html>
""",
        encoding="utf-8",
    )


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
    provider_catalog = read(PROVIDER_CATALOG)
    chatgpt = read(CHATGPT)
    core = read(CORE)
    browser = read(BROWSER)
    preview_proxy = read(PREVIEW_PROXY)
    db = read(DB)
    store = read(STORE)
    checklist = read(CHECKLIST)
    native_smoke = read(NATIVE_SMOKE)
    native_record = read(NATIVE_RECORD)
    update = read(UPDATE)
    hooks = read(HOOKS)
    automation = read(AUTOMATION)

    require(
        "pre-token progress uses one status surface",
        contains_all(
            gui,
            [
                "&& thinking.read().is_empty()",
                "&& !live_answer_visible",
                "&& !has_running_activity",
                "The live reasoning block and StatusPill already communicate",
            ],
        )
        and 'class: "row agent agent-waiting streaming-message"' not in gui,
        f"{rel(GUI)} suppresses the duplicate empty-agent Thinking row when Reasoning, Working, or the live answer already owns progress",
    )
    require(
        "settled answer avoids full-block blur",
        contains_all(
            css,
            [
                "@keyframes oxide-focus-in",
                "transform: translateY(2px)",
                "animation: oxide-focus-in .24s var(--ease-out) both;",
            ],
        )
        and "filter: blur(6px)" not in css,
        f"{rel(CSS)} settles long Markdown answers with compositor-only opacity/transform instead of rasterizing blur",
    )
    require(
        "unicode activity micro-motion stays single-node",
        contains_all(
            gui,
            [
                "fn UnicodeSpinner",
                'rsx! { span { class: "unicode-spinner {class}", aria_hidden: "true" } }',
                'UnicodeSpinner { class: "status-spinner" }',
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
    reduced_motion_query = "@media (prefers-reduced-motion: reduce)"
    impossible_motion_query = (
        "@media (prefers-reduced-motion: reduce) "
        "and (prefers-reduced-motion: no-preference)"
    )
    require(
        "host reduced motion disables decorative motion",
        impossible_motion_query not in css
        and css.count(reduced_motion_query) > 0
        and contains_all(
            css,
            [
                "/* Canonical host Reduced Motion contract.",
                "*, *::before, *::after {",
                "animation: none !important;",
                "transition: none !important;",
                ".activity-card.running .activity-status .unicode-spinner.activity-spin::after,",
                ".typing .unicode-spinner.status-spinner::after {",
                "animation: oxide-unicode-frame 2.4s steps(10, end) infinite !important;",
                "animation: bg-orbit-rot 2.4s steps(8, end) infinite !important;",
            ],
        ),
        f"{rel(CSS)} honors the host preference, removes decorative/repeating motion, and keeps only slow stepped active-progress indicators",
    )
    require(
        "streaming tail motion is keyed and bounded",
        contains_all(
            gui,
            [
                '"row agent streaming-message"',
                '"agent-text agent-md live"',
                "fn LiveMarkdown(",
                "fn live_tail_chunks(",
                "const FRESH_WORDS: usize = 6;",
                'key: "live-word-{key}"',
                'class: "live-word fresh"',
            ],
        )
        and contains_all(
            css,
            [
                "@keyframes oxide-stream-first-token",
                "@keyframes oxide-stream-word",
                "@keyframes oxide-stream-rail",
                ".row.agent.streaming-message::before",
                ".agent-md.live .live-word.fresh",
                "animation: oxide-stream-word var(--dur-med) var(--ease-enter) both;",
            ],
        ),
        f"{rel(GUI)} and {rel(CSS)} animate only six keyed tail words while completed markdown remains stable",
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
        "foreground streaming coalesces answer and reasoning deltas",
        contains_all(
            gui,
            [
                "macro_rules! flush_reasoning_live",
                "if !agent_buf.is_empty() || !reasoning_buf.is_empty()",
                "agent_buf.len() + reasoning_buf.len() > 800",
                "flush_reasoning_live!();",
            ],
        )
        and nearby(gui, "let mut view_tab: u64", "macro_rules! flush_reasoning_live", 9000),
        f"{rel(GUI)} batches foreground answer/reasoning paints at frame cadence instead of re-rendering per token",
    )
    require(
        "tool lifecycle uses stable status and disclosure slots",
        contains_all(
            gui,
            [
                "fn ActivityStatus",
                'class: "activity-status"',
                'UnicodeSpinner { class: "activity-spin" }',
                'span { class: "activity-ic approval"',
                'span { class: "activity-ic ok"',
                'span { class: "activity-ic fail"',
                "let live_output =",
                "has_output && running && !waiting && matches!(view.kind, ActivityKind::Command);",
                '"has-out live-output"',
                'details { class: "{cls}", open: has_output && (auto_open || live_output)',
                'class: "activity-caret"',
            ],
        )
        and contains_all(
            css,
            [
                "@keyframes oxide-tool-enter",
                ".activity-status",
                ".activity-card.has-out::details-content",
                ".activity-card.has-out[open] .activity-caret",
            ],
        ),
        f"{rel(GUI)} and {rel(CSS)} cross-fade running/approval/success/failure in a fixed slot and keep disclosure state stable",
    )
    require(
        "coalesced activity rows preserve stable render identity",
        contains_all(
            gui,
            [
                "struct ActivityRenderRow",
                "id: m.id,",
                "last.count += row.count;",
                'key: "a-{row_key}"',
                "fn coalesced_activity_rows_keep_the_first_stable_id()",
            ],
        ),
        f"{rel(GUI)} keeps the first activity id through coalescing so streamed updates do not remount the wrong motion row",
    )
    require(
        "live command output follows the tail in a compact window",
        contains_all(
            gui,
            [
                '"has-out live-output"',
                "const followLiveOutputs = (s) => {",
                "s.querySelectorAll('.activity-card.live-output[open] .activity-out')",
                "out.scrollHeight - out.scrollTop - out.clientHeight < 60",
                "out.scrollTop = out.scrollHeight;",
                "state.observer.observe(s, { childList: true, subtree: true, characterData: true });",
            ],
        )
        and contains_all(
            css,
            [
                ".activity-card.live-output .activity-out {",
                "max-height: 92px;",
                "scrollbar-gutter: stable;",
                "mask-image: linear-gradient(to bottom, transparent 0, #000 14px, #000 100%);",
            ],
        ),
        f"{rel(GUI)} and {rel(CSS)} auto-open active commands, coalesce tail-follow with transcript scrolling, and fade old output",
    )
    require(
        "reasoning and tool disclosures follow measured height",
        contains_all(
            gui,
            [
                "card.matches('.activity-card.has-out, .thinking-box, .thought-row')",
                "new ResizeObserver(() => state.stick())",
                "card.addEventListener('transitionend', finish, { once: true });",
                "getComputedStyle(card).getPropertyValue('--dur-slow')",
            ],
        )
        and "if (now - started < 240) requestAnimationFrame(followTween);" not in gui
        and contains_all(
            css,
            [
                ".activity-card.has-out::details-content {",
                ".thinking-box::details-content,",
                ".thought-row::details-content {",
                "grid-template-rows: 0fr;",
                "grid-template-rows: 1fr;",
                "transition: grid-template-rows var(--dur-med) var(--ease-out),",
            ],
        ),
        f"{rel(GUI)} and {rel(CSS)} interpolate disclosure height while preserving intent-based bottom anchoring",
    )
    require(
        "active reasoning tool and edit labels share lifecycle shimmer",
        contains_all(
            css,
            [
                ".thinking-glow,",
                ".activity-card.running .activity-verb,",
                ".composer-live-changes .live-changes-title {",
                "animation: ox-shimmer 2s linear infinite;",
                ".col.streaming .row.diffrow,",
                ".thinking-box[open] > .thinking-body {",
            ],
        ),
        f"{rel(CSS)} keeps one Emdash-style shimmer per active tool plus reasoning/edit motion without repainting tool details",
    )
    require(
        "approval lifecycle reuses the keyed tool row",
        contains_all(
            gui,
            [
                "fn mark_activity_waiting_for_approval(",
                "activity_idx(messages, call_id)",
                "fn resume_activity_after_approval(",
                'view.verb = "Waiting for approval".to_string();',
                'span { class: "activity-ic approval"',
                '"waiting-approval"',
            ],
        )
        and contains_all(
            css,
            [
                ".activity-card.waiting-approval .activity-status .activity-ic.approval,",
                ".activity-card.waiting-approval .activity-verb { color: var(--warn); }",
            ],
        )
        and contains_all(
            protocol,
            [
                "ApprovalRequested {",
                "call_id: String,",
            ],
        ),
        f"{rel(PROTOCOL)}, {rel(GUI)}, and {rel(CSS)} pair approval by call_id and cross-fade spinner/shield/result in one row",
    )
    require(
        "continuous motion stays bounded to useful feedback",
        contains_all(
            css,
            [
                ".brain-edge.active {",
                "animation: brain-edge-flow calc(var(--dur-slow) * 18) linear infinite;",
                ".brain-core-halo {",
                "background: var(--warn, #d9a35c); opacity: .9;",
                "static gradient avoids repainting the full",
            ],
        )
        and "brain-core-pulse" not in css
        and "bg-pulse" not in css
        and "ultrathink-rainbow" not in css
        and ".activity-card.running .activity-text," not in css,
        f"{rel(CSS)} leaves functional spinner/shimmer feedback active while removing large or duplicate infinite repaint loops",
    )
    require(
        "reasoning settles before disclosure collapse",
        contains_all(
            gui,
            [
                "fn ThoughtRow(",
                '"thought-row settling"',
                'class: "thought-label-live"',
                'class: "thought-label-settled"',
                'details { class: "{class}",',
                "settling_thought.set(Some(thought_id));",
                "onanimationend: move |event|",
                'event.animation_name() == "oxide-thought-settled-in"',
                "window.matchMedia('(prefers-reduced-motion: reduce)').matches",
            ],
        )
        and "from_millis(320)" not in gui
        and contains_all(
            css,
            [
                "@keyframes oxide-thought-live-out",
                "@keyframes oxide-thought-settled-in",
                ".thought-label-stack",
            ],
        )
        and 'open: settling' not in gui,
        f"{rel(GUI)} and {rel(CSS)} cross-fade Reasoning into a collapsed Thought label without auto-expanding its body",
    )
    require(
        "reasoning segments coalesce per turn",
        contains_all(
            gui,
            [
                "fn upsert_turn_thought(",
                "previous_secs + secs.max(1)",
                'format!("{previous_body}\\n\\n{text}")',
                "coalesce_transcript_thoughts(messages)",
                "let thought_id = upsert_turn_thought(&mut m, secs, &text);",
                "let thought_id = upsert_turn_thought(&mut rows, secs, &text);",
            ],
        ),
        f"{rel(GUI)} keeps one stable Thought disclosure per user turn in primary, pane, and replay paths",
    )
    require(
        "working and reasoning surfaces do not duplicate",
        contains_all(
            gui,
            [
                ".unwrap_or(false);",
                "active_thought_id != Some(m.id)",
                "active_thought_id != Some(thought_id)",
                "&& !has_running_activity",
                "&& !live_answer_visible",
            ],
        )
        and "unwrap_or(tool_detail == \"detailed\")" not in gui,
        f"{rel(GUI)} keeps Working collapsed by default, hides settled Thought while live Reasoning owns the turn, and suppresses redundant Thinking status",
    )
    require(
        "motion policy preserves normal polish with a reduced fallback",
        contains_all(
            css,
            [
                "@keyframes oxide-stream-first-token",
                "@keyframes oxide-stream-word",
                "@keyframes oxide-stream-rail",
                "@keyframes oxide-thought-settled-in",
                "@keyframes oxide-unicode-frame",
                "@keyframes surface-rise",
                "animation:surface-rise .22s ease-out both;",
                reduced_motion_query,
                "/* Canonical host Reduced Motion contract.",
            ],
        )
        and "@keyframes oxide-tool-halo" not in css,
        f"{rel(CSS)} retains normal lifecycle polish while the canonical host fallback removes nonessential motion",
    )
    require(
        "pending edit shimmer becomes a legible static status",
        contains_all(
            css,
            [
                ".edits-row.pending .edits-rowcounts.shimmer {",
                "animation: shimmer 1.7s linear infinite;",
                ".slot-char",
                ".edits-row.pending .edits-rowcounts.shimmer {",
                "-webkit-text-fill-color: currentColor;",
            ],
        )
        and 'class: "edits-rowcounts shimmer slot-status"' in gui,
        f"{rel(CSS)} and {rel(GUI)} keep the transcript edit state visible without shimmer when host Reduced Motion is enabled",
    )
    require(
        "composer orchestration surfaces stay compact",
        contains_all(
            gui,
            [
                'details { class: "todo-card run-disclosure"',
                'class: "run-preview"',
                'class: "composer-live-changes"',
                'class: "env-card-row env-subagents-running"',
                'inspector_tab.set("agents".to_string())',
                'select_env_tab(env_tab, show_env, env_tab_by_tab, tabs, active_tab, "changes", false)',
            ],
        )
        and 'details { class: "subagents-card run-disclosure"' not in gui
        and "live-changes-files" not in gui
        and contains_all(
            css,
            [
                ".run-disclosure { overflow: hidden; }",
                ".todo-card { width: 100%; max-width: 760px; max-height: 40px;",
                ".live-changes-copy { min-width: 0; display: flex; align-items: baseline;",
                ".env-subagent-copy { min-width: 0; flex: 1 1 auto;",
            ],
        ),
        f"{rel(GUI)} routes running sub-agents to Environment → Agents while composer task/change surfaces stay compact",
    )
    require(
        "queued prompts stay compact above composer",
        contains_all(
            gui,
            [
                "fn QueuedPromptBar(",
                "restore_queued_prompt(queue, 0)",
                "steer_queued_prompt(queue, engine, 0)",
                'details { class: "queue-more"',
                '"Clear all"',
                "QueuedPromptBar { queue, engine }",
                "Some(q.remove(0))",
            ],
        )
        and nearby(gui, "QueuedPromptBar { queue, engine }", 'class: match (*streaming.read(), cur_effort.as_str())', 320)
        and contains_all(
            css,
            [
                ".queue-primary { flex: 1 1 360px; max-width: 470px; }",
                ".queue-text { min-width: 0; overflow: hidden; text-overflow: ellipsis; white-space: nowrap; }",
                ".queue-menu {",
                ".composer-stack > .queue-bar { margin: 0 auto 8px; }",
            ],
        ),
        "Queued prompts use one Codex-style rail with edit, steer, remove, reorder, clear, and automatic next-turn drain controls",
    )
    require(
        "dollar skill mentions are keyboard-first",
        contains_all(
            gui,
            [
                "let q=null, skill=null;",
                'v["skill"].as_str()',
                "skill_candidates(&ws, &query_for_scan)",
                "ce_insert_js(&token, &label, '$')",
                "chip.dataset.token={token}; chip.dataset.prefix=marker;",
                "body+=(c.dataset.prefix||'@')",
                'class: "mention-menu skill-menu"',
                "Key::Enter | Key::Tab if !e.modifiers().shift()",
            ],
        )
        and contains_all(
            css,
            [
                ".skill-menu {",
                ".skill-mention-mark {",
                '.ce-chip[data-prefix="$"]::before',
            ],
        ),
        f"{rel(GUI)} and {rel(CSS)} expose Codex-style $skill discovery, keyboard insertion, visible chips, and existing skill instruction injection",
    )
    require(
        "generated image citations become artifact previews",
        contains_all(
            gui,
            [
                "fn image_artifacts(text: &str, workspace: &Path)",
                "if !path.starts_with(&workspace)",
                'class: "artifact-grid"',
                'class: "artifact-card"',
                'loading: "lazy"',
                "open_file(ui, path.clone())",
            ],
        )
        and contains_all(
            css,
            [
                ".artifact-grid {",
                ".artifact-card {",
                ".artifact-image {",
                ".artifact-caption {",
            ],
        ),
        f"{rel(GUI)} renders up to four workspace-confined screenshot/image citations as lazy Codex-style cards that open the existing image viewer",
    )
    require(
        "browser automation ownership stays browser-scoped",
        contains_all(
            browser,
            [
                "pub async fn close(mut self) -> Result<()>",
                "self.browser.close()).await",
                "remove_profile_dir(&self.profile_dir).await",
                "tokio::fs::remove_dir_all(path).await",
            ],
        )
        and contains_all(
            protocol,
            [
                "pub enum BrowserControlAction",
                "TakeOver,",
                "Resume,",
                "Cancel,",
                "BrowserSessionState {",
            ],
        )
        and contains_all(
            core,
            [
                "browser_control_tx: mpsc::UnboundedSender<BrowserControlAction>",
                "async fn finish_turn(&mut self, turn: TurnId)",
                "if self.browser_control == BrowserControlState::HumanControlled",
                'self.close_browser("Browser session closed at the end of the turn")',
                "async fn apply_browser_control(&mut self, action: BrowserControlAction)",
            ],
        )
        and contains_all(
            gui,
            [
                "EngineCmd::BrowserControl(BrowserControlAction::TakeOver)",
                "EngineCmd::BrowserControl(BrowserControlAction::Resume)",
                "EngineCmd::BrowserControl(BrowserControlAction::Cancel)",
                "browser_session.is_open() && !browser_session.visible",
            ],
        )
        and core.count("self.finish_turn(turn).await;") >= 6
        and core.count("self.emit(Event::TurnFinished { turn }).await;") == 1,
        f"{rel(BROWSER)}, {rel(CORE)}, and {rel(GUI)} keep takeover/resume/cancel independent from the owning turn and expose headless state truthfully",
    )
    require(
        "preview annotations persist across navigation",
        contains_all(
            preview_proxy,
            [
                "oxide-annotate-on",
                "oxide-annotations-clear",
                "oxide-annotations-restore",
                "data-oxide-annotation",
                "renderAnnotations",
                "CSS.escape",
                "pageKey(entry.url)",
                "addAnnotation(el,requested)",
                "skipped:skipped",
            ],
        )
        and contains_all(
            gui,
            [
                "fn preview_annotations_context(",
                "e.source !== frame.contentWindow",
                "oxide-annotations-restored",
                "preview_annotations.write().clear();",
                '("url", "url")',
                "annotations:{annotations}",
                "Add to prompt",
            ],
        ),
        f"{rel(PREVIEW_PROXY)} and {rel(GUI)} keep numbered element context mounted, scoped to the preview frame, restorable, and explicitly attachable",
    )
    require(
        "environment views remain mounted between tabs",
        contains_all(
            gui,
            [
                'key: "env-changes"',
                'key: "env-preview"',
                'key: "env-files"',
                'key: "env-term"',
                '"env-pane env-pane-on"',
                '"env-pane env-pane-off"',
            ],
        )
        and contains_all(
            css,
            [
                ".env-pane {",
                ".env-pane-on {",
                ".env-pane-off {",
                "pointer-events: none;",
            ],
        ),
        f"{rel(GUI)} and {rel(CSS)} preserve preview, diff, files, and PTY DOM state while inactive panes are visually and interactively hidden",
    )
    require(
        "model picker is catalog-backed and readiness-aware",
        "MODEL_PRESETS" not in gui
        and contains_all(
            gui,
            [
                "oxide_providers::list_providers()",
                "oxide_providers::list_provider_models(preset.provider)",
                "oxide_providers::diagnose_provider",
                "fn production_model_presets()",
                "fn legacy_config_model_is_rendered_without_entering_production_picker()",
            ],
        )
        and contains_all(
            provider_catalog,
            [
                'id: "gpt-5.6-sol"',
                'id: "gpt-5.4"',
                'id: "claude-fable-5"',
                "fn catalog_contains_every_production_gui_model_pair()",
            ],
        ),
        f"{rel(GUI)} derives selectable production models and readiness from {rel(PROVIDER_CATALOG)} while retaining legacy config aliases",
    )
    require(
        "brain graph exposes keyboard selection semantics",
        contains_all(
            gui,
            [
                'role: "group"',
                '"aria-label": "Interactive workspace knowledge map"',
                '"aria-pressed": "{is_selected}"',
                "event.prevent_default();",
                'aria_live: "polite"',
            ],
        ),
        f"{rel(GUI)} exposes the SVG workspace map as an interactive group with selected-state announcements and Enter/Space behavior",
    )
    require(
        "slash command palette is keyboard-first",
        contains_all(
            gui,
            [
                'let mut slash_q = use_signal(|| None::<String>);',
                'let mut slash_sel = use_signal(|| 0usize);',
                '("mcp", "Manage connected MCP servers")',
                '("plan", "Plan a task before implementation")',
                '("goal", "Set or manage the active agent goal")',
                'Key::ArrowDown if !items.is_empty()',
                'Key::ArrowUp if !items.is_empty()',
                'Key::Enter | Key::Tab if !e.modifiers().shift()',
                '"mcp" => {',
                '"plan" => {',
                '"goal" => {',
            ],
        ),
        f"{rel(GUI)} exposes native/custom slash commands with arrow, Enter, Tab, and Escape navigation",
    )
    require(
        "default external destructive command guard",
        contains_all(
            hooks,
            [
                "pub fn dcg_binary() -> Option<PathBuf>",
                "pub async fn dcg_tool_reason",
                '.args(["--robot", "test", command])',
                "std::time::Duration::from_millis(1500)",
                "output.status.code() != Some(1)",
            ],
        )
        and "hooks::dcg_tool_reason(&name, &arguments).await" in core
        and contains_all(
            gui,
            [
                '"Destructive command guard"',
                '"DCG active · {path.display()}"',
                '"Built-in guard active · DCG not found"',
                '"Enabled by default for Oxide shell tools, including ChatGPT Subscription.',
            ],
        ),
        "Oxide auto-detects user-installed DCG for native/subscription shell tools and exposes its status in Access settings",
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
                'const transcriptSelector = \'[data-oxide-transcript]\';',
                '"data-oxide-transcript": "main"',
                '"data-oxide-transcript": "pane"',
                "bottomDistance",
                "hasSelection",
                "typingTarget",
                "jumpScroll: false",
                "const finishJump = () => {",
                "const cancelJump = () => {",
                "if (state.jumpScroll) {",
                "cancelJump();",
                "const trackJump = () => {",
                "setSticky(s, state, bottomDistance(s) < 8);",
                "detach(target)",
                "jump(target, behavior)",
                "state.observer.observe(s, { childList: true, subtree: true, characterData: true });",
                "}).observe(shell, { childList: true, subtree: true });",
            ],
        )
        and "}).observe(document.body, { childList: true, subtree: true, characterData: true });" not in gui
        and contains_all(css, [".scroll", "overflow-anchor: none"]),
        "main and split transcripts keep independent sticky state, detach on reader intent, and avoid a document-wide token observer",
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
                'EngineCmd::submit(prompt, "/review (Bugbot)".into())',
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
                "tokio::time::sleep(std::time::Duration::from_secs(10))",
                "whats_new.set(None)",
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
        "workspace brain source graph",
        contains_all(
            gui,
            [
                'sidebar_tab.set("threads".to_string())',
                'sidebar_tab.set("brain".to_string())',
                'class: if sidebar_tab.read().as_str() == "threads" { "on" }',
                'role: "group"',
                'aria_pressed: if sidebar_tab.read().as_str() == "threads"',
                'class: if *sidebar_tab.read() == "brain" { "nav-item brain-nav on"',
                'aria_current: if *sidebar_tab.read() == "brain" { "page" }',
                '"brain" => rsx! {',
                'd: "M4.22222 21.9948V18.4451C4.22222 17.1737',
                'class: "brain-view"',
                'class: "brain-map"',
                '"brain-edge active"',
                'onclick: move |_| selected.set(index)',
                'class: "brain-memory-row fact"',
                'class: "brain-skill-row"',
                "fn brain_projects(",
            ],
        )
        and nearby(
            gui,
            'Icon { name: "search" } span { class: "nav-label", "Search" }',
            'class: if *sidebar_tab.read() == "brain" { "nav-item brain-nav on"',
            900,
        )
        and contains_all(
            css,
            [
                ".sidebar .nav-item.on,",
                ".brain-layout {",
                ".brain-map {",
                ".brain-edge {",
                ".brain-edge.active {",
                "animation: brain-edge-flow calc(var(--dur-slow) * 18) linear infinite;",
                ".brain-core-halo {",
                "opacity: .62;",
                ".brain-node.active rect {",
                "@media (max-width: 980px)",
            ],
        ),
        "Brain stays compact below Search while only the selected knowledge edge animates; workspace nodes, keyboard focus, and facts/skills inspector remain interactive",
    )
    require(
        "sidebar navigation and compact drawer contract",
        contains_all(
            gui,
            [
                "let mut sidebar_drawer_open = use_signal(|| false);",
                'id: "primary-sidebar"',
                'class: "logo-btn desktop-sidebar-toggle"',
                'class: "logo-btn mobile-sidebar-toggle"',
                'aria_controls: "primary-sidebar"',
                '"data-tooltip": "New chat"',
                'class: "project-main"',
                'role: "separator"',
                'aria_orientation: "vertical"',
                "let mut session_menu_pos = use_signal",
                'aria_label: "More session actions"',
                'Icon { name: "more" }',
                'class: "sr-only"',
                'Icon { name: "kanban" }',
                'Icon { name: "automation" }',
            ],
        )
        and contains_all(
            css,
            [
                ".sidebar .nav-item.on,",
                ".sidebar .thread-anchor:focus-within .row-actions",
                ".sidebar .project:focus-within .project-add",
                ".sidebar [data-tooltip]::after",
                ".sidebar.drawer-open {",
                ".desktop-sidebar-toggle { display: none; }",
                ".mobile-sidebar-toggle { display: inline-flex; }",
                ".app:has(.sidebar.drawer-open) .sidebar-drawer-backdrop",
                ".sidebar ~ .panel-resizer:focus-visible",
            ],
        ),
        "Sidebar exposes unambiguous active state, keyboard navigation, stable row actions, named icon rail controls, a dismissible compact drawer, and keyboard resizing",
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
                'class: "logo-btn desktop-sidebar-toggle"',
                'aria_label: if *sidebar_collapsed.read()',
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
        "Brain Source Graph",
        "Sidebar Navigation And Compact Drawer",
        "target/gui-visual-qa/sidebar.html",
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
        write_brain_fixture(css)
        write_sidebar_fixture(css)
        fixture_html = read(FIXTURE)
        sidebar_fixture_html = read(SIDEBAR_FIXTURE)
        require(
            "browser fixture exposes the full visual surface",
            contains_all(
                fixture_html,
                [
                    "body > .app { width: 100%; height: auto; min-height: 100vh; overflow: visible; }",
                    "body > .app > .chat { width: min(920px, 100%);",
                ],
            ),
            f"{rel(FIXTURE)} grows with its packed QA states so browser scrolling reaches every component",
        )
        require(
            "browser fixture exposes sidebar interaction states",
            contains_all(
                sidebar_fixture_html,
                [
                    'class="sidebar"',
                    "desktop-sidebar-toggle",
                    "mobile-sidebar-toggle",
                    "sidebar-drawer-backdrop",
                    "data-tooltip=\"New chat\"",
                    "thread active",
                    "row-actions",
                    "project-main",
                    "sidebar.classList.toggle('collapsed')",
                    "sidebar.classList.toggle('drawer-open')",
                ],
            ),
            f"{rel(SIDEBAR_FIXTURE)} supports expanded, icon-rail, compact drawer, active-thread, and stable action-slot inspection",
        )
        print(f"INFO fixture: {rel(FIXTURE)}")
        print(f"INFO brain fixture: {rel(BRAIN_FIXTURE)}")
        print(f"INFO sidebar fixture: {rel(SIDEBAR_FIXTURE)}")
        print(f"INFO compact sidebar fixture: {rel(SIDEBAR_COMPACT_FIXTURE)}")

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
