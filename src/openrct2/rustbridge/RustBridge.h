/*****************************************************************************
 * Fork-only file (wseaton/OpenRCT2): C++ side of the Rust agent bridge.
 * See rust/orct2-agent for the Rust side; orct2_agent.h is cbindgen output.
 *****************************************************************************/

#pragma once

#ifdef ENABLE_RUST_AGENT

    #include <cstdint>
    #include <string_view>

struct Orct2ProgramOutcome;

namespace OpenRCT2::RustBridge
{
    // Calls orct2_agent_init(). Safe to call when the agent is disabled at
    // runtime; logs and carries on if the Rust side reports failure.
    void Initialise();

    // Forwards a game tick to the Rust agent.
    void Tick(uint32_t tick);

    // Asks the Rust agent to log its end-of-eval ride summary.
    void EvalSummary();

    // Executes a JSON track program file; the returned handle is owned by the
    // Rust side and must be passed to EvalFinish exactly once.
    Orct2ProgramOutcome* RunProgram(const char* path);

    // Writes the JSON eval report (consumes the outcome; either may be null).
    int32_t EvalFinish(Orct2ProgramOutcome* outcome, const char* outPath);

    // Park screenshot to a PNG path; fitTrack crops to the track's bounding
    // box (full map when no track exists). Returns 0 on success.
    int32_t Capture(const char* path, int32_t zoom, uint8_t rotation, bool fitTrack, bool xray);

    // Films the park's coaster into an mp4 (ffmpeg encodes), one station-to-
    // station cycle, bounded by maxSeconds. Advances the simulation, so call it
    // after the report and any screenshot. Returns 0 on success.
    int32_t CaptureReplay(const char* path, uint32_t maxSeconds, int32_t zoom);

    // Writes the park as a .park save, the artifact that lets a result be
    // reopened and checked instead of taken on trust. Returns true on success.
    bool SavePark(std::string_view path);

    // Runs the MCP server on bind:port (null bind = 127.0.0.1) and, when
    // controlPort is non-zero, a loopback-only control server beside it for the
    // harness. Blocks the game thread until the process exits.
    int32_t Serve(const char* bind, uint16_t port, uint16_t controlPort);

    // Writes the stock track design library as JSON for the eval driver's
    // library mode. Returns 0 on success.
    int32_t DumpLibrary(const char* path);

    // Renders a preview PNG (370x217, rotation 0) of every stock track design
    // into outDir, named after the design. Needs a park loaded (the preview
    // renderer stashes and restores the live map). Returns 0 on success.
    int32_t RenderTrackLibrary(const char* outDir);
} // namespace OpenRCT2::RustBridge

#endif // ENABLE_RUST_AGENT
