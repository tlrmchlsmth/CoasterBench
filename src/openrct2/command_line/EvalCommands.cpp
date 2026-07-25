/*****************************************************************************
 * Fork-only file (wseaton/OpenRCT2): `openrct2-cli eval` command.
 *
 * Loads a park headless, runs the game loop for a fixed number of ticks with
 * the Rust agent bridge hooked in, then asks the agent for its summary.
 * Modeled on SimulateCommands.cpp, with two differences: options for data
 * paths (root options are not parsed for subcommands), and NoGraphics stays
 * false so the software renderer is available for screenshot capture.
 *****************************************************************************/

#ifdef ENABLE_RUST_AGENT

    #include "../Context.h"
    #include "../GameState.h"
    #include "../OpenRCT2.h"
    #include "../core/Console.hpp"
    #include "../core/Path.hpp"
    #include "../ride/Ride.h"
    #include "../ride/RideManager.hpp"
    #include "../ride/RideRatings.h"
    #include "../rustbridge/ParkGen.h"
    #include "../rustbridge/RustBridge.h"
    #include "CommandLine.hpp"

    #include <memory>

using namespace OpenRCT2::CommandLine;

namespace OpenRCT2
{
    static int32_t _ticks = 10000;
    static u8string _evalOpenRCT2DataPath{};
    static u8string _evalRCT2DataPath{};
    static u8string _programPath{};
    static u8string _reportPath{};
    static u8string _capturePath{};
    static int32_t _servePort = 0;
    static u8string _serveBind{};
    static u8string _dumpLibraryPath{};
    static u8string _renderLibraryDir{};
    static bool _captureAllRotations = false;
    static bool _captureXray = false;
    static bool _noGraphics = false;
    static u8string _makeParkPath{};
    static int32_t _seed = 0;
    static int32_t _mapSize = 100;
    static int32_t _hilliness = -1;
    static int32_t _water = -1;
    static int32_t _cash = -1;

    // clang-format off
    static constexpr CommandLineOptionDefinition kEvalOptions[]
    {
        { CMDLINE_TYPE_INTEGER, &_ticks,                kNAC, "ticks",              "number of game ticks to simulate (default 10000)"       },
        { CMDLINE_TYPE_STRING,  &_evalOpenRCT2DataPath, kNAC, "openrct2-data-path", "path to the OpenRCT2 data directory"                    },
        { CMDLINE_TYPE_STRING,  &_evalRCT2DataPath,     kNAC, "rct2-data-path",     "path to the RollerCoaster Tycoon 2 install directory"   },
        { CMDLINE_TYPE_STRING,  &_programPath,          kNAC, "program",            "JSON track program to build before simulating"          },
        { CMDLINE_TYPE_STRING,  &_reportPath,           kNAC, "out",                "write a JSON eval report to this path"                  },
        { CMDLINE_TYPE_STRING,  &_capturePath,          kNAC, "capture",            "write a giant park screenshot (PNG) to this path"       },
        { CMDLINE_TYPE_INTEGER, &_servePort,            kNAC, "serve",              "run the MCP server on this port instead of a batch eval" },
        { CMDLINE_TYPE_STRING,  &_serveBind,            kNAC, "serve-bind",         "MCP server bind address (default 127.0.0.1; use 0.0.0.0 for containers)" },
        { CMDLINE_TYPE_STRING,  &_dumpLibraryPath,      kNAC, "dump-library",       "write the stock track design library as JSON to this path and exit"      },
        { CMDLINE_TYPE_STRING,  &_renderLibraryDir,     kNAC, "render-library",     "render a preview PNG of every stock track design into this directory and exit" },
        { CMDLINE_TYPE_SWITCH,  &_captureAllRotations,  kNAC, "capture-all-rotations", "with --capture, also write the other three view rotations as <name>-r1/-r2/-r3.png" },
        { CMDLINE_TYPE_SWITCH,  &_captureXray,          kNAC, "capture-xray",       "with --capture, also write a see-through verification view (terrain and supports hidden, every placed piece visible) as <name>-x.png" },
        { CMDLINE_TYPE_SWITCH,  &_noGraphics,           kNAC, "no-graphics",        "skip loading sprite data: no RCT2 assets required, but screenshots and library previews are unavailable" },
        { CMDLINE_TYPE_STRING,  &_makeParkPath,         kNAC, "make-park",          "generate a deterministic eval park at this path (plus a .hints.json sidecar) and exit; no park file argument needed" },
        { CMDLINE_TYPE_INTEGER, &_seed,                 kNAC, "seed",               "with --make-park, the generation seed (same seed, same park bytes; default 0)" },
        { CMDLINE_TYPE_INTEGER, &_mapSize,              kNAC, "map-size",           "with --make-park, tiles per side including the void border (default 100)" },
        { CMDLINE_TYPE_INTEGER, &_hilliness,            kNAC, "hilliness",          "with --make-park, terrain roughness 0 (flat) to 8 (default: derived from the seed)" },
        { CMDLINE_TYPE_INTEGER, &_water,                kNAC, "water",              "with --make-park, water amount 0 (none) to 6 (default: derived from the seed)" },
        { CMDLINE_TYPE_INTEGER, &_cash,                 kNAC, "cash",               "with --make-park, starting cash in whole currency units (default: money disabled)" },
        kOptionTableEnd
    };

