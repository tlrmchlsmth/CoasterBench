# /// script
# requires-python = ">=3.11"
# dependencies = ["anthropic[vertex]>=0.40", "openai>=1.40", "pillow>=10"]
# ///
"""Coaster design head-to-head: two Claude models iteratively design a coaster.

Each round the model submits a JSON track program (via forced tool use); the
harness runs `openrct2-cli eval` on a fresh copy of the scenario, then feeds
back the eval report and a park screenshot. Best excitement across rounds wins.

Two modes:
  design  (default) — the model designs from scratch; pure design ability.
  library — the model can additionally search the stock RCT2 track design
            library and read full piece sequences; tests information retrieval
            and adaptation. Scores are penalized for similarity to any stock
            design (mirrored copies included), so copying outright scores zero.

Usage (first-party API):
  ANTHROPIC_API_KEY=... uv run evals/driver.py \
      --models claude-fable-5 claude-sonnet-5 --rounds 4 --mode library

Usage (Google Vertex AI; auth via `gcloud auth application-default login`):
  uv run evals/driver.py --vertex --project my-gcp-project \
      --models claude-opus-4-6 claude-sonnet-5 --rounds 4

Vertex model IDs for current-generation models are the bare first-party
strings (claude-opus-4-6, claude-sonnet-5) — no prefix, no @date suffix.

Usage (any OpenAI-compatible endpoint, e.g. a local vLLM server):
  vllm serve Qwen/Qwen2.5-7B-Instruct --enable-auto-tool-choice ...
  uv run evals/driver.py --base-url http://localhost:8000/v1 \
      --models Qwen/Qwen2.5-7B-Instruct --rounds 4 --no-graphics

--no-graphics runs the game without RCT2 assets (design mode only): the
scenario defaults to a checked-in test park, feedback is the eval report
alone (no park screenshot), and the similarity penalty is inert because
there is no stock library to compare against.
"""

from __future__ import annotations

import argparse
import base64
import json
import os
import re
import subprocess
import sys
import tempfile
import time
from dataclasses import dataclass, field
from pathlib import Path

import anthropic

REPO = Path(__file__).resolve().parent.parent
# COASTERBENCH_CLI lets a CI environment point at a binary that didn't come
# from this checkout's build dir (e.g. extracted from the game image).
CLI = Path(os.environ.get("COASTERBENCH_CLI", REPO / "build" / "openrct2-cli"))
DEFAULT_SCENARIO = Path.home() / "rct2-assets" / "Scenarios" / "Build your own Six Flags Park.SC6"
RCT2_DATA = Path.home() / "rct2-assets"
# Assetless default: a checked-in upstream test park (large, mostly-open flat
# grass, cash-rich) that loads and builds with only the bundled JSON objects.
CI_SCENARIO = REPO / "test" / "tests" / "testdata" / "parks" / "BigMapTest.sv6"

MAP_LINES = {
    DEFAULT_SCENARIO.name: (
        "Flat grass around tile (60, 60); a lake sits near map centre roughly tiles (68-85, 55-75) — do NOT "
        "build into it. Stay within tiles 20-120. Directions: dir 0 faces -x, dir 1 faces +y, dir 2 faces +x, "
        "dir 3 faces -y."
    ),
    CI_SCENARIO.name: (
        "A large park: flat open grass across roughly tiles 30-190 on both axes, with scattered existing "
        "rides and footpaths (placement errors will name what is in the way; shift a few tiles and retry). "
        "Flat grass around tile (60, 60) is a good anchor. Directions: dir 0 faces -x, dir 1 faces +y, "
        "dir 2 faces +x, dir 3 faces -y."
    ),
}

# Set from --no-graphics in main(): the game loads no sprite data, so no RCT2
# assets are needed and nothing can render (no screenshots, no previews).
NO_GRAPHICS = False

# Set from --schematic-feedback: attach the schematic track diagram (rendered
# from the report's cursor trace, no assets needed) to round feedback, for
# multimodal contenders in no-graphics runs.
SCHEMATIC_FEEDBACK = False

# Set to the run dir by main(): calls that fail (e.g. a reasoning model
# exhausting its budget) dump their raw response here, because the response
# body — especially a 131k-token thinking trace — is the evidence, and
# raising without saving it has already lost that evidence twice.
FAILED_CALL_DIR: Path | None = None


def rct2_args() -> list[str]:
    """The eval CLI either loads the RCT2 install or runs assetless."""
    args = []
    # Containers/chroots can't always resolve the data dir relative to the
    # binary (/proc may be absent); CI sets this explicitly.
    data = os.environ.get("COASTERBENCH_OPENRCT2_DATA")
    if data:
        args += ["--openrct2-data-path", data]
    if NO_GRAPHICS:
        return args + ["--no-graphics"]
    return args + ["--rct2-data-path", str(RCT2_DATA)]

