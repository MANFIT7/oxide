#!/usr/bin/env python3
"""Record and compare deterministic native Oxide GUI states on macOS."""

from __future__ import annotations

import argparse
import importlib.util
import json
import shutil
import subprocess
import sys
from pathlib import Path


ROOT = Path(__file__).resolve().parents[1]
SMOKE = ROOT / "scripts/gui-native-visual-smoke.py"
SMOKE_IMAGE = ROOT / "target/gui-native-visual-smoke/oxide-gui-native.png"
DEFAULT_RECORD_DIR = ROOT / "target/gui-native-visual-states"
STATES = ("streaming", "review", "verification")


def load_smoke_module():
    spec = importlib.util.spec_from_file_location("oxide_gui_native_smoke", SMOKE)
    if spec is None or spec.loader is None:
        raise RuntimeError(f"unable to load {SMOKE}")
    module = importlib.util.module_from_spec(spec)
    sys.modules[spec.name] = module
    spec.loader.exec_module(module)
    return module


def compare_png(actual: Path, golden: Path) -> tuple[bool, dict[str, float | int]]:
    smoke = load_smoke_module()
    aw, ah, abpp, _, apixels = smoke.decode_png(actual)
    gw, gh, gbpp, _, gpixels = smoke.decode_png(golden)
    if (aw, ah, abpp) != (gw, gh, gbpp):
        return False, {"actual_width": aw, "actual_height": ah, "golden_width": gw, "golden_height": gh}
    channels = min(abpp, 3)
    changed = 0
    total_delta = 0
    samples = aw * ah
    for index in range(samples):
        offset = index * abpp
        delta = max(abs(apixels[offset + channel] - gpixels[offset + channel]) for channel in range(channels))
        total_delta += delta
        if delta > 24:
            changed += 1
    metrics = {
        "width": aw,
        "height": ah,
        "mean_delta": round(total_delta / max(samples, 1), 3),
        "changed_ratio": round(changed / max(samples, 1), 5),
    }
    return metrics["mean_delta"] <= 6.0 and metrics["changed_ratio"] <= 0.08, metrics


def main() -> int:
    parser = argparse.ArgumentParser(description="Record Oxide native GUI visual states and compare optional goldens.")
    parser.add_argument("--no-build", action="store_true", help="reuse target/debug/oxide")
    parser.add_argument("--strict", action="store_true", help="treat native permission skips as failures")
    parser.add_argument("--record-dir", type=Path, default=DEFAULT_RECORD_DIR)
    parser.add_argument("--golden-dir", type=Path, help="compare against PNGs in this directory")
    parser.add_argument("--accept", action="store_true", help="replace goldens with the freshly recorded states")
    args = parser.parse_args()

    args.record_dir.mkdir(parents=True, exist_ok=True)
    results: list[dict[str, object]] = []
    failed = False
    for index, state in enumerate(STATES):
        command = [sys.executable, str(SMOKE), "--fixture-state", state]
        if args.strict:
            command.append("--strict")
        if args.no_build or index > 0:
            command.append("--no-build")
        result = subprocess.run(command, cwd=ROOT, check=False)
        if result.returncode != 0 or not SMOKE_IMAGE.is_file():
            failed = True
            results.append({"state": state, "status": "capture-failed", "exit_code": result.returncode})
            continue
        output = args.record_dir / f"{state}.png"
        shutil.copy2(SMOKE_IMAGE, output)
        item: dict[str, object] = {"state": state, "status": "recorded", "path": str(output)}
        if args.golden_dir is not None:
            golden = args.golden_dir / f"{state}.png"
            if args.accept:
                args.golden_dir.mkdir(parents=True, exist_ok=True)
                shutil.copy2(output, golden)
                item["golden"] = "accepted"
            elif golden.is_file():
                passed, metrics = compare_png(output, golden)
                item["golden"] = "passed" if passed else "failed"
                item["metrics"] = metrics
                failed = failed or not passed
            else:
                item["golden"] = "missing"
                failed = True
        results.append(item)

    manifest = args.record_dir / "manifest.json"
    manifest.write_text(json.dumps({"states": results}, indent=2) + "\n", encoding="utf-8")
    print(f"visual state manifest: {manifest}")
    return 1 if failed else 0


if __name__ == "__main__":
    raise SystemExit(main())
