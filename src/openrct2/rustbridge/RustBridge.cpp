/*****************************************************************************
 * Fork-only file (wseaton/OpenRCT2): C++ side of the Rust agent bridge.
 *
 * Also implements the orct2_host_* functions the Rust side imports. They live
 * in this TU on purpose: Context.cpp references RustBridge::Initialise, which
 * guarantees the linker pulls these definitions in before resolving the Rust
 * staticlib's undefined host symbols.
 *****************************************************************************/

#ifdef ENABLE_RUST_AGENT

    #include "RustBridge.h"

    #include "../Cheats.h"
    #include "../Context.h"
    #include "../Diagnostic.h"
    #include "../GameState.h"
    #include "../PlatformEnvironment.h"
    #include "../actions/GameActionRunner.h"
    #include "../actions/cheats/CheatSetAction.h"
    #include "../actions/ride/RideCreateAction.h"
    #include "../actions/ride/RideDemolishAction.h"
    #include "../actions/ride/RideEntranceExitPlaceAction.h"
    #include "../actions/ride/RideSetStatusAction.h"
    #include "../actions/track/TrackPlaceAction.h"
    #include "../actions/track/TrackRemoveAction.h"
    #include "../drawing/Drawing.h"
    #include "../OpenRCT2.h"
    #include "../core/Console.hpp"
    #include "../core/FileScanner.h"
    #include "../core/Imaging.h"
    #include "../core/Json.hpp"
    #include "../core/Path.hpp"
    #include "../core/String.hpp"
    #include "../interface/Screenshot.h"
    #include "../interface/Viewport.h"
    #include "../object/ObjectLimits.h"
    #include "../ride/Ride.h"
    #include "../ride/RideData.h"
    #include "../ride/RideEntry.h"
    #include "../ride/RideManager.hpp"
    #include "../ride/RideRatings.h"
    #include "../ride/TrackData.h"
    #include "../ride/TrackStyle.h"
    #include "../ride/TrackDesign.h"
    #include "../ride/TrackDesignRepository.h"
    #include "../ride/ted/TrackElemType.h"
    #include "../world/Map.h"
    #include "orct2_agent.h"

    #include <algorithm>
    #include <cctype>
    #include <cstdlib>
    #include <cstring>
    #include <unistd.h>

using namespace OpenRCT2;

namespace
{
    void CopyError(char* err, size_t errLen, const std::string& message)
    {
        if (err != nullptr && errLen > 0)
        {
            String::safeUtf8Copy(err, message.c_str(), errLen);
        }
    }

    // Runs an action with noSpend (evals are judged on reported cost, not park
    // cash) and copies any error message into the caller's buffer.
    bool ExecuteForBridge(GameActions::GameAction& action, char* err, size_t errLen, GameActions::Result& outResult)
    {
        action.SetFlags(action.GetFlags() | GameActions::CommandFlag::noSpend);
        outResult = GameActions::Execute(&action, getGameState());
        if (outResult.error != GameActions::Status::ok)
        {
            auto message = outResult.getErrorMessage();
            if (message.empty())
            {
                message = outResult.getErrorTitle();
            }
            CopyError(err, errLen, message);
            return false;
        }
        return true;
    }

    // Query-only variant of ExecuteForBridge: validates without mutating.
    bool QueryForBridge(GameActions::GameAction& action, char* err, size_t errLen)
    {
        action.SetFlags(action.GetFlags() | GameActions::CommandFlag::noSpend);
        auto result = GameActions::Query(&action, getGameState());
        if (result.error != GameActions::Status::ok)
        {
            auto message = result.getErrorMessage();
            if (message.empty())
            {
                message = result.getErrorTitle();
            }
            CopyError(err, errLen, message);
            return false;
        }
        return true;
    }