PIECE_CATALOG = """
Station (required, place these FIRST, 3+ in a row): begin_station, middle_station, end_station
Straight & slopes: flat, up_25, up_60, down_25, down_60
Slope transitions: flat_to_up_25, up_25_to_up_60, up_60_to_up_25, up_25_to_flat,
  flat_to_down_25, down_25_to_down_60, down_60_to_down_25, down_25_to_flat,
  flat_to_up_60, up_60_to_flat, flat_to_down_60, down_60_to_flat
Turns (90 degrees, radius in tiles): left_turn_5, right_turn_5, left_turn_3, right_turn_3, left_turn_1, right_turn_1
Sloped turns: left_turn_5_up_25, right_turn_5_up_25, left_turn_5_down_25, right_turn_5_down_25,
  left_turn_3_up_25, right_turn_3_up_25, left_turn_3_down_25, right_turn_3_down_25
Banking: flat_to_left_bank, flat_to_right_bank, left_bank_to_flat, right_bank_to_flat,
  left_bank, right_bank, banked_left_turn_5, banked_right_turn_5, banked_left_turn_3, banked_right_turn_3,
  left_bank_to_up_25, right_bank_to_up_25, up_25_to_left_bank, up_25_to_right_bank,
  left_bank_to_down_25, right_bank_to_down_25, down_25_to_left_bank, down_25_to_right_bank
S-bends: s_bend_left, s_bend_right
Inversions (steel types only; wooden does NOT support these). Exact cursor geometry, measured in-game:
  left_vertical_loop / right_vertical_loop: a COMPLETE loop in ONE piece. Enter at a 25-up slope
    (flat_to_up_25 first), exit at a 25-down slope (follow with down_25 or down_25_to_flat).
    Net cursor move: 2 tiles forward, 1 tile toward the named side, exit at the SAME height and heading
    as entry. The go-to inversion; needs lots of entry speed.
  left_corkscrew_up + right_corkscrew_down (or right_up + left_down): the standard corkscrew pair,
    placed back-to-back. Enters FLAT unbanked. Net for the pair: 3 tiles forward, 3 tiles toward the
    first piece's named side, same heading, same height. Do not put anything between the two pieces.
  half_loop_up: climbs 152 z-units, REVERSES your heading, and ends upside down directly above its own
    entry tile. WARNING: half_loop_down placed right after descends along the corridor you approached on
    and collides with your own approach track. Either follow half_loop_up with a corkscrew_down (exits
    sideways and rights the train), or just use a vertical_loop instead.
Helices: left_helix_up_small, right_helix_up_small, left_helix_down_small, right_helix_down_small,
  left_helix_up_large, right_helix_up_large, left_helix_down_large, right_helix_down_large
Special: brakes, booster
"""

# Competition ride types the prompt knows how to describe; other ids work but
# get a generic description.
RIDE_TYPES = {
    51: ("steel twister coaster", "51 = steel twister roller coaster (inversions ALLOWED and rewarded)."),
    52: ("wooden coaster", "52 = wooden roller coaster (no inversions)."),
}


def ride_type_info(ride_type: int) -> tuple[str, str]:
    name, line = RIDE_TYPES.get(ride_type, (f"ride type {ride_type} coaster", f"{ride_type} = the required ride type."))
    return name, line + " This is the required type for this competition."


def build_system_prompt(ride_type: int, scenario: Path) -> str:
    _, ride_line = ride_type_info(ride_type)
    map_line = MAP_LINES.get(
        scenario.name,
        "Terrain unknown; flat grass around tile (60, 60) is a reasonable first bet. Use validation "
        "errors to find open ground. Directions: dir 0 faces -x, dir 1 faces +y, dir 2 faces +x, dir 3 faces -y.",
    )
    return SYSTEM_PROMPT.replace("{RIDE_TYPE_LINE}", ride_line).replace("{MAP_LINE}", map_line)


SYSTEM_PROMPT = f"""You are competing to design the best RollerCoaster Tycoon 2 roller coaster.
You submit a "track program": a ride type, a start tile, and an ordered list of track pieces.
The game engine builds it piece by piece, tests it with a real train, and rates it.

## Rules of track geometry
- Pieces chain sequentially from a cursor (position + facing direction). Each piece moves/rotates the cursor.
- The track must form a CLOSED CIRCUIT: the last piece must end exactly where the first begins, facing the same direction, at the same height. Total up-slope pieces must equal total down-slope pieces of the same steepness.
- A trick for closure: any identical piece sequence ending in a 90-degree turn, repeated 4 times, closes a rectangle.
- Start with begin_station, middle_station, end_station (station must be on flat ground, 3-7 pieces).
- up_25 rises 16 z-units per piece; up_60 rises 48. You cannot go below the starting height (the ground).
- Use {{"t": "up_25", "chain": true}} for chain lift hill pieces (needed to climb; trains start slow!). Chain lifts only work on 25-degree slopes, never on 60-degree pieces.
- Banking must be entered and exited: flat_to_left_bank ... left_bank ... left_bank_to_flat.
- Sloped pieces cannot be banked. Transitions matter: up_25 cannot follow flat directly, use flat_to_up_25.
- The train coasts on gravity after the lift. If it stalls (too little energy for a hill), the test fails or takes forever. Drops give speed; friction bleeds it.

## Ride types
{{RIDE_TYPE_LINE}}

## Scoring (from the real game engine)
Excitement is primary (higher wins). It rewards: drops, speed, airtime, direction changes, banked turns, length. Intensity above ~10 tanks excitement (guests won't ride); keep intensity under 10.00. Crashes disqualify.

Your track is also compared against the stock RCT2 track design library (mirrored variants included). Similarity up to 0.5 is free; above that your excitement is scaled down linearly, reaching zero for an exact copy. Design something original; reproducing a stock coaster from memory scores nothing.

## Piece catalog
{PIECE_CATALOG}

## Map
{{MAP_LINE}}

Before submitting, use the validate_track_program tool (same payload) to dry-run your program: it reports placement errors with the exact piece index, or whether the circuit closes, without spending your round. You get a limited number of validations per round, use them to fix geometry, then submit.

Submit via the submit_track_program tool. After each attempt you get the eval report (placement errors with exact piece index, or ride stats) and a park screenshot. Iterate and maximise excitement."""

