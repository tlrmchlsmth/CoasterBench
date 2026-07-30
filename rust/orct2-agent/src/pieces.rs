//! Track piece catalog: snake_case names -> game TrackElemType values.
//!
//! Values mirror src/openrct2/ride/ted/TrackElemType.h. This is the vocabulary
//! exposed to LLM agents in track programs, so names favour readability over
//! matching the C++ identifiers exactly. Raw numeric ids are also accepted in
//! programs for anything not listed here.

/// The coaster-relevant subset of TrackElemType, by agent-facing name.
pub const CATALOG: &[(&str, u16)] = &[
    ("flat", 0),
    ("end_station", 1),
    ("begin_station", 2),
    ("middle_station", 3),
    ("up_25", 4),
    ("up_60", 5),
    ("flat_to_up_25", 6),
    ("up_25_to_up_60", 7),
    ("up_60_to_up_25", 8),
    ("up_25_to_flat", 9),
    ("down_25", 10),
    ("down_60", 11),
    ("flat_to_down_25", 12),
    ("down_25_to_down_60", 13),
    ("down_60_to_down_25", 14),
    ("down_25_to_flat", 15),
    ("left_turn_5", 16),
    ("right_turn_5", 17),
    ("flat_to_left_bank", 18),
    ("flat_to_right_bank", 19),
    ("left_bank_to_flat", 20),
    ("right_bank_to_flat", 21),
    ("banked_left_turn_5", 22),
    ("banked_right_turn_5", 23),
    ("left_bank_to_up_25", 24),
    ("right_bank_to_up_25", 25),
    ("up_25_to_left_bank", 26),
    ("up_25_to_right_bank", 27),
    ("left_bank_to_down_25", 28),
    ("right_bank_to_down_25", 29),
    ("down_25_to_left_bank", 30),
    ("down_25_to_right_bank", 31),
    ("left_bank", 32),
    ("right_bank", 33),
    ("left_turn_5_up_25", 34),
    ("right_turn_5_up_25", 35),
    ("left_turn_5_down_25", 36),
    ("right_turn_5_down_25", 37),
    ("s_bend_left", 38),
    ("s_bend_right", 39),
    ("left_vertical_loop", 40),
    ("right_vertical_loop", 41),
    ("left_turn_3", 42),
    ("right_turn_3", 43),
    ("banked_left_turn_3", 44),
    ("banked_right_turn_3", 45),
    ("left_turn_3_up_25", 46),
    ("right_turn_3_up_25", 47),
    ("left_turn_3_down_25", 48),
    ("right_turn_3_down_25", 49),
    ("left_turn_1", 50),
    ("right_turn_1", 51),
    ("half_loop_up", 56),
    ("half_loop_down", 57),
    ("left_corkscrew_up", 58),
    ("right_corkscrew_up", 59),
    ("left_corkscrew_down", 60),
    ("right_corkscrew_down", 61),
    ("flat_to_up_60", 62),
    ("up_60_to_flat", 63),
    ("flat_to_down_60", 64),
    ("down_60_to_flat", 65),
    ("left_helix_up_small", 87),
    ("right_helix_up_small", 88),
    ("left_helix_down_small", 89),
    ("right_helix_down_small", 90),
    ("left_helix_up_large", 91),
    ("right_helix_up_large", 92),
    ("left_helix_down_large", 93),
    ("right_helix_down_large", 94),
    ("brakes", 99),
    ("booster", 100),
];

pub fn lookup(name: &str) -> Option<u16> {
    CATALOG.iter().find(|(n, _)| *n == name).map(|&(_, id)| id)
}

pub fn name_of(id: u16) -> Option<&'static str> {
    CATALOG.iter().find(|(_, i)| *i == id).map(|&(n, _)| n)
}

#[cfg(test)]
mod tests {
    use crate::pieces::*;

    #[test]
    fn lookup_round_trips() {
        assert_eq!(lookup("begin_station"), Some(2));
        assert_eq!(lookup("left_vertical_loop"), Some(40));
        assert_eq!(lookup("nonexistent"), None);
        assert_eq!(name_of(40), Some("left_vertical_loop"));
    }

    #[test]
    fn catalog_has_no_duplicate_names_or_ids() {
        use std::collections::HashSet;
        let names: HashSet<_> = CATALOG.iter().map(|(n, _)| n).collect();
        let ids: HashSet<_> = CATALOG.iter().map(|(_, i)| i).collect();
        assert_eq!(names.len(), CATALOG.len());
        assert_eq!(ids.len(), CATALOG.len());
    }
}

/// Track types whose brake/booster speed the game actually reads: brakes slow a
/// train to it, boosters accelerate up to it, and everything else stores it and
/// ignores it. Offering the setting on a piece that ignores it would be a knob
/// connected to nothing.
pub fn takes_speed(track_type: u16) -> bool {
    matches!(track_type, 99 | 100)
}

/// What the game's own construction window starts a brake or booster at
/// (`_currentBrakeSpeed = 8`), and therefore what a piece placed without an
/// explicit speed should get. Zero is not a neutral default: it makes a booster
/// inert and a brake a full stop.
pub const DEFAULT_BRAKE_SPEED: u8 = 8;

/// The game refuses anything above this (`kMaximumTrackSpeed`).
pub const MAX_BRAKE_SPEED: u8 = 30;

