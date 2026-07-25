/*****************************************************************************
 * Fork-only file (wseaton/OpenRCT2): deterministic scenario park generator.
 * See ParkGen.h for the contract.
 *****************************************************************************/

#ifdef ENABLE_RUST_AGENT

    #include "ParkGen.h"

    #include "../Context.h"
    #include "../GameState.h"
    #include "../core/Console.hpp"
    #include "../core/File.h"
    #include "../core/Json.hpp"
    #include "../management/Research.h"
    #include "../object/ObjectManager.h"
    #include "../park/ParkFile.h"
    #include "../scenario/Scenario.h"
    #include "../util/Util.h"
    #include "../world/Map.h"
    #include "../world/MapLimits.h"
    #include "../world/ParkData.h"
    #include "../world/map_generator/MapGen.h"
    #include "../world/tile_element/SurfaceElement.h"

    #include <array>
    #include <string_view>
    #include <vector>

using namespace OpenRCT2;

namespace OpenRCT2::ParkGen
{
    namespace
    {
        // Everything a generated park needs, loadable from the bundled JSON
        // object pack alone (no RCT2 assets under --no-graphics). Grass and
        // rock go first so they land at object entry index 0, the fallback the
        // map generator reaches for.
        constexpr std::array<std::string_view, 12> kBaseObjects = {
            "rct2.terrain_surface.grass",
            "rct2.terrain_surface.sand",
            "rct2.terrain_surface.sand_brown",
            "rct2.terrain_surface.dirt",
            "rct2.terrain_surface.ice",
            "rct2.terrain_edge.rock",
            "rct2.terrain_edge.wood_red",
            "rct2.terrain_edge.ice",
            "rct2.station.plain",
            "rct2.water.wtrcyan",
            "rct2.climate.warm",
            "rct2.park_entrance.pkent1",
        };

        // The competition coaster types: wooden (52) and steel twister (51).
        // A blank park has no ride objects selected, and without these
        // FindSubtypeForRideType finds nothing and every new_ride fails.
        constexpr std::array<std::string_view, 2> kRideObjects = {
            "rct2.ride.ptct1", // wooden roller coaster trains (ride type 52)
            "rct2.ride.bmsd",  // steel twister sit-down trains (ride type 51)
        };

        // Deterministic per-seed parameter derivation (splitmix64 steps).
        uint64_t SplitMix64(uint64_t& state)
        {
            state += 0x9E3779B97F4A7C15uLL;
            uint64_t z = state;
            z = (z ^ (z >> 30)) * 0xBF58476D1CE4E5B9uLL;
            z = (z ^ (z >> 27)) * 0x94D049BB133111EBuLL;
            return z ^ (z >> 31);
        }

        // Base land height in the generator's raw surface units (baseHeight).
        constexpr int32_t kBaseLandHeight = 14;

        struct Derived
        {
            int32_t hilliness;  // 0..8
            int32_t water;      // 0..6, forced 0 on flat maps
            int32_t heightHigh; // Settings::heightmapHigh
            std::string_view surface;
            std::string_view edge;
        };

        Derived DeriveParams(const Options& options)
        {
            uint64_t rng = static_cast<uint64_t>(options.seed) * 0x9E3779B97F4A7C15uLL + 0x243F6A8885A308D3uLL;
            Derived d{};
            d.hilliness = options.hilliness >= 0 ? std::min(options.hilliness, 8)
                                                 : static_cast<int32_t>(SplitMix64(rng) % 9);
            d.water = options.water >= 0 ? std::min(options.water, 6) : static_cast<int32_t>(SplitMix64(rng) % 7);
            if (d.hilliness == 0)
            {
                // A blank map is uniformly at base height; any water level above
                // it floods everything, so flat maps are dry maps.
                d.water = 0;
            }
            d.heightHigh = kBaseLandHeight + 4 * std::max(d.hilliness, 1);

            constexpr std::array<std::string_view, 5> surfaces = {
                "rct2.terrain_surface.grass", "rct2.terrain_surface.sand", "rct2.terrain_surface.sand_brown",
                "rct2.terrain_surface.dirt",  "rct2.terrain_surface.ice",
            };
            d.surface = surfaces[SplitMix64(rng) % surfaces.size()];
            if (d.surface == "rct2.terrain_surface.dirt")
                d.edge = "rct2.terrain_edge.wood_red";
            else if (d.surface == "rct2.terrain_surface.ice")
                d.edge = "rct2.terrain_edge.ice";
            else
                d.edge = "rct2.terrain_edge.rock";
            return d;
        }

