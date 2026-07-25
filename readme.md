# CoasterBench

CoasterBench is a benchmark harness in which language models design roller
coasters inside RollerCoaster Tycoon 2. The game computes the score.

The repository is a fork of [OpenRCT2](https://github.com/OpenRCT2/OpenRCT2)
with a Rust agent layer linked into the game binary. A model connects to a
running game, places track pieces individually, and runs a test train. The
reported score is the excitement rating produced by the game's own ride ratings
function.

Published results: https://wseaton.github.io/CoasterBench

## Architecture

```mermaid
flowchart LR
    subgraph host["host machine"]
        bench["coaster-bench<br/>(orchestrator)"]
        game["openrct2-cli eval --serve<br/>game + Rust agent"]
        runs[("evals/runs/&lt;run&gt;/")]
    end
    subgraph sandbox["OpenShell sandbox: no host files, filtered network"]
        agent["Claude Code / opencode<br/>one session per round"]
    end

    bench -->|spawns| game
    bench -->|one session per model per round| agent
    agent <-->|Model Context Protocol over HTTP| game
    bench -->|report.json, program.json, park.png, usage.json| runs
    runs --> site["coaster-site → evals/site/ → GitHub Pages"]
```

The agent has no filesystem or repository access. Its only interface is the tool
set the game exposes over the Model Context Protocol (MCP), an open standard for
connecting models to external tools. All state changes pass through
`GameActions::Execute`, the validation path used by the game's own plugins, so
the harness accepts the placements the game accepts, minus one class the game
accepts but cannot draw (see [War stories](#war-stories)).

## Repository layout

| Path | Contents |
| --- | --- |
| `rust/orct2-agent` | Agent layer linked into the game binary: MCP server, track program executor, piece vocabulary, ratings report, similarity scoring |
| `rust/coaster-bench` | Orchestrator: starts the game, runs sandboxed agent sessions, collects per-round artifacts |
| `rust/coaster-site` | Static site generator for run records |
| `src/openrct2/rustbridge` | C++ side of the Rust bridge |
| `src/openrct2/command_line/EvalCommands.cpp` | The `eval` subcommand |
| `evals/driver.py` | Second harness: direct Anthropic API tool loop, whole-program submission |
| `evals/runs/` | Run records. Git tracks the JSON; images are published separately |

All other paths are unmodified upstream OpenRCT2.

## Requirements

The original RollerCoaster Tycoon 2 data files are required at runtime, not at
compile time. Extract them from the GOG installer with `innoextract`, or copy
them from an RCT Classic installation.

## Building

```bash
cmake -S . -B build -G Ninja -DCMAKE_BUILD_TYPE=RelWithDebInfo
cmake --build build

# macOS only: non-bundled binaries look for data/ next to the executable
ln -sf $PWD/build/OpenRCT2.app/Contents/Resources build/data
```

Corrosion compiles the agent crate as a static library and links it into the
binary. The `ENABLE_RUST_AGENT` option controls this and defaults to on.
Changing any `#[no_mangle] extern "C"` item requires regenerating the
checked-in header with `./scripts/generate-rust-header.sh`.

Per-crate checks:

```bash
cd rust/orct2-agent && cargo fmt && cargo clippy --all-targets && cargo test
```

## Running a benchmark

```bash
./rust/coaster-bench/target/release/coaster-bench \
  --models claude-sonnet-5 \
  --rounds 6 --ride-type 51 --name my-run
```

The model specification selects the harness. A bare name runs Claude Code in the
`coaster-sub` sandbox; `opencode:openrouter/<author>/<model>` runs opencode in
the `coaster-or` sandbox against OpenRouter. Ride type 51 is a steel twister,
which permits inversions; ride type 52 is a wooden coaster, which does not.
Artifacts are written to `evals/runs/<yyyymmdd>-<name>/<model>/round_N/`.

### Round sequence

```mermaid
sequenceDiagram
    participant B as coaster-bench
    participant A as agent session
    participant G as game (MCP)

    B->>G: demolish (empty the park)
    B->>A: round prompt + previous round's report
    loop until the model stops
        A->>G: new_ride / place_pieces
        G-->>A: cursor, circuit closed, or rejection reason
        A->>G: valid_next_pieces / piece_geometry
        G-->>A: placements the game will accept
    end
    A->>G: finish_and_test
    G-->>A: excitement / intensity / nausea
    B->>G: get_state + finish_and_test (authoritative)
    B->>B: writes report.json, program.json, park.png, usage.json
```

Each round starts from an empty park and receives the previous round's report as
feedback. A model's highest-scoring round is its score for the run.

### Scoring

The score is excitement, reduced by a similarity penalty. Each track is compared
against the stock RollerCoaster Tycoon 2 design library using edit distance and
longest common substring, including mirrored variants. Similarity at or below
0.5 incurs no penalty; above 0.5 the score scales linearly to zero.

```
score = excitement                                    if similarity <= 0.5
score = excitement * (1 - similarity) / (1 - 0.5)     otherwise
```

A copied or mirrored stock design therefore scores zero. Each `run.json` records
the threshold used, so earlier runs render under the rules they were scored
with.

### Alternative harness

`evals/driver.py` submits one complete track program as JSON instead of building
incrementally. It supports two modes: `design`, which starts from nothing, and
`library`, which permits searching the stock designs first and measures
retrieval and adaptation.

```bash
uv run evals/driver.py --models claude-sonnet-5 --rounds 4 --mode library
```

### Running without RCT2 assets

Everything the eval scores — park loading, placement, ride testing, ratings,
the drawability gate — is pure game logic; only rendering needs `g1.dat`.
`--no-graphics` runs the whole benchmark with zero RollerCoaster Tycoon 2
files (ride/scenery objects come from OpenRCT2's bundled JSON pack, and the
scenario defaults to a checked-in test park):

```bash
./build/openrct2-cli eval test/tests/testdata/parks/BigMapTest.sv6 \
    --no-graphics --ticks 25000 --program evals/programs/test_oval.json --out report.json
```

The trade: no screenshots (the MCP server drops its image tools, contenders
run text-only), no stock library (library mode unavailable, similarity
penalty inert — `run.json` records `no_graphics` so such runs are not
compared against asset-full leaderboards).

This is what makes the benchmark shippable in CI. The driver speaks to any
OpenAI-compatible endpoint (a `vllm serve` under test, llama.cpp, ...) via
`--base-url`; `evals/ci/` has the CPU-only Dockerfile and the pass/fail gate
(protocol success — a built, tested coaster — not score, which is model
quality, not infrastructure health).

```bash
uv run evals/driver.py --base-url http://localhost:8000/v1 \
    --models Qwen/Qwen2.5-7B-Instruct --rounds 2 --no-graphics
python3 evals/ci/check_run.py evals/runs/<run-dir>
```

## MCP server

```bash
./build/openrct2-cli eval <scenario.SC6> --rct2-data-path ~/rct2-assets --serve 8791
claude mcp add --transport http coaster http://127.0.0.1:8791/mcp
```

This exposes the benchmark tool set to an interactive session against a loaded
park.

The server is implemented directly in `rust/orct2-agent/src/mcp.rs` and runs on
the game thread, because the game API is single-threaded. A tool call invokes
game functions directly and `run_ticks` advances the simulation inline, with no
async runtime and no cross-thread marshaling. It implements the streamable-HTTP
JSON response mode with `initialize`, `tools/list`, and `tools/call`.

| Tool | Behaviour |
| --- | --- |
| `new_ride` | Creates a ride and sets the build cursor |
| `place_piece` / `place_pieces` | Places one piece, or a batch that halts at the first rejection |
| `valid_next_pieces` | Returns the catalog pieces the game accepts at the current cursor |
| `piece_geometry` | Returns the exact cursor delta for every piece from a given direction |
| `undo_piece` / `demolish` | Removes the last piece, or the entire ride |
| `get_state` | Returns cursor, start, piece count, and circuit closure |
| `finish_and_test` | Places entrance and exit, runs a test train, returns ratings |
| `screenshot` | Returns a park image |
| `search_track_designs` / `get_track_design` | Browses the stock .TD6 library; recorded ratings are withheld |

`valid_next_pieces` and `piece_geometry` make the game the authority on track
geometry, so models query placement rules rather than recalling them.

### Cursor semantics

Direction 0 faces -x, 1 faces +y, 2 faces +x, 3 faces -y. The cursor z step is
16. The cursor carries bank and slope in addition to position, and circuit
closure compares all components, so a track returning to the station while
banked or sloped is treated as open.

### Modality gating

Clients declare the content types they accept in the request target:

```
/mcp?modalities=text,image     # all tools
/mcp?modalities=text           # screenshot hidden and refused
/mcp                           # unspecified: all tools
```

The vocabulary matches OpenRouter's `input_modalities` field, allowing a harness
to forward a model's declared capabilities unchanged. Tools answering outside
the declared set are omitted from `tools/list` and rejected on call. This is
required because an image content block sent to a text-only model fails the
entire request; coaster-bench resolves each OpenRouter model against the
catalogue and constructs both the URL and the prompt's tool list accordingly.

## Site generation

```bash
cargo run --manifest-path rust/coaster-site/Cargo.toml   # writes evals/site/
```

The generator reads `evals/runs/**/*.json` and produces an index of all runs
with one row per model, a page per run, and a page per model per run containing
the track, piece list, ratings, and token and cost totals. Incomplete runs are
skipped unless `--include-partial` is passed.

Screenshots are excluded from Git. `uv run evals/publish.py` uploads them to a
Cloudflare R2 bucket through the local `wrangler login` session, so no static
credentials are stored in the repository, and writes a manifest alongside the
run. The generator prefers local images and falls back to manifest URLs, which
allows a continuous integration (CI) build to work from JSON alone. Both the run
JSON and the manifest must be committed; otherwise the published site renders
without images.

Pushes to the `eval` branch touching `evals/` or `rust/coaster-site/` trigger a
GitHub Pages deployment.

## Continuous integration

`coasterbench-ci.yml` runs formatting, Clippy, and tests for all three Rust
crates, then builds the game and command line tool on Linux to verify the C++
and Rust components still link. `coasterbench-site.yml` builds and deploys the
results site.

Upstream's release, packaging, and translation workflows are removed in this
fork. The fork rebases onto upstream `develop` regularly, so modifications to
existing OpenRCT2 files are kept minimal and mechanical, with new code in
separate directories.

## War stories

### Track that builds, tests, rates, and renders as nothing

Some runs produced park screenshots with long stretches of the coaster simply
absent: no track, no supports, grass where a ride had just been rated. The ride
was continuous by every measure the game reports, and the gaps moved when the
view was rotated, which pointed at the painter.

It was not the painter. Instrumenting the paint path showed every track tile
visited and painted, every created paint struct drawn (1166 of 1166, nothing
lost in quadrant sorting), and no change when creation-time culling was
disabled outright. What did show up: 32 tiles whose paint function was called
and emitted nothing. Sixteen were legitimate — the empty filler sequences of
banked five-tile turns and large helices have no sprites by design. The other
sixteen were exactly the program's sixteen one-tile flat↔60° transitions on a
wooden coaster.

RCT2 only ever drew the three-tile long-base steep transitions for wooden
coasters, so the wooden paint dispatch has no case for the one-tile versions
and returns `TrackPaintFunctionDummy`, which draws nothing. The ride type's
descriptor agrees: `flatToSteepSlope` is not among its track groups, so the
in-game construction window will never offer those pieces. But that gate feeds
only the window. `TrackPlaceAction` never consults it, so programmatic
placement is accepted, and the physics rates the result happily because ratings
read the track element descriptor rather than the artwork. The same hole is
reachable without this harness: the ride-type-change cheat rewrites every
piece's ride type with no drawability check, and plugins call the same
unchecked action.

The harness now asks the renderer's own dispatch whether a piece can be drawn
before placing or offering it, so a model that reaches for an undrawable piece
gets a clear rejection instead of an invisible ride. Details in
[issue #1](https://github.com/wseaton/CoasterBench/issues/1).

The lesson generalises past this fork: "the game accepted it" is a weaker
oracle than it sounds. Validation, simulation, and rendering are three separate
authorities in RCT2, and they disagree.

## Licence

OpenRCT2, and therefore this fork, is licensed under the GNU General Public
License version 3 or later. See [`licence.txt`](licence.txt). A legitimate copy
of the original RollerCoaster Tycoon 2 data files is required.

## Citation

```bibtex
@misc{eaton2026coasterbench,
  title        = {CoasterBench: An Agentic Coaster Design Benchmark Scored by
                  RollerCoaster Tycoon 2},
  author       = {Eaton, Will},
  year         = {2026},
  howpublished = {\url{https://github.com/wseaton/CoasterBench}}
}
```