#[cfg(test)]
mod speed_tests {
    use super::*;

    #[test]
    fn only_brakes_and_booster_read_a_speed() {
        assert!(takes_speed(lookup("brakes").unwrap()));
        assert!(takes_speed(lookup("booster").unwrap()));
        for inert in ["flat", "up_25", "left_turn_5", "begin_station"] {
            assert!(
                !takes_speed(lookup(inert).unwrap()),
                "{inert} stores a speed the game never reads"
            );
        }
    }

    #[test]
    fn the_default_is_the_games_own() {
        // _currentBrakeSpeed in the construction window. Zero is not neutral:
        // a booster at 0 never accelerates and a brake at 0 is a full stop.
        assert_eq!(DEFAULT_BRAKE_SPEED, 8);
        assert_eq!(MAX_BRAKE_SPEED, 30, "kMaximumTrackSpeed");
    }
}

/// One piece as a caller may write it, in a track program or an MCP batch.
///
/// The two used to be parsed separately and drifted: programs took
/// `{"t": name}`, place_pieces took `{"piece": name}`, and the piece lists the
/// server hands back (get_state, placed_pieces) are in program form. Replaying
/// a session's own batch as a program therefore needed translation. One type
/// now parses both spellings, so any piece list works in either place.
#[derive(Debug, serde::Deserialize)]
#[serde(untagged)]
pub enum PieceSpec {
    /// Bare piece: "flat", or a raw TrackElemType id.
    Simple(PieceRef),
    /// Piece with options: {"t": "up_25", "chain": true},
    /// {"piece": "booster", "speed": 20}.
    Full {
        #[serde(alias = "piece")]
        t: PieceRef,
        #[serde(default)]
        chain: bool,
        /// Brake/booster speed. Absent means the game's default for the pieces
        /// that read one; see DEFAULT_BRAKE_SPEED.
        #[serde(default)]
        speed: Option<u8>,
    },
}

#[derive(Debug, serde::Deserialize)]
#[serde(untagged)]
pub enum PieceRef {
    Name(String),
    Raw(u16),
}

impl PieceSpec {
    pub fn parts(&self) -> (&PieceRef, bool, Option<u8>) {
        match self {
            PieceSpec::Simple(r) => (r, false, None),
            PieceSpec::Full { t, chain, speed } => (t, *chain, *speed),
        }
    }

    /// Track type, or why the name is not a piece.
    pub fn track_type(&self) -> Result<u16, String> {
        match self.parts().0 {
            PieceRef::Raw(id) => Ok(*id),
            PieceRef::Name(name) => {
                lookup(name).ok_or_else(|| format!("unknown piece name: {name}"))
            }
        }
    }
}

/// The speed to place a piece at: the caller's, the game's default for a piece
/// that reads one, or nothing. `Err` when a caller set a speed on a piece that
/// would ignore it, which is worth saying out loud rather than dropping.
pub fn resolve_speed(track_type: u16, requested: Option<u8>) -> Result<u8, String> {
    match (requested, takes_speed(track_type)) {
        (Some(_), false) => Err(format!(
            "{} has no speed setting; only brakes and booster do",
            name_of(track_type).unwrap_or("that piece")
        )),
        (Some(s), true) if s > MAX_BRAKE_SPEED => Err(format!(
            "speed {s} is above the game's maximum of {MAX_BRAKE_SPEED}"
        )),
        (Some(s), true) => Ok(s),
        (None, true) => Ok(DEFAULT_BRAKE_SPEED),
        (None, false) => Ok(0),
    }
}

#[cfg(test)]
mod spec_tests {
    use super::*;

    fn spec(json: &str) -> PieceSpec {
        serde_json::from_str(json).expect("parse")
    }

    #[test]
    fn both_spellings_of_a_piece_object_parse_the_same() {
        // place_pieces wrote "piece", programs wrote "t". Anything the server
        // hands back has to feed straight back in, either way round.
        for json in [
            r#"{"t": "booster", "speed": 20}"#,
            r#"{"piece": "booster", "speed": 20}"#,
        ] {
            let s = spec(json);
            assert_eq!(s.track_type().unwrap(), lookup("booster").unwrap());
            assert_eq!(s.parts().2, Some(20));
        }
        assert_eq!(
            spec(r#""flat""#).track_type().unwrap(),
            lookup("flat").unwrap()
        );
        assert_eq!(spec("99").track_type().unwrap(), 99);
        assert!(spec(r#"{"t": "chain_lift"}"#).track_type().is_err());
        assert!(spec(r#"{"t": "up_25", "chain": true}"#).parts().1);
    }

    #[test]
    fn speed_resolves_to_the_default_and_refuses_pieces_that_ignore_it() {
        let booster = lookup("booster").unwrap();
        let flat = lookup("flat").unwrap();
        assert_eq!(resolve_speed(booster, None), Ok(DEFAULT_BRAKE_SPEED));
        assert_eq!(resolve_speed(booster, Some(25)), Ok(25));
        assert!(
            resolve_speed(booster, Some(99)).is_err(),
            "above the game maximum"
        );
        assert_eq!(resolve_speed(flat, None), Ok(0), "stored and ignored");
        assert!(
            resolve_speed(flat, Some(10)).is_err(),
            "a knob attached to nothing"
        );
    }
}
