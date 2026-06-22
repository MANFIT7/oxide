#!/usr/bin/env python3
"""Optional native-window smoke for the Oxide Dioxus GUI on macOS.

The normal visual QA script verifies source contracts and a browser-rendered
fixture. This script launches the real `oxide gui` binary, finds the native
window through System Events, captures that window region with screencapture,
and performs a small PNG pixel sanity check.

It is intentionally optional because macOS may require Accessibility and Screen
Recording permissions for the terminal/Codex host that runs it.
"""

from __future__ import annotations

import argparse
import os
import platform
import shutil
import struct
import subprocess
import sys
import time
import zlib
from dataclasses import dataclass
from pathlib import Path


ROOT = Path(__file__).resolve().parents[1]
OUT_DIR = ROOT / "target/gui-native-visual-smoke"
WORKSPACE = OUT_DIR / "workspace"
BINARY = ROOT / "target/debug/oxide"
SCREENSHOT = OUT_DIR / "oxide-gui-native.png"
FULLSCREEN_DIAGNOSTIC = OUT_DIR / "oxide-gui-fullscreen-diagnostic.png"
LOG = OUT_DIR / "oxide-gui.log"


@dataclass(frozen=True)
class WindowInfo:
    process_name: str
    title: str
    x: int
    y: int
    width: int
    height: int


@dataclass(frozen=True)
class PngStats:
    width: int
    height: int
    sampled: int
    min_luma: int
    max_luma: int
    bright: int
    nonblack: int

    @property
    def contrast(self) -> int:
        return self.max_luma - self.min_luma


def report(status: str, name: str, detail: str) -> None:
    print(f"{status} {name}: {detail}")


def skip(message: str, strict: bool) -> int:
    report("SKIP", "native GUI visual smoke", message)
    return 1 if strict else 0


def fail(message: str) -> int:
    report("FAIL", "native GUI visual smoke", message)
    return 1


def run(
    command: list[str],
    *,
    cwd: Path | None = None,
    timeout: float = 30.0,
) -> subprocess.CompletedProcess[str]:
    return subprocess.run(command, cwd=cwd, capture_output=True, text=True, timeout=timeout)


def ensure_workspace() -> None:
    OUT_DIR.mkdir(parents=True, exist_ok=True)
    WORKSPACE.mkdir(parents=True, exist_ok=True)
    (WORKSPACE / "README.md").write_text(
        "# Oxide Native Visual Smoke\n\nTemporary workspace for GUI launch QA.\n",
        encoding="utf-8",
    )


def ensure_binary(no_build: bool) -> int:
    if no_build:
        return 0 if BINARY.is_file() else fail(f"missing binary: {BINARY}")
    result = run(["cargo", "build", "-p", "oxide-cli"], cwd=ROOT, timeout=240.0)
    if result.returncode == 0 and BINARY.is_file():
        report("PASS", "build", "cargo build -p oxide-cli")
        return 0
    detail = (result.stderr or result.stdout or "cargo build failed").strip()
    return fail(detail[-1200:])


def apple_script_window_query() -> str:
    return r'''
set tabChar to ASCII character 9
tell application "System Events"
    repeat with proc in every process
        set procName to name of proc as text
        if procName is "oxide" or procName is "Oxide" or procName contains "oxide" then
            tell proc
                repeat with win in windows
                    try
                        set winTitle to name of win as text
                        set winPos to position of win
                        set winSize to size of win
                        set px to item 1 of winPos as integer
                        set py to item 2 of winPos as integer
                        set ww to item 1 of winSize as integer
                        set wh to item 2 of winSize as integer
                        if ww >= 320 and wh >= 240 then
                            set frontmost to true
                            return procName & tabChar & winTitle & tabChar & px & tabChar & py & tabChar & ww & tabChar & wh
                        end if
                    end try
                end repeat
            end tell
        end if
    end repeat
end tell
return ""
'''


def parse_window_info(line: str) -> WindowInfo | None:
    parts = line.strip().split("\t")
    if len(parts) != 6:
        return None
    try:
        x, y, width, height = [int(part) for part in parts[2:]]
    except ValueError:
        return None
    if width < 320 or height < 240:
        return None
    return WindowInfo(parts[0], parts[1], x, y, width, height)