    // Scans the whole map for track elements and returns their tile bbox and
    // world-z range. Engine-authoritative: includes exactly what was placed.
    bool FindTrackBounds(Orct2TrackBounds& out)
    {
        bool found = false;
        out = { INT32_MAX, INT32_MAX, INT32_MIN, INT32_MIN, INT32_MAX, INT32_MIN };
        const auto mapSize = getGameState().mapSize;
        for (int32_t ty = 0; ty < mapSize.y; ty++)
        {
            for (int32_t tx = 0; tx < mapSize.x; tx++)
            {
                auto* element = MapGetFirstElementAt(TileCoordsXY{ tx, ty });
                if (element == nullptr)
                {
                    continue;
                }
                do
                {
                    if (element->getType() != TileElementType::track)
                    {
                        continue;
                    }
                    found = true;
                    out.min_tile_x = std::min(out.min_tile_x, tx);
                    out.min_tile_y = std::min(out.min_tile_y, ty);
                    out.max_tile_x = std::max(out.max_tile_x, tx);
                    out.max_tile_y = std::max(out.max_tile_y, ty);
                    out.min_z = std::min(out.min_z, element->getBaseZ());
                    out.max_z = std::max(out.max_z, element->getClearanceZ());
                } while (!(element++)->isLastForTile());
            }
        }
        return found;
    }

    // View that tightly frames the track bbox: project all 8 corners into
    // screen space, take their extent, and pad. CaptureImage centres the view
    // on Position at terrain height, so pad further by the offset between that
    // centre and the projected bbox centre.
    CaptureView FitViewToTrack(const Orct2TrackBounds& bounds, uint8_t rotation)
    {
        constexpr int32_t kMarginPx = 48;
        const CoordsXY worldMin{ bounds.min_tile_x * kCoordsXYStep, bounds.min_tile_y * kCoordsXYStep };
        const CoordsXY worldMax{ (bounds.max_tile_x + 1) * kCoordsXYStep, (bounds.max_tile_y + 1) * kCoordsXYStep };

        ScreenCoordsXY projMin{ INT32_MAX, INT32_MAX };
        ScreenCoordsXY projMax{ INT32_MIN, INT32_MIN };
        for (auto x : { worldMin.x, worldMax.x })
        {
            for (auto y : { worldMin.y, worldMax.y })
            {
                for (auto z : { bounds.min_z, bounds.max_z })
                {
                    auto proj = Translate3DTo2DWithZ(rotation, CoordsXYZ{ x, y, z });
                    projMin.x = std::min(projMin.x, proj.x);
                    projMin.y = std::min(projMin.y, proj.y);
                    projMax.x = std::max(projMax.x, proj.x);
                    projMax.y = std::max(projMax.y, proj.y);
                }
            }
        }

        CaptureView view;
        view.Position = { (worldMin.x + worldMax.x) / 2, (worldMin.y + worldMax.y) / 2 };
        auto centreZ = TileElementHeight(view.Position);
        auto centreProj = Translate3DTo2DWithZ(rotation, CoordsXYZ(view.Position, centreZ));
        view.Width = projMax.x - projMin.x + 2 * (kMarginPx + std::abs(centreProj.x - (projMin.x + projMax.x) / 2));
        view.Height = projMax.y - projMin.y + 2 * (kMarginPx + std::abs(centreProj.y - (projMin.y + projMax.y) / 2));
        return view;
    }

    const char* RollName(TrackMetadata::TrackRoll roll)
    {
        switch (roll)
        {
            case TrackMetadata::TrackRoll::none:
                return "unbanked";
            case TrackMetadata::TrackRoll::left:
                return "left-banked";
            case TrackMetadata::TrackRoll::right:
                return "right-banked";
            default:
                return "inverted";
        }
    }

    const char* PitchName(TrackMetadata::TrackPitch pitch)
    {
        switch (pitch)
        {
            case TrackMetadata::TrackPitch::none:
                return "flat";
            case TrackMetadata::TrackPitch::up25:
                return "25-up";
            case TrackMetadata::TrackPitch::up60:
                return "60-up";
            case TrackMetadata::TrackPitch::down25:
                return "25-down";
            case TrackMetadata::TrackPitch::down60:
                return "60-down";
            default:
                return "vertical";
        }
    }