LIBRARY_PROMPT = """

## Track design library
You can browse the stock RCT2 track design library before submitting:
- search_track_designs lists designs (name, ride type, piece count); filter with the ride_type parameter.
- get_track_design returns a design's full piece sequence in the same format you submit.
Use them to study proven layouts, then design your own. The similarity penalty applies to these exact designs, so copying (or mirroring) one scores zero; the winning move is understanding why they work and building something original with that knowledge."""

LIBRARY_TOOLS = [
    {
        "name": "search_track_designs",
        "description": "List the stock track design library, optionally filtered by ride type. Returns name, ride type, and piece count per design.",
        "input_schema": {
            "type": "object",
            "properties": {"ride_type": {"type": "integer"}},
        },
    },
    {
        "name": "get_track_design",
        "description": "Full piece sequence of one stock library design, in the same format submit_track_program accepts.",
        "input_schema": {
            "type": "object",
            "required": ["name"],
            "properties": {"name": {"type": "string"}},
        },
    },
]

VALIDATE_TOOL = {
    "name": "validate_track_program",
    "description": "Dry-run a track program: builds it in the game and reports placement errors "
    "(with exact piece index) or whether the circuit closes, WITHOUT spending your round. "
    "Same payload as submit_track_program. Use it to iterate before submitting.",
    "input_schema": None,  # filled below with TOOL's schema
}

TOOL = {
    "name": "submit_track_program",
    "description": "Submit the coaster track program to build and test.",
    "input_schema": {
        "type": "object",
        "required": ["ride_type", "start", "pieces"],
        "properties": {
            "ride_type": {"type": "integer"},
            "start": {
                "type": "object",
                "required": ["x", "y", "dir"],
                "properties": {
                    "x": {"type": "integer"},
                    "y": {"type": "integer"},
                    "dir": {"type": "integer", "minimum": 0, "maximum": 3},
                },
            },
            "pieces": {
                "type": "array",
                "minItems": 4,
                "items": {
                    "anyOf": [
                        {"type": "string"},
                        {
                            "type": "object",
                            "required": ["t"],
                            "properties": {"t": {"type": "string"}, "chain": {"type": "boolean"}},
                        },
                    ]
                },
            },
        },
    },
}


VALIDATE_TOOL["input_schema"] = TOOL["input_schema"]


# Similarity below this is free; above it the score scales linearly to zero
# at 1.0 (an exact copy of a stock design).
SIMILARITY_GRACE = 0.5


def similarity_multiplier(similarity: float) -> float:
    if similarity <= SIMILARITY_GRACE:
        return 1.0
    return max(0.0, (1.0 - similarity) / (1.0 - SIMILARITY_GRACE))


@dataclass
class Attempt:
    round: int
    program: dict
    report: dict
    screenshot: Path | None
    # Library tool calls made before this round's submission (library mode).
    lookups: list[dict] = field(default_factory=list)

    @property
    def raw_excitement(self) -> float:
        for ride in self.report.get("rides", []):
            if ride.get("excitement") is not None:
                return ride["excitement"]
        return 0.0

    @property
    def similarity(self) -> float:
        sim = self.report.get("similarity") or {}
        return sim.get("similarity", 0.0)

    @property
    def excitement(self) -> float:
        """Raw excitement scaled down for copying a stock library design."""
        return self.raw_excitement * similarity_multiplier(self.similarity)

    @property
    def summary(self) -> str:
        prog = self.report.get("program") or {}
        if not prog.get("ok"):
            err = (prog.get("error") or {}).get("message", "unknown error")
            placed = prog.get("pieces_placed", 0)
            total = prog.get("pieces_total", 0)
            idx = (prog.get("error") or {}).get("piece_index")
            where = f" at piece {idx}" if idx is not None else ""
            return f"BUILD FAILED{where} ({placed}/{total} placed): {err}"
        rides = self.report.get("rides", [])
        if not rides:
            return "built but no ride data"
        r = rides[0]
        text = (
            f"excitement={r.get('excitement')} intensity={r.get('intensity')} nausea={r.get('nausea')} "
            f"tested={r.get('tested')} crashed={r.get('crashed')} length={r.get('ride_length')} "
            f"drops={r.get('num_drops')} airtime={r.get('total_air_time')}"
        )
        sim = self.report.get("similarity") or {}
        if sim:
            text += f" similarity={sim.get('similarity', 0.0):.2f} (nearest: {sim.get('nearest_design')})"
        if similarity_multiplier(self.similarity) < 1.0:
            text += f" -> penalized excitement {self.excitement:.2f}"
        return text


@dataclass
class Contender:
    model: str
    attempts: list[Attempt] = field(default_factory=list)

    @property
    def best(self) -> Attempt | None:
        rated = [a for a in self.attempts if a.excitement > 0]
        return max(rated, key=lambda a: a.excitement) if rated else None


STATION_PIECES = {"begin_station", "middle_station", "end_station"}


