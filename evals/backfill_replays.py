# /// script
# requires-python = ">=3.11"
# ///
"""Refilm and re-render rounds recorded before the current artifacts existed.

Reruns a round's program.json to film it and re-render park.png cropped. The
rerun is checked: the video needs the excitement to match the round's own (a clip
shows motion, which is what an inert booster changed), the picture only needs the
program to build in full (a still shows track). Neither is written otherwise.

Usage:
  uv run evals/backfill_replays.py                        # best round of every run
  uv run evals/backfill_replays.py 20260725-opus5-opennote
  uv run evals/backfill_replays.py --all --no-replay      # pictures only
"""

from __future__ import annotations

import argparse
import json
import subprocess
import sys
import time
from pathlib import Path

EVALS_DIR = Path(__file__).resolve().parent
REPO = EVALS_DIR.parent
RUNS_DIR = EVALS_DIR / "runs"
CLI = REPO / "build" / "coasterbench-cli"
DEFAULT_SCENARIO = "~/rct2-assets/Scenarios/Build your own Six Flags Park.SC6"
DEFAULT_RCT2_DATA = "~/rct2-assets"


def penalised_excitement(report: dict, grace: float) -> float:
    """Round score, by the same rule as driver.py and coaster-bench."""
    rides = report.get("rides") or []
    raw = max((r.get("excitement") or 0.0 for r in rides), default=0.0)
    similarity = (report.get("similarity") or {}).get("similarity") or 0.0
    if similarity <= grace:
        return raw
    return raw * max((1.0 - similarity) / (1.0 - grace) if grace < 1.0 else 0.0, 0.0)


def rounds_of(model_dir: Path) -> list[Path]:
    return sorted(
        (d for d in model_dir.iterdir() if d.is_dir() and d.name.startswith("round_")),
        key=lambda d: int(d.name.split("_")[1]),
    )


def pick_rounds(run_dir: Path, every_round: bool) -> list[Path]:
    """The round directories to film in one run."""
    run_json = run_dir / "run.json"
    grace = 0.5
    if run_json.exists():
        grace = json.loads(run_json.read_text()).get("similarity_grace", 0.5)
    picked: list[Path] = []
    for model_dir in sorted(p for p in run_dir.iterdir() if p.is_dir()):
        candidates = [d for d in rounds_of(model_dir) if (d / "program.json").exists()]
        if every_round:
            picked.extend(candidates)
            continue
        scored = []
        for round_dir in candidates:
            report = round_dir / "report.json"
            if not report.exists():
                continue
            scored.append(
                (penalised_excitement(json.loads(report.read_text()), grace), round_dir)
            )
        best = max(scored, default=None)
        if best is not None and best[0] > 0:
            picked.append(best[1])
    return picked


def built_in_full(report: dict) -> bool:
    """Every piece went down, so the track on screen is the round's."""
    program = report.get("program") or {}
    total = program.get("pieces_total") or 0
    return bool(program.get("ok")) and total > 0 and program.get("pieces_placed") == total


def raw_excitement(report: dict) -> float:
    rides = report.get("rides") or []
    return max((r.get("excitement") or 0.0 for r in rides), default=0.0)