        struct MapSurvey
        {
            int32_t heightMin = std::numeric_limits<int32_t>::max();
            int32_t heightMax = 0;
            double waterFraction = 0.0;
            double flatOpenFraction = 0.0;
            // Centre of the flattest fully-dry window — the suggested build anchor.
            int32_t startX = 0;
            int32_t startY = 0;
            int32_t startHeight = kBaseLandHeight;
            int32_t openSquare = 0; // side of the dry window the anchor sits in
        };

        // Scans the generated surface for the hints sidecar: height range,
        // water coverage, and the flattest open square to anchor a station on.
        MapSurvey SurveyMap(int32_t mapSize)
        {
            MapSurvey survey{};
            const int32_t lo = 1;
            const int32_t hi = mapSize - 2; // inclusive playable bounds
            const int32_t n = hi - lo + 1;
            if (n <= 0)
                return survey;

            std::vector<int32_t> height(static_cast<size_t>(n) * n, 0);
            std::vector<uint8_t> open(static_cast<size_t>(n) * n, 0);
            int64_t waterTiles = 0;
            int64_t flatOpenTiles = 0;
            for (int32_t y = lo; y <= hi; y++)
            {
                for (int32_t x = lo; x <= hi; x++)
                {
                    auto* surface = MapGetSurfaceElementAt(TileCoordsXY{ x, y });
                    if (surface == nullptr)
                        continue;
                    auto idx = static_cast<size_t>(y - lo) * n + (x - lo);
                    height[idx] = surface->baseHeight;
                    bool dry = surface->GetWaterHeight() == 0;
                    open[idx] = dry ? 1 : 0;
                    if (!dry)
                        waterTiles++;
                    if (dry && surface->GetSlope() == 0)
                        flatOpenTiles++;
                    survey.heightMin = std::min(survey.heightMin, static_cast<int32_t>(surface->baseHeight));
                    survey.heightMax = std::max(survey.heightMax, static_cast<int32_t>(surface->baseHeight));
                }
            }
            const double total = static_cast<double>(n) * n;
            survey.waterFraction = waterTiles / total;
            survey.flatOpenFraction = flatOpenTiles / total;

            // Prefix sums over openness, height, and height² let each candidate
            // window be scored in O(1): all-dry, then minimal height variance,
            // ties broken toward the map centre.
            const int32_t stride = n + 1;
            std::vector<int64_t> pOpen(static_cast<size_t>(stride) * stride, 0);
            std::vector<int64_t> pH(static_cast<size_t>(stride) * stride, 0);
            std::vector<int64_t> pH2(static_cast<size_t>(stride) * stride, 0);
            for (int32_t y = 0; y < n; y++)
            {
                for (int32_t x = 0; x < n; x++)
                {
                    auto idx = static_cast<size_t>(y) * n + x;
                    auto p = static_cast<size_t>(y + 1) * stride + (x + 1);
                    auto up = p - stride;
                    pOpen[p] = open[idx] + pOpen[p - 1] + pOpen[up] - pOpen[up - 1];
                    pH[p] = height[idx] + pH[p - 1] + pH[up] - pH[up - 1];
                    pH2[p] = static_cast<int64_t>(height[idx]) * height[idx] + pH2[p - 1] + pH2[up] - pH2[up - 1];
                }
            }
            auto windowSum = [&](const std::vector<int64_t>& p, int32_t x, int32_t y, int32_t w) {
                auto a = static_cast<size_t>(y) * stride + x;
                auto b = static_cast<size_t>(y + w) * stride + (x + w);
                auto c = static_cast<size_t>(y) * stride + (x + w);
                auto d = static_cast<size_t>(y + w) * stride + x;
                return p[b] + p[a] - p[c] - p[d];
            };

            const int32_t centre = n / 2;
            bool found = false;
            for (int32_t w : { 31, 25, 19, 15, 9, 5 })
            {
                if (w > n)
                    continue;
                double bestScore = 0.0;
                int64_t bestDist = 0;
                int32_t bestX = -1, bestY = -1;
                for (int32_t y = 0; y + w <= n; y++)
                {
                    for (int32_t x = 0; x + w <= n; x++)
                    {
                        if (windowSum(pOpen, x, y, w) != static_cast<int64_t>(w) * w)
                            continue;
                        auto count = static_cast<double>(w) * w;
                        auto sum = static_cast<double>(windowSum(pH, x, y, w));
                        auto sum2 = static_cast<double>(windowSum(pH2, x, y, w));
                        auto variance = sum2 / count - (sum / count) * (sum / count);
                        auto cx = x + w / 2;
                        auto cy = y + w / 2;
                        int64_t dist = static_cast<int64_t>(cx - centre) * (cx - centre)
                            + static_cast<int64_t>(cy - centre) * (cy - centre);
                        if (bestX < 0 || variance < bestScore - 1e-9
                            || (variance < bestScore + 1e-9 && dist < bestDist))
                        {
                            bestScore = variance;
                            bestDist = dist;
                            bestX = x;
                            bestY = y;
                        }
                    }
                }
                // Big windows must also be genuinely flat (their whole point is
                // guaranteeing buildable ground); at 15 tiles and below any dry
                // window beats sliding further down the size list.
                if (bestX >= 0 && (w <= 15 || bestScore <= 1.0))
                {
                    survey.startX = lo + bestX + w / 2;
                    survey.startY = lo + bestY + w / 2;
                    survey.openSquare = w;
                    found = true;
                    break;
                }
            }
            if (!found)
            {
                survey.startX = lo + centre;
                survey.startY = lo + centre;
            }
            auto* startSurface = MapGetSurfaceElementAt(TileCoordsXY{ survey.startX, survey.startY });
            if (startSurface != nullptr)
                survey.startHeight = startSurface->baseHeight;
            if (survey.heightMin > survey.heightMax)
                survey.heightMin = survey.heightMax;
            return survey;
        }

