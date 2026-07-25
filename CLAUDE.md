# OpenRCT2 Fork: Rust Agentic Eval Harness

This is a fork of OpenRCT2 (github.com/wseaton/OpenRCT2) with one goal: a Rust
plugin system that runs an agentic eval harness through the game. The flagship
eval: an LLM designs the best roller coaster, scored by in-game ratings.

## Fork rules

- We rebase onto upstream `develop` regularly. Almost all new code lives in new
  directories; edits to existing OpenRCT2 files must stay tiny and mechanical
  (single call sites, not refactors).
- New C++ files under `src/openrct2/` need no CMake edits (GLOB_RECURSE picks
  them up).

## Architecture

- Rust attaches as a **staticlib linked into the binary** (not dlopen, not IPC).
  Crate lives in `rust/orct2-agent`, bridged via cbindgen C ABI.
- C++ side of the bridge goes in `src/openrct2/rustbridge/` with a
  `NativeHookEngine` (the existing `scripting/HookEngine.h` Hook is
  QuickJS-specific and gated on ENABLE_SCRIPTING; we stay independent of it).
- Eval entry point: `openrct2-cli eval` command, modeled on
  `src/openrct2/command_line/SimulateCommands.cpp` (load park, tick N via
  `gameStateUpdateLogic()`, score).
- Mutations go through `GameActions::Execute` only, never direct game state
  pokes (same validation path as JS plugins).

## Key facts (verified in code)

- `gOpenRCT2Headless` and `gOpenRCT2NoGraphics` are separate flags. The eval
  command needs Headless=true, NoGraphics=false so the software renderer works:
  `CaptureImage()` (`src/openrct2/interface/Screenshot.cpp`) renders PNGs with
  no window/GPU, but asserts if sprite data wasn't loaded.
- Ride ratings hook site: `src/openrct2/ride/RideRatings.cpp` (~line 1076),
  next to the JS `rideRatingsCalculate` hook.
- Replay video = per-tick `CaptureImage` chase-cam frames (game logic is 40
  ticks/sec) + external ffmpeg encode in the harness. No video encoder in-tree,
  keep it that way.
- Determinism/repro: `ReplayManager.h`, `GameStateSnapshots.h` exist upstream.

## Rust bridge workflow

- ABI changes in `rust/orct2-agent/src/lib.rs` (any `#[no_mangle] extern "C"`
  item) require regenerating the checked-in header:
  `./scripts/generate-rust-header.sh` (needs `cargo install cbindgen`).
- Corrosion (pinned v0.6.1, FetchContent in `cmake/rustagent.cmake`) builds the
  crate; `ENABLE_RUST_AGENT` (default ON) gates everything, same pattern as
  ENABLE_SCRIPTING.
- Rust checks: `cargo fmt && cargo clippy --all --benches --tests --examples
  --all-features && cargo test` inside `rust/orct2-agent`.

## Build (macOS)

```bash
cmake -S . -B build -G Ninja -DCMAKE_BUILD_TYPE=RelWithDebInfo
cmake --build build
```

CMake auto-downloads prebuilt universal dylibs on macOS (MACOS_USE_DEPENDENCIES).
Note `LIBOPENRCT2_LINKAGE STATIC` on APPLE in `src/openrct2/CMakeLists.txt` (RTTI
across dylib boundary is broken on macOS, so libopenrct2 links static there).

## Running the CLI from the build dir

Non-bundled binaries look for `data/` next to the exe. One-time setup:
`ln -sf $PWD/build/OpenRCT2.app/Contents/Resources build/data`. Bare
`openrct2-cli` (launch mode) sets NoGraphics and works without RCT2 assets;
`simulate` (and our `eval`) load g1.dat and need the real game files.

## Eval workflow (phase 3+)

- Track programs: JSON (ride_type, start tile+dir, piece list) executed by
  `rust/orct2-agent/src/program.rs`; piece vocabulary in `pieces.rs` mirrors
  `src/openrct2/ride/ted/TrackElemType.h`. Example: `evals/programs/test_oval.json`.
- Full run: `./build/openrct2-cli eval <scenario> --ticks 25000
  --rct2-data-path ~/rct2-assets --program p.json --out report.json --capture park.png`
- Entrance/exit are brute-force auto-placed next to station tiles (game
  validation is the oracle). Testing status requires them.
- Direction semantics: dir 0 faces -x, 1 +y, 2 +x, 3 -y. Cursor z step 16.
- Placement is NOT a sufficient oracle for drawability: TrackPlaceAction never
  checks the ride's enabled track groups (that gate only feeds the construction
  window), so pieces the ride style has no artwork for build and rate fine and
  render as nothing (wooden RC + 1-tile flat<->60 transitions is the classic
  case). `CheckPieceDrawable` in RustBridge.cpp gates place and query against
  the renderer's own dispatch (GetTrackPaintFunction vs TrackPaintFunctionDummy).
  See issue #1 and the readme war story.
