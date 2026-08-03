# Oxide GUI Visual QA Checklist

Use this checklist before calling the GUI Cursor/Synara parity pass production-ready.
It focuses on states that compile tests cannot prove.

## Setup

1. Run `cargo build -p oxide-cli`.
2. Launch `./target/debug/oxide gui` from a real workspace.
3. Keep DevTools console visible if available.
4. Repeat the motion checks once with macOS Reduce Motion enabled.
5. Run `python3 scripts/gui-visual-qa.py` before manual inspection. It checks the source-level visual-state contracts and writes `target/gui-visual-qa/fixture.html` plus the interactive `target/gui-visual-qa/brain.html` source-graph preview.
6. When a Chromium-compatible browser is available, run `python3 scripts/gui-visual-qa.py --runtime`. It opens the fixture through the existing CDP harness, checks required selectors/layout order, captures `target/gui-visual-qa/fixture-cdp.png`, and performs a PNG nonblank sanity check.
7. For a native app/window smoke on macOS, run `python3 scripts/gui-native-visual-smoke.py --no-build --strict` after building. It launches `./target/debug/oxide gui` with `OXIDE_GUI_VISUAL_FIXTURE=streaming`, captures the Oxide window region to `target/gui-native-visual-smoke/oxide-gui-native.png`, and performs PNG pixel sanity. This requires Accessibility and Screen Recording permission for the host terminal/Codex app.
8. Record all deterministic native states (`streaming`, `review`, `verification`, `board`, and `settings`) with `python3 scripts/gui-native-visual-record.py --no-build`. Add `--golden-dir docs/gui-goldens --accept` to establish baselines, then rerun without `--accept` to enforce the pixel-difference thresholds.

## Streaming And Motion

- Submit a normal prompt and confirm the empty live agent row shows a compact Braille spinner beside the subtle `Thinking…` shimmer.
- Confirm the streaming rail breathes beside the active assistant row without restarting or flickering as each text chunk arrives.
- Confirm the first token rises in once, while subsequent tokens remain stable, the six newest words use only brief entrance motion, and the live tail keeps full contrast without a persistent fade mask.
- While reasoning streams, confirm the `Reasoning` panel appears inside the current turn, above the live answer, updates without flicker or scroll hitching, and cross-fades smoothly into `Thought for Ns` when the turn finishes.
- Confirm the final `Thought for Ns` row stays in transcript order above the final assistant text.
- Trigger a tool call with streamed arguments, then confirm the activity row shows `Preparing` with a compact semantic command/path/query preview (never raw JSON), updates as args stream, and settles into the final tool label when execution starts.
- Confirm command/activity rows keep one fixed status slot: the Braille spinner cross-fades to a check or failure icon without nudging the label horizontally.
- Confirm the agent and tool Braille sequence advances one fixed-width cell at a time without rotating, resizing, or shifting adjacent text.
- While a command streams output, confirm its card opens automatically, stays near four lines tall, follows the newest line, and fades the old-output edge without moving the outer transcript sideways.
- Expand and collapse Reasoning or a completed tool with output; confirm the real content height tweens smoothly, the caret stays synchronized, and a transcript already pinned to the bottom follows the tween without recapturing manual scrollback.
- In both the main transcript and split pane, scroll upward during a stream; confirm new tokens do not yank the reader back down, `Jump to latest` appears, and clicking it restores tail-follow for only that transcript.
- Stream an answer containing a very long unbroken code line (e.g. ask for a one-line shell pipeline over 200 chars); confirm the transcript never pans sideways — the code block scrolls internally while prose wraps.

## Compact Orchestration Surfaces

- Start a turn that produces Subagents, Tasks, and several file edits at the same time.
- Confirm each surface occupies one summary row by default and the three rows do not cover most of the transcript.
- Confirm Subagents and Tasks show their current item in an ellipsized preview and disclose bounded, internally scrollable detail when clicked.
- Confirm `Changing N files` stays one row and its branch button opens full file detail in the Environment `Diff` tab.
- Confirm collapsing either disclosure returns it to approximately 40px without losing task or subagent state.