def render_schematic(trace: list[dict], out_path: Path) -> Path | None:
    """Draws the placed track as a two-panel PNG (top-down + isometric) from
    the report's cursor trace — no game assets involved. Stations are green,
    chain lift red, everything else shaded by height; an open circuit gets a
    dashed gap line from track end back to the start."""
    if len(trace) < 2:
        return None
    from PIL import Image, ImageDraw

    pts = [(p["x"], p["y"], p["z"]) for p in trace]
    zs = [z for _, _, z in pts]
    z0, z1 = min(zs), max(zs)

    def color(i: int) -> tuple[int, int, int]:
        piece = trace[i]["piece"]
        if piece in STATION_PIECES:
            return (46, 160, 67)
        if trace[i].get("chain"):
            return (220, 68, 61)
        t = (pts[i][2] - z0) / (z1 - z0) if z1 > z0 else 0.0
        return (int(60 + 195 * t), int(120 - 40 * t), int(220 - 160 * t))

    panels = {
        "top": lambda x, y, z: (x, y),
        "iso": lambda x, y, z: (x - y, (x + y) * 0.5 - z / 24),
    }
    size, margin = 640, 40
    img = Image.new("RGB", (size * 2, size), (250, 250, 248))
    draw = ImageDraw.Draw(img)

    closed = pts[0][:2] == pts[-1][:2] and trace[0]["z"] == trace[-1]["z"]
    for panel, (name, proj) in enumerate(panels.items()):
        proj_pts = [proj(*p) for p in pts]
        xs = [u for u, _ in proj_pts]
        ys = [v for _, v in proj_pts]
        span = max(max(xs) - min(xs), max(ys) - min(ys)) or 1.0
        scale = (size - 2 * margin) / span

        def to_px(uv, panel=panel, xs=xs, ys=ys, scale=scale):
            return (
                panel * size + margin + (uv[0] - min(xs)) * scale,
                margin + (uv[1] - min(ys)) * scale,
            )

        px = [to_px(p) for p in proj_pts]
        for i in range(1, len(px)):
            draw.line([px[i - 1], px[i]], fill=color(i), width=4)
        if not closed:
            draw.line([px[-1], px[0]], fill=(150, 150, 150), width=2)
        sx, sy = px[0]
        draw.ellipse([sx - 5, sy - 5, sx + 5, sy + 5], outline=(0, 0, 0), width=2)
        draw.text((panel * size + margin, size - margin + 8), name, fill=(90, 90, 90))

    if not closed:
        dx = pts[0][0] - pts[-1][0]
        dy = pts[0][1] - pts[-1][1]
        dz = trace[0]["z"] - trace[-1]["z"]
        draw.text((margin, 8), f"OPEN CIRCUIT: gap to start  dx={dx}  dy={dy}  dz={dz}", fill=(180, 30, 30))
    draw.text((size + margin, 8), "green=station  red=chain-lift  blue->orange=height", fill=(90, 90, 90))
    img.save(out_path)
    return out_path


def run_eval(program: dict, scenario: Path, workdir: Path, ticks: int) -> tuple[dict, Path | None]:
    workdir.mkdir(parents=True, exist_ok=True)
    program_path = workdir / "program.json"
    report_path = workdir / "report.json"
    capture_path = workdir / "park.png"
    program_path.write_text(json.dumps(program, indent=2))

    cmd = [
        str(CLI), "eval", str(scenario),
        "--ticks", str(ticks),
        *rct2_args(),
        "--program", str(program_path),
        "--out", str(report_path),
    ]
    if not NO_GRAPHICS:
        cmd += ["--capture", str(capture_path), "--capture-xray"]
    proc = subprocess.run(cmd, capture_output=True, text=True, timeout=600)
    if not report_path.exists():
        return {"program": {"ok": False, "error": {"message": f"eval crashed: {proc.stderr[-500:]}"}}}, None

    report = json.loads(report_path.read_text())
    trace = (report.get("program") or {}).get("trace") or []
    schematic = None
    if trace:
        try:
            schematic = render_schematic(trace, workdir / "track.png")
        except Exception as e:  # a diagram must never sink the round
            print(f"  schematic render failed: {e}", file=sys.stderr)
    shot = schematic if SCHEMATIC_FEEDBACK else None
    if capture_path.exists():
        small = workdir / "park_small.png"
        # The API rejects images over 5 MB of base64 (~3.7 MB raw); tall parks
        # can exceed that even downscaled, so keep shrinking until it fits.
        for px in (1500, 1100, 800, 600):
            subprocess.run(["sips", "-Z", str(px), str(capture_path), "--out", str(small)], capture_output=True)
            if small.exists() and small.stat().st_size * 4 / 3 < 4_900_000:
                shot = small
                break
    return report, shot


CLOSURE_RE = re.compile(
    r"starts at tile \((\d+), (\d+), z=(\d+), dir=(\d+), bank=(\d+), slope=(\d+)\) "
    r"and ends at tile \((\d+), (\d+), z=(\d+), dir=(\d+), bank=(\d+), slope=(\d+)\)"
)


def closure_hint(message: str) -> str | None:
    """Turns the closure error's two cursors into the net move still needed."""
    m = CLOSURE_RE.search(message)
    if m is None:
        return None
    sx, sy, sz, sd, sb, ss, ex, ey, ez, ed, eb, es = (int(g) for g in m.groups())
    parts = [f"from the end cursor you still need net dx={sx - ex} tiles, dy={sy - ey} tiles, dz={sz - ez} z-units"]
    if ed != sd:
        parts.append(f"turn heading from dir {ed} to dir {sd}")
    if eb != sb or es != ss:
        parts.append("level out bank/slope before the station")
    return "; ".join(parts)


def validate_program(program: dict, scenario: Path) -> str:
    """Placement + circuit-closure dry run; a few ticks is enough because both
    are checked at build time, before any real simulation."""
    with tempfile.TemporaryDirectory() as d:
        program_path = Path(d) / "program.json"
        report_path = Path(d) / "report.json"
        program_path.write_text(json.dumps(program))
        subprocess.run(
            [
                str(CLI), "eval", str(scenario),
                "--ticks", "5",
                *rct2_args(),
                "--program", str(program_path),
                "--out", str(report_path),
            ],
            capture_output=True,
            timeout=300,
        )
        if not report_path.exists():
            return json.dumps({"ok": False, "error": "eval crashed"})
        prog = json.loads(report_path.read_text()).get("program", {})
    if prog.get("ok"):
        return json.dumps({"ok": True, "note": "placement OK and circuit closed; ready to submit"})
    err = prog.get("error") or {}
    result = {
        "ok": False,
        "piece_index": err.get("piece_index"),
        "error": err.get("message"),
        "pieces_placed": prog.get("pieces_placed"),
        "pieces_total": prog.get("pieces_total"),
    }
    hint = closure_hint(err.get("message") or "")
    if hint:
        result["hint"] = hint
    return json.dumps(result)


