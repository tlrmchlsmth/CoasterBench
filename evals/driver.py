# /// script
# requires-python = ">=3.11"
# dependencies = ["anthropic[vertex]>=0.40", "openai>=1.40", "pillow>=10"]
# ///
"""Park-building head-to-head: models compete on a declared eval goal.

Each round the model submits a plan (via forced tool use); the harness runs
`openrct2-cli eval` on a fresh copy of the scenario, then feeds back the
eval report and a park screenshot. Best score across rounds wins.

Three goals (--goal, recorded in run.json; scenario x goal compose freely):
  best-coaster       (default) — design a coaster from scratch; score is the
                     tested ride's excitement, similarity-penalized.
  finish-the-coaster — the harness builds a committed track prefix (station,
                     lift, first stretch; return path missing) and the model
                     must close the circuit within a piece budget. Score is
                     excitement; only the model's continuation counts in the
                     similarity check.
  guest-services     — a park with paths and guests; the model places stalls
                     and sets prices. Score = stall profit (dollars) + park
                     rating delta over the simulated ticks. Needs a scenario
                     with guests flowing.
  max-vomit          — the model's coaster is OPENED in a guest-filled park and
                     the score is the increase in vomit piles on the ground
                     (engine-counted Litter entities). High nausea fills
                     stomachs; too-high intensity empties queues. Needs a
                     scenario with guests flowing.

Two modes (coaster goals only):
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
import copy
import json
import os
import re
import statistics
import subprocess
import sys
import tempfile
import time
from concurrent.futures import ThreadPoolExecutor
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
# Multi-scenario runs: committed seed list; parks regenerate deterministically
# per seed (same seed, same bytes) into a gitignored cache.
SEEDS_FILE = REPO / "evals" / "scenarios" / "seeds.json"
GENERATED_DIR = REPO / "evals" / "scenarios" / "generated"

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


DIRECTIONS_LINE = "Directions: dir 0 faces -x, dir 1 faces +y, dir 2 faces +x, dir 3 faces -y."


def map_line_from_hints(hints: dict) -> str:
    """Renders the generator's machine hints sidecar into the prompt map line."""
    size = hints["map_size"]
    playable = hints["playable"]
    start = hints["start"]
    square = start.get("open_square", 0)
    anchor = f"around tile ({start['x']}, {start['y']})"
    if square:
        anchor = f"a {square}x{square}-tile dry square centred on tile ({start['x']}, {start['y']})"
    parts = [
        f"Generated park, {size}x{size} tiles (playable {playable['min']}-{playable['max']} on both axes).",
        f"The most open ground is {anchor} — anchor your station there and grow the layout outward, "
        "using validation errors to feel out the terrain.",
    ]
    hilliness = hints.get("hilliness", 0)
    if hilliness == 0:
        parts.append("The terrain is entirely flat.")
    else:
        surface = hints.get("surface_z", {})
        roughness = "gently rolling" if hilliness <= 3 else "hilly" if hilliness <= 6 else "mountainous"
        parts.append(
            f"Terrain is {roughness}: surface heights span {surface.get('min')}-{surface.get('max')} z-units "
            "(one up_25 piece climbs 16 z-units), so expect placement errors on slopes and re-route around them."
        )
    water = hints.get("water_fraction", 0.0)
    if water > 0.01:
        parts.append(
            f"About {round(water * 100)}% of the map is under water; you cannot build below the waterline, "
            "and placement errors will say when water is in the way."
        )
    parts.append(DIRECTIONS_LINE)
    return " ".join(parts)


def scenario_map_line(scenario: Path) -> str:
    hints_path = scenario.with_suffix(".hints.json")
    if hints_path.exists():
        return map_line_from_hints(json.loads(hints_path.read_text()))
    return MAP_LINES.get(
        scenario.name,
        "Terrain unknown; flat grass around tile (60, 60) is a reasonable first bet. Use validation "
        f"errors to find open ground. {DIRECTIONS_LINE}",
    )


def build_system_prompt(ride_type: int, scenario: Path) -> str:
    _, ride_line = ride_type_info(ride_type)
    return SYSTEM_PROMPT.replace("{RIDE_TYPE_LINE}", ride_line).replace("{MAP_LINE}", scenario_map_line(scenario))


# Shared by every coaster goal (best-coaster and finish-the-coaster).
TRACK_GEOMETRY = f"""## Rules of track geometry
- Pieces chain sequentially from a cursor (position + facing direction). Each piece moves/rotates the cursor.
- The track must form a CLOSED CIRCUIT: the last piece must end exactly where the first begins, facing the same direction, at the same height. Total up-slope pieces must equal total down-slope pieces of the same steepness.
- A trick for closure: any identical piece sequence ending in a 90-degree turn, repeated 4 times, closes a rectangle.
- Start with begin_station, middle_station, end_station (station must be on flat ground, 3-7 pieces).
- up_25 rises 16 z-units per piece; up_60 rises 48. You cannot go below the starting height (the ground).
- Use {{"t": "up_25", "chain": true}} for chain lift hill pieces (needed to climb; trains start slow!). Chain lifts only work on 25-degree slopes, never on 60-degree pieces.
- Banking must be entered and exited: flat_to_left_bank ... left_bank ... left_bank_to_flat.
- Sloped pieces cannot be banked. Transitions matter: up_25 cannot follow flat directly, use flat_to_up_25.
- The train coasts on gravity after the lift. If it stalls (too little energy for a hill), the test fails or takes forever. Drops give speed; friction bleeds it.

## Piece catalog
{PIECE_CATALOG}

## Map
{{MAP_LINE}}"""

COASTER_SCORING = """## Scoring (from the real game engine)
Excitement is primary (higher wins). It rewards: drops, speed, airtime, direction changes, banked turns, length. Intensity above ~10 tanks excitement (guests won't ride); keep intensity under 10.00. Crashes disqualify."""

