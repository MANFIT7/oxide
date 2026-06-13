#!/usr/bin/env bash
# Build the dev binary and sign it with the stable "Oxide Dev" identity.
#
# Why: macOS TCC (Files & Folders / "removable volume" access) keys a grant to
# the binary's code-signing identity. An ad-hoc / unsigned binary gets a fresh
# identity on EVERY `cargo build`, so macOS treats each rebuild as a new app and
# re-prompts ("oxide-bin would like to access files on a removable volume").
# Signing with the stable self-signed "Oxide Dev" cert + the same identifier the
# release app uses (com.oxide.desktop) gives a constant designated requirement,
# so the grant sticks across rebuilds — and is shared with the installed .app.
#
# Usage:
#   scripts/dev-build.sh            # debug build + sign  → target/debug/oxide
#   scripts/dev-build.sh --release  # release build + sign → target/release/oxide
#
# First-time setup: run scripts/make-cert.sh once to create the "Oxide Dev"
# identity, then Allow the prompt a single time — it won't ask again.
set -euo pipefail
cd "$(dirname "$0")/.."

PROFILE="debug"
CARGO_FLAGS=()
if [[ "${1:-}" == "--release" ]]; then
  PROFILE="release"
  CARGO_FLAGS+=(--release)
fi

echo "▶ building oxide ($PROFILE)…"
cargo build -p oxide-cli "${CARGO_FLAGS[@]}"

BIN="target/$PROFILE/oxide"
SIGN_ID="$(security find-identity -v -p codesigning 2>/dev/null | grep -m1 'Oxide Dev' | awk '{print $2}' || true)"

if [[ -n "$SIGN_ID" ]]; then
  echo "▶ signing $BIN with Oxide Dev (stable TCC identity)…"
  codesign --force --sign "$SIGN_ID" --identifier com.oxide.desktop "$BIN"
  codesign -dvv "$BIN" 2>&1 | grep -E 'Identifier=|Authority=' || true
  echo "✓ signed → run it: $BIN gui"
else
  echo "⚠ no 'Oxide Dev' identity in keychain — run scripts/make-cert.sh first."
  echo "  Binary left ad-hoc; macOS will keep re-prompting on each rebuild."
  exit 1
fi