- Stalls never get ratings (RatingsCalculationType::Stall); tracked rides need
  a completed test circuit (RideFlag::tested) before ratings compute.
- No-assets mode: `eval --no-graphics` sets gOpenRCT2NoGraphics so the whole
  scoring path (park load, placement, testing, ratings, drawability gate) runs
  with zero RCT2 files — objects come from the bundled JSON pack; only pixels
  need g1.dat. Screenshots/--render-library are refused, the MCP server drops
  image tools server-side (Modalities::server_side), and the similarity
  penalty is inert (no stock library; report says `similarity: null`).
  Assetless scenario default: test/tests/testdata/parks/BigMapTest.sv6
  (mostly-open flat grass, cash-rich, builds fine). driver.py mirrors the
  flag (`--no-graphics`, design mode only) and also takes `--base-url` for
  any OpenAI-compatible endpoint (e.g. `vllm serve`); `evals/ci/` has the
  CPU-only Dockerfile, a protocol-success gate (check_run.py), and the CI
  job shape.
- Head-to-head driver: `uv run evals/driver.py` (needs ANTHROPIC_API_KEY, or
  `--vertex` with GCP ADC; project defaults from $ANTHROPIC_VERTEX_PROJECT_ID);
  results under `evals/runs/<timestamp>/`. Models get a validate_track_program
  dry-run tool (placement + closure errors with a net-displacement hint) with
  a shared per-round budget alongside library lookups.
- Sub-lane orchestrator: `rust/coaster-bench` (own crate, not linked into the
  game). Spawns the game MCP server, then one sandboxed Claude Code session
  per model per round (OpenShell sandbox `coaster-sub`, personal-sub OAuth via
  the `claude-sub` provider) building interactively through the MCP tools;
  collects report/program/park.png per round into the same evals/runs layout.
  `./rust/coaster-bench/target/release/coaster-bench --models claude-fable-5
  --rounds 4 --ride-type 51 --name my-run`. Port must be in the sandbox
  policy (default 8791).
- Second coaster-bench lane: `--models opencode:openrouter/<author>/<model>`
  runs opencode in the `coaster-or` sandbox against OpenRouter (key in the
  login keychain as `openrouter-api-key`, cost tracked by spend delta).
  coaster-bench writes that sandbox's ~/.config/opencode/opencode.json per
  session, so the MCP endpoint is per contender. Model input modalities come
  from the OpenRouter catalogue and are recorded in run.json; a text-only
  model gets `?modalities=text` and a prompt with no screenshot tool.
- Two driver modes (`--mode`, recorded in run.json + standings.json, separate
  leaderboard sections in the site): `design` (from scratch) and `library`
  (model can search the stock .TD6 library via extra tools; tests retrieval +
  adaptation). Both modes penalize similarity to stock designs: report.json
  carries the nearest-design similarity (edit distance + longest common
  substring, mirrored variants included; rust similarity.rs), and scores scale
  linearly to zero above the grace threshold (driver's SIMILARITY_GRACE,
  recorded per run in run.json; site reads it from there). Library JSON dump:
  `openrct2-cli eval <scenario> --rct2-data-path ... --dump-library lib.json`;
  preview PNGs (game's own TrackDesignDrawPreview, 370x217, needs a park
  loaded): `... --render-library <dir>`, cached at evals/library-previews/
  (gitignored, auto-rendered by the driver in library mode).
- Library lookups are persisted per round as `round_N/lookups.json` (written
  by driver.py, but any harness can drop the same file); the site shows
  per-round lookup chips, a "studied designs" preview gallery per model, and
  a full library.html gallery.
- Heavy artifacts (PNGs, videos) are gitignored, not tracked; only run JSON
  is. After a run, `uv run evals/publish.py` uploads them to the Cloudflare
  R2 bucket `coasterbench` (public at https://artifacts.wseaton.com) through
  the local `wrangler login` OAuth session (no static keys anywhere) and
  records manifests (`runs/<run>/artifacts.json`, `evals/library-previews.json`).
  The SSG prefers local files, falls back to manifest URLs, so the GitHub
  Pages CI build works from JSON alone. **Both must be committed and pushed**
  or the deployed site renders with no images at all.

## Site generator (rust/coaster-site)

- `cargo run --manifest-path rust/coaster-site/Cargo.toml` writes `evals/site/`
  (own crate, askama templates in `templates/`, CSS/JS in `static/`, raster
  work — index thumbnails, og-card, favicon — via the `image` crate). This is
  what the Pages workflow builds; `evals/site.py` is the superseded Python
  version, kept until the Rust output has a few deploys behind it.
- Three page types: `index.html` (one row per model per run, runs pivoted,
  grouped by run, faceted by mode/coaster/harness/model), `run-<run>.html`
  (models side by side for comparison), and `run-<run>-<model>.html` (one
  model's whole run in detail: headline stats, chart, studied designs, every
  round). Index rows link to the model page, the run name to the comparison.
- Runs that never finished are skipped, with the reason on stderr and a note
  on the page: any model short of run.json's promised `models`/`rounds` (or,
  for runs from before run.json recorded them, short of the run's own best
  round count). `--include-partial` renders them anyway.

