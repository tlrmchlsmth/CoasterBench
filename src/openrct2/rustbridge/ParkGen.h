/*****************************************************************************
 * Fork-only file (wseaton/OpenRCT2): deterministic scenario park generator.
 *
 * Backs `coasterbench-cli eval --make-park`: builds a blank game state, runs
 * the game's own MapGen with seeded randomness, loads the coaster + terrain
 * objects the eval needs (works with zero RCT2 assets under --no-graphics),
 * writes the .park plus a machine-readable map-hints sidecar JSON.
 *****************************************************************************/

#pragma once

#ifdef ENABLE_RUST_AGENT

    #include <cstdint>
    #include <string>

namespace OpenRCT2::ParkGen
{
    struct Options
    {
        std::string outPath;
        uint32_t seed = 0;
        int32_t mapSize = 100; // tiles per side, including the 1-tile void border
        int32_t hilliness = -1; // 0 flat .. 8 mountainous; -1 = derive from seed
        int32_t water = -1;     // 0 none .. 6 lots; -1 = derive from seed
        int64_t cashGBP = -1;   // starting cash; -1 = money disabled (PARK_FLAGS_NO_MONEY)
    };

    // Returns 0 on success. Requires an initialised headless context.
    int Generate(const Options& options);
} // namespace OpenRCT2::ParkGen

#endif // ENABLE_RUST_AGENT