    // TrackPlaceAction validates placement legality but NOT continuity with
    // the previous piece; the game's circuit walk (findTrackGap) additionally
    // requires each piece's entry roll/pitch to match the previous exit. A
    // mismatched piece places fine but leaves an invisible gap, so enforce the
    // continuity rule here and fail with an actionable message instead.
    bool CheckEntryContinuity(TrackElemType trackType, const Orct2TrackCursor& cursor, char* err, size_t errLen)
    {
        const auto& def = TrackMetadata::GetTrackElementDescriptor(trackType).definition;
        auto cursorRoll = static_cast<TrackMetadata::TrackRoll>(cursor.bank);
        auto cursorPitch = static_cast<TrackMetadata::TrackPitch>(cursor.slope);
        if (def.rollStart != cursorRoll)
        {
            CopyError(
                err, errLen,
                std::string("piece enters ") + RollName(def.rollStart) + " but the track here is "
                    + RollName(cursorRoll) + "; add a banking transition piece first");
            return false;
        }
        if (def.pitchStart != cursorPitch)
        {
            CopyError(
                err, errLen,
                std::string("piece enters at ") + PitchName(def.pitchStart) + " slope but the track here is "
                    + PitchName(cursorPitch) + "; add a slope transition piece first");
            return false;
        }
        return true;
    }

    // TrackPlaceAction also never checks whether the ride type can *draw* the
    // piece: the enabled-track-group gate only feeds the construction window,
    // so a piece the ride style has no artwork for places fine, rates fine, and
    // then paints nothing at all (no track, no supports) because the paint
    // dispatch falls through to TrackPaintFunctionDummy. Wooden coasters and
    // the 1-tile flat<->60 transitions are the classic case. Ask the same
    // dispatch the renderer uses rather than the ride's track groups: the
    // groups also gate pieces that draw perfectly well.
    bool CheckPieceDrawable(const Ride& ride, TrackElemType trackType, char* err, size_t errLen)
    {
        const auto& rtd = ride.getRideTypeDescriptor();
        const auto drawerEntry = getTrackDrawerEntry(rtd, false, trackTypeIsCovered(trackType));
        auto& paintFunction = GetTrackPaintFunction(
            drawerEntry.trackStyle, uncoverTrackType(trackType));
        if (&paintFunction != &TrackPaintFunctionDummy)
        {
            return true;
        }
        CopyError(
            err, errLen,
            "ride type " + std::to_string(ride.type)
                + " has no artwork for this piece: it would build and rate but render as nothing (invisible track). "
                  "valid_next_pieces lists what this ride can draw here");
        return false;
    }

    // First loaded ride object whose entry supports the requested ride type.
    ObjectEntryIndex FindSubtypeForRideType(ride_type_t rideType)
    {
        for (ObjectEntryIndex i = 0; i < kMaxRideObjects; i++)
        {
            const auto* entry = GetRideEntryByIndex(i);
            if (entry == nullptr)
            {
                continue;
            }
            for (auto entryRideType : entry->ride_type)
            {
                if (entryRideType == rideType)
                {
                    return i;
                }
            }
        }
        return kObjectEntryIndexNull;
    }

    // Iterates every importable track design in the same directories the
    // game's own track design index scans. Skips mazes (no track elements).
    template<typename TCallback>
    void ForEachLibraryDesign(TCallback&& callback)
    {
        auto& env = GetContext()->GetPlatformEnvironment();
        for (auto base : { DirBase::rct1, DirBase::rct2, DirBase::user })
        {
            auto directory = env.GetDirectoryPath(base, DirId::trackDesigns);
            if (directory.empty())
            {
                continue;
            }
            auto pattern = Path::Combine(directory, u8"*.td4;*.td6;*.td7");
            auto scanner = Path::ScanDirectory(pattern, true);
            while (scanner->Next())
            {
                const auto& path = scanner->GetPath();
                auto td = TrackDesignImport(path.c_str());
                if (td == nullptr || td->trackElements.empty())
                {
                    continue;
                }
                callback(GetNameFromTrackPath(path), *td);
            }
        }
    }
} // namespace