def dump_library(scenario: Path, run_dir: Path) -> list[dict]:
    """Exports the stock design library via the CLI (library mode only)."""
    out = run_dir / "library.json"
    cmd = [
        str(CLI), "eval", str(scenario),
        "--rct2-data-path", str(RCT2_DATA),
        "--dump-library", str(out),
    ]
    proc = subprocess.run(cmd, capture_output=True, text=True, timeout=600)
    if not out.exists():
        raise RuntimeError(f"library dump failed: {proc.stderr[-500:]}")
    return json.loads(out.read_text())


PREVIEWS_DIR = REPO / "evals" / "library-previews"


def ensure_library_previews(scenario: Path) -> None:
    """Renders design preview PNGs once; the library is static, so the cache
    survives across runs (evals/library-previews/, gitignored)."""
    if PREVIEWS_DIR.is_dir() and any(PREVIEWS_DIR.glob("*.png")):
        return
    print("rendering track design previews (one-time)...")
    cmd = [
        str(CLI), "eval", str(scenario),
        "--rct2-data-path", str(RCT2_DATA),
        "--render-library", str(PREVIEWS_DIR),
    ]
    proc = subprocess.run(cmd, capture_output=True, text=True, timeout=600)
    count = len(list(PREVIEWS_DIR.glob("*.png"))) if PREVIEWS_DIR.is_dir() else 0
    if count == 0:
        # Previews only feed the site gallery; a render failure shouldn't
        # block the eval itself.
        print(f"warning: preview render produced no images: {proc.stderr[-300:]}", file=sys.stderr)
    else:
        print(f"library previews: {count} images in {PREVIEWS_DIR}")


# In library mode the model may look designs up before submitting; cap the
# lookups per round so a browsing spree cannot stall the eval.
MAX_LOOKUPS_PER_ROUND = 6


def library_tool_result(name: str, tool_input: dict, library: list[dict]) -> tuple[str, dict]:
    """Answers a library tool call. Returns (tool_result_json, lookup_record)."""
    if name == "search_track_designs":
        ride_type = tool_input.get("ride_type")
        designs = [
            {"name": d["name"], "ride_type": d["ride_type"], "piece_count": d["piece_count"]}
            for d in library
            if ride_type is None or d["ride_type"] == ride_type
        ]
        result = json.dumps(
            {"designs": designs, "note": "final score is penalized for similarity to any of these designs"}
        )
        return result, {"tool": "search", "ride_type": ride_type, "results": len(designs)}
    target = tool_input.get("name", "")
    for d in library:
        if d["name"].lower() == target.lower():
            result = json.dumps({k: d[k] for k in ("name", "ride_type", "piece_count", "pieces")})
            return result, {"tool": "get", "name": d["name"], "found": True}
    return json.dumps({"error": f"no such design: {target}"}), {"tool": "get", "name": target, "found": False}


def prune_history(messages: list[dict]) -> None:
    """Trims completed-round bulk from the message history, in place.

    Two payloads dominate context growth: the base64 park screenshot in each
    round's feedback and the full piece list of every get_track_design result.
    The model has already consumed both, and the submitted programs stay in
    history, so all but the most recent screenshot collapse to a stub and old
    design payloads keep only their name and piece count.
    """
    image_stub = {"type": "text", "text": "[park screenshot elided; see latest round]"}
    last_image: tuple[int, int, int] | None = None
    for m, message in enumerate(messages):
        if message.get("role") != "user" or not isinstance(message.get("content"), list):
            continue
        for c, block in enumerate(message["content"]):
            if not isinstance(block, dict) or block.get("type") != "tool_result":
                continue
            content = block.get("content")
            if isinstance(content, list):
                for b, inner in enumerate(content):
                    if isinstance(inner, dict) and inner.get("type") == "image":
                        if last_image is not None:
                            pm, pc, pb = last_image
                            messages[pm]["content"][pc]["content"][pb] = image_stub
                        last_image = (m, c, b)
            elif isinstance(content, str) and '"pieces"' in content:
                try:
                    payload = json.loads(content)
                except json.JSONDecodeError:
                    continue
                if "pieces" in payload:
                    payload["pieces"] = f"[{payload.get('piece_count', '?')} pieces elided; fetch again if needed]"
                    block["content"] = json.dumps(payload)


def feedback_content(attempt: Attempt) -> list[dict]:
    content: list[dict] = [
        {
            "type": "text",
            "text": (
                f"Round {attempt.round} result: {attempt.summary}\n\n"
                f"Full report:\n{json.dumps(attempt.report, indent=1)}\n\n"
                "Revise your design and submit again. Aim for higher excitement (intensity < 10, no crashes)."
            ),
        }
    ]
    if attempt.screenshot is not None:
        content.append(
            {
                "type": "image",
                "source": {
                    "type": "base64",
                    "media_type": "image/png",
                    "data": base64.b64encode(attempt.screenshot.read_bytes()).decode(),
                },
            }
        )
    return content


@dataclass
class ToolUseBlock:
    id: str
    name: str
    input: dict
    type: str = "tool_use"


@dataclass
class TextBlock:
    text: str
    type: str = "text"