SYSTEM_PROMPT = f"""You are competing to design the best RollerCoaster Tycoon 2 roller coaster.
You submit a "track program": a ride type, a start tile, and an ordered list of track pieces.
The game engine builds it piece by piece, tests it with a real train, and rates it.

{TRACK_GEOMETRY}

## Ride types
{{RIDE_TYPE_LINE}}

{COASTER_SCORING}

Your track is also compared against the stock RCT2 track design library (mirrored variants included). Similarity up to 0.5 is free; above that your excitement is scaled down linearly, reaching zero for an exact copy. Design something original; reproducing a stock coaster from memory scores nothing.

Before submitting, use the validate_track_program tool (same payload) to dry-run your program: it reports placement errors with the exact piece index, or whether the circuit closes, without spending your round. You get a limited number of validations per round, use them to fix geometry, then submit.

Submit via the submit_track_program tool. After each attempt you get the eval report (placement errors with exact piece index, or ride stats) and a park screenshot. Iterate and maximise excitement."""

# finish-the-coaster: {PREFIX_*} placeholders are filled per run by the goal.
FINISH_SYSTEM_PROMPT = f"""You are competing to FINISH a partially built RollerCoaster Tycoon 2 roller coaster.
The harness has already committed a track prefix — station, lift hill, and the first stretch — but the return path is missing. You submit only the CONTINUATION: an ordered list of pieces that carries the track from the current cursor back to the station and closes the circuit. The game engine then builds prefix + continuation, tests the ride with a real train, and rates it.

{TRACK_GEOMETRY}

## Ride types
{{RIDE_TYPE_LINE}}

## The committed prefix (already built; you cannot change or remove it)
{{PREFIX_JSON}}

The track begins at tile {{PREFIX_START}} and after the prefix the cursor stands at {{PREFIX_END}} (z is height units above sea level, bank 0 = unbanked, slope 0 = flat). Your continuation starts exactly there and must return the cursor to the start tile at the start height, facing the start direction, unbanked and flat.

## Budget
Your continuation may use at most {{PREFIX_BUDGET}} pieces.

{COASTER_SCORING}

Only YOUR continuation pieces are compared against the stock design library for the similarity penalty (up to 0.5 free, scaled to zero at an exact copy); the committed prefix is not held against you.

Before submitting, use the validate_track_completion tool (same payload) to dry-run: the harness prepends the prefix and reports placement errors with the exact continuation piece index, or whether the circuit closes, without spending your round. Validations per round are limited.

Submit via the submit_track_completion tool. After each attempt you get the eval report and a park screenshot. Iterate and maximise excitement."""

VOMIT_SYSTEM_PROMPT = f"""You are competing to build the most SICKENING RollerCoaster Tycoon 2 roller coaster: the winner is the one whose riders throw up the most.
You submit a "track program": a ride type, a start tile, and an ordered list of track pieces.
The park is live, with real guests walking real footpaths. The harness builds your track, OPENS the ride, and lets guests queue, board, ride, and stagger off. Every pile of vomit they leave on the ground is counted by the game engine.

{TRACK_GEOMETRY}

## Ride types
{{RIDE_TYPE_LINE}}

## How vomit works (real game mechanics)
- Nausea makes riders sick, and sick guests vomit onto the paths shortly after disembarking. Nausea comes from helices, tight turns, rapid direction changes, and sustained lateral G-forces (unbanked turns at speed).
- A FULL STOMACH DOUBLES NAUSEA: the engine scales ride nausea by the guest's fullness, from +0% at half-full up to +100% completely full. Vomiting halves the stomach again, so without food nearby a guest's best vomit is their first.
- Guests check the ride's INTENSITY rating against their own tolerance before boarding. A ride so intense nobody dares queue produces zero riders and zero vomit. The sweet spot is maximum nausea at an intensity guests still accept.
- Throughput matters: more riders per hour means more stomachs emptied. Guests also refuse to re-ride when too hungry, too thirsty, or still queasy, so refreshments sustain the loop.
- Guests must be able to REACH the ride: place the station beside the park's footpaths. The harness auto-places the entrance and exit on tiles next to the station.
- Crashes close the ride. A closed ride collects no vomit.

## Stalls (optional, recommended)
Your submission may include a "stalls" array to build food and drink alongside the coaster: {{"stalls": [{{"stall_type": "food", "x": 62, "y": 60, "dir": 1, "price": 1.5}}]}}. Types: food, drink, shop, information_kiosk, toilets, cash_machine, first_aid. A stall occupies one tile; its facing side (dir: 0=-x, 1=+y, 2=+x, 3=-y) is the door and must touch a footpath. Stalls are built AFTER the track; a stall rejection reports an index continuing past your track pieces. The classic play: food by the ride exit to refill stomachs, drink to keep guests re-riding. (Stall litter is not vomit; only actual vomit counts.)

## Scoring
score = the number of times guests throw up during the simulated period, counted by the engine at the moment of the act. Every heave counts, even ones handymen sweep away afterwards. Build failures, open circuits, and rejected stalls score nothing.

Before submitting, use the validate_track_program tool (same payload) to dry-run your program: it reports placement errors with the exact index, or whether the circuit closes, without spending your round. You get a limited number of validations per round.

Submit via the submit_track_program tool. After each attempt you get the eval report (vomit count, ride ratings, guest count, per-stall sales) and a park screenshot — look for the pale green-grey piles near your exit. Iterate and maximise vomit."""

