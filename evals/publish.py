# /// script
# requires-python = ">=3.11"
# ///
"""Upload run artifacts (screenshots, videos, parks) to the Cloudflare R2 bucket.

Heavy artifacts live in R2 (served at https://artifacts.wseaton.com), not in
git; only the JSON metadata is tracked. This script uploads every artifact
under evals/runs/ (and the library preview PNGs) via `wrangler r2 object put`
(needs a `wrangler login` session, no static keys anywhere) and records what
was uploaded in manifests that the site generator reads:

  evals/runs/<run>/artifacts.json   run-relative artifact paths
  evals/library-previews.json       preview file names

Uploads are incremental: a file goes up when it is new, or when its contents
changed. The manifest records paths (what the site reads); the digests that
decide "changed" live beside it in `.artifacts-digests.json`. Both are committed.
Skipping on path alone left regenerated artifacts serving stale bytes from R2.

Usage:
  uv run evals/publish.py                     # publish all runs + previews
  uv run evals/publish.py 20260723-showdown   # just one run
  uv run evals/publish.py --force             # re-upload everything
"""

from __future__ import annotations

import argparse
import json
import subprocess
import sys
import hashlib
from concurrent.futures import ThreadPoolExecutor
from pathlib import Path

BUCKET = "coasterbench"
EVALS_DIR = Path(__file__).resolve().parent
RUNS_DIR = EVALS_DIR / "runs"
PREVIEWS_DIR = EVALS_DIR / "library-previews"
PREVIEWS_MANIFEST = EVALS_DIR / "library-previews.json"

CONTENT_TYPES = {
    ".png": "image/png",
    ".mp4": "video/mp4",
    ".webm": "video/webm",
    ".park": "application/octet-stream",
}


def upload(key: str, path: Path) -> str:
    """Puts one object into the bucket; returns the key. Raises on failure."""
    result = subprocess.run(
        [
            "wrangler",
            "r2",
            "object",
            "put",
            f"{BUCKET}/{key}",
            "--file",
            str(path),
            "--content-type",
            CONTENT_TYPES[path.suffix],
            # A day of caching, deliberately NOT immutable: artifacts are
            # usually write-once, but a re-publish (screenshot regen, backfill)
            # must actually propagate. immutable + a year meant an overwritten
            # object served the stale copy from the edge until 2027.
            "--cache-control",
            "public, max-age=86400",
            "--remote",
        ],
        capture_output=True,
        text=True,
    )
    if result.returncode != 0:
        raise RuntimeError(f"upload of {key} failed:\n{result.stderr.strip()}")
    return key


def upload_all(jobs: list[tuple[str, Path]], workers: int) -> None:
    done = 0
    with ThreadPoolExecutor(max_workers=workers) as pool:
        for key in pool.map(lambda j: upload(*j), jobs):
            done += 1
            print(f"  [{done}/{len(jobs)}] {key}")


def load_manifest(path: Path) -> set[str]:
    return set(json.loads(path.read_text())) if path.is_file() else set()


def write_manifest(path: Path, entries: set[str]) -> None:
    path.write_text(json.dumps(sorted(entries), indent=1) + "\n")


def digest_path(manifest_path: Path) -> Path:
    """Same directory, dotted name."""
    return manifest_path.with_name(f".{manifest_path.stem}-digests.json")


def load_digests(manifest_path: Path) -> dict[str, str]:
    path = digest_path(manifest_path)
    return json.loads(path.read_text()) if path.is_file() else {}


def write_digests(manifest_path: Path, digests: dict[str, str]) -> None:
    path = digest_path(manifest_path)
    path.write_text(json.dumps(dict(sorted(digests.items())), indent=1) + "\n")


def digest(path: Path) -> str:
    """Full SHA-256 used as the durable artifact-integrity manifest."""
    return hashlib.sha256(path.read_bytes()).hexdigest()


def needs_upload(rel: str, path: Path, manifest: set[str], digests: dict[str, str]) -> bool:
    """New, or changed. No recorded digest means upload: the bucket's contents
    are unknown, so the first run after this landed re-uploaded everything."""
    if rel not in manifest:
        return True
    return digests.get(rel) != digest(path)


def publish_run(run_dir: Path, force: bool, workers: int) -> None:
    manifest_path = run_dir / "artifacts.json"
    manifest = load_manifest(manifest_path)
    digests = load_digests(manifest_path)
    jobs: list[tuple[str, Path]] = []
    entries = set(manifest)
    for path in sorted(run_dir.rglob("*")):
        if path.suffix not in CONTENT_TYPES or not path.is_file():
            continue
        rel = path.relative_to(run_dir).as_posix()
        entries.add(rel)
        if not force and not needs_upload(rel, path, manifest, digests):
            continue
        digests[rel] = digest(path)
        jobs.append((f"runs/{run_dir.name}/{rel}", path))
    if not jobs:
        print(f"{run_dir.name}: up to date ({len(entries)} artifacts)")
        return
    print(f"{run_dir.name}: uploading {len(jobs)} artifact(s)")
    upload_all(jobs, workers)
    write_manifest(manifest_path, entries)
    write_digests(manifest_path, digests)


def publish_previews(force: bool, workers: int) -> None:
    if not PREVIEWS_DIR.is_dir():
        return
    manifest = load_manifest(PREVIEWS_MANIFEST)
    digests = load_digests(PREVIEWS_MANIFEST)
    entries = set(manifest)
    jobs: list[tuple[str, Path]] = []
    for path in sorted(PREVIEWS_DIR.glob("*.png")):
        entries.add(path.name)
        if not force and not needs_upload(path.name, path, manifest, digests):
            continue
        digests[path.name] = digest(path)
        jobs.append((f"library-previews/{path.name}", path))
    if not jobs:
        print(f"library-previews: up to date ({len(entries)} previews)")
        return
    print(f"library-previews: uploading {len(jobs)} preview(s)")
    upload_all(jobs, workers)
    write_manifest(PREVIEWS_MANIFEST, entries)
    write_digests(PREVIEWS_MANIFEST, digests)


def main() -> int:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("runs", nargs="*", help="run names to publish (default: all)")
    parser.add_argument("--force", action="store_true", help="re-upload files already in a manifest")
    parser.add_argument("--workers", type=int, default=8, help="concurrent uploads (default %(default)s)")
    args = parser.parse_args()

    if args.runs:
        run_dirs = [RUNS_DIR / name for name in args.runs]
        missing = [d.name for d in run_dirs if not d.is_dir()]
        if missing:
            print(f"no such run(s): {', '.join(missing)}", file=sys.stderr)
            return 1
    else:
        run_dirs = sorted(d for d in RUNS_DIR.iterdir() if d.is_dir())

    for run_dir in run_dirs:
        publish_run(run_dir, args.force, args.workers)
    if not args.runs:
        publish_previews(args.force, args.workers)
    return 0


if __name__ == "__main__":
    sys.exit(main())