extern "C" {

uint32_t orct2_host_ride_count(void)
{
    return static_cast<uint32_t>(RideManager(getGameState()).size());
}

bool orct2_host_ride_stats(uint32_t index, Orct2RideStats* out)
{
    if (out == nullptr)
    {
        return false;
    }

    // RideIds are sparse; the manager iterator skips gaps, so walk to the
    // index-th live ride. O(n) per call but ride counts are tiny.
    uint32_t position = 0;
    for (const auto& ride : RideManager(getGameState()))
    {
        if (position == index)
        {
            *out = Orct2RideStats{
                .id = ride.id.ToUnderlying(),
                .ride_type = ride.type,
                .status = static_cast<uint8_t>(ride.status),
                .is_stall = ride.getRideTypeDescriptor().RatingsData.Type == RatingsCalculationType::Stall,
                .has_ratings = !ride.ratings.isNull(),
                .excitement = ride.ratings.excitement,
                .intensity = ride.ratings.intensity,
                .nausea = ride.ratings.nausea,
            };
            return true;
        }
        position++;
    }
    return false;
}

void orct2_host_log(const char* msg)
{
    if (msg != nullptr)
    {
        Console::WriteLine("%s", msg);
    }
}

bool orct2_host_sandbox(void)
{
    auto action = GameActions::CheatSetAction(CheatType::sandboxMode, 1);
    GameActions::Result result;
    return ExecuteForBridge(action, nullptr, 0, result);
}

int32_t orct2_host_surface_z(int32_t tile_x, int32_t tile_y)
{
    return TileElementHeight(CoordsXY{ tile_x * kCoordsXYStep + 16, tile_y * kCoordsXYStep + 16 });
}

bool orct2_host_ride_create(uint16_t ride_type, uint16_t* out_ride_id, char* err, size_t err_len)
{
    if (out_ride_id == nullptr)
    {
        return false;
    }
    auto subtype = FindSubtypeForRideType(ride_type);
    if (subtype == kObjectEntryIndexNull)
    {
        CopyError(err, err_len, "no ride object available for this ride type in the loaded park");
        return false;
    }
    auto action = GameActions::RideCreateAction(
        ride_type, subtype, 0, 0, getGameState().lastEntranceStyle, RideInspection::every30Minutes);
    GameActions::Result result;
    if (!ExecuteForBridge(action, err, err_len, result))
    {
        return false;
    }
    *out_ride_id = result.getData<RideId>().ToUnderlying();
    return true;
}

bool orct2_host_track_place(
    uint16_t ride_id, uint16_t track_type, bool chain_lift, Orct2TrackCursor* cursor, int64_t* out_cost, char* err,
    size_t err_len)
{
    if (cursor == nullptr || out_cost == nullptr)
    {
        return false;
    }
    auto rideId = RideId::FromUnderlying(ride_id);
    const auto* ride = GetRide(rideId);
    if (ride == nullptr)
    {
        CopyError(err, err_len, "no such ride");
        return false;
    }

    auto trackType = static_cast<TrackElemType>(track_type);
    const auto& ted = TrackMetadata::GetTrackElementDescriptor(trackType);
    const auto& coords = ted.coordinates;

    if (!CheckPieceDrawable(*ride, trackType, err, err_len)
        || !CheckEntryContinuity(trackType, *cursor, err, err_len))
    {
        return false;
    }

    SelectedLiftAndInverted liftFlags{};
    if (chain_lift)
    {
        liftFlags.set(LiftHillAndInverted::liftHill);
    }

    // The piece origin z compensates for pieces whose geometry starts above
    // their base (mirrors TrackDesignPlaceRide, TrackDesign.cpp).
    CoordsXYZD origin{ cursor->x, cursor->y, cursor->z - coords.zBegin, static_cast<Direction>(cursor->direction & 3) };
    auto action = GameActions::TrackPlaceAction(
        rideId, trackType, ride->type, origin, 0 /*brakeSpeed*/, 0 /*colour*/, 4 /*seatRotation*/, liftFlags,
        false /*fromTrackDesign*/);
    GameActions::Result result;
    if (!ExecuteForBridge(action, err, err_len, result))
    {
        return false;
    }
    *out_cost = result.cost;

    // Advance the cursor along the piece geometry (TrackDesign.cpp:1707).
    uint8_t rotation = cursor->direction & 3;
    auto offset = CoordsXY{ coords.x, coords.y }.Rotate(rotation);
    CoordsXYZ next{ cursor->x + offset.x, cursor->y + offset.y, cursor->z - coords.zBegin + coords.zEnd };
    rotation = (rotation + coords.rotationEnd - coords.rotationBegin) & 3;
    if ((coords.rotationEnd & (1 << 2)) == 0)
    {
        next += CoordsDirectionDelta[rotation];
    }
    cursor->x = next.x;
    cursor->y = next.y;
    cursor->z = next.z;
    cursor->direction = rotation;
    cursor->bank = static_cast<uint8_t>(ted.definition.rollEnd);
    cursor->slope = static_cast<uint8_t>(ted.definition.pitchEnd);
    return true;
}

bool orct2_host_ride_set_status(uint16_t ride_id, uint8_t status, char* err, size_t err_len)
{
    if (status >= static_cast<uint8_t>(RideStatus::count))
    {
        CopyError(err, err_len, "invalid ride status");
        return false;
    }
    auto action = GameActions::RideSetStatusAction(RideId::FromUnderlying(ride_id), static_cast<RideStatus>(status));
    GameActions::Result result;
    return ExecuteForBridge(action, err, err_len, result);
}

bool orct2_host_ride_detail(uint16_t ride_id, Orct2RideDetail* out)
{
    if (out == nullptr)
    {
        return false;
    }
    const auto* ride = GetRide(RideId::FromUnderlying(ride_id));
    if (ride == nullptr)
    {
        return false;
    }
    *out = Orct2RideDetail{
        .tested = ride->flags.has(RideFlag::tested),
        .crashed = ride->flags.has(RideFlag::crashed),
        .excitement = ride->ratings.excitement,
        .intensity = ride->ratings.intensity,
        .nausea = ride->ratings.nausea,
        .max_speed = ride->maxSpeed,
        .average_speed = ride->averageSpeed,
        .ride_length = ride->getTotalLength(),
        .max_positive_g = ride->maxPositiveVerticalG,
        .max_negative_g = ride->maxNegativeVerticalG,
        .max_lateral_g = ride->maxLateralG,
        .total_air_time = ride->totalAirTime,
        .num_drops = ride->numDrops,
        .highest_drop = ride->highestDropHeight,
        .num_inversions = ride->numInversions,
    };
    return true;
}

bool orct2_host_entrance_place(
    uint16_t ride_id, int32_t tile_x, int32_t tile_y, uint8_t direction, bool is_exit, char* err, size_t err_len)
{
    auto action = GameActions::RideEntranceExitPlaceAction(
        CoordsXY{ tile_x * kCoordsXYStep, tile_y * kCoordsXYStep }, static_cast<Direction>(direction & 3),
        RideId::FromUnderlying(ride_id), StationIndex::FromUnderlying(0), is_exit);
    GameActions::Result result;
    return ExecuteForBridge(action, err, err_len, result);
}

bool orct2_host_track_remove(uint16_t ride_id, uint16_t track_type, const Orct2TrackCursor* origin, char* err, size_t err_len)
{
    if (origin == nullptr)
    {
        return false;
    }
    auto trackType = static_cast<TrackElemType>(track_type);
    const auto& coords = TrackMetadata::GetTrackElementDescriptor(trackType).coordinates;
    // Same origin-z compensation as placement.
    CoordsXYZD at{ origin->x, origin->y, origin->z - coords.zBegin, static_cast<Direction>(origin->direction & 3) };
    auto action = GameActions::TrackRemoveAction(trackType, 0 /*sequence*/, at);
    GameActions::Result result;
    if (!ExecuteForBridge(action, err, err_len, result))
    {
        return false;
    }
    (void)ride_id;
    return true;
}

bool orct2_host_piece_delta(uint16_t track_type, uint8_t dir_in, Orct2TrackCursor* out)
{
    if (out == nullptr)
    {
        return false;
    }
    const auto& ted = TrackMetadata::GetTrackElementDescriptor(static_cast<TrackElemType>(track_type));
    const auto& coords = ted.coordinates;
    // Mirror of the cursor-advance math in orct2_host_track_place, from a
    // zero origin, so callers see the pure displacement for this facing.
    uint8_t rotation = dir_in & 3;
    auto offset = CoordsXY{ coords.x, coords.y }.Rotate(rotation);
    CoordsXYZ next{ offset.x, offset.y, coords.zEnd - coords.zBegin };
    rotation = (rotation + coords.rotationEnd - coords.rotationBegin) & 3;
    if ((coords.rotationEnd & (1 << 2)) == 0)
    {
        next += CoordsDirectionDelta[rotation];
    }
    out->x = next.x;
    out->y = next.y;
    out->z = next.z;
    out->direction = rotation;
    out->bank = static_cast<uint8_t>(ted.definition.rollEnd);
    out->slope = static_cast<uint8_t>(ted.definition.pitchEnd);
    return true;
}

void orct2_host_run_ticks(uint32_t n)
{
    for (uint32_t i = 0; i < n; i++)
    {
        gameStateUpdateLogic();
    }
}

bool orct2_host_track_query(uint16_t ride_id, uint16_t track_type, const Orct2TrackCursor* cursor, char* err, size_t err_len)
{
    if (cursor == nullptr)
    {
        return false;
    }
    auto rideId = RideId::FromUnderlying(ride_id);
    const auto* ride = GetRide(rideId);
    if (ride == nullptr)
    {
        CopyError(err, err_len, "no such ride");
        return false;
    }
    auto trackType = static_cast<TrackElemType>(track_type);
    const auto& coords = TrackMetadata::GetTrackElementDescriptor(trackType).coordinates;
    if (!CheckPieceDrawable(*ride, trackType, err, err_len)
        || !CheckEntryContinuity(trackType, *cursor, err, err_len))
    {
        return false;
    }
    CoordsXYZD origin{ cursor->x, cursor->y, cursor->z - coords.zBegin, static_cast<Direction>(cursor->direction & 3) };
    auto action = GameActions::TrackPlaceAction(
        rideId, trackType, ride->type, origin, 0, 0, 4, SelectedLiftAndInverted{}, false);
    return QueryForBridge(action, err, err_len);
}

bool orct2_host_ride_demolish(uint16_t ride_id, char* err, size_t err_len)
{
    auto action = GameActions::RideDemolishAction(
        RideId::FromUnderlying(ride_id), GameActions::RideModifyType::demolish);
    GameActions::Result result;
    return ExecuteForBridge(action, err, err_len, result);
}

void orct2_host_update_ratings(uint16_t ride_id)
{
    const auto* ride = GetRide(RideId::FromUnderlying(ride_id));
    if (ride != nullptr && ride->status != RideStatus::closed)
    {
        RideRating::UpdateRide(*ride);
    }
}

bool orct2_host_track_bounds(Orct2TrackBounds* out)
{
    if (out == nullptr)
    {
        return false;
    }
    return FindTrackBounds(*out);
}

// Scans the same directories as the game's track design index and imports
// every design into a JSON array of {name, ride_type, pieces}. The result is
// malloc'd; the Rust side frees it via orct2_host_string_free. Maze designs
// (no track elements) are skipped: there is nothing to compare.
char* orct2_host_track_library_json(void)
{
    try
    {
        json_t library = json_t::array();
        ForEachLibraryDesign([&library](const std::string& name, const TrackDesign& td) {
            json_t pieces = json_t::array();
            for (const auto& element : td.trackElements)
            {
                pieces.push_back(static_cast<uint16_t>(element.type));
            }
            json_t entry;
            entry["name"] = name;
            entry["ride_type"] = static_cast<uint16_t>(td.trackAndVehicle.rtdIndex);
            entry["pieces"] = std::move(pieces);
            library.push_back(std::move(entry));
        });
        auto dumped = library.dump();
        auto* out = static_cast<char*>(std::malloc(dumped.size() + 1));
        if (out == nullptr)
        {
            return nullptr;
        }
        std::memcpy(out, dumped.c_str(), dumped.size() + 1);
        return out;
    }
    catch (const std::exception& e)
    {
        LOG_ERROR("track library scan failed: %s", e.what());
        return nullptr;
    }
}

void orct2_host_string_free(char* s)
{
    std::free(s);
}

uint16_t orct2_host_track_mirror(uint16_t track_type)
{
    const auto& ted = TrackMetadata::GetTrackElementDescriptor(static_cast<TrackElemType>(track_type));
    auto mirror = ted.mirrorElement;
    return mirror == TrackElemType::none ? track_type : static_cast<uint16_t>(mirror);
}

bool orct2_host_graphics_available(void)
{
    return !gOpenRCT2NoGraphics;
}

bool orct2_host_capture(const char* path, int32_t zoom, uint8_t rotation, bool fit_track, bool xray)
{
    if (path == nullptr)
    {
        return false;
    }
    if (gOpenRCT2NoGraphics)
    {
        LOG_ERROR("screenshot capture unavailable: running with --no-graphics (no sprite data loaded)");
        return false;
    }
    try
    {
        // CaptureImage refuses paths outside the screenshot directory, so
        // render to a fixed name there and copy to the requested path.
        auto& env = GetContext()->GetPlatformEnvironment();
        auto screenshotDir = fs::u8path(env.GetDirectoryPath(DirBase::user, DirId::screenshots));
        fs::create_directories(screenshotDir);

        CaptureOptions options;
        // Per-process temp name: parallel replay harnesses run several
        // game instances at once, and a shared temp file gets clobbered
        // mid-copy (truncated PNGs).
        options.Filename = fs::u8path("orct2-agent-capture-" + std::to_string(getpid()) + ".png");
        options.Zoom = ZoomLevel{ static_cast<int8_t>(zoom) };
        options.Rotation = rotation & 3;
        if (xray)
        {
            // Verification view: hide terrain so tunnelled track is visible,
            // and hide supports to declutter. Note the upstream sprite-sort
            // glitch that hides track near track-over-track crossings at some
            // rotations is NOT fixed by these flags; it lives in the painter's
            // draw ordering itself.
            options.ViewFlags = VIEWPORT_FLAG_UNDERGROUND_INSIDE | VIEWPORT_FLAG_HIDE_BASE | VIEWPORT_FLAG_HIDE_VERTICAL
                | VIEWPORT_FLAG_HIDE_SUPPORTS;
        }
        if (fit_track)
        {
            Orct2TrackBounds bounds;
            if (FindTrackBounds(bounds))
            {
                options.View = FitViewToTrack(bounds, options.Rotation);
            }
        }
        CaptureImage(options);

        auto rendered = screenshotDir / options.Filename;
        fs::copy_file(rendered, fs::u8path(path), fs::copy_options::overwrite_existing);
        fs::remove(rendered);
        return true;
    }
    catch (const std::exception& e)
    {
        LOG_ERROR("capture failed: %s", e.what());
        return false;
    }
}

} // extern "C"

