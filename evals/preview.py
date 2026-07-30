# /// script
# requires-python = ">=3.11"
# ///
"""Look at the generated site from another device (usually a phone).

  uv run evals/preview.py             # LAN, same wifi
  uv run evals/preview.py --tunnel    # public https via cloudflared
  uv run evals/preview.py --deploy    # publish to Cloudflare Pages

The local server handles Range requests, which http.server does not: mobile
Safari will not play an mp4 without them. See CLAUDE.local.md.
"""

from __future__ import annotations

import argparse
import http.server
import os
import re
import socket
import socketserver
import subprocess
import sys
import threading
from pathlib import Path

EVALS_DIR = Path(__file__).resolve().parent
REPO = EVALS_DIR.parent
SITE_DIR = EVALS_DIR / "site"
SITE_CRATE = REPO / "rust" / "coaster-site" / "Cargo.toml"
RANGE_RE = re.compile(r"^bytes=(\d*)-(\d*)$")
PAGES_PROJECT = "coasterbench-preview"
# The project's own subdomain. A custom domain on wseaton.com is attached once in
# the Cloudflare dashboard (Pages project -> Custom domains, as wiimi has);
# point --preview-url at it afterwards.
PREVIEW_URL = f"https://{PAGES_PROJECT}.pages.dev"


class RangeHandler(http.server.SimpleHTTPRequestHandler):
    """SimpleHTTPRequestHandler plus HTTP Range, so video seeks and plays."""

    # Every response carries a Content-Length, so keep-alive is safe.
    protocol_version = "HTTP/1.1"

    def send_head(self):  # noqa: N802 (stdlib naming)
        header = self.headers.get("Range")
        if not header:
            return super().send_head()
        path = self.translate_path(self.path)
        if os.path.isdir(path):
            return super().send_head()
        match = RANGE_RE.match(header.strip())
        try:
            size = os.path.getsize(path)
        except OSError:
            return super().send_head()
        if not match:
            return super().send_head()

        start_text, end_text = match.groups()
        if start_text:
            start = int(start_text)
            end = int(end_text) if end_text else size - 1
        else:
            # "bytes=-500": the last 500 bytes.
            if not end_text:
                return super().send_head()
            start = max(size - int(end_text), 0)
            end = size - 1
        end = min(end, size - 1)
        if start > end or start >= size:
            self.send_response(416)
            self.send_header("Content-Range", f"bytes */{size}")
            self.end_headers()
            return None

        handle = open(path, "rb")
        handle.seek(start)
        self.send_response(206)
        self.send_header("Content-Type", self.guess_type(path))
        self.send_header("Content-Range", f"bytes {start}-{end}/{size}")
        self.send_header("Content-Length", str(end - start + 1))
        self.send_header("Accept-Ranges", "bytes")
        self.end_headers()
        self.wfile.write(handle.read(end - start + 1))
        handle.close()
        return None

    def end_headers(self):
        # A stale page is worse than a slow one while iterating.
        self.send_header("Cache-Control", "no-store")
        self.send_header("Accept-Ranges", "bytes")
        super().end_headers()

    def log_message(self, format, *args):  # noqa: A002 (stdlib signature)
        sys.stderr.write(f"  {format % args}\n")


class Server(socketserver.ThreadingTCPServer):
    daemon_threads = True
    allow_reuse_address = True


def lan_address() -> str:
    """This machine's address on the local network."""
    with socket.socket(socket.AF_INET, socket.SOCK_DGRAM) as probe:
        try:
            probe.connect(("192.0.2.1", 53))
            return probe.getsockname()[0]
        except OSError:
            return "127.0.0.1"


def build(
    remote_artifacts: bool = False,
    base_url: str | None = None,
    cf_images: bool = False,
) -> bool:
    print("building site ...", flush=True)
    cmd = ["cargo", "run", "--release", "--quiet", "--manifest-path", str(SITE_CRATE)]
    site_args = []
    if remote_artifacts:
        site_args.append("--remote-artifacts")
        if base_url is not None:
            site_args += ["--base-url", base_url]
    if cf_images:
        site_args.append("--cf-images")
    if site_args:
        cmd += ["--", *site_args]
    result = subprocess.run(cmd, cwd=REPO)
    return result.returncode == 0