## MCP server (interactive per-piece building)

- `./build/openrct2-cli eval <scenario> --rct2-data-path ~/rct2-assets
  --serve 8791` runs an MCP server at `http://127.0.0.1:8791/mcp`
  (streamable HTTP, JSON response mode).
- Hand-rolled sync server in `rust/orct2-agent/src/mcp.rs` — runs ON the game
  thread by design (game API is single-threaded); rmcp/tokio deliberately
  avoided. Tools: new_ride, place_piece, place_pieces (batch placement, max 200
  pieces), valid_next_pieces (game-as-oracle query of the whole catalog),
  get_state, finish_and_test, screenshot (MCP image content), demolish,
  search_track_designs + get_track_design (stock .TD6 library browsing;
  recorded ratings deliberately hidden).
- Cursor state now tracks bank (TrackRoll: 0=none, 2=left, 4=right, 15=upside_down)
  and slope (TrackPitch: 0=none, 2=up25, 4=up60, 6=down25, 8=down60); circuit
  closure check compares full cursor (x/y/z/dir/bank/slope) to catch incomplete
  circuits that re-enter the station still banked or sloped.
- Clients advertise what content they can take in the request target:
  `/mcp?modalities=text,image` (vocabulary lifted from OpenRouter's
  `input_modalities`; absent = everything). Tools answering outside the set are
  dropped from tools/list and refused on call, so a text-only model never sees
  screenshot — one image content block fails the whole upstream request
  (Poolside answers "please check the model you provided").
- Register with Claude Code:
  `claude mcp add --transport http coaster http://127.0.0.1:8791/mcp`
- Rust unit tests link against `#[cfg(test)]` stubs in host.rs (the C++ host
  only exists in the game binary); game behavior is tested e2e via the CLI.

## OpenShell sandbox harness (containerized Claude Code + one MCP)

Docs live in ~/git/OpenShell/docs — READ THEM before changing anything here.
- Gateway: `openshell-gateway --config ~/.config/openshell/gateway.toml`
  (podman driver). Client mTLS certs live in
  ~/.config/openshell/gateways/<name>/mtls and must match the server PKI in
  ~/.local/state/openshell/tls (copy ca.crt + client/tls.* on BadSignature).
- Sandbox image (scratchpad/coaster-sandbox/Dockerfile): node:22-slim +
  iproute2 (required for netns isolation) + `useradd sandbox` (required by
  installed 0.0.88; newer versions accept bare UIDs). Image CMD is replaced by
  the supervisor — pass start commands via `-- <cmd>` or `sandbox exec`.
- Game server must bind 0.0.0.0 (`--serve-bind 0.0.0.0`); sandboxes reach the
  host at host.containers.internal.
- Policy (scratchpad/coaster-sandbox/policy.yaml): vertex endpoints + game MCP
  only. Binary attribution uses kernel-resolved paths — Claude Code 2.x is
  `/usr/local/lib/node_modules/@anthropic-ai/claude-code/bin/claude.exe`, not
  node. Denials log to `openshell logs <sandbox>` with binary + reason.
- Inference: gateway-managed vertex provider (`--from-gcloud-adc` +
  VERTEX_AI_PROJECT_ID/REGION/PUBLISHER config). inference.local rejects
  Claude Code's `context_management` param (Vertex schema), so the working
  path is Claude Code vertex mode + placeholder substitution:
  `CLAUDE_CODE_USE_VERTEX=1 CLAUDE_CODE_SKIP_VERTEX_AUTH=1
  ANTHROPIC_AUTH_TOKEN="$GOOGLE_VERTEX_AI_TOKEN"` — the proxy resolves the
  placeholder to the gateway-minted token at egress; no real creds in sandbox.

## Runtime assets

RCT2 asset files are required at **runtime** (even headless park loading), not
compile time. Source: GOG installer + innoextract (preferred on macOS) or RCT
Classic. Point `general.game_path` in the OpenRCT2 config at them.
