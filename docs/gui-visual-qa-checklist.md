# Oxide GUI Visual QA Checklist

Use this checklist before calling the GUI Cursor/Synara parity pass production-ready.
It focuses on states that compile tests cannot prove.

## Setup

1. Run `cargo build -p oxide-cli`.
2. Launch `./target/debug/oxide gui` from a real workspace.
3. Keep DevTools console visible if available.
4. Repeat the motion checks once with macOS Reduce Motion enabled.
5. Run `python3 scripts/gui-visual-qa.py` before manual inspection. It checks the source-level visual-state contracts and writes `target/gui-visual-qa/fixture.html` for quick browser inspection.
6. When a Chromium-compatible browser is available, run `python3 scripts/gui-visual-qa.py --runtime`. It opens the fixture through the existing CDP harness, checks required selectors/layout order, captures `target/gui-visual-qa/fixture-cdp.png`, and performs a PNG nonblank sanity check.
7. For a native app/window smoke on macOS, run `python3 scripts/gui-native-visual-smoke.py --no-build --strict` after building. It launches `./target/debug/oxide gui` with `OXIDE_GUI_VISUAL_FIXTURE=streaming`, captures the Oxide window region to `target/gui-native-visual-smoke/oxide-gui-native.png`, and performs PNG pixel sanity. This requires Accessibility and Screen Recording permission for the host terminal/Codex app.

## Streaming And Motion

- Submit a normal prompt and confirm the empty live agent row shows the subtle pre-first-token shimmer.
- While reasoning streams, confirm the `Reasoning` panel appears inside the current turn, above the live answer, and does not jump when the turn finishes.
- Confirm the final `Thought for Ns` row stays in transcript order above the final assistant text.
- Trigger a tool call with streamed arguments, then confirm the activity row first shows `Preparing <tool> ...`, updates as args stream, and settles into the final tool label when execution starts.
- Confirm command/activity rows spin only while running and settle to a check or failure icon after completion or error.

## Reduced Motion

- Enable Reduce Motion and repeat a streaming prompt.
- Confirm the pre-token shimmer row collapses so it does not leave a blank bar between the user message and the working status.
- Confirm shimmer/sweep animations stop, while active status spinner rings still rotate and do not become solid dots.
- Confirm status text still communicates the active state.
- Confirm tab switch and panel transitions are instant and do not overlap content.

## Review Surface

- Ask the agent to edit a small file.
- Confirm the chat `Edited files` card and the inspector `Review` tab show the same changed files.
- Click `Keep` in the chat card and confirm the row shows `Kept`.
- Click `Accept` in the inspector and confirm the row remains visible as `Kept`, instead of disappearing.
- Click `Reject`/`Undo` and confirm the checkpoint rewind event appears and the row is removed or marked reverted.
- Confirm edit/remove status labels such as `editing...`, `Keep`, `Reject`, `Kept`, `Reverted`, and `Undone` use the slot-roll transition without shifting row width.

## Structured UI Artifacts

- Trigger or seed a `render_ui_spec` response and confirm the `Rust-native UI Spec` artifact appears as a native card, not markdown or raw JSON.
- Confirm metrics, table rows, alerts, code blocks, and action buttons stay inside the chat width without horizontal page overflow.
- Click an action button and confirm it copies the action payload instead of navigating or executing arbitrary model-provided code.

## Agents Window

- Open the `Agents` toolbar button and confirm the inspector switches to the local Agents Window.
- Confirm `New Codex` opens a fresh local agent tab and `Split` toggles the split workspace view.
- Confirm each agent session row switches to the correct tab and shows provider/model harness context without cloud-only state.
- Confirm `Review queue`, `Changes`, and `Preview` route to their existing local panes.
- Click `Bugbot review` with local git changes and confirm it submits or queues a `/review (Bugbot)` prompt built from local `git diff`.
- Confirm sub-agent and recent-activity sections render empty states cleanly when nothing is running.

## Tab And Session Replay

- Switch model, harness, and effort in Settings.
- Send a message, close/reopen the session from the sidebar, and confirm provider/model/harness/effort are restored for that session.
- Open another tab with a different provider/model and switch between tabs; confirm each tab keeps its own provider/model/harness/effort.

## Pass Criteria

- `python3 scripts/gui-visual-qa.py` passes.
- Optional runtime gate passes: `python3 scripts/gui-visual-qa.py --runtime`.
- Optional native window smoke passes on macOS: `python3 scripts/gui-native-visual-smoke.py --no-build --strict`.
- No visible text overlap at the default 1280x820 window.
- No activity row remains running after turn error/finish.
- No reasoning panel jumps below the answer on completion.
- Edit/remove slot text freezes cleanly when Reduce Motion is enabled.
- Structured UI artifacts render from the native catalog and never expose arbitrary HTML/JS.
- Agents Window controls stay local-only: no cloud sync/index/background execution dependency is required.
- No stale session opens with the wrong provider/model/harness/effort.
- Console has no new runtime errors.