@dataclass
class ReasoningBlock:
    """A reasoning model's thinking, preserved so it can be passed back on the
    next turn (vLLM renders `reasoning` on assistant messages into the chat
    template — verified: prompt_tokens grows by the trace length). Without
    passback the model re-derives everything from scratch every call."""

    text: str
    type: str = "reasoning"


@dataclass
class _Usage:
    input_tokens: int
    output_tokens: int


@dataclass
class _Response:
    content: list
    usage: _Usage


def _to_openai(message: dict) -> list[dict]:
    """One anthropic-form history entry -> the OpenAI messages it becomes.

    The driver only ever builds three shapes: a plain-string user message, a
    user message holding tool_result blocks, and an assistant message whose
    content is the block list a previous create() returned.
    """
    role = message["role"]
    content = message["content"]
    if role == "assistant":
        text = "".join(b.text for b in content if b.type == "text")
        reasoning = "".join(b.text for b in content if b.type == "reasoning")
        calls = [
            {"id": b.id, "type": "function", "function": {"name": b.name, "arguments": json.dumps(b.input)}}
            for b in content
            if b.type == "tool_use"
        ]
        msg: dict = {"role": "assistant", "content": text or None}
        if reasoning:
            msg["reasoning"] = reasoning
        if calls:
            msg["tool_calls"] = calls
        return [msg]
    if isinstance(content, str):
        return [{"role": "user", "content": content}]
    out: list[dict] = []
    images: list[str] = []
    for block in content:
        if not isinstance(block, dict) or block.get("type") != "tool_result":
            continue
        inner = block.get("content")
        texts: list[str] = []
        if isinstance(inner, str):
            texts.append(inner)
        else:
            for part in inner or []:
                if part.get("type") == "text":
                    texts.append(part["text"])
                elif part.get("type") == "image":
                    images.append(part["source"]["data"])
        out.append(
            {"role": "tool", "tool_call_id": block["tool_use_id"], "content": "\n".join(texts) or "(no text)"}
        )
    for data in images:
        # OpenAI tool messages are text-only; a park screenshot rides along
        # as a follow-up user message instead.
        out.append(
            {
                "role": "user",
                "content": [{"type": "image_url", "image_url": {"url": f"data:image/png;base64,{data}"}}],
            }
        )
    return out


class OpenAICompat:
    """Anthropic-messages-shaped facade over an OpenAI chat-completions
    endpoint (vLLM serve, llama.cpp, OpenRouter, ...). compete() only touches
    client.messages.create, response.content, and response.usage, so the tool
    loop stays identical across lanes. Named and required tool_choice both map
    onto the endpoint's structured-output support (vLLM: guided decoding via
    --enable-auto-tool-choice)."""

    def __init__(self, base_url: str, api_key: str, extra_body: dict | None = None):
        import openai

        # A thinking model can legitimately generate for well over the SDK's
        # 10-minute default timeout (131k tokens at ~140 tok/s is ~15 min).
        self._client = openai.OpenAI(base_url=base_url, api_key=api_key, timeout=3600)
        # Endpoint-specific request extras, e.g. vLLM's chat_template_kwargs
        # ({"enable_thinking": false} tames reasoning models whose thinking
        # would otherwise exhaust any completion budget on this task).
        self._extra_body = extra_body or {}
        self.messages = self  # so client.messages.create(...) resolves here

    def create(self, *, model: str, max_tokens: int, system: str, messages: list[dict], tools: list[dict], tool_choice: dict) -> _Response:
        payload: list[dict] = [{"role": "system", "content": system}]
        for message in messages:
            payload.extend(_to_openai(message))
        oa_tools = [
            {
                "type": "function",
                "function": {"name": t["name"], "description": t.get("description", ""), "parameters": t["input_schema"]},
            }
            for t in tools
        ]
        oa_choice: str | dict = (
            {"type": "function", "function": {"name": tool_choice["name"]}}
            if tool_choice.get("type") == "tool"
            else "required"
        )
        # tool_choice="required" is not airtight in the wild: vLLM's guided
        # grammar can emit an empty call array, so a callless response gets
        # retried rather than killing the run.
        input_tokens = output_tokens = 0
        for attempt in range(3):
            resp = self._client.chat.completions.create(
                model=model,
                max_tokens=max_tokens,
                messages=payload,
                tools=oa_tools,
                tool_choice=oa_choice,
                extra_body=self._extra_body,
            )
            if resp.usage:
                input_tokens += resp.usage.prompt_tokens
                output_tokens += resp.usage.completion_tokens
            choice = resp.choices[0].message
            content: list = []
            # The SDK model keeps unknown fields; reasoning arrives as an
            # extra ("reasoning" on vLLM, "reasoning_content" on some stacks).
            extra = choice.model_dump() if hasattr(choice, "model_dump") else {}
            reasoning = extra.get("reasoning") or extra.get("reasoning_content")
            if reasoning:
                content.append(ReasoningBlock(text=reasoning))
            if choice.content:
                content.append(TextBlock(text=choice.content))
            for call in choice.tool_calls or []:
                try:
                    args = json.loads(call.function.arguments or "{}")
                except json.JSONDecodeError:
                    args = {}
                content.append(ToolUseBlock(id=call.id, name=call.function.name, input=args))
            if any(b.type == "tool_use" for b in content):
                return _Response(content=content, usage=_Usage(input_tokens, output_tokens))
            if resp.choices[0].finish_reason == "length":
                # Deterministic, so retrying just burns tokens: the model (a
                # reasoning model, usually) hit the token ceiling while still
                # thinking and never got to the call.
                where = ""
                if FAILED_CALL_DIR is not None:
                    dump = FAILED_CALL_DIR / f"failed-call-{model.replace('/', '_')}.json"
                    dump.write_text(json.dumps(resp.model_dump(), indent=1))
                    where = f"; full response (thinking trace included) saved to {dump}"
                raise RuntimeError(
                    f"{model} exhausted max_tokens={max_tokens} before emitting a tool call "
                    f"(reasoning models spend the budget thinking first; raise --max-tokens){where}"
                )
            print(f"  [{model}] no tool call (attempt {attempt + 1}/3), retrying", flush=True)
        raise RuntimeError(f"{model} returned no tool call in 3 attempts despite tool_choice={oa_choice!r}")