## Host Reduce Motion

- Enable macOS Reduce Motion and repeat a streaming prompt.
- Confirm shimmer, pulse, streaming-rail breathing, entrance motion, disclosure tweening, smooth scrolling, and hover/press scaling stop.
- Confirm reasoning, tool, edit, and background-process states remain legible through static text, icons, and color instead of motion alone.
- Confirm only the slow stepped indicator for actively running work continues; settled rows and decorative surfaces must remain still.
- Confirm opening panels, dialogs, disclosures, and tabs remains immediate and does not hide content or change the final layout.
- Disable Reduce Motion and confirm the normal motion cadence returns without shifting surrounding content.

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
- Enable Reduce Motion and confirm toast entrance/lifecycle animation stops while its content, action, and dismiss control remain readable and usable.

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

## Browser Ownership And Annotations

- Disable headless browser automation in Settings, start a browser tool, and confirm the Browser pane shows the live URL, `Agent controlled`, `Take over`, and `Cancel browser`.
- Click `Take over` while navigation or a click is running; confirm only that browser operation pauses, the Chromium window comes forward, and the agent turn/composer remains active.
- Click `Resume agent`; confirm the existing session continues instead of opening a fresh profile. Click `Cancel browser`; confirm Chromium closes while the agent turn continues.
- Repeat with headless automation enabled; confirm Take over is disabled, the surface says `Headless`, and Resume/Cancel remain truthful.
- In a proxied local preview, enable `Annotate`, select multiple elements, and confirm stable numbered outlines plus one multi-element context chip appear.
- Navigate or reload the preview; confirm selectors are restored, missing targets are reported, and the annotation numbers/context remain intact. `Clear` must remove every outline and `Add to prompt` must reattach retained annotations after a send.
- Switch repeatedly among Diff, Terminal, Browser, and Files; confirm the preview URL/scroll position, expanded diffs, file inspector state, and PTY processes survive tab switches without remount flashes.

## Provider Catalog

- Open the model picker and confirm only current production choices are shown for ChatGPT, Codex, and Claude; legacy aliases must remain loadable from an old config but not appear as primary choices.
- Confirm provider readiness is visible. A missing CLI/login must disable selection with an actionable diagnostic instead of silently saving an unusable provider.
- Toggle Fast and switch providers; confirm default/fast models come from the catalog and effort is clamped to the chosen model's supported ceiling.

## Board And Compact Layout

- Open Board with the Environment dock both open and closed at 1280×820, 1024×768, and the narrowest supported window width.
- Confirm all four lanes retain a usable minimum width and scroll horizontally instead of compressing labels, cards, or actions.
- Confirm the Board title, new-task field, `Run To-Do`, and `Sync issues` wrap without clipping.
- With no cards, confirm every lane shows its count and a specific empty-state message instead of a blank slab.
- Add a task and move it through the normal run/review flow; confirm the card settles once without replaying motion on unrelated renders.
- Open the Environment dock below 900px and confirm it behaves as a deliberate right-side drawer with a visible close control, not an accidental overlap.

## Brain Source Graph

- Open `Brain` from the compact navigation item directly below `Search`; confirm the `Threads | Workspace` switcher remains two options wide and the chat/TUI surface yields to the graph without leaving overlapping message-trail or Environment controls.
- Confirm every known project folder appears in the inspector and up to eight workspace nodes remain readable around the central Oxide Memory node.
- Confirm only the selected knowledge edge uses restrained motion; the central halo and unselected graph remain static so node labels never shift or trigger full-map repainting.
- Click workspace nodes and navigate them with Tab plus Enter/Space; confirm the active edge/node updates and the facts/skills inspector follows the selected workspace.
- Open a learned skill and confirm its `.oxide/memory/skills/*.md` file opens in the editor, then return to Brain without losing the graph state.
- Check a workspace with no memory and confirm it shows a zero-memory node and clear empty inspector instead of disappearing.
- Open `target/gui-visual-qa/brain.html` in a browser and confirm the Sourcegraph-style graph remains usable at 1280×820, below 980px, and in light/dark themes.

## Settings Navigation