GUEST_SYSTEM_PROMPT = """You are competing to run the best guest services in a live RollerCoaster Tycoon 2 park.
The park already has footpaths, rides, and guests walking around. You submit a "stall plan": a list of stalls to build, each with a type, a tile, a facing, and an item price. The harness builds and opens them all, simulates the park, and measures what your stalls earned and what happened to the park rating.

## Rules of stall placement
- Stall types: food, drink, shop (souvenirs), information_kiosk (park maps/umbrellas), toilets, cash_machine, first_aid.
- A stall occupies one tile of buildable land. Its facing side (dir) is the door: guests can only enter from that side, so it must directly touch a footpath tile. Directions: dir 0 faces -x, dir 1 faces +y, dir 2 faces +x, dir 3 faces -y.
- Placement is validated by the real game (terrain, clearance, ownership); the first rejected stall aborts the plan at that index.
- price is the primary item price in dollars (e.g. 1.50). Omit it to keep the stall's default. Guests refuse prices they consider a rip-off, and cheap essentials (toilets!) draw crowds.
- Guests get hungry, thirsty, and desperate over time; coverage near busy paths and ride exits earns the most. Toilets and information kiosks earn little directly but lift guest happiness — and the park rating.

## Scoring
score = combined lifetime profit of YOUR stalls in dollars + the park rating change (0-999 scale) over the simulated period. Both are read from the game engine.

Study the park screenshot to find the paths and where guests cluster. Use the validate_stall_plan tool (same payload) to dry-run placement without spending your round; validations per round are limited. Submit via the submit_stall_plan tool. After each attempt you get the eval report (per-stall profit and customers, park rating, guest count) and a fresh screenshot. Iterate and maximise your score."""

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

# finish-the-coaster: the model sends only the continuation pieces; the
# harness prepends the committed prefix before running the eval.
COMPLETION_SCHEMA = {
    "type": "object",
    "required": ["pieces"],
    "properties": {
        "pieces": TOOL["input_schema"]["properties"]["pieces"],
    },
}

FINISH_TOOL = {
    "name": "submit_track_completion",
    "description": "Submit the continuation pieces that close the committed prefix into a full circuit. "
    "The harness prepends the prefix, builds the whole track, tests it, and rates it.",
    "input_schema": COMPLETION_SCHEMA,
}

FINISH_VALIDATE_TOOL = {
    "name": "validate_track_completion",
    "description": "Dry-run a continuation: the harness prepends the committed prefix and builds the "
    "whole track, reporting placement errors (with exact piece index into the full program) or whether "
    "the circuit closes, WITHOUT spending your round. Same payload as submit_track_completion.",
    "input_schema": COMPLETION_SCHEMA,
}

# guest-services: a stall plan instead of a track program.
STALL_PLAN_SCHEMA = {
    "type": "object",
    "required": ["stalls"],
    "properties": {
        "stalls": {
            "type": "array",
            "minItems": 1,
            "items": {
                "type": "object",
                "required": ["stall_type", "x", "y", "dir"],
                "properties": {
                    "stall_type": {
                        "type": "string",
                        "enum": [
                            "food",
                            "drink",
                            "shop",
                            "information_kiosk",
                            "toilets",
                            "cash_machine",
                            "first_aid",
                        ],
                    },
                    "x": {"type": "integer"},
                    "y": {"type": "integer"},
                    "dir": {"type": "integer", "minimum": 0, "maximum": 3},
                    "price": {"type": "number", "description": "item price in dollars, e.g. 1.5"},
                },
            },
        },
    },
}

STALL_TOOL = {
    "name": "submit_stall_plan",
    "description": "Submit the stall plan to build, open, and simulate.",
    "input_schema": STALL_PLAN_SCHEMA,
}

STALL_VALIDATE_TOOL = {
    "name": "validate_stall_plan",
    "description": "Dry-run a stall plan: builds it in the game and reports the first placement "
    "rejection (with its index) or that every stall placed, WITHOUT spending your round. Same "
    "payload as submit_stall_plan.",
    "input_schema": STALL_PLAN_SCHEMA,
}

# max-vomit: a track program that may also carry stalls. The harness builds
# the track first, then the stalls around it; a fed rider vomits twice as
# hard, so the two submit together as one plan.
VOMIT_PROGRAM_SCHEMA = {
    "type": "object",
    "required": ["ride_type", "start", "pieces"],
    "properties": {
        **TOOL["input_schema"]["properties"],
        "stalls": STALL_PLAN_SCHEMA["properties"]["stalls"],
    },
}

VOMIT_TOOL = {
    "name": "submit_track_program",
    "description": "Submit the coaster track program to build and open to guests, optionally "
    "with a stall plan built alongside it (stalls place after the track).",
    "input_schema": VOMIT_PROGRAM_SCHEMA,
}

VOMIT_VALIDATE_TOOL = {
    "name": "validate_track_program",
    "description": "Dry-run a track program (with optional stalls): builds everything in the game "
    "and reports placement errors with the exact index — stall indices continue past the track "
    "pieces — or whether the circuit closes, WITHOUT spending your round.",
    "input_schema": VOMIT_PROGRAM_SCHEMA,
}


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
    goal: "Goal"
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
    def score(self) -> float | None:
        """The goal's scalar for ranking; None when the attempt produced
        nothing scoreable (build failed, ride never rated)."""
        return self.goal.score(self)

    @property
    def build_failure(self) -> str | None:
        prog = self.report.get("program") or {}
        if prog.get("ok"):
            return None
        err = (prog.get("error") or {}).get("message", "unknown error")
        placed = prog.get("pieces_placed", 0)
        total = prog.get("pieces_total", 0)
        idx = (prog.get("error") or {}).get("piece_index")
        where = f" at piece {idx}" if idx is not None else ""
        return f"BUILD FAILED{where} ({placed}/{total} placed): {err}"

    @property
    def summary(self) -> str:
        return self.goal.summary(self)


@dataclass
class Contender:
    model: str
    attempts: list[Attempt] = field(default_factory=list)

    @property
    def best(self) -> Attempt | None:
        rated = [a for a in self.attempts if a.score is not None]
        return max(rated, key=lambda a: a.score) if rated else None


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


def run_eval(
    program: dict, scenario: Path, workdir: Path, ticks: int, xray: bool = True
) -> tuple[dict, Path | None]:
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
        cmd += ["--capture", str(capture_path)]
        # The x-ray view hides terrain so every track piece shows; for stall
        # goals the normal view is the useful one (paths and guests must stay
        # visible).
        if xray:
            cmd.append("--capture-xray")
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


def dry_run(program: dict, scenario: Path) -> dict:
    """Runs a program for a few ticks and returns the report's `program`
    outcome; placement and circuit closure are checked at build time, before
    any real simulation."""
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
            return {}
        return json.loads(report_path.read_text()).get("program", {})


