# oxide-term — native GPU terminal

A standalone native terminal renderer bundled with Oxide. It runs as a sibling
process because a wgpu/Metal surface cannot be embedded directly in the Dioxus
webview.

## Stack

- **portable-pty** — PTY and child-process lifecycle.
- **alacritty_terminal** — VTE emulation and terminal grid.
- **glyphon** / **cosmic-text** — GPU text shaping and atlas rendering.
- **wgpu** — Metal on macOS.
- **winit** — native window and input loop.
- **JetBrainsMono Nerd Font Mono** — bundled terminal font.

`oxide-term` is a member of the root Cargo workspace and is built, tested, and
packaged next to the main `oxide` executable.

## Usage

```sh
cargo run -p oxide-term
cargo run -p oxide-term -- --cwd /path/to/workspace
cargo run -p oxide-term -- --cwd /path/to/workspace PROGRAM ARGS...
```

The GUI launcher resolves only the sibling `oxide-term` binary. It does not run
a workspace-relative or `PATH` fallback binary. Each launch receives the active
workspace explicitly and writes stderr to a unique private log file.

```text
oxide-term [--cwd DIR] [PROGRAM ARGS...]
oxide-term --help
oxide-term --version
```

An explicit invalid `--cwd` is an error rather than silently falling back to a
different directory.

## Current capabilities

- Native window and wgpu/Metal rendering.
- Per-cell foreground/background colors, bold text, and xterm-256 fallback.
- Inverse block cursor.
- Scrollback and mouse-wheel navigation.
- Keyboard input including common Ctrl combinations and navigation keys.
- PTY/grid resize using the measured bundled-font cell advance.
- Bounded PTY reader queue.
- Explicit child exit detection, termination, and reaping.
- GUI launch from the active workspace.

The embedded GUI terminal remains the primary integrated terminal surface. It
supports selection/copy and bracketed paste through WTerm. The native renderer
still lacks mouse selection/copy-paste, underline/strikethrough, and complete
beam/underline cursor-shape rendering.

## Shared terminals

Environment → Terminal can create a managed shared terminal through `tmux` when
`tmux` is installed. Oxide uses a private socket below `~/.oxide/terminal/` and
a stable workspace-specific session name. The UI can copy an attach command for
Terminal.app, Ghostty, iTerm, or another external terminal.

Agent access is intentionally not a raw writable PTY connection:

- **Share output** inserts a bounded, read-only terminal snapshot into the chat
  composer for user review.
- The user must review the snapshot for secrets and explicitly send it.
- Shell tools continue to run through Oxide approval, sandbox, timeout, and
  bounded-output handling.
- External terminals share input only through the explicit managed tmux session.

This keeps interactive user shells separate from automatic agent execution.