def rerun(
    round_dir: Path,
    scenario: str,
    rct2_data: str,
    ticks: int,
    seconds: int,
    capture: bool,
    replay: bool,
) -> tuple[bool, bool]:
    """Rebuilds the round's ride. Returns what was written: (picture, video)."""
    out = round_dir / "replay.mp4"
    # These live and die with the video, or a rejected round keeps a picture of
    # a ride it never had.
    poster = round_dir / "replay.png"
    sidecar = round_dir / "replay.json"
    check = round_dir / "replay-check.json"
    shot = round_dir / "park-new.png"
    result = subprocess.run(
        [
            str(CLI),
            "eval",
            str(Path(scenario).expanduser()),
            "--ticks",
            str(ticks),
            "--rct2-data-path",
            str(Path(rct2_data).expanduser()),
            "--program",
            str(round_dir / "program.json"),
            "--out",
            str(check),
        ]
        + (["--replay", str(out), "--replay-seconds", str(seconds)] if replay else [])
        # Staged: a rerun that builds a different coaster must not have already
        # overwritten the round's picture on its way to being rejected.
        + (["--capture", str(shot)] if capture else []),
        capture_output=True,
        text=True,
    )
    try:
        if result.returncode != 0 or (replay and not out.exists()):
            tail = (result.stderr or result.stdout).strip().splitlines()[-3:]
            print(f"  failed: {' / '.join(tail)}", file=sys.stderr)
            out.unlink(missing_ok=True)
            poster.unlink(missing_ok=True)
            sidecar.unlink(missing_ok=True)
            return (False, False)
        rerun_report = json.loads(check.read_text()) if check.exists() else {}
        want = raw_excitement(json.loads((round_dir / "report.json").read_text()))
        got = raw_excitement(rerun_report)
        # Ratings are fixed-point hundredths, so the ride is either exactly the
        # round's or it is a different one.
        same_ride = abs(want - got) <= 0.005
        drew_track = built_in_full(rerun_report)
        wrote_picture = capture and shot.exists() and drew_track
        if wrote_picture:
            shot.replace(round_dir / "park.png")
        if not same_ride:
            out.unlink(missing_ok=True)
            poster.unlink(missing_ok=True)
            sidecar.unlink(missing_ok=True)
            drawn = " (picture kept, track is the same)" if drew_track else ""
            print(
                f"  rated {want:.2f}, rerun {got:.2f}: no video{drawn}",
                file=sys.stderr,
            )
        return (wrote_picture, same_ride and replay)
    finally:
        check.unlink(missing_ok=True)
        shot.unlink(missing_ok=True)


def main() -> int:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("runs", nargs="*", help="run names (default: all)")
    parser.add_argument("--all", action="store_true", help="every round, not just the best")
    parser.add_argument("--force", action="store_true", help="redo rounds that already have one")
    parser.add_argument(
        "--no-replay",
        action="store_true",
        help="only re-render park.png, do not film (much faster, no new videos)",
    )
    parser.add_argument(
        "--no-capture",
        action="store_true",
        help="leave park.png alone instead of re-rendering it cropped",
    )
    parser.add_argument("--scenario", default=DEFAULT_SCENARIO)
    parser.add_argument("--rct2-data", default=DEFAULT_RCT2_DATA)
    parser.add_argument("--ticks", type=int, default=25000)
    parser.add_argument("--replay-seconds", type=int, default=90)
    args = parser.parse_args()

    if not CLI.exists():
        print(f"{CLI} not built (cmake --build build)", file=sys.stderr)
        return 1

    names = args.runs or sorted(p.name for p in RUNS_DIR.iterdir() if p.is_dir())
    pictures = videos = skipped = failed = 0
    for name in names:
        run_dir = RUNS_DIR / name
        if not run_dir.is_dir():
            print(f"no such run: {name}", file=sys.stderr)
            failed += 1
            continue
        for round_dir in pick_rounds(run_dir, args.all):
            rel = round_dir.relative_to(RUNS_DIR)
            if not args.no_replay and (round_dir / "replay.mp4").exists() and not args.force:
                skipped += 1
                continue
            print(f"{'rendering' if args.no_replay else 'filming'} {rel} ...", flush=True)
            started = time.monotonic()
            picture, video = rerun(
                round_dir,
                args.scenario,
                args.rct2_data,
                args.ticks,
                args.replay_seconds,
                not args.no_capture,
                not args.no_replay,
            )
            pictures += picture
            videos += video
            if not picture and not video:
                failed += 1
            elif video or picture:
                print(f"  done in {time.monotonic() - started:.0f}s")
    print(
        f"{pictures} picture(s), {videos} video(s), "
        f"{skipped} already had one, {failed} wrote nothing"
    )
    return 1 if failed else 0


if __name__ == "__main__":
    sys.exit(main())