def validate_program(
    program: dict,
    scenario: Path,
    ok_note: str = "placement OK and circuit closed; ready to submit",
) -> str:
    """Placement + circuit-closure dry run, formatted as a tool result."""
    prog = dry_run(program, scenario)
    if not prog:
        return json.dumps({"ok": False, "error": "eval crashed"})
    if prog.get("ok"):
        return json.dumps({"ok": True, "note": ok_note})
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


def ensure_generated_park(seed: int) -> Path:
    """Generates (or reuses) the deterministic park for a seed.

    Same seed always gives the same bytes, so the gitignored cache never goes
    stale; --make-park needs no RCT2 assets regardless of the run's mode.
    """
    park = GENERATED_DIR / f"seed_{seed}.park"
    hints = park.with_suffix(".hints.json")
    if park.exists() and hints.exists():
        return park
    GENERATED_DIR.mkdir(parents=True, exist_ok=True)
    cmd = [str(CLI), "eval", "--make-park", str(park), "--seed", str(seed)]
    proc = subprocess.run(cmd, capture_output=True, text=True, timeout=300)
    if not park.exists() or not hints.exists():
        raise RuntimeError(f"park generation failed for seed {seed}: {proc.stderr[-500:]}")
    return park


def load_seeds(count: int) -> list[int]:
    seeds = json.loads(SEEDS_FILE.read_text())["seeds"]
    if count > len(seeds):
        raise SystemExit(f"error: --scenarios {count} but {SEEDS_FILE} only lists {len(seeds)} seeds")
    return seeds[:count]


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


def image_block(path: Path) -> dict:
    return {
        "type": "image",
        "source": {
            "type": "base64",
            "media_type": "image/png",
            "data": base64.b64encode(path.read_bytes()).decode(),
        },
    }


def feedback_content(attempt: Attempt) -> list[dict]:
    content: list[dict] = [
        {
            "type": "text",
            "text": (
                f"Round {attempt.round} result: {attempt.summary}\n\n"
                f"Full report:\n{json.dumps(attempt.report, indent=1)}\n\n"
                + attempt.goal.feedback_instruction()
            ),
        }
    ]
    if attempt.screenshot is not None:
        content.append(image_block(attempt.screenshot))
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
def coaster_summary(attempt: Attempt) -> str:
    """One-line result for a track-building attempt."""
    if attempt.build_failure is not None:
        return attempt.build_failure
    rides = attempt.report.get("rides", [])
    if not rides:
        return "built but no ride data"
    r = rides[0]
    text = (
        f"excitement={r.get('excitement')} intensity={r.get('intensity')} nausea={r.get('nausea')} "
        f"tested={r.get('tested')} crashed={r.get('crashed')} length={r.get('ride_length')} "
        f"drops={r.get('num_drops')} airtime={r.get('total_air_time')}"
    )
    sim = attempt.report.get("similarity") or {}
    if sim:
        text += f" similarity={sim.get('similarity', 0.0):.2f} (nearest: {sim.get('nearest_design')})"
    if similarity_multiplier(attempt.similarity) < 1.0:
        text += f" -> penalized excitement {attempt.excitement:.2f}"
    return text


# --- Goal registry ----------------------------------------------------------
#
# A goal declares the objective of a run: what the model is prompted to do,
# which submit/validate tools it gets, how a submission becomes an eval
# program, and how the report is scored. Goals compose with scenarios (any
# goal can run on any suitable park) and, for coaster goals, with library
# mode. run.json records the goal so standings and the site never rank
# different goals against each other.


class BestCoasterGoal:
    """The original objective: design the highest-excitement coaster."""

    name = "best-coaster"
    submit_tool = "submit_track_program"
    validate_tool = "validate_track_program"
    capture_xray = True
    supports_library = True

    def __init__(self, args, scenario: Path):
        self.ride_type = args.ride_type
        self.scenario = scenario
        # Filled by main() in library mode; tools/prompt react to it.
        self.library: list[dict] | None = None

    def setup(self, run_dir: Path) -> None:
        pass

    def config(self) -> dict:
        """Goal-specific parameters worth recording in run.json."""
        return {}

    def system_prompt(self) -> str:
        prompt = build_system_prompt(self.ride_type, self.scenario)
        return prompt + (LIBRARY_PROMPT if self.library is not None else "")

    def tools(self) -> list[dict]:
        return [TOOL, VALIDATE_TOOL] + (LIBRARY_TOOLS if self.library is not None else [])

    def opening_content(self) -> str | list[dict]:
        ride_name, _ = ride_type_info(self.ride_type)
        return f"Design your best {ride_name} (ride_type {self.ride_type}). Submit your first track program."

    def validate(self, tool_input: dict) -> str:
        return validate_program(tool_input, self.scenario)

    def build_program(self, tool_input: dict, model: str, rnd: int) -> tuple[dict | None, str | None]:
        """Submission -> program dict for the CLI, or a rejection message."""
        program = tool_input
        if program.get("ride_type") != self.ride_type:
            print(
                f"  [{model}] round {rnd}: submitted ride_type {program.get('ride_type')}, forcing {self.ride_type}",
                flush=True,
            )
            program["ride_type"] = self.ride_type
        return program, None

    def describe_submission(self, program: dict) -> str:
        return f"{len(program.get('pieces', []))} pieces submitted"

    def score(self, attempt: Attempt) -> float | None:
        return attempt.excitement if attempt.raw_excitement > 0 else None

    def summary(self, attempt: Attempt) -> str:
        return coaster_summary(attempt)

    def feedback_instruction(self) -> str:
        return "Revise your design and submit again. Aim for higher excitement (intensity < 10, no crashes)."

    def metrics(self, attempt: Attempt) -> dict:
        return {
            "excitement": attempt.excitement,
            "raw_excitement": attempt.raw_excitement,
            "similarity": attempt.similarity,
        }


