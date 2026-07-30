# Running the benchmark yourself

This walks through one complete run, from an empty machine to a scored coaster
you can look at. Follow it in order; each step depends on the one before it.

Rough time: 20 minutes of setup, then a run takes 10 to 40 minutes depending on
model and round count.

| Step | What you get |
| --- | --- |
| [1. Buy the game](#1-buy-the-game) | A legal copy of RollerCoaster Tycoon 2 |
| [2. Extract the data files](#2-extract-the-data-files) | `~/rct2-assets` |
| [3. Install the toolchain](#3-install-the-toolchain) | cmake, a compiler, Rust, uv |
| [4. Build](#4-build) | `build/coasterbench-cli` |
| [5. Smoke test](#5-smoke-test-no-model-required) | A scored report from a canned track, proving the setup works |
| [6a. Run the simple harness](#6a-the-simple-harness-evalsdriverpy) | A scored multi-model run via the Anthropic API |
| [6b. Run the interactive harness](#6b-the-interactive-harness-mcp) | A model building piece by piece over MCP |
| [7. Read the output](#7-reading-the-output) | Understanding report.json and standings.json |
| [8. Browse results locally](#8-browsing-results-locally) | The results site in your browser |

## 1. Buy the game

The benchmark runs inside the real game, so it needs the original
RollerCoaster Tycoon 2 data files. They are not redistributable and are not in
this repository. Both stores work:

- **GOG** ([RollerCoaster Tycoon 2: Triple Thrill Pack](https://www.gog.com/game/rollercoaster_tycoon_2)).
  Preferred on macOS and Linux, because the offline installer can be unpacked
  without running Windows.
- **Steam** ([RollerCoaster Tycoon 2: Triple Thrill Pack](https://store.steampowered.com/app/285330/)).
  Fine if you already own it or are on Windows; on macOS and Linux you need
  Steam's Proton install or a copy of the game directory from a Windows machine.

RCT Classic (iOS/Android/Switch) data files also work if you can get at them.

## 2. Extract the data files

The goal is a directory containing `Data/`, `ObjData/`, `Scenarios/` and
`Tracks/`. This guide assumes `~/rct2-assets`.

**From the GOG offline installer** (macOS/Linux):

```bash
brew install innoextract        # or: apt install innoextract
innoextract -d ~/rct2-extract setup_rollercoaster_tycoon_2_*.exe
mv ~/rct2-extract/app ~/rct2-assets
```

**From a Steam or GOG Windows install**: copy the game directory itself.

```bash
cp -r "/path/to/RollerCoaster Tycoon 2" ~/rct2-assets
```

Verify:

```bash
ls ~/rct2-assets/Data/g1.dat
ls ~/rct2-assets/Scenarios/"Build your own Six Flags Park.SC6"
```

Both must exist. `g1.dat` holds the sprites; the harness renders screenshots
headlessly and will assert without it. The Six Flags scenario is the default
arena because it is a large flat plot with money off.

## 3. Install the toolchain

- **C++ build**: cmake 3.24+, Ninja, and a compiler. macOS: `brew install cmake
  ninja` plus Xcode command line tools. Linux: follow upstream OpenRCT2's
  [build instructions](https://github.com/OpenRCT2/OpenRCT2#building-openrct2)
  for the dependency list of your distribution; the fork adds no C++
  dependencies.
- **Rust**: stable toolchain via [rustup](https://rustup.rs). The agent layer is
  a static library compiled during the cmake build.
- **uv**: [astral.sh/uv](https://docs.astral.sh/uv/), only for
  `evals/driver.py`. It resolves that script's dependencies inline, so there is
  no virtualenv to manage.
- **ImageMagick** (Linux only, optional): used to shrink park screenshots before
  they are sent to a model. macOS uses the built-in `sips`. Without either, runs
  proceed without screenshot feedback.

## 4. Build

If a prebuilt `coasterbench-cli` is attached to a
[release](https://github.com/wseaton/CoasterBench/releases) for your platform,
download it instead and skip to step 5. Release binaries contain no game data,
so steps 1 and 2 are still required.

```bash
cmake -S . -B build -G Ninja -DCMAKE_BUILD_TYPE=RelWithDebInfo
cmake --build build --target openrct2-cli
```

The target keeps its upstream name; the binary it produces is
`build/coasterbench-cli`, because it is a modified OpenRCT2 command line tool.

macOS only, one time: non-bundled binaries look for `data/` next to the
executable.

```bash
ln -sf $PWD/build/OpenRCT2.app/Contents/Resources build/data
```

Everything the benchmark needs lives in `coasterbench-cli`. Building the full
`openrct2` GUI target also works and is useful if you want to open a saved park
by hand, but it is not required.

## 5. Smoke test (no model required)

Run the canned track program that ships with the repository. This exercises the
whole pipeline (park load, track placement, test train, ratings, screenshot)
with no API key and no network.

```bash
./build/coasterbench-cli eval \
  ~/rct2-assets/Scenarios/"Build your own Six Flags Park.SC6" \
  --rct2-data-path ~/rct2-assets \
  --ticks 25000 \
  --program evals/programs/test_oval.json \
  --out /tmp/report.json \
  --capture /tmp/park.png
```

Then check the result:

```bash
python3 -c 'import json;r=json.load(open("/tmp/report.json"))["rides"][0];print(r["excitement"], r["tested"], r["status"])'
open /tmp/park.png        # Linux: xdg-open
```

You should get a small excitement number, `tested: true`, and a screenshot with
a visible oval coaster. If this works, everything below is just automation on
top of it.

Common failures here:

| Symptom | Cause |
| --- | --- |
| `Unable to initialise object manager` or an assert about g1 | `--rct2-data-path` is wrong, or the extract is incomplete |
| Scenario not found | Path or quoting; the filename contains spaces |
| `tested: false`, no ratings | Not enough ticks, or the track is not a closed circuit |

## 6a. The simple harness (`evals/driver.py`)

The model writes one complete track program as JSON, the harness builds and
scores it, and the report plus a screenshot go back as feedback for the next
round. Best round wins. This is the easiest path: one script, one API key, no
containers.

```bash
export ANTHROPIC_API_KEY=sk-ant-...
uv run evals/driver.py \
  --models claude-sonnet-5 claude-haiku-4-5 \
  --rounds 4 \
  --ride-type 51 \
  --name my-first-run
```

Useful flags:

| Flag | Default | Notes |
| --- | --- | --- |
| `--models` | `claude-fable-5 claude-sonnet-5` | One entry per contender |
| `--rounds` | 4 | Each round starts from an empty park |
| `--ride-type` | 52 (wooden) | 51 is the steel twister, which allows inversions |
| `--mode` | `design` | `library` also lets the model search the stock designs |
| `--ticks` | 25000 | Simulation budget for the test train |
| `--scenario` | Six Flags | Any `.SC6` |
| `--name` | timestamp | Run directory suffix |
| `--vertex` | off | Use Google Vertex AI instead; auth via `gcloud auth application-default login` |

Output lands in `evals/runs/<yyyymmdd>-<name>/`. Progress prints per round, so a
run that is going badly is obvious early (usually as repeated
`BUILD FAILED ... not a complete circuit`).

## 6b. The interactive harness (MCP)

Here the model builds one piece at a time against a live game, asking the game
what it will accept. Start the server:

```bash
./build/coasterbench-cli eval \
  ~/rct2-assets/Scenarios/"Build your own Six Flags Park.SC6" \
  --rct2-data-path ~/rct2-assets \
  --serve 8791
```

Leave it running and connect a client. With Claude Code:

```bash
claude mcp add --transport http coaster http://127.0.0.1:8791/mcp
claude
```

Then ask for a coaster: *"Using the coaster tools, build the most exciting
twister coaster you can (ride type 51), then run finish_and_test and report the
ratings."* Any MCP client works; the endpoint is plain streamable HTTP.

The tool set is listed in the [readme](../readme.md#mcp-server). The two that
matter most are `valid_next_pieces` and `piece_geometry`, which make the game
the authority on what fits, and `finish_and_test`, which places the entrance and
exit, runs a test train, and returns the ratings.

To keep the artifacts a run would normally produce, ask the client to call
`get_state` and `screenshot` at the end, or re-run the resulting piece list
through step 5 as a program.

### The full orchestrator (`rust/coaster-bench`)

`coaster-bench` is what produces the published runs: it starts the game, runs
one sandboxed agent session per model per round, and collects artifacts in the
same layout as the simple harness.

```bash
cargo build --release --manifest-path rust/coaster-bench/Cargo.toml
./rust/coaster-bench/target/release/coaster-bench \
  --models claude-sonnet-5 --rounds 6 --ride-type 51 --name my-run
```

It requires an [OpenShell](https://github.com/wseaton/OpenShell) gateway with a
sandbox named `coaster-sub` (or `coaster-or` for the OpenRouter/opencode lane),
configured with network policy allowing the model endpoint and the MCP port. That
setup is host-specific and lives outside this repository, so if you do not
already run OpenShell, use the simple harness or plain MCP above. `--attach`
reuses an already-running server instead of spawning one, which is handy when
debugging.

## 7. Reading the output

```
evals/runs/<yyyymmdd>-<name>/
├── run.json              parameters the run was scored under
├── standings.json        final ranking with a per-round summary line
└── <model>/round_N/
    ├── program.json      the track the model submitted or built
    ├── report.json       the game's verdict
    ├── park.png          screenshot (plus park-x.png x-ray, if captured)
    ├── usage.json        tokens and cost for the round
    ├── lookups.json      stock designs consulted (library mode)
    ├── session.log       raw agent transcript (coaster-bench only)
    └── trace.jsonl       tool calls in order (coaster-bench only)
```

Start with `standings.json`. Each attempt is one line of prose, so a whole run
reads at a glance:

```
excitement=7.13 intensity=10.17 nausea=6.24 tested=True crashed=False
  length=853 drops=14 airtime=84 similarity=0.39 (nearest: Woodchip)
BUILD FAILED (45/45 placed): set_status(testing): Track is not a complete
  circuit [track starts at tile (30, 50, ...) and ends at tile (42, 53, ...)]
BUILD FAILED at piece 56 (56/66 placed): rejected at cursor (x=62, y=56, ...):
  Twister Roller Coaster 1 in the way
```

Those three cover nearly every outcome: a scored ride, a track that never
closed, or a track that ran into itself.

Then `report.json` for the round you care about:

| Field | Meaning |
| --- | --- |
| `rides[0].excitement` | The score, before the similarity penalty |
| `rides[0].intensity` / `nausea` | The other two ratings; not scored, but a nausea monster is usually a bad design |
| `rides[0].tested` | False means no ratings were computed; the ride never completed a test circuit |
| `rides[0].crashed` | A crashed train invalidates the round |
| `num_inversions`, `num_drops`, `total_air_time`, `max_positive_g` | The ingredients the ratings function actually rewards |
| `similarity.similarity` | Distance from the nearest stock design, 0 to 1 |
| `similarity.nearest_design` | Which stock design it resembles |
| `placed_pieces` | Every piece in order, as the game recorded it |
| `bounds`, `start` | Footprint and the station cursor |

Final score, with `similarity_grace` recorded in `run.json` (currently 0.5):

```
score = excitement                                    if similarity <= 0.5
score = excitement * (1 - similarity) / (1 - 0.5)     otherwise
```

A copy of a stock design, mirrored or not, scores zero. Older runs keep their
own threshold in `run.json` so they stay readable under the rules they were
scored with.

Finally, look at `park.png`. The ratings function is not a taste function, and a
7.0 that looks like a pile of spaghetti is a normal outcome. The `-x` x-ray
capture hides terrain and supports, which is the fastest way to see whether the
track is really one continuous circuit.

## 8. Browsing results locally

```bash
cargo run --manifest-path rust/coaster-site/Cargo.toml
open evals/site/index.html      # Linux: xdg-open
```

This renders every run under `evals/runs/` into a browsable site: an index of
all runs, a comparison page per run, and a detail page per model showing every
round, its track, and its ratings. It prefers local images, so your own run's
screenshots appear without any upload step.

Runs that did not finish every promised model and round are skipped, with the
reason on stderr. Pass `--include-partial` to render them anyway, which is what
you want while a run is still going.

## Troubleshooting

| Symptom | Fix |
| --- | --- |
| `error: build/coasterbench-cli not built` | Run step 4 from the repository root |
| Assert about sprites or g1 on `--capture` | `--rct2-data-path` is not pointing at a complete extract |
| Every round is `not a complete circuit` | Expected for weaker models; the closure error reports the exact remaining offset, which the harness feeds back |
| MCP client cannot connect | The server binds 127.0.0.1 by default; containers need `--serve-bind 0.0.0.0` and a matching port in the sandbox policy |
| Ratings never appear for a ride | Stalls never get ratings, and tracked rides need a completed test circuit first |
| Screenshot shows missing track | See the readme's note on undrawable pieces; the harness now rejects those at placement time |