def compete(
    client: anthropic.Anthropic | anthropic.AnthropicVertex | OpenAICompat,
    model: str,
    rounds: int,
    scenario: Path,
    run_dir: Path,
    ticks: int,
    ride_type: int,
    library: list[dict] | None = None,
    max_tokens: int = 8000,
) -> Contender:
    contender = Contender(model=model)
    ride_name, _ = ride_type_info(ride_type)
    system_prompt = build_system_prompt(ride_type, scenario) + (LIBRARY_PROMPT if library is not None else "")
    tools = [TOOL, VALIDATE_TOOL] + (LIBRARY_TOOLS if library is not None else [])
    messages: list[dict] = [
        {
            "role": "user",
            "content": f"Design your best {ride_name} (ride_type {ride_type}). Submit your first track program.",
        }
    ]
    for rnd in range(1, rounds + 1):
        # In library mode the model may browse designs first; the last step
        # forces a submission so every round produces an attempt.
        program = None
        tool_use = None
        lookups: list[dict] = []
        round_usage = {"input_tokens": 0, "output_tokens": 0}
        # Two extra forced-submit attempts: named tool_choice is not actually
        # enforced by every endpoint (vLLM + poolside_v1 returned a different
        # tool than the one forced), so the "guaranteed" final step isn't.
        for step in range(MAX_LOOKUPS_PER_ROUND + 3):
            force_submit = step >= MAX_LOOKUPS_PER_ROUND
            response = client.messages.create(
                model=model,
                max_tokens=max_tokens,
                system=system_prompt,
                messages=messages,
                tools=tools,
                tool_choice=(
                    {"type": "tool", "name": "submit_track_program"} if force_submit else {"type": "any"}
                ),
            )
            round_usage["input_tokens"] += response.usage.input_tokens
            round_usage["output_tokens"] += response.usage.output_tokens
            tool_use = next(b for b in response.content if b.type == "tool_use")
            messages.append({"role": "assistant", "content": response.content})
            if tool_use.name == "submit_track_program":
                program = tool_use.input
                break
            if tool_use.name == "validate_track_program":
                result = validate_program(tool_use.input, scenario)
                print(f"  [{model}] round {rnd}: validate -> {result[:120]}", flush=True)
            else:
                print(f"  [{model}] round {rnd}: {tool_use.name}({json.dumps(tool_use.input)})", flush=True)
                result, lookup = library_tool_result(tool_use.name, tool_use.input, library or [])
                lookups.append(lookup)
            if step + 1 >= MAX_LOOKUPS_PER_ROUND:
                # Named tool_choice is advisory on some stacks, so forcing has
                # to happen in-band too: agentic models otherwise keep
                # validating forever instead of ever submitting.
                result += (
                    "\n\nVALIDATION BUDGET EXHAUSTED: you must now call "
                    "submit_track_program with your best current program. Do not "
                    "call any other tool."
                )
            messages.append(
                {
                    "role": "user",
                    "content": [{"type": "tool_result", "tool_use_id": tool_use.id, "content": result}],
                }
            )
        if program is None or tool_use is None:
            # Three forced-submit attempts all returned something else.
            raise RuntimeError(f"{model} never submitted a program in round {rnd}")
        if program.get("ride_type") != ride_type:
            print(
                f"  [{model}] round {rnd}: submitted ride_type {program.get('ride_type')}, forcing {ride_type}",
                flush=True,
            )
            program["ride_type"] = ride_type
        print(f"  [{model}] round {rnd}: {len(program.get('pieces', []))} pieces submitted", flush=True)

        round_dir = run_dir / model.replace("/", "_") / f"round_{rnd}"
        report, shot = run_eval(program, scenario, round_dir, ticks)
        attempt = Attempt(round=rnd, program=program, report=report, screenshot=shot, lookups=lookups)
        contender.attempts.append(attempt)
        if lookups:
            (round_dir / "lookups.json").write_text(json.dumps(lookups, indent=2))
        (round_dir / "usage.json").write_text(
            json.dumps({"harness": "driver-api", "model": model, **round_usage}, indent=2)
        )
        print(f"  [{model}] round {rnd}: {attempt.summary}", flush=True)

        messages.append(
            {
                "role": "user",
                "content": [
                    {"type": "tool_result", "tool_use_id": tool_use.id, "content": feedback_content(attempt)}
                ],
            }
        )
        prune_history(messages)
    return contender