class FinishCoasterGoal(BestCoasterGoal):
    """Circuit closure under a budget: the harness commits a prefix (station,
    lift, first stretch — no return path) and the model must bring it home.
    Directly targets the universal failure mode of from-scratch runs."""

    name = "finish-the-coaster"
    submit_tool = "submit_track_completion"
    validate_tool = "validate_track_completion"

    DEFAULT_BUDGET = 80

    def __init__(self, args, scenario: Path):
        super().__init__(args, scenario)
        self.prefix_path: Path = args.prefix or (
            REPO / "evals" / "programs" / f"finish_prefix_{self.ride_type}.json"
        )
        self.prefix: dict = {}
        self.budget: int = self.DEFAULT_BUDGET
        self.end_cursor: dict = {}

    def setup(self, run_dir: Path) -> None:
        """Loads the committed prefix and dry-runs it: the game is the oracle
        for both prefix validity and the cursor the model must pick up from."""
        if not self.prefix_path.exists():
            raise RuntimeError(
                f"no prefix program for ride type {self.ride_type}: {self.prefix_path}"
            )
        self.prefix = json.loads(self.prefix_path.read_text())
        self.budget = int(self.prefix.get("completion_budget", self.DEFAULT_BUDGET))
        prefix_program = {k: self.prefix[k] for k in ("ride_type", "start", "pieces")}
        outcome = dry_run(prefix_program, self.scenario)
        placed = outcome.get("pieces_placed", 0)
        if placed != len(self.prefix["pieces"]):
            err = (outcome.get("error") or {}).get("message", "no report")
            raise RuntimeError(
                f"prefix program failed to build ({placed}/{len(self.prefix['pieces'])} placed): {err}"
            )
        # The prefix is deliberately unclosed, so the outcome reports a
        # closure failure; end_cursor is where the model must continue from.
        if not outcome.get("end_cursor"):
            raise RuntimeError("eval CLI reported no end_cursor; rebuild openrct2-cli")
        self.end_cursor = outcome["end_cursor"]

    def config(self) -> dict:
        return {
            "prefix_file": self.prefix_path.name,
            "prefix_pieces": len(self.prefix.get("pieces", [])),
            "completion_budget": self.budget,
            "prefix_end_cursor": self.end_cursor,
        }

    def system_prompt(self) -> str:
        _, ride_line = ride_type_info(self.ride_type)
        start = self.prefix["start"]
        prompt = (
            FINISH_SYSTEM_PROMPT.replace("{RIDE_TYPE_LINE}", ride_line)
            .replace("{PREFIX_JSON}", json.dumps(self.prefix["pieces"]))
            .replace(
                "{PREFIX_START}",
                f"(x={start['x']}, y={start['y']}, dir={start['dir']})",
            )
            .replace("{PREFIX_END}", json.dumps(self.end_cursor))
            .replace("{PREFIX_BUDGET}", str(self.budget))
            .replace("{MAP_LINE}", scenario_map_line(self.scenario))
        )
        return prompt + (LIBRARY_PROMPT if self.library is not None else "")

    def tools(self) -> list[dict]:
        return [FINISH_TOOL, FINISH_VALIDATE_TOOL] + (
            LIBRARY_TOOLS if self.library is not None else []
        )

    def opening_content(self) -> str | list[dict]:
        ride_name, _ = ride_type_info(self.ride_type)
        return (
            f"Finish the {ride_name} (ride_type {self.ride_type}): close the circuit from the "
            f"prefix's end cursor back to the station, within {self.budget} pieces. "
            "Submit your first continuation."
        )

    def merged_program(self, pieces: list) -> dict:
        return {
            "ride_type": self.prefix["ride_type"],
            "start": self.prefix["start"],
            "pieces": list(self.prefix["pieces"]) + list(pieces),
            # The committed prefix is the harness's design, not the model's;
            # the similarity penalty applies to the continuation only.
            "similarity_skip": len(self.prefix["pieces"]),
        }

    def over_budget(self, pieces: list) -> str | None:
        if len(pieces) > self.budget:
            return (
                f"continuation uses {len(pieces)} pieces; the budget is {self.budget}. "
                "Submit a shorter continuation."
            )
        return None

    def validate(self, tool_input: dict) -> str:
        pieces = tool_input.get("pieces") or []
        if reason := self.over_budget(pieces):
            return json.dumps({"ok": False, "error": reason})
        return validate_program(self.merged_program(pieces), self.scenario)

    def build_program(self, tool_input: dict, model: str, rnd: int) -> tuple[dict | None, str | None]:
        pieces = tool_input.get("pieces") or []
        if reason := self.over_budget(pieces):
            return None, reason
        return self.merged_program(pieces), None

    def describe_submission(self, program: dict) -> str:
        continuation = len(program.get("pieces", [])) - len(self.prefix.get("pieces", []))
        return f"{continuation} continuation pieces submitted (budget {self.budget})"