    static ExitCode HandleEval(CommandLineArgEnumerator* argEnumerator);

    const CommandLineCommand CommandLine::kEvalCommands[]{
        DefineCommand("", "<park file> [--ticks N]", kEvalOptions, HandleEval),
        kCommandTableEnd
    };
    // clang-format on

    static ExitCode HandleEval(CommandLineArgEnumerator* argEnumerator)
    {
        const utf8* inputPath = nullptr;
        if (!argEnumerator->TryPopString(&inputPath) && _makeParkPath.empty())
        {
            Console::Error::WriteLine("Expected a park/save/scenario file path");
            return ExitCode::fail;
        }

        if (_ticks <= 0)
        {
            Console::Error::WriteLine("--ticks must be positive");
            return ExitCode::fail;
        }

        if (!_evalOpenRCT2DataPath.empty())
        {
            gCustomOpenRCT2DataPath = Path::GetAbsolute(_evalOpenRCT2DataPath);
        }
        if (!_evalRCT2DataPath.empty())
        {
            gCustomRCT2DataPath = Path::GetAbsolute(_evalRCT2DataPath);
        }

        // Headless, but by default keep graphics data loaded (gOpenRCT2NoGraphics
        // stays false) so CaptureImage can render screenshots of the result.
        // --no-graphics drops that: the whole scoring path (park load, placement,
        // testing, ratings) works without sprite data, so no RCT2 assets are
        // needed — only anything that renders pixels is off the table.
        gOpenRCT2Headless = true;
        if (!_makeParkPath.empty())
        {
            // Generation never renders anything, so it always runs assetless.
            gOpenRCT2NoGraphics = true;
        }
        if (_noGraphics)
        {
            if (!_capturePath.empty() || _captureAllRotations || _captureXray || !_renderLibraryDir.empty())
            {
                Console::Error::WriteLine(
                    "--no-graphics cannot render: remove --capture/--capture-all-rotations/--capture-xray/--render-library");
                return ExitCode::fail;
            }
            gOpenRCT2NoGraphics = true;
        }

        std::unique_ptr<IContext> context(CreateContext());
        if (!context->Initialise())
        {
            Console::Error::WriteLine("Context initialization failed.");
            return ExitCode::fail;
        }

        if (!_makeParkPath.empty())
        {
            // Standalone mode: build a fresh deterministic park and exit.
            ParkGen::Options options;
            options.outPath = Path::GetAbsolute(_makeParkPath);
            options.seed = static_cast<uint32_t>(_seed);
            options.mapSize = _mapSize;
            options.hilliness = _hilliness;
            options.water = _water;
            options.cashGBP = _cash;
            return ParkGen::Generate(options) == 0 ? ExitCode::ok : ExitCode::fail;
        }

        if (!_dumpLibraryPath.empty())
        {
            // Standalone mode: the library scan only needs the data paths, so
            // skip park loading and simulation entirely.
            return RustBridge::DumpLibrary(_dumpLibraryPath.c_str()) == 0 ? ExitCode::ok : ExitCode::fail;
        }

        if (!context->LoadParkFromFile(inputPath))
        {
            return ExitCode::fail;
        }

        if (!_renderLibraryDir.empty())
        {
            // Standalone mode: the preview renderer needs a live map to stash,
            // but no simulation.
            return RustBridge::RenderTrackLibrary(_renderLibraryDir.c_str()) == 0 ? ExitCode::ok : ExitCode::fail;
        }

        if (_servePort > 0)
        {
            // Interactive mode: the MCP server owns the game loop from here.
            // Blocks until the process is terminated.
            RustBridge::Serve(_serveBind.empty() ? nullptr : _serveBind.c_str(), static_cast<uint16_t>(_servePort));
            return ExitCode::ok;
        }

        Orct2ProgramOutcome* outcome = nullptr;
        if (!_programPath.empty())
        {
            outcome = RustBridge::RunProgram(_programPath.c_str());
        }

        Console::WriteLine("Running eval for %d ticks...", _ticks);
        for (int32_t i = 0; i < _ticks; i++)
        {
            gameStateUpdateLogic();
            RustBridge::Tick(static_cast<uint32_t>(i));
        }

        // The in-game ratings state machine only processes a few rides per
        // tick; force the remainder synchronously so the summary is complete.
        for (auto& ride : RideManager(getGameState()))
        {
            if (ride.status != RideStatus::closed && ride.ratings.isNull())
            {
                RideRating::UpdateRide(ride);
            }
        }
        RustBridge::EvalSummary();

        auto exitCode = ExitCode::ok;
        if (!_reportPath.empty() || outcome != nullptr)
        {
            if (RustBridge::EvalFinish(outcome, _reportPath.empty() ? nullptr : _reportPath.c_str()) != 0
                && !_reportPath.empty())
            {
                exitCode = ExitCode::fail;
            }
        }
        if (!_capturePath.empty())
        {
            if (RustBridge::Capture(_capturePath.c_str(), 0 /*zoom*/, 0 /*rotation*/, true /*fitTrack*/, false) != 0)
            {
                Console::Error::WriteLine("Screenshot capture failed.");
                exitCode = ExitCode::fail;
            }
            auto dot = _capturePath.find_last_of('.');
            auto stem = dot == u8string::npos ? _capturePath : _capturePath.substr(0, dot);
            auto ext = dot == u8string::npos ? u8string{} : _capturePath.substr(dot);
            if (_captureAllRotations)
            {
                for (uint8_t rotation = 1; rotation < 4; rotation++)
                {
                    auto path = stem + "-r" + std::to_string(rotation) + ext;
                    if (RustBridge::Capture(path.c_str(), 0, rotation, true, false) != 0)
                    {
                        Console::Error::WriteLine("Screenshot capture failed (rotation %d).", rotation);
                        exitCode = ExitCode::fail;
                    }
                }
            }
            if (_captureXray)
            {
                auto path = stem + "-x" + ext;
                if (RustBridge::Capture(path.c_str(), 0, 0, true, true /*xray*/) != 0)
                {
                    Console::Error::WriteLine("Screenshot capture failed (xray view).");
                    exitCode = ExitCode::fail;
                }
            }
        }

        return exitCode;
    }
} // namespace OpenRCT2

#endif // ENABLE_RUST_AGENT