def wait_for_window(proc: subprocess.Popen[object], timeout: float) -> tuple[WindowInfo | None, str]:
    deadline = time.monotonic() + timeout
    last_detail = ""
    script = apple_script_window_query()
    while time.monotonic() < deadline:
        if proc.poll() is not None:
            return None, f"oxide gui exited early with code {proc.returncode}; see {LOG}"
        try:
            result = run(["osascript", "-e", script], timeout=5.0)
        except subprocess.TimeoutExpired:
            last_detail = "osascript timed out while querying windows"
            time.sleep(0.5)
            continue
        detail = (result.stderr or result.stdout).strip()
        if result.returncode == 0:
            info = parse_window_info(result.stdout)
            if info is not None:
                return info, ""
            last_detail = detail or "Oxide process/window not visible yet"
        else:
            last_detail = detail or f"osascript exited with {result.returncode}"
        time.sleep(0.5)
    return None, last_detail


def capture_window(info: WindowInfo, path: Path) -> tuple[bool, str]:
    path.parent.mkdir(parents=True, exist_ok=True)
    region = f"{info.x},{info.y},{info.width},{info.height}"
    result = run(["screencapture", "-x", "-R", region, str(path)], timeout=20.0)
    if result.returncode == 0 and path.is_file():
        return True, region
    detail = (result.stderr or result.stdout or f"screencapture exited with {result.returncode}").strip()
    return False, detail


def capture_fullscreen(path: Path) -> tuple[bool, str]:
    result = run(["screencapture", "-x", str(path)], timeout=20.0)
    if result.returncode == 0 and path.is_file():
        return True, str(path)
    detail = (result.stderr or result.stdout or f"screencapture exited with {result.returncode}").strip()
    return False, detail


def paeth(left: int, up: int, upper_left: int) -> int:
    estimate = left + up - upper_left
    left_distance = abs(estimate - left)
    up_distance = abs(estimate - up)
    upper_left_distance = abs(estimate - upper_left)
    if left_distance <= up_distance and left_distance <= upper_left_distance:
        return left
    if up_distance <= upper_left_distance:
        return up
    return upper_left