def deploy(project: str, base_url: str, cf_images: bool = False, branch: str = "preview") -> int:
    """Publishes artifacts to R2, then the pages themselves to Cloudflare Pages."""
    print("publishing artifacts to R2 ...", flush=True)
    if subprocess.run([sys.executable, str(EVALS_DIR / "publish.py")], cwd=REPO).returncode:
        print("artifact upload failed; not deploying a site with missing images", file=sys.stderr)
        return 1
    if not build(remote_artifacts=True, base_url=base_url, cf_images=cf_images):
        return 1

    # A no-op error when the project exists, which is fine.
    subprocess.run(
        ["wrangler", "pages", "project", "create", project, "--production-branch", branch],
        cwd=REPO,
        capture_output=True,
        text=True,
    )
    result = subprocess.run(
        [
            "wrangler",
            "pages",
            "deploy",
            str(SITE_DIR),
            "--project-name",
            project,
            "--branch",
            branch,
            "--commit-dirty",
            "true",
        ],
        cwd=REPO,
    )
    if result.returncode:
        return result.returncode
    print(f"\n  preview: {base_url}/")
    return 0


def start_tunnel(port: int) -> subprocess.Popen | None:
    """Spawns a cloudflared quick tunnel and prints the URL it hands back."""
    try:
        proc = subprocess.Popen(
            ["cloudflared", "tunnel", "--url", f"http://localhost:{port}"],
            stdout=subprocess.PIPE,
            stderr=subprocess.STDOUT,
            text=True,
        )
    except FileNotFoundError:
        print("cloudflared not installed (brew install cloudflared)", file=sys.stderr)
        return None

    def watch() -> None:
        for line in proc.stdout or []:
            if "trycloudflare.com" in line:
                url = re.search(r"https://[\w.-]*trycloudflare\.com", line)
                if url:
                    print(f"\n  public: {url.group(0)}\n", flush=True)

    threading.Thread(target=watch, daemon=True).start()
    return proc


def main() -> int:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--port", type=int, default=8000)
    parser.add_argument("--no-build", action="store_true", help="serve what is already built")
    parser.add_argument(
        "--tunnel",
        action="store_true",
        help="also expose a public https URL through cloudflared",
    )
    parser.add_argument(
        "--deploy",
        action="store_true",
        help="publish to Cloudflare Pages instead of serving locally",
    )
    parser.add_argument(
        "--cf-images",
        action="store_true",
        help="serve artifact images through Cloudflare's edge resizer (needs "
        "image transformations enabled on the zone; the build checks and refuses if not)",
    )
    parser.add_argument("--project", default=PAGES_PROJECT, help="Cloudflare Pages project")
    parser.add_argument(
        "--branch",
        default="preview",
        help="Pages branch to deploy as. A custom domain serves the project's "
        "production branch, so publishing to coasterbench needs --branch eval",
    )
    parser.add_argument(
        "--preview-url",
        default=PREVIEW_URL,
        help="where the deploy will be reachable (used for the unfurl tags)",
    )
    args = parser.parse_args()

    if args.deploy:
        return deploy(args.project, args.preview_url.rstrip("/"), args.cf_images, args.branch)

    if not args.no_build and not build():
        return 1
    if not (SITE_DIR / "index.html").exists():
        print(f"no site at {SITE_DIR}", file=sys.stderr)
        return 1

    os.chdir(SITE_DIR)
    tunnel = start_tunnel(args.port) if args.tunnel else None
    with Server(("0.0.0.0", args.port), RangeHandler) as server:
        print(f"  local:  http://localhost:{args.port}/")
        print(f"  phone:  http://{lan_address()}:{args.port}/   (same wifi)")
        print("  ctrl-c to stop")
        try:
            server.serve_forever()
        except KeyboardInterrupt:
            print("\nstopped")
        finally:
            if tunnel is not None:
                tunnel.terminate()
    return 0


if __name__ == "__main__":
    sys.exit(main())