class MaxVomitGoal(BestCoasterGoal):
    """Design the ride whose passengers throw up the most. The coaster is
    opened to real guests instead of test-run, and the score is the engine's
    own count of vomit piles added to the ground. Rewards the nausea/intensity
    trade-off: maximum sickness from a ride guests still agree to board."""

    name = "max-vomit"
    # Normal park view: paths, guests, and the vomit itself must be visible.
    capture_xray = False

    def system_prompt(self) -> str:
        _, ride_line = ride_type_info(self.ride_type)
        prompt = VOMIT_SYSTEM_PROMPT.replace("{RIDE_TYPE_LINE}", ride_line).replace(
            "{MAP_LINE}", scenario_map_line(self.scenario)
        )
        return prompt + (LIBRARY_PROMPT if self.library is not None else "")

    def tools(self) -> list[dict]:
        return [VOMIT_TOOL, VOMIT_VALIDATE_TOOL] + (
            LIBRARY_TOOLS if self.library is not None else []
        )

    def opening_content(self) -> str | list[dict]:
        ride_name, _ = ride_type_info(self.ride_type)
        return (
            f"Build your most nauseating {ride_name} (ride_type {self.ride_type}). "
            "It will be opened to the park's guests. Submit your first track program."
        )

    def build_program(self, tool_input: dict, model: str, rnd: int) -> tuple[dict | None, str | None]:
        program, rejection = super().build_program(tool_input, model, rnd)
        if program is not None:
            # Guests only queue for open rides; testing produces no riders
            # and therefore no vomit.
            program["open"] = True
        return program, rejection

    def describe_submission(self, program: dict) -> str:
        text = f"{len(program.get('pieces', []))} pieces submitted"
        if program.get("stalls"):
            text += f" + {len(program['stalls'])} stalls"
        return text

    def score(self, attempt: Attempt) -> float | None:
        """Vomit events (the true cumulative count from the Guest::throwUp
        hook); pile delta as a fallback for reports from older binaries."""
        if attempt.build_failure is not None:
            return None
        park = attempt.report.get("park") or {}
        events = park.get("vomit_events")
        if events is not None:
            return float(events)
        delta = park.get("vomit_delta")
        return float(delta) if delta is not None else None

    def summary(self, attempt: Attempt) -> str:
        if attempt.build_failure is not None:
            return attempt.build_failure
        park = attempt.report.get("park") or {}
        rides = attempt.report.get("rides", [])
        r = rides[0] if rides else {}
        text = (
            f"vomit_events={park.get('vomit_events')} "
            f"(piles on ground: {park.get('vomit_count')}) "
            f"guests={park.get('guests_count')} "
            f"nausea={r.get('nausea')} intensity={r.get('intensity')} excitement={r.get('excitement')}"
        )
        stalls_placed = sum(
            1 for s in park.get("stalls") or [] if s.get("placed_by_program")
        )
        if stalls_placed:
            text += f" stalls={stalls_placed}"
        return text

    def feedback_instruction(self) -> str:
        return (
            "Revise your design and submit again. More nausea, but keep intensity low enough "
            "that guests still board — an empty ride produces no vomit."
        )

    def metrics(self, attempt: Attempt) -> dict:
        park = attempt.report.get("park") or {}
        rides = attempt.report.get("rides", [])
        r = rides[0] if rides else {}
        return {
            "vomit_events": park.get("vomit_events"),
            "vomit_count": park.get("vomit_count"),
            "vomit_delta": park.get("vomit_delta"),
            "guests_count": park.get("guests_count"),
            "nausea": r.get("nausea"),
            "intensity": r.get("intensity"),
            "stalls_placed": sum(
                1 for s in park.get("stalls") or [] if s.get("placed_by_program")
            ),
        }


class GuestServicesGoal:
    """Stall economics on a living park: place stalls, set prices, and be
    scored on stall profit plus the park-rating change over the simulation."""

    name = "guest-services"
    submit_tool = "submit_stall_plan"
    validate_tool = "validate_stall_plan"
    capture_xray = False
    supports_library = False

    def __init__(self, args, scenario: Path):
        self.scenario = scenario
        self.library: list[dict] | None = None
        self.baseline_report: dict = {}
        self.baseline_shot: Path | None = None

    def setup(self, run_dir: Path) -> None:
        """Captures the untouched park once: opening screenshot + baseline
        counters every contender starts from."""
        report, shot = run_eval(
            {"stalls": []}, self.scenario, run_dir / "baseline", ticks=5, xray=False
        )
        park = report.get("park")
        if not park:
            raise RuntimeError(
                "scenario produced no park metrics; rebuild openrct2-cli"
            )
        self.baseline_report = report
        self.baseline_shot = shot

    def config(self) -> dict:
        park = self.baseline_report.get("park") or {}
        return {
            "baseline_park_rating": park.get("park_rating"),
            "baseline_guests": park.get("guests_count"),
        }

    def system_prompt(self) -> str:
        return GUEST_SYSTEM_PROMPT

    def tools(self) -> list[dict]:
        return [STALL_TOOL, STALL_VALIDATE_TOOL]

    def opening_content(self) -> str | list[dict]:
        park = self.baseline_report.get("park") or {}
        text = (
            f"The park is live: rating {park.get('park_rating')}, "
            f"{park.get('guests_count')} guests in the park, "
            f"{len(park.get('stalls') or [])} pre-existing stall(s). "
            "Study the screenshot for paths and guest clusters, then submit your first stall plan."
        )
        content: list[dict] = [{"type": "text", "text": text}]
        if self.baseline_shot is not None:
            content.append(image_block(self.baseline_shot))
        return content

    def validate(self, tool_input: dict) -> str:
        return validate_program(
            tool_input, self.scenario, ok_note="every stall placed OK; ready to submit"
        )

    def build_program(self, tool_input: dict, model: str, rnd: int) -> tuple[dict | None, str | None]:
        return {"stalls": tool_input.get("stalls") or []}, None

    def describe_submission(self, program: dict) -> str:
        return f"{len(program.get('stalls', []))} stalls submitted"

    def score(self, attempt: Attempt) -> float | None:
        park = attempt.report.get("park") or {}
        profit = park.get("stall_profit")
        delta = park.get("park_rating_delta")
        if profit is None and delta is None:
            return None
        return (profit or 0.0) + float(delta or 0)

    def summary(self, attempt: Attempt) -> str:
        if attempt.build_failure is not None:
            return attempt.build_failure
        park = attempt.report.get("park") or {}
        profit = park.get("stall_profit") or 0.0
        placed = sum(1 for s in park.get("stalls") or [] if s.get("placed_by_program"))
        text = (
            f"stalls={placed} profit=${profit:.2f} "
            f"park_rating={park.get('park_rating')} ({(park.get('park_rating_delta') or 0):+d}) "
            f"guests={park.get('guests_count')}"
        )
        if (score := self.score(attempt)) is not None:
            text += f" score={score:.2f}"
        return text

    def feedback_instruction(self) -> str:
        return (
            "Revise your stall plan and submit again. Aim for higher combined stall profit "
            "and park rating."
        )

    def metrics(self, attempt: Attempt) -> dict:
        park = attempt.report.get("park") or {}
        return {
            "stall_profit": park.get("stall_profit"),
            "park_rating": park.get("park_rating"),
            "park_rating_delta": park.get("park_rating_delta"),
            "guests_count": park.get("guests_count"),
        }