- Open Settings at desktop width and confirm all nine destinations are visible in the left rail without scrolling horizontally.
- At compact width, confirm the destination row scrolls horizontally with a visible scrollbar and `Sessions`/`Updates` remain reachable.
- Switch between Model, Automations, Sessions, and Updates; confirm the modal body keeps its position and does not flash to a backdrop-only frame.
- Confirm long forms scroll inside the body while the title, navigation, and Save/Cancel actions remain stable.

## Sidebar Navigation And Compact Drawer

- Open `target/gui-visual-qa/sidebar.html` and `sidebar-compact.html` after running the static QA script; exercise expanded, collapsed, and sub-900px drawer states without rebuilding the native app.
- Confirm Threads, Workspace, Brain, and Board never show contradictory active states; the current thread must remain visibly stronger than hover.
- Traverse projects, current/open sessions, pinned sessions, disclosure controls, and row actions using only Tab/Shift-Tab/Enter/Space.
- Confirm Pin, Archive, Add, and Hide actions become visible on keyboard focus before activation and do not shift the thread title.
- Collapse the desktop sidebar and verify every icon-only action has a delayed tooltip, accessible name, stable 16px optical slot, and no clipped MCP logo.
- Resize below 900px, open the sidebar from the logo rail, select a session, and confirm the drawer closes while the main view remains in place.
- Reopen the compact drawer, then close it with Escape and the backdrop. Confirm Projects, Chats, Usage, Update, and Settings remain reachable.
- Resize the desktop sidebar with Left/Right/Home/End on the separator and confirm the announced value remains between 170px and 440px.
- Open a session menu near the bottom of the project list and confirm it stays inside the viewport instead of clipping in the scroller.

## Theme Contrast And Keyboard

- Repeat the Board, Settings, sidebar, and command palette checks in dark, light, and system themes.
- Confirm tertiary labels, counts, timestamps, and empty-state copy remain readable at normal brightness; column boundaries must not disappear in light mode.
- Navigate from the sidebar logo through Board, Settings, modal tabs, fields, and close actions using only Tab/Shift-Tab/Enter/Space.
- Confirm every focused control has a visible accent ring and the sidebar logo behaves as a button.
- With a screen reader or accessibility inspector, confirm Settings, Skills, and MCP surfaces expose dialog semantics and icon-only destructive controls have names.

## Tab And Session Replay

- Switch model, harness, and effort in Settings.
- Send a message, close/reopen the session from the sidebar, and confirm provider/model/harness/effort are restored for that session.
- Open another tab with a different provider/model and switch between tabs; confirm each tab keeps its own provider/model/harness/effort.

## Pass Criteria

- `python3 scripts/gui-visual-qa.py` passes.
- Optional runtime gate passes: `python3 scripts/gui-visual-qa.py --runtime`.
- Optional native window smoke passes on macOS: `python3 scripts/gui-native-visual-smoke.py --no-build --strict`.
- Native state recorder captures `streaming`, `review`, `verification`, `board`, and `settings` with a manifest.
- No visible text overlap at the default 1280x820 window.
- No activity row remains running after turn error/finish.
- Streaming and tool status motion stays compositor-only and never restarts on every token update.
- No reasoning panel jumps below the answer on completion.
- Edit/remove slot text settles immediately and stays legible when Reduce Motion is enabled.
- Browser takeover/resume/cancel never interrupts the owning agent turn, and headless sessions never pretend to be user-controllable.
- Numbered preview annotations survive reload/navigation and can be explicitly reattached or cleared.
- Diff, Terminal, Browser, and Files panes preserve state across dock-tab switches.
- Model choices and readiness are sourced from the provider catalog; legacy aliases are compatibility-only.
- Structured UI artifacts render from the native catalog and never expose arbitrary HTML/JS.
- Toasts use the top-center compact/expanded notification surface with semantic icons and explicit dismissal.
- Agents Window controls stay local-only: no cloud sync/index/background execution dependency is required.
- Local Servers controls work with local dev-server processes only.
- No stale session opens with the wrong provider/model/harness/effort.
- Console has no new runtime errors.