def main() -> int:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--models", nargs="+", default=["claude-fable-5", "claude-sonnet-5"])
    parser.add_argument(
        "--mode",
        choices=["design", "library"],
        default="design",
        help="design = from scratch; library = with track design library search (retrieval eval)",
    )
    parser.add_argument("--rounds", type=int, default=6)
    parser.add_argument(
        "--ride-type",
        type=int,
        default=52,
        help="required coaster ride type for the competition (52 wooden, 51 steel twister)",
    )
    parser.add_argument("--ticks", type=int, default=25000)
    parser.add_argument(
        "--max-tokens",
        type=int,
        default=8000,
        help="completion token budget per request; reasoning models think before "
        "they call tools, so give them room (e.g. 24000 for Laguna)",
    )
    parser.add_argument("--scenario", type=Path, default=DEFAULT_SCENARIO)
    parser.add_argument("--vertex", action="store_true", help="use Google Vertex AI instead of the first-party API")
    parser.add_argument(
        "--base-url",
        help="OpenAI-compatible endpoint (e.g. a vLLM server: http://localhost:8000/v1); "
        "auth from $OPENAI_API_KEY, defaulting to 'EMPTY' for local servers",
    )
    parser.add_argument(
        "--schematic-feedback",
        action="store_true",
        help="attach the asset-free schematic track diagram to round feedback "
        "(multimodal contenders only; text-only models will reject image content)",
    )
    parser.add_argument(
        "--chat-template-kwargs",
        help="JSON merged into each request as vLLM chat_template_kwargs "
        "(OpenAI lane only), e.g. '{\"enable_thinking\": false}'",
    )
    parser.add_argument(
        "--no-graphics",
        action="store_true",
        help="run the game without RCT2 assets (design mode only): no screenshots in "
        "feedback and no stock library, so the similarity penalty is inert",
    )
    parser.add_argument(
        "--project",
        default=os.environ.get("ANTHROPIC_VERTEX_PROJECT_ID"),
        help="GCP project id (default: $ANTHROPIC_VERTEX_PROJECT_ID)",
    )
    parser.add_argument(
        "--region",
        default=os.environ.get("CLOUD_ML_REGION", "global"),
        help="Vertex region (default: $CLOUD_ML_REGION or global)",
    )
    parser.add_argument(
        "--name",
        help="run dir suffix: evals/runs/<yyyymmdd>-<name> instead of a bare timestamp "
        "(names must sort lexically-chronologically; the site keys on it)",
    )
    args = parser.parse_args()

    if not CLI.exists():
        print(f"error: {CLI} not built", file=sys.stderr)
        return 1
    if args.no_graphics:
        if args.mode == "library":
            print("error: library mode needs the RCT2 track designs; --no-graphics is design mode only", file=sys.stderr)
            return 1
        global NO_GRAPHICS
        NO_GRAPHICS = True
        if args.schematic_feedback:
            global SCHEMATIC_FEEDBACK
            SCHEMATIC_FEEDBACK = True
        if args.scenario == DEFAULT_SCENARIO:
            # The graphics-lane default lives in the RCT2 install; assetless
            # runs default to the checked-in test park instead.
            args.scenario = CI_SCENARIO
    if args.vertex and args.base_url:
        print("error: pick one of --vertex and --base-url", file=sys.stderr)
        return 1
    if not args.scenario.exists():
        print(f"error: scenario not found: {args.scenario}", file=sys.stderr)
        return 1

    suffix = args.name if args.name else time.strftime("%H%M%S")
    run_dir = REPO / "evals" / "runs" / f"{time.strftime('%Y%m%d')}-{suffix}"
    run_dir.mkdir(parents=True)
    global FAILED_CALL_DIR
    FAILED_CALL_DIR = run_dir
    print(f"run dir: {run_dir} (mode: {args.mode})")

    library = None
    if args.mode == "library":
        library = dump_library(args.scenario, run_dir)
        print(f"track design library: {len(library)} designs")
        ensure_library_previews(args.scenario)

    (run_dir / "run.json").write_text(
        json.dumps(
            {
                "mode": args.mode,
                "harness": "driver-api",
                "models": args.models,
                "rounds": args.rounds,
                "ticks": args.ticks,
                "ride_type": args.ride_type,
                "scenario": args.scenario.name,
                "no_graphics": args.no_graphics,
                **({"endpoint": args.base_url} if args.base_url else {}),
                # The site reads the penalty parameters from here; keep the
                # driver the single source of truth for the scoring math.
                "similarity_grace": SIMILARITY_GRACE,
            },
            indent=2,
        )
    )

    if args.base_url:
        extra_body = (
            {"chat_template_kwargs": json.loads(args.chat_template_kwargs)} if args.chat_template_kwargs else None
        )
        client = OpenAICompat(args.base_url, os.environ.get("OPENAI_API_KEY", "EMPTY"), extra_body)
    elif args.vertex:
        # Auth is GCP application-default credentials, not an Anthropic key.
        kwargs = {"region": args.region}
        if args.project:
            kwargs["project_id"] = args.project
        client = anthropic.AnthropicVertex(**kwargs)
    else:
        client = anthropic.Anthropic()
    contenders = [
        compete(client, model, args.rounds, args.scenario, run_dir, args.ticks, args.ride_type, library, args.max_tokens)
        for model in args.models
    ]

    print("\n=== FINAL STANDINGS ===")
    ranked = sorted(contenders, key=lambda c: c.best.excitement if c.best else 0.0, reverse=True)
    for place, contender in enumerate(ranked, 1):
        best = contender.best
        if best is None:
            print(f"{place}. {contender.model}: no successful coaster")
        else:
            print(f"{place}. {contender.model}: excitement {best.excitement:.2f} (round {best.round}) — {best.summary}")
    (run_dir / "standings.json").write_text(
        json.dumps(
            {
                "mode": args.mode,
                "standings": [
                    {
                        "model": c.model,
                        "best_excitement": c.best.excitement if c.best else None,
                        "best_raw_excitement": c.best.raw_excitement if c.best else None,
                        "best_similarity": c.best.similarity if c.best else None,
                        "attempts": [
                            {"round": a.round, "summary": a.summary, "lookups": a.lookups} for a in c.attempts
                        ],
                    }
                    for c in ranked
                ],
            },
            indent=2,
        )
    )
    return 0


if __name__ == "__main__":
    sys.exit(main())
