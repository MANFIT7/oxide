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
8. Record all deterministic native states with `python3 scripts/gui-native-visual-record.py --no-build`. Add `--golden-dir docs/gui-goldens --accept` to establish baselines, then rerun without `--accept` to enforce the pixel-difference thresholds.

## Streaming And Motion

- Submit a normal prompt and confirm the empty live agent row shows the subtle pre-first-token shimmer.
- Confirm the streaming rail breathes beside the active assistant row without restarting or flickering as each text chunk arrives.
- Confirm the first token rises in once, while subsequent tokens remain stable and only the live tail uses the soft readability fade.
- While reasoning streams, confirm the `Reasoning` panel appears inside the current turn, above the live answer, and does not jump when the turn finishes.
- Confirm the final `Thought for Ns` row stays in transcript order above the final assistant text.
- Trigger a tool call with streamed arguments, then confirm the activity row first shows `Preparing <tool> ...`, updates as args stream, and settles into the final tool label when execution starts.
- Confirm command/activity rows keep one fixed status slot: the spinner cross-fades to a check or failure icon without nudging the label horizontally.
- Expand and collapse a tool with output; confirm the content and caret transition smoothly without shifting neighboring transcript rows.

## Reduced Motion

- Enable Reduce Motion and repeat a streaming prompt.
- Confirm the pre-token shimmer row collapses so it does not leave a blank bar between the user message and the working status.
- Confirm shimmer/sweep animations stop, while active status spinner rings still rotate and do not become solid dots.
- Confirm the streaming rail becomes static, tool halos stop pulsing, and disclosure content remains immediately readable.
- Confirm status text still communicates the active state.
- Confirm tab switch and panel transitions are instant and do not overlap content.

## Review Surface

- Ask the agent to edit a small file.
- Confirm the chat `Edited files` card and the inspector `Review` tab show the same changed files.
- Click `Keep` in the chat card and confirm the row shows `Kept`.
- Click `Accept` in the inspector and confirm the row remains visible as `Kept`, instead of disappearing.
- Click `Reject`/`Undo` and confirm the checkpoint rewind event appears and the row is removed or marked reverted.
- Confirm edit/remove status labels such as `editing...`, `Keep`, `Reject`, `Kept`, `Reverted`, and `Undone` use the slot-roll transition without shifting row width.
- Expand a changed file, click `Comment` on two hunks, and confirm `Fix feedback` submits both exact hunk locations and notes in one turn.

## Verification Center

- Trigger an edit with auto-verify enabled and confirm `Verify` shows started then passed/failed evidence with the exact command.
- Confirm a timeout, spawn error, unsupported project, and unrelated pre-existing failure are labeled distinctly instead of appearing as passed.

## Structured UI Artifacts

- Trigger or seed a `render_ui_spec` response and confirm the `Rust-native UI Spec` artifact appears as a native card, not markdown or raw JSON.
- Confirm metrics, table rows, alerts, code blocks, and action buttons stay inside the chat width without horizontal page overflow.
- Click an action button and confirm it copies the action payload instead of navigating or executing arbitrary model-provided code.

## Toast Notifications

- Trigger a compact toast such as `Copied` or `Changes committed`; confirm it appears at the top center with an icon and explicit dismiss button.
- Archive a chat or project and confirm the expanded toast keeps its `Undo` action on a separate row without text overlap.
- Stack two or more toasts and confirm they remain readable within the window width in dark, light, and system themes.
- Confirm clicking the toast body does not dismiss it; only the dismiss button, action, or timeout should close it.
- Enable Reduce Motion and confirm toast content remains readable without entrance/lifecycle animation.

## Agents Window

- Open the `Agents` toolbar button and confirm the inspector switches to the local Agents Window.
- Confirm `New Codex` opens a fresh local agent tab and `Split` toggles the split workspace view.
- Confirm each agent session row switches to the correct tab and shows provider/model harness context without cloud-only state.
- Confirm `Review queue`, `Changes`, and `Preview` route to their existing local panes.
- Click `Bugbot review` with local git changes and confirm it submits or queues a `/review (Bugbot)` prompt built from local `git diff`.
- Confirm sub-agent and recent-activity sections render empty states cleanly when nothing is running.

## Local Servers

- Start a local dev server in the workspace, then confirm the Environment card `Local Servers` section shows its process name, `localhost:<port>`, and a green running dot after refresh.
- Confirm internal agent processes such as `agent-*` do not appear in the Local Servers list or count.
- Click the server row and confirm it opens the right Preview target through the local preview proxy.
- Click `Open dev server` and confirm it opens the same localhost URL in the system browser.
- Click `Stop dev server` and confirm the process exits and the server list refreshes without leaving a stale running row.

## Tab And Session Replay

- Switch model, harness, and effort in Settings.
- Send a message, close/reopen the session from the sidebar, and confirm provider/model/harness/effort are restored for that session.
- Open another tab with a different provider/model and switch between tabs; confirm each tab keeps its own provider/model/harness/effort.

## Pass Criteria

- `python3 scripts/gui-visual-qa.py` passes.
- Optional runtime gate passes: `python3 scripts/gui-visual-qa.py --runtime`.
- Optional native window smoke passes on macOS: `python3 scripts/gui-native-visual-smoke.py --no-build --strict`.
- Native state recorder captures `streaming`, `review`, and `verification` with a manifest.
- No visible text overlap at the default 1280x820 window.
- No activity row remains running after turn error/finish.
- Streaming and tool status motion stays compositor-only and never restarts on every token update.
- No reasoning panel jumps below the answer on completion.
- Edit/remove slot text freezes cleanly when Reduce Motion is enabled.
- Structured UI artifacts render from the native catalog and never expose arbitrary HTML/JS.
- Toasts use the top-center compact/expanded notification surface with semantic icons and explicit dismissal.
- Agents Window controls stay local-only: no cloud sync/index/background execution dependency is required.
- Local Servers controls work with local dev-server processes only.
- No stale session opens with the wrong provider/model/harness/effort.
- Console has no new runtime errors.