        u8string HintsPathFor(const std::string& parkPath)
        {
            auto dot = parkPath.find_last_of('.');
            auto stem = dot == std::string::npos ? parkPath : parkPath.substr(0, dot);
            return stem + ".hints.json";
        }
    } // namespace

    int Generate(const Options& options)
    {
        if (options.mapSize < 30 || options.mapSize > 500)
        {
            Console::Error::WriteLine("--map-size must be between 30 and 500 tiles");
            return 1;
        }

        auto& objectManager = GetContext()->GetObjectManager();
        for (auto identifier : kBaseObjects)
        {
            if (objectManager.LoadObject(identifier) == nullptr)
            {
                Console::Error::WriteLine("Failed to load object '%s' (is the OpenRCT2 data path set up?)",
                                          std::string(identifier).c_str());
                return 1;
            }
        }
        for (auto identifier : kRideObjects)
        {
            if (objectManager.LoadObject(identifier) == nullptr)
            {
                Console::Error::WriteLine("Failed to load ride object '%s'", std::string(identifier).c_str());
                return 1;
            }
        }

        auto derived = DeriveParams(options);

        // Weather::reset (inside gameStateInitAll) draws from scenarioRand, so
        // the scenario PRNG must be seeded before init for a reproducible park.
        ScenarioRandSeed(options.seed, options.seed ^ 0x9E3779B9u);
        auto& gameState = getGameState();
        gameStateInitAll(gameState, TileCoordsXY{ options.mapSize, options.mapSize });

        World::MapGenerator::Settings settings{};
        settings.algorithm = derived.hilliness == 0 ? World::MapGenerator::Algorithm::blank
                                                    : World::MapGenerator::Algorithm::simplexNoise;
        settings.mapSize = TileCoordsXY{ options.mapSize, options.mapSize };
        settings.heightmapLow = kBaseLandHeight;
        settings.heightmapHigh = derived.heightHigh;
        settings.waterLevel = 0; // water is added after generation, see below
        settings.smoothTileEdges = true;
        settings.beaches = false;
        settings.trees = false; // scenery just blocks track placement in the eval
        settings.landTexture = objectManager.GetLoadedObjectEntryIndex(ObjectEntryDescriptor(derived.surface));
        settings.edgeTexture = objectManager.GetLoadedObjectEntryIndex(ObjectEntryDescriptor(derived.edge));

        // Everything the map generator randomises goes through UtilRand.
        UtilSRand(options.seed);
        World::MapGenerator::generate(&settings);

        // FBM noise piles heights up around the middle of the configured range,
        // so a water level derived from that range floods half the map. Pick it
        // from the actual height distribution instead: flood roughly the lowest
        // 6% of tiles per water step.
        int32_t waterLevel = 0;
        if (derived.water > 0)
        {
            std::array<int64_t, 256> histogram{};
            int64_t tiles = 0;
            for (int32_t y = 1; y <= options.mapSize - 2; y++)
            {
                for (int32_t x = 1; x <= options.mapSize - 2; x++)
                {
                    auto* surface = MapGetSurfaceElementAt(TileCoordsXY{ x, y });
                    if (surface == nullptr)
                        continue;
                    histogram[surface->baseHeight]++;
                    tiles++;
                }
            }
            // Flood while staying under ~8% of tiles per water step: with a
            // coarse height distribution (low hilliness) a single level can
            // hold most of the map, and overshooting there drowns the park.
            const int64_t cap = tiles * derived.water * 8 / 100;
            int64_t cumulative = 0;
            for (int32_t level = 0; level < 255; level++)
            {
                cumulative += histogram[level];
                if (cumulative > cap)
                    break;
                if (histogram[level] > 0)
                    waterLevel = level + 1;
            }
            if (waterLevel > 0)
                World::MapGenerator::setWaterLevel(waterLevel);
        }

        gameState.park.name = "CoasterBench seed " + std::to_string(options.seed);
        SetEveryRideTypeInvented();
        if (options.cashGBP >= 0)
        {
            gameState.park.flags &= ~PARK_FLAGS_NO_MONEY;
            gameState.park.cash = ToMoney64FromGBP(options.cashGBP);
            gameState.scenarioOptions.initialCash = gameState.park.cash;
        }

        auto survey = SurveyMap(options.mapSize);

        json_t hints;
        hints["generator"] = "coasterbench-make-park";
        hints["seed"] = options.seed;
        hints["map_size"] = options.mapSize;
        hints["playable"] = { { "min", 1 }, { "max", options.mapSize - 2 } };
        hints["hilliness"] = derived.hilliness;
        hints["water"] = derived.water;
        // Heights in world z-units, the same scale as track pieces (up_25
        // rises 16 z-units per tile): surface baseHeight * kCoordsZStep.
        hints["surface_z"] = { { "min", survey.heightMin * kCoordsZStep }, { "max", survey.heightMax * kCoordsZStep } };
        if (waterLevel > 0)
            hints["water_level_z"] = waterLevel * kCoordsZStep;
        hints["water_fraction"] = survey.waterFraction;
        hints["flat_open_fraction"] = survey.flatOpenFraction;
        hints["start"] = { { "x", survey.startX },
                           { "y", survey.startY },
                           { "surface_z", survey.startHeight * kCoordsZStep },
                           { "open_square", survey.openSquare } };
        if (options.cashGBP >= 0)
            hints["cash"] = options.cashGBP;

        auto hintsPath = HintsPathFor(options.outPath);
        try
        {
            File::WriteAllBytes(hintsPath, hints.dump(2).c_str(), hints.dump(2).size());
        }
        catch (const std::exception& e)
        {
            Console::Error::WriteLine("Failed to write hints sidecar %s: %s", hintsPath.c_str(), e.what());
            return 1;
        }

        try
        {
            // Pin the authoring timestamp so identical seeds give identical bytes.
            gParkFileAuthoringTimeOverride = 0x5EEDuLL;
            auto exporter = std::make_unique<ParkFileExporter>();
            exporter->Export(gameState, options.outPath, kParkFileSaveCompressionLevel);
            gParkFileAuthoringTimeOverride = 0;
        }
        catch (const std::exception& e)
        {
            gParkFileAuthoringTimeOverride = 0;
            Console::Error::WriteLine("Failed to export park %s: %s", options.outPath.c_str(), e.what());
            return 1;
        }

        Console::WriteLine(
            "Generated %s (seed %u): size %d, hilliness %d, water %d (%.0f%% flooded), start anchor (%d, %d)",
            options.outPath.c_str(), options.seed, options.mapSize, derived.hilliness, derived.water,
            survey.waterFraction * 100.0, survey.startX, survey.startY);
        return 0;
    }
} // namespace OpenRCT2::ParkGen

#endif // ENABLE_RUST_AGENT
