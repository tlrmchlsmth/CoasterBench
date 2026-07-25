//! Round prompt for the sandboxed Claude Code agent. Interactive edition of
//! the driver.py system prompt: the MCP tools replace the memorised geometry
//! tables (valid_next_pieces and piece_geometry are ground truth).

use crate::Modalities;

pub fn ride_name(ride_type: u16) -> &'static str {
    match ride_type {
        51 => "steel twister roller coaster",
        52 => "wooden roller coaster",
        _ => "roller coaster",
    }
}

/// The default map line, describing the asset-full default scenario
/// (Build your own Six Flags Park). Generated parks pass their own line,
/// rendered from the --make-park hints sidecar.
pub const DEFAULT_MAP_LINE: &str = "flat grass around tile (60, 60); a lake sits roughly at tiles \
    (68-85, 55-75) - do NOT build into it. Stay within tiles 20-120.";

pub fn round_prompt(
    ride_type: u16,
    round: u32,
    rounds: u32,
    previous_feedback: Option<&str>,
    modalities: Modalities,
    budget_secs: u64,
    map_line: Option<&str>,
) -> String {
    let name = ride_name(ride_type);
    let map_line = map_line.unwrap_or(DEFAULT_MAP_LINE);
    let budget_mins = budget_secs / 60;
    // Models that can't take images get an MCP endpoint with screenshot
    // stripped, so the tool list must not advertise it either.
    let screenshot = if modalities.contains(Modalities::IMAGE) {
        "\n- screenshot(): park image."
    } else {
        ""
    };
    let inversions = if ride_type == 51 {
        "Inversions are ALLOWED and rewarded (vertical loops, corkscrews, half loops)."
    } else {
        "This type does NOT support inversions."
    };
    let feedback = match previous_feedback {
        Some(report) => format!(
            "\n## Previous round result\nYour previous attempt's eval report (learn from it):\n{report}\n"
        ),
        None => String::new(),
    };
    format!(
        r#"You are competing to design the best RollerCoaster Tycoon 2 {name} (ride_type {ride_type}).
This is round {round} of {rounds}. You build interactively through the "coaster" MCP tools:

- new_ride(ride_type, x, y, dir): start building; the cursor starts at that tile. Directions: 0 faces -x, 1 faces +y, 2 faces +x, 3 faces -y.
- place_piece(piece, chain?): place one piece at the cursor. Returns the new cursor and circuit_closed.
- place_pieces(pieces): batch placement; stops at the first rejection.
- valid_next_pieces(): which catalog pieces the game will accept at the current cursor. Ground truth - use it whenever unsure.
- piece_geometry(dir): exact cursor delta (tiles, z, direction, bank, slope) for every piece from a given direction. Use this to PLAN closure.
- undo_piece(): remove the last piece.
- get_state(): cursor, start, pieces placed, circuit_closed.
- finish_and_test(ticks?): places entrance/exit, runs a real test train, returns the full eval report with excitement/intensity/nausea.
- best_result(): the highest-excitement finish_and_test from this round so far. Your score is this, not your latest build.{screenshot}
- demolish(): tear down and start over (then new_ride again).

## Rules
- Build a CLOSED CIRCUIT: get_state must show circuit_closed before finish_and_test will pass.
- Start with begin_station, middle_station, end_station on flat ground (3-7 station pieces).
- Chain lift ({{"chain": true}}) only works on 25-degree slopes. Trains start slow; climb first, then coast.
- Intensity above ~10 tanks excitement; keep it under 10. Crashes disqualify.
- {inversions}
- Your track is compared to the stock design library; similarity above 0.5 scales your score toward zero. Design something original.
- Map: {map_line}

## How to work
1. Plan a layout, then new_ride and build with place_pieces in chunks.
2. On rejection, read the error, use valid_next_pieces/piece_geometry, fix, continue.
3. Close the circuit (watch cursor vs start in get_state; plan the return leg with piece_geometry).
4. finish_and_test as soon as you have a closed circuit, so you bank a score early.
5. Only then experiment: demolish and rebuild to beat it. A worse or unfinished rebuild costs nothing, because best_result keeps your highest score.
6. End with a one-line summary of your best coaster and its excitement.

## Budget
You have about {budget_mins} minutes of wall-clock this round, then the session is cut off.
Get a working, tested circuit banked EARLY; do not spend the whole budget on one perfect build.
Your score is your best finish_and_test of the round (see best_result), not your final build,
so it is always safe to stop once you are happy.
{feedback}"#
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    fn both() -> Modalities {
        Modalities::TEXT | Modalities::IMAGE
    }

    #[test]
    fn twister_prompt_mentions_inversions_allowed() {
        let p = round_prompt(51, 1, 6, None, both(), 1800, None);
        assert!(p.contains("ALLOWED"));
        assert!(p.contains("ride_type 51"));
        assert!(p.contains("round 1 of 6"));
    }

    #[test]
    fn wooden_prompt_forbids_inversions() {
        assert!(round_prompt(52, 2, 4, None, both(), 1800, None).contains("NOT support"));
    }

    #[test]
    fn feedback_is_included_when_present() {
        let p = round_prompt(51, 2, 6, Some("{\"excitement\": 5.0}"), both(), 1800, None);
        assert!(p.contains("Previous round result"));
        assert!(p.contains("excitement"));
    }

    #[test]
    fn text_only_prompt_omits_the_screenshot_tool() {
        let p = round_prompt(52, 1, 6, None, Modalities::TEXT, 1800, None);
        assert!(!p.contains("screenshot"));
        assert!(p.contains("demolish()"), "other tools still listed");
    }

    #[test]
    fn budget_is_stated_in_minutes() {
        let p = round_prompt(51, 1, 6, None, both(), 1800, None);
        assert!(p.contains("30 minutes"), "1800s -> 30 minutes");
        assert!(
            p.contains("best_result"),
            "wrap-up guidance points at the tool"
        );
    }
}