Goal = BestCoasterGoal | FinishCoasterGoal | MaxVomitGoal | GuestServicesGoal

GOALS: dict[str, type] = {
    BestCoasterGoal.name: BestCoasterGoal,
    FinishCoasterGoal.name: FinishCoasterGoal,
    MaxVomitGoal.name: MaxVomitGoal,
    GuestServicesGoal.name: GuestServicesGoal,
}


def compete(
    client: anthropic.Anthropic | anthropic.AnthropicVertex | OpenAICompat,
    model: str,
    rounds: int,
    scenario: Path,
    run_dir: Path,
    ticks: int,
    goal: Goal,
    max_tokens: int = 8000,
    scenario_label: str | None = None,
) -> Contender:
    contender = Contender(model=model)
    # Multi-scenario runs nest each scenario's rounds under the model dir and
    # tag log lines so concurrent competes stay readable.
    model_dir = run_dir / model.replace("/", "_")
    rounds_base = model_dir / scenario_label if scenario_label else model_dir
    tag = f"{model} @ {scenario_label}" if scenario_label else model
    # Each scenario park gets its own goal view (prompt map line, validation
    # target); a shallow copy keeps shared state (library, prefix) intact.
    if scenario != goal.scenario:
        goal = copy.copy(goal)
        goal.scenario = scenario
    system_prompt = goal.system_prompt()
    tools = goal.tools()
    messages: list[dict] = [{"role": "user", "content": goal.opening_content()}]
    for rnd in range(1, rounds + 1):
        # The model may validate (and, in library mode, browse designs) first;
        # the last step forces a submission so every round produces an attempt.
        submission = None
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
                    {"type": "tool", "name": goal.submit_tool} if force_submit else {"type": "any"}
                ),
            )
            round_usage["input_tokens"] += response.usage.input_tokens
            round_usage["output_tokens"] += response.usage.output_tokens
            tool_use = next(b for b in response.content if b.type == "tool_use")
            messages.append({"role": "assistant", "content": response.content})
            if tool_use.name == goal.submit_tool:
                submission = tool_use.input
                break
            if tool_use.name == goal.validate_tool:
                result = goal.validate(tool_use.input)
                print(f"  [{tag}] round {rnd}: validate -> {result[:120]}", flush=True)
            else:
                print(f"  [{tag}] round {rnd}: {tool_use.name}({json.dumps(tool_use.input)})", flush=True)
                result, lookup = library_tool_result(tool_use.name, tool_use.input, goal.library or [])
                lookups.append(lookup)
            if step + 1 >= MAX_LOOKUPS_PER_ROUND:
                # Named tool_choice is advisory on some stacks, so forcing has
                # to happen in-band too: agentic models otherwise keep
                # validating forever instead of ever submitting.
                result += (
                    "\n\nVALIDATION BUDGET EXHAUSTED: you must now call "
                    f"{goal.submit_tool} with your best current submission. Do not "
                    "call any other tool."
                )
            messages.append(
                {
                    "role": "user",
                    "content": [{"type": "tool_result", "tool_use_id": tool_use.id, "content": result}],
                }
            )
        if submission is None or tool_use is None:
            # Three forced-submit attempts all returned something else.
            raise RuntimeError(f"{tag} never submitted in round {rnd}")

        program, rejection = goal.build_program(submission, tag, rnd)
        round_dir = rounds_base / f"round_{rnd}"
        if program is None:
            # Rejected before reaching the game (e.g. over the piece budget):
            # feed the reason back as a failed attempt without running an eval.
            round_dir.mkdir(parents=True, exist_ok=True)
            report = {"program": {"ok": False, "error": {"message": rejection}}}
            shot = None
            program = dict(submission)
        else:
            print(f"  [{tag}] round {rnd}: {goal.describe_submission(program)}", flush=True)
            report, shot = run_eval(program, scenario, round_dir, ticks, xray=goal.capture_xray)
        attempt = Attempt(
            round=rnd, program=program, report=report, screenshot=shot, goal=goal, lookups=lookups
        )
        contender.attempts.append(attempt)
        if lookups:
            (round_dir / "lookups.json").write_text(json.dumps(lookups, indent=2))
        (round_dir / "usage.json").write_text(
            json.dumps({"harness": "driver-api", "model": model, **round_usage}, indent=2)
        )
        print(f"  [{tag}] round {rnd}: {attempt.summary}", flush=True)

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
        "--goal",
        choices=sorted(GOALS),
        default=BestCoasterGoal.name,
        help="eval objective for the run (recorded in run.json; scenario x goal compose)",
    )
    parser.add_argument(
        "--mode",
        choices=["design", "library"],
        default="design",
        help="design = from scratch; library = with track design library search (retrieval eval)",
    )
    parser.add_argument(
        "--prefix",
        type=Path,
        help="finish-the-coaster only: prefix program JSON "
        "(default: evals/programs/finish_prefix_<ride_type>.json)",
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
    parser.add_argument(
        "--scenarios",
        type=int,
        default=0,
        help="run each model across the first N generated scenario parks (seeds from "
        "evals/scenarios/seeds.json, regenerated deterministically on demand) and "
        "aggregate scores across them; overrides --scenario",
    )
    parser.add_argument(
        "--concurrency",
        type=int,
        default=0,
        help="max concurrent (model, scenario) competitions with --scenarios "
        "(default: min(8, models*scenarios)); each runs its own game subprocess",
    )
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

    scenarios: list[tuple[str, Path]] = []
    if args.scenarios:
        seeds = load_seeds(args.scenarios)
        print(f"generating {len(seeds)} scenario park(s)...")
        for seed in seeds:
            park = ensure_generated_park(seed)
            scenarios.append((f"seed_{seed}", park))
    elif not args.scenario.exists():
        print(f"error: scenario not found: {args.scenario}", file=sys.stderr)
        return 1

    goal = GOALS[args.goal](args, args.scenario)
    if args.mode == "library" and not goal.supports_library:
        print(f"error: goal {goal.name} does not compose with library mode", file=sys.stderr)
        return 1
    if args.scenarios and goal.name != BestCoasterGoal.name:
        # Generated parks have no guests (guest goals) and no per-park prefix
        # dry-run (finish); wire those up before opening the combination.
        print("error: --scenarios currently composes with the best-coaster goal only", file=sys.stderr)
        return 1

    suffix = args.name if args.name else time.strftime("%H%M%S")
    run_dir = REPO / "evals" / "runs" / f"{time.strftime('%Y%m%d')}-{suffix}"
    run_dir.mkdir(parents=True)
    global FAILED_CALL_DIR
    FAILED_CALL_DIR = run_dir
    print(f"run dir: {run_dir} (goal: {goal.name}, mode: {args.mode})")

    try:
        goal.setup(run_dir)
    except RuntimeError as e:
        print(f"error: goal setup failed: {e}", file=sys.stderr)
        return 1

    if args.mode == "library":
        library_scenario = scenarios[0][1] if scenarios else args.scenario
        goal.library = dump_library(library_scenario, run_dir)
        print(f"track design library: {len(goal.library)} designs")
        ensure_library_previews(library_scenario)

    (run_dir / "run.json").write_text(
        json.dumps(
            {
                "goal": goal.name,
                "goal_config": goal.config(),
                "mode": args.mode,
                "harness": "driver-api",
                "models": args.models,
                "rounds": args.rounds,
                "ticks": args.ticks,
                "ride_type": args.ride_type,
                **(
                    {"scenarios": [label for label, _ in scenarios], "seeds": seeds}
                    if scenarios
                    else {"scenario": args.scenario.name}
                ),
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
    if scenarios:
        return run_multi_scenario(client, args, scenarios, run_dir, goal)

    contenders = [
        compete(client, model, args.rounds, args.scenario, run_dir, args.ticks, goal, args.max_tokens)
        for model in args.models
    ]

    print("\n=== FINAL STANDINGS ===")
    ranked = sorted(
        contenders,
        key=lambda c: c.best.score if c.best and c.best.score is not None else float("-inf"),
        reverse=True,
    )
    for place, contender in enumerate(ranked, 1):
        best = contender.best
        if best is None:
            print(f"{place}. {contender.model}: no scoreable attempt")
        else:
            print(f"{place}. {contender.model}: score {best.score:.2f} (round {best.round}) — {best.summary}")
    (run_dir / "standings.json").write_text(
        json.dumps(
            {
                "goal": goal.name,
                "mode": args.mode,
                "standings": [
                    {
                        "model": c.model,
                        "best_score": c.best.score if c.best else None,
                        "best_metrics": goal.metrics(c.best) if c.best else None,
                        # Kept for older tooling; meaningful for coaster goals only.
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


def run_multi_scenario(
    client,
    args,
    scenarios: list[tuple[str, Path]],
    run_dir: Path,
    goal: "Goal",
) -> int:
    """Fans (model, scenario) competitions out over a thread pool and writes
    aggregate standings: per-scenario best, then mean/median across scenarios.

    Each competition is independent (its own conversation and its own game
    subprocesses), so a failure in one records a zero for that scenario rather
    than sinking the run.
    """
    pairs = [(model, label, park) for model in args.models for label, park in scenarios]
    workers = args.concurrency if args.concurrency > 0 else min(8, len(pairs))
    print(f"running {len(pairs)} competitions ({len(args.models)} models x {len(scenarios)} scenarios, {workers} workers)")

    def one(model: str, label: str, park: Path) -> Contender:
        try:
            return compete(
                client, model, args.rounds, park, run_dir, args.ticks, goal, args.max_tokens, label
            )
        except Exception as e:
            print(f"  [{model} @ {label}] FAILED: {e}", file=sys.stderr, flush=True)
            return Contender(model=model)

    with ThreadPoolExecutor(max_workers=workers) as pool:
        futures = {(model, label): pool.submit(one, model, label, park) for model, label, park in pairs}
    results = {key: future.result() for key, future in futures.items()}

    def best_excitement(c: Contender) -> float:
        return c.best.excitement if c.best else 0.0

    standings = []
    for model in args.models:
        per_scenario = []
        for label, _ in scenarios:
            c = results[(model, label)]
            per_scenario.append(
                {
                    "scenario": label,
                    "best_excitement": c.best.excitement if c.best else None,
                    "best_raw_excitement": c.best.raw_excitement if c.best else None,
                    "best_similarity": c.best.similarity if c.best else None,
                    "rounds": len(c.attempts),
                }
            )
        # A scenario with no rated coaster scores zero: aggregates must reflect
        # reliability across parks, not just the parks that went well.
        bests = [s["best_excitement"] or 0.0 for s in per_scenario]
        standings.append(
            {
                "model": model,
                "aggregate": {
                    "mean_best_excitement": statistics.mean(bests),
                    "median_best_excitement": statistics.median(bests),
                    "scenarios_scored": sum(1 for b in bests if b > 0),
                    "scenarios_total": len(bests),
                },
                "per_scenario": per_scenario,
                "attempts": [
                    {"scenario": label, "round": a.round, "summary": a.summary, "lookups": a.lookups}
                    for label, _ in scenarios
                    for a in results[(model, label)].attempts
                ],
            }
        )
    standings.sort(key=lambda s: s["aggregate"]["mean_best_excitement"], reverse=True)

    print("\n=== FINAL STANDINGS (aggregate over scenarios) ===")
    for place, entry in enumerate(standings, 1):
        agg = entry["aggregate"]
        print(
            f"{place}. {entry['model']}: mean best excitement {agg['mean_best_excitement']:.2f} "
            f"(median {agg['median_best_excitement']:.2f}, "
            f"scored {agg['scenarios_scored']}/{agg['scenarios_total']} scenarios)"
        )
    (run_dir / "standings.json").write_text(
        json.dumps(
            {
                "goal": goal.name,
                "mode": args.mode,
                "scenarios": [label for label, _ in scenarios],
                "standings": standings,
            },
            indent=2,
        )
    )
    return 0


if __name__ == "__main__":
    sys.exit(main())
