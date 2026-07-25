#!/usr/bin/env python3
"""CI pass/fail gate for a CoasterBench run directory.

Scores are model quality, not infrastructure health, so CI asserts protocol
success only: every model must have at least one round whose program built OK
and whose ride completed a test circuit. Score stays a reported metric.

  usage: check_run.py evals/runs/<run-dir>
"""
import json
import sys
from pathlib import Path


def main() -> int:
    if len(sys.argv) != 2:
        print(__doc__, file=sys.stderr)
        return 2
    run_dir = Path(sys.argv[1])
    run = json.loads((run_dir / "run.json").read_text())
    failures = []
    for model in run["models"]:
        reports = sorted(run_dir.glob(f"{model.replace('/', '_')}/round_*/report.json"))
        tested = []
        for path in reports:
            report = json.loads(path.read_text())
            # Batch runs name the program's ride; MCP-harness runs have no
            # program, so any tested ride in the round's report counts.
            ride_id = (report.get("program") or {}).get("ride_id")
            tested += [
                r
                for r in report.get("rides", []) or []
                if r.get("tested") and (ride_id is None or r["id"] == ride_id)
            ]
        best = max((r.get("excitement") or 0.0 for r in tested), default=None)
        if not reports:
            failures.append(f"{model}: no rounds ran")
        elif not tested:
            failures.append(f"{model}: {len(reports)} round(s), none produced a tested coaster")
        else:
            print(f"ok: {model}: {len(tested)}/{len(reports)} rounds tested, best excitement {best:.2f}")
    for failure in failures:
        print(f"FAIL: {failure}", file=sys.stderr)
    return 1 if failures else 0


if __name__ == "__main__":
    sys.exit(main())