def decode_png(path: Path) -> tuple[int, int, int, int, bytes]:
    data = path.read_bytes()
    if not data.startswith(b"\x89PNG\r\n\x1a\n"):
        raise ValueError("not a PNG file")
    offset = 8
    width = height = bit_depth = color_type = 0
    idat = bytearray()
    while offset + 8 <= len(data):
        length = struct.unpack(">I", data[offset : offset + 4])[0]
        chunk_type = data[offset + 4 : offset + 8]
        chunk = data[offset + 8 : offset + 8 + length]
        offset += 12 + length
        if chunk_type == b"IHDR":
            width, height, bit_depth, color_type = struct.unpack(">IIBB", chunk[:10])
        elif chunk_type == b"IDAT":
            idat.extend(chunk)
        elif chunk_type == b"IEND":
            break
    if bit_depth != 8 or color_type not in {0, 2, 4, 6}:
        raise ValueError(f"unsupported PNG format: bit_depth={bit_depth} color_type={color_type}")
    bpp = {0: 1, 2: 3, 4: 2, 6: 4}[color_type]
    row_len = width * bpp
    raw = zlib.decompress(bytes(idat))
    decoded = bytearray()
    previous = bytearray(row_len)
    cursor = 0
    for _ in range(height):
        filter_type = raw[cursor]
        cursor += 1
        row = bytearray(raw[cursor : cursor + row_len])
        cursor += row_len
        for i, value in enumerate(row):
            left = row[i - bpp] if i >= bpp else 0
            up = previous[i]
            upper_left = previous[i - bpp] if i >= bpp else 0
            if filter_type == 1:
                row[i] = (value + left) & 0xFF
            elif filter_type == 2:
                row[i] = (value + up) & 0xFF
            elif filter_type == 3:
                row[i] = (value + ((left + up) // 2)) & 0xFF
            elif filter_type == 4:
                row[i] = (value + paeth(left, up, upper_left)) & 0xFF
            elif filter_type != 0:
                raise ValueError(f"unsupported PNG filter: {filter_type}")
        decoded.extend(row)
        previous = row
    return width, height, bpp, color_type, bytes(decoded)


def png_stats(path: Path) -> PngStats:
    width, height, bpp, color_type, pixels = decode_png(path)
    step_x = max(1, width // 220)
    step_y = max(1, height // 220)
    sampled = 0
    min_luma = 255
    max_luma = 0
    bright = 0
    nonblack = 0
    for y in range(0, height, step_y):
        for x in range(0, width, step_x):
            idx = (y * width + x) * bpp
            if color_type == 0:
                r = g = b = pixels[idx]
            elif color_type == 4:
                r = g = b = pixels[idx]
            else:
                r, g, b = pixels[idx], pixels[idx + 1], pixels[idx + 2]
            luma = (299 * r + 587 * g + 114 * b) // 1000
            sampled += 1
            min_luma = min(min_luma, luma)
            max_luma = max(max_luma, luma)
            if luma > 96:
                bright += 1
            if luma > 20:
                nonblack += 1
    return PngStats(width, height, sampled, min_luma, max_luma, bright, nonblack)


def validate_png(path: Path) -> tuple[bool, str]:
    stats = png_stats(path)
    issues: list[str] = []
    if stats.width < 360 or stats.height < 240:
        issues.append(f"image too small: {stats.width}x{stats.height}")
    if stats.sampled < 500:
        issues.append(f"too few samples: {stats.sampled}")
    if stats.contrast < 24:
        issues.append(f"low contrast: {stats.contrast}")
    if stats.nonblack < 20 or stats.bright < 4:
        issues.append(f"too dark: bright={stats.bright} nonblack={stats.nonblack}")
    detail = (
        f"{path.relative_to(ROOT)} {stats.width}x{stats.height} "
        f"contrast={stats.contrast} bright={stats.bright} nonblack={stats.nonblack}"
    )
    if issues:
        return False, f"{detail}; {'; '.join(issues)}"
    return True, detail


def terminate_process(proc: subprocess.Popen[object]) -> None:
    if proc.poll() is not None:
        return
    proc.terminate()
    try:
        proc.wait(timeout=5)
    except subprocess.TimeoutExpired:
        proc.kill()
        proc.wait(timeout=5)


def main() -> int:
    parser = argparse.ArgumentParser(description="Launch and screenshot the native Oxide GUI window.")
    parser.add_argument("--no-build", action="store_true", help="use the existing target/debug/oxide binary")
    parser.add_argument("--strict", action="store_true", help="treat permission/platform skips as failures")
    parser.add_argument(
        "--fixture-state",
        choices=["streaming", "off"],
        default="streaming",
        help="seed deterministic GUI visual state before capture",
    )
    parser.add_argument(
        "--allow-fullscreen-fallback",
        action="store_true",
        help="allow a nonblank fullscreen diagnostic to pass when window bounds are unavailable",
    )
    parser.add_argument("--timeout", type=float, default=35.0, help="seconds to wait for the native GUI window")
    args = parser.parse_args()

    if platform.system() != "Darwin":
        return skip("requires macOS screencapture/System Events", args.strict)
    if shutil.which("screencapture") is None or shutil.which("osascript") is None:
        return skip("requires screencapture and osascript", args.strict)

    ensure_workspace()
    build_status = ensure_binary(args.no_build)
    if build_status != 0:
        return build_status

    env = os.environ.copy()
    env.setdefault("RUST_LOG", "warn")
    if args.fixture_state != "off":
        env["OXIDE_GUI_VISUAL_FIXTURE"] = args.fixture_state
    with LOG.open("w", encoding="utf-8") as log:
        proc = subprocess.Popen(
            [str(BINARY), "gui"],
            cwd=WORKSPACE,
            env=env,
            stdout=log,
            stderr=log,
            stdin=subprocess.DEVNULL,
        )
        try:
            info, detail = wait_for_window(proc, args.timeout)
            if info is None:
                captured, capture_detail = capture_fullscreen(FULLSCREEN_DIAGNOSTIC)
                if captured:
                    ok, stats_detail = validate_png(FULLSCREEN_DIAGNOSTIC)
                    if ok and args.allow_fullscreen_fallback:
                        report(
                            "PASS",
                            "fullscreen fallback",
                            f"{stats_detail}; window bounds unavailable: {detail or capture_detail}",
                        )
                        return 0
                    report(
                        "INFO",
                        "fullscreen diagnostic",
                        f"{stats_detail}; window bounds unavailable: {detail or capture_detail}",
                    )
                message = (
                    "could not read Oxide window bounds via System Events; "
                    "grant Accessibility permission to the terminal/Codex host and rerun"
                )
                if detail:
                    message = f"{message}; last error: {detail}"
                return skip(message, args.strict)

            captured, capture_detail = capture_window(info, SCREENSHOT)
            if not captured:
                return fail(f"window capture failed for {info}: {capture_detail}")
            ok, stats_detail = validate_png(SCREENSHOT)
            if not ok:
                return fail(stats_detail)
            report(
                "PASS",
                "native GUI visual smoke",
                f"{stats_detail}; fixture={args.fixture_state}; process={info.process_name!r} title={info.title!r} region={info.x},{info.y},{info.width},{info.height}",
            )
            return 0
        finally:
            terminate_process(proc)


if __name__ == "__main__":
    sys.exit(main())
