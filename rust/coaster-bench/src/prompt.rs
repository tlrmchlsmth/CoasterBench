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

pub struct Round<'a> {
    pub ride_type: u16,
    pub round: u32,
    pub rounds: u32,
    pub previous_feedback: Option<&'a str>,
    pub modalities: Modalities,
    pub budget_secs: u64,
    pub open_note_dir: Option<&'a str>,
    pub single_turn: bool,
    /// Scenario-specific map description; None falls back to DEFAULT_MAP_LINE.
    pub map_line: Option<&'a str>,
}

pub fn round_prompt(round_spec: &Round) -> String {
    let &Round {
        ride_type,
        round,
        rounds,
        previous_feedback,
        modalities,
        budget_secs,
        open_note_dir,
        single_turn,
        map_line,
    } = round_spec;
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
    let one_shot = if single_turn {
        "\n## This is one shot\n\
         Your session ends the moment you stop calling tools. A progress update, a\n\
         plan for approval, or a question to the user all end the round for good,\n\
         with your remaining time unused and nothing scored. Never ask whether to\n\
         proceed; you already have permission. Keep calling tools until\n\
         finish_and_test has banked a score, and write your summary only after that.\n"
    } else {
        ""
    };
    let feedback = match previous_feedback {
        Some(report) => format!(
            "\n## Previous round result\nYour previous attempt's eval report (learn from it):\n{report}\n"
        ),
        None => String::new(),
    };
    // Deliberately does not name the file that computes the rating: finding it
    // is part of what this measures.
    let open_note = match open_note_dir {
        Some(dir) => format!(
            "\n## Engine source\nA read-only checkout of the game engine that scores you is at {dir}.\n\
             It is the upstream OpenRCT2 revision this build is based on; the benchmark harness\n\
             itself is not in it. Your excitement rating is computed by that C++ code, so you can\n\
             read it to find out what excitement actually rewards instead of guessing. Use your own\n\
             file and search tools for this; the coaster MCP tools do not read files.\n\
             python3 is available for working out geometry offline before you place it.\n\
             It has no network access and cannot reach the game, so the park state\n\
             (which tiles are already occupied) is only knowable through the MCP tools.\n"
        ),
        None => String::new(),
    };
    format!(
        r#"You are competing to design the best RollerCoaster Tycoon 2 {name} (ride_type {ride_type}).
This is round {round} of {rounds}. You build interactively through the "coaster" MCP tools:

- new_ride(ride_type, x, y, dir): start building; the cursor starts at that tile. Directions: 0 faces -x, 1 faces +y, 2 faces +x, 3 faces -y.
- place_piece(piece, chain?, speed?): place one piece at the cursor. Returns the new cursor and circuit_closed.
- place_pieces(pieces): batch placement; stops at the first rejection.
- valid_next_pieces(): which catalog pieces the game will accept at the current cursor. Ground truth - use it whenever unsure.
- piece_geometry(dir): exact cursor delta (tiles, z, direction, bank, slope) for every piece from a given direction. Use this to PLAN closure.
- undo_piece(): remove the last piece.
- get_state(): cursor, start, pieces placed, circuit_closed.
- finish_and_test(ticks?): places entrance/exit, runs a real test train, returns the full eval report with excitement/intensity/nausea.
- best_result(): the highest adjusted-score finish_and_test from this round so far, after the stock-design similarity penalty. Your score is this, not your latest build.{screenshot}
- style_best_ride(name, track_color, rail_color, support_color): after you have banked a score, give the winner its final name and colours. This retroactively updates the saved winner without retesting or changing its score.
- demolish(): tear down and start over (then new_ride again).

## Rules
- Build a CLOSED CIRCUIT: get_state must show circuit_closed before finish_and_test will pass.
- Start with begin_station, middle_station, end_station on flat ground (3-7 station pieces).
- Chain lift ({{"chain": true}}) only works on 25-degree slopes. Trains start slow; climb first, then coast.
- A `booster` accelerates the train up to its speed and `brakes` slows it down to theirs: {{"t": "booster", "speed": 25}}, 0-30, default 8. A booster is the fix for a layout that runs out of energy and valleys; brakes are for shedding speed before the station.
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
6. At the end, call best_result, then use style_best_ride to name and colour that winner.
7. End with a one-line summary of your best coaster and its excitement.

## Budget
You have about {budget_mins} minutes of wall-clock this round, then the session is cut off.
Get a working, tested circuit banked EARLY; do not spend the whole budget on one perfect build.
Your score is your best finish_and_test of the round (see best_result), not your final build,
so it is always safe to stop once you are happy.
{one_shot}{open_note}{feedback}"#
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    fn both() -> Modalities {
        Modalities::TEXT | Modalities::IMAGE
    }

    #[test]
    fn single_turn_harnesses_are_told_that_stopping_is_final() {
        let looping = round_prompt(&Round {
            ride_type: 51,
            round: 1,
            rounds: 6,
            previous_feedback: None,
            modalities: both(),
            budget_secs: 1800,
            open_note_dir: None,
            single_turn: false,
            map_line: None,
        });
        assert!(!looping.contains("one shot"));

        let one_shot = round_prompt(&Round {
            ride_type: 51,
            round: 1,
            rounds: 6,
            previous_feedback: None,
            modalities: both(),
            budget_secs: 1800,
            open_note_dir: None,
            single_turn: true,
            map_line: None,
        });
        assert!(one_shot.contains("one shot"));
        assert!(
            one_shot.contains("you already have permission"),
            "asking for confirmation is what actually ended a codex round"
        );
        assert!(
            one_shot.contains("finish_and_test has banked a score"),
            "the bar for stopping is a banked score, not a plan"
        );
        assert!(
            one_shot.contains("highest adjusted-score"),
            "best_result must describe the score the server actually optimises"
        );
        assert!(
            one_shot.contains("retroactively updates the saved winner"),
            "presentation must be clearly independent of scoring"
        );
    }

    /// The one agent that ever reached for a booster did so after reading the
    /// catalog dump, concluded from a harness bug that boosters do not work,
    /// and wrote that down. Now that they work, the prompt says so rather than
    /// leaving the piece to be rediscovered.
    #[test]
    fn both_ride_types_are_told_about_boosters_and_their_speed() {
        for ride_type in [51, 52] {
            let p = round_prompt(&Round {
                ride_type,
                round: 1,
                rounds: 6,
                previous_feedback: None,
                modalities: both(),
                budget_secs: 1800,
                open_note_dir: None,
                single_turn: false,
                map_line: None,
            });
            assert!(p.contains("booster"), "ride type {ride_type}");
            assert!(p.contains("\"speed\": 25"), "the syntax, not just the name");
            assert!(p.contains("valleys"), "says what it is for");
        }
    }

    #[test]
    fn twister_prompt_mentions_inversions_allowed() {
        let p = round_prompt(&Round {
            ride_type: 51,
            round: 1,
            rounds: 6,
            previous_feedback: None,
            modalities: both(),
            budget_secs: 1800,
            open_note_dir: None,
            single_turn: false,
            map_line: None,
        });
        assert!(p.contains("ALLOWED"));
        assert!(p.contains("ride_type 51"));
        assert!(p.contains("round 1 of 6"));
    }

    #[test]
    fn wooden_prompt_forbids_inversions() {
        assert!(round_prompt(&Round {
            ride_type: 52,
            round: 2,
            rounds: 4,
            previous_feedback: None,
            modalities: both(),
            budget_secs: 1800,
            open_note_dir: None,
            single_turn: false,
            map_line: None,
        })
        .contains("NOT support"));
    }

    #[test]
    fn feedback_is_included_when_present() {
        let p = round_prompt(&Round {
            ride_type: 51,
            round: 2,
            rounds: 6,
            previous_feedback: Some("{\"excitement\": 5.0}"),
            modalities: both(),
            budget_secs: 1800,
            open_note_dir: None,
            single_turn: false,
            map_line: None,
        });
        assert!(p.contains("Previous round result"));
        assert!(p.contains("excitement"));
    }

    #[test]
    fn text_only_prompt_omits_the_screenshot_tool() {
        let p = round_prompt(&Round {
            ride_type: 52,
            round: 1,
            rounds: 6,
            previous_feedback: None,
            modalities: Modalities::TEXT,
            budget_secs: 1800,
            open_note_dir: None,
            single_turn: false,
            map_line: None,
        });
        assert!(!p.contains("screenshot"));
        assert!(p.contains("demolish()"), "other tools still listed");
    }

    #[test]
    fn open_note_points_at_the_source_without_naming_the_ratings_file() {
        let p = round_prompt(&Round {
            ride_type: 51,
            round: 1,
            rounds: 6,
            previous_feedback: None,
            modalities: both(),
            budget_secs: 1800,
            open_note_dir: Some("/tmp/openrct2-src"),
            single_turn: false,
            map_line: None,
        });
        assert!(p.contains("Engine source"));
        assert!(p.contains("/tmp/openrct2-src"));
        assert!(
            !p.contains("RideRatings"),
            "finding the ratings code is the point"
        );
    }

    #[test]
    fn without_open_note_no_source_is_mentioned() {
        let p = round_prompt(&Round {
            ride_type: 51,
            round: 1,
            rounds: 6,
            previous_feedback: None,
            modalities: both(),
            budget_secs: 1800,
            open_note_dir: None,
            single_turn: false,
            map_line: None,
        });
        assert!(!p.contains("Engine source"));
        assert!(!p.contains("python3"), "python is an open-note capability");
    }

    #[test]
    fn open_note_offers_python_and_says_what_it_cannot_do() {
        let p = round_prompt(&Round {
            ride_type: 51,
            round: 1,
            rounds: 6,
            previous_feedback: None,
            modalities: both(),
            budget_secs: 1800,
            open_note_dir: Some("/tmp/openrct2-src"),
            single_turn: false,
            map_line: None,
        });
        assert!(p.contains("python3"));
        assert!(p.contains("no network access"), "isolation stated");
        assert!(p.contains("occupied"), "collision blind spot called out");
    }

    #[test]
    fn budget_is_stated_in_minutes() {
        let p = round_prompt(&Round {
            ride_type: 51,
            round: 1,
            rounds: 6,
            previous_feedback: None,
            modalities: both(),
            budget_secs: 1800,
            open_note_dir: None,
            single_turn: false,
            map_line: None,
        });
        assert!(p.contains("30 minutes"), "1800s -> 30 minutes");
        assert!(
            p.contains("best_result"),
            "wrap-up guidance points at the tool"
        );
    }
}