namespace OpenRCT2::RustBridge
{
    void Initialise()
    {
        auto result = orct2_agent_init();
        if (result != 0)
        {
            LOG_ERROR("Rust agent bridge failed to initialise (code %d)", result);
        }
        else
        {
            LOG_VERBOSE("Rust agent bridge initialised");
        }
    }

    void Tick(uint32_t tick)
    {
        orct2_agent_tick(tick);
    }

    void EvalSummary()
    {
        orct2_agent_eval_summary();
    }

    Orct2ProgramOutcome* RunProgram(const char* path)
    {
        return orct2_agent_run_program(path);
    }

    int32_t EvalFinish(Orct2ProgramOutcome* outcome, const char* outPath)
    {
        return orct2_agent_eval_finish(outcome, outPath);
    }

    int32_t Capture(const char* path, int32_t zoom, uint8_t rotation, bool fitTrack, bool xray)
    {
        return orct2_agent_capture(path, zoom, rotation, fitTrack, xray);
    }

    int32_t Serve(const char* bind, uint16_t port)
    {
        return orct2_agent_serve(bind, port);
    }

    int32_t DumpLibrary(const char* path)
    {
        return orct2_agent_dump_library(path);
    }

    int32_t RenderTrackLibrary(const char* outDir)
    {
        // Same rule as evals/site.py sanitise_name(): [^A-Za-z0-9._-] -> '_'.
        auto sanitise = [](const std::string& name) {
            std::string out = name;
            for (auto& c : out)
            {
                auto uc = static_cast<unsigned char>(c);
                if (!(std::isalnum(uc) || c == '.' || c == '_' || c == '-'))
                {
                    c = '_';
                }
            }
            return out;
        };

        constexpr uint32_t kPreviewWidth = 370;
        constexpr uint32_t kPreviewHeight = 217;
        static_assert(kPreviewWidth * kPreviewHeight == kTrackPreviewImageSize);

        try
        {
            u8string out{ outDir };
            if (!Path::CreateDirectory(out))
            {
                LOG_ERROR("could not create %s", out.c_str());
                return 1;
            }

            // TrackDesignDrawPreview only auto-loads the design's vehicle and
            // scenery objects in the track designs manager scene; flip it for
            // the duration of the render loop.
            auto previousScene = gLegacyScene;
            gLegacyScene = LegacyScene::trackDesignsManager;

            size_t rendered = 0;
            size_t failed = 0;
            // 4 rotations x 370x217 = ~321 KiB; too big for the stack.
            auto pixels = std::make_unique<TrackDesignPreviewBuffer>();
            ForEachLibraryDesign([&](const std::string& name, TrackDesign& td) {
                TrackDesignDrawPreview(td, *pixels, true /*placeScenery*/);
                // Placement failure leaves the buffer fully transparent.
                auto begin = pixels->begin();
                auto end = begin + kTrackPreviewImageSize;
                if (std::all_of(begin, end, [](Drawing::PaletteIndex p) { return p == Drawing::PaletteIndex::transparent; }))
                {
                    LOG_WARNING("track preview failed for %s", name.c_str());
                    failed++;
                    return;
                }
                // Rotation 0 is the first 370x217 slice of the buffer.
                Image image;
                image.Width = kPreviewWidth;
                image.Height = kPreviewHeight;
                image.Depth = 8;
                image.Stride = kPreviewWidth;
                image.Palette = gPalette;
                auto* bytes = reinterpret_cast<const uint8_t*>(pixels->data());
                image.Pixels = std::vector<uint8_t>(bytes, bytes + kTrackPreviewImageSize);
                auto path = Path::Combine(out, sanitise(name) + ".png");
                // Duplicate design names exist (two stock "Goliath"s); first
                // wins, matching get_track_design's first-match lookup.
                if (fs::exists(fs::u8path(path)))
                {
                    return;
                }
                Imaging::WriteToFile(path, image, ImageFormat::png);
                rendered++;
            });

            gLegacyScene = previousScene;
            Console::WriteLine("orct2-agent: rendered %zu track design preview(s), %zu failed", rendered, failed);
            return 0;
        }
        catch (const std::exception& e)
        {
            LOG_ERROR("track library render failed: %s", e.what());
            return 1;
        }
    }
} // namespace OpenRCT2::RustBridge

#endif // ENABLE_RUST_AGENT
