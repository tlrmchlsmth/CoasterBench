//! How far each track piece climbs, in the game's own z units: dumped from
//! the game's `piece_geometry` tool and checked in. Regenerate after adding
//! to `pieces::CATALOG`.

/// (piece name, z units gained) for every piece that changes height.
const CLIMB: [(&str, i32); 72] = [
    ("banked_left_turn_3", 0),
    ("banked_left_turn_5", 0),
    ("banked_right_turn_3", 0),
    ("banked_right_turn_5", 0),
    ("begin_station", 0),
    ("booster", 0),
    ("brakes", 0),
    ("down_25", -16),
    ("down_25_to_down_60", -32),
    ("down_25_to_flat", -8),
    ("down_25_to_left_bank", -8),
    ("down_25_to_right_bank", -8),
    ("down_60", -64),
    ("down_60_to_down_25", -32),
    ("down_60_to_flat", -24),
    ("end_station", 0),
    ("flat", 0),
    ("flat_to_down_25", -8),
    ("flat_to_down_60", -24),
    ("flat_to_left_bank", 0),
    ("flat_to_right_bank", 0),
    ("flat_to_up_25", 8),
    ("flat_to_up_60", 24),
    ("half_loop_down", -152),
    ("half_loop_up", 152),
    ("left_bank", 0),
    ("left_bank_to_down_25", -8),
    ("left_bank_to_flat", 0),
    ("left_bank_to_up_25", 8),
    ("left_corkscrew_down", -80),
    ("left_corkscrew_up", 80),
    ("left_helix_down_large", -16),
    ("left_helix_down_small", -16),
    ("left_helix_up_large", 16),
    ("left_helix_up_small", 16),
    ("left_turn_1", 0),
    ("left_turn_3", 0),
    ("left_turn_3_down_25", -32),
    ("left_turn_3_up_25", 32),
    ("left_turn_5", 0),
    ("left_turn_5_down_25", -64),
    ("left_turn_5_up_25", 64),
    ("left_vertical_loop", 0),
    ("middle_station", 0),
    ("right_bank", 0),
    ("right_bank_to_down_25", -8),
    ("right_bank_to_flat", 0),
    ("right_bank_to_up_25", 8),
    ("right_corkscrew_down", -80),
    ("right_corkscrew_up", 80),
    ("right_helix_down_large", -16),
    ("right_helix_down_small", -16),
    ("right_helix_up_large", 16),
    ("right_helix_up_small", 16),
    ("right_turn_1", 0),
    ("right_turn_3", 0),
    ("right_turn_3_down_25", -32),
    ("right_turn_3_up_25", 32),
    ("right_turn_5", 0),
    ("right_turn_5_down_25", -64),
    ("right_turn_5_up_25", 64),
    ("right_vertical_loop", 0),
    ("s_bend_left", 0),
    ("s_bend_right", 0),
    ("up_25", 16),
    ("up_25_to_flat", 8),
    ("up_25_to_left_bank", 8),
    ("up_25_to_right_bank", 8),
    ("up_25_to_up_60", 32),
    ("up_60", 64),
    ("up_60_to_flat", 24),
    ("up_60_to_up_25", 32),
];

/// Height gained by one piece, 0 for anything level or unknown.
pub fn climb(piece: &str) -> i32 {
    CLIMB
        .iter()
        .find(|(name, _)| *name == piece)
        .map_or(0, |(_, dz)| *dz)
}

/// Named rather than derived: dz cannot tell an inversion from a steep climb.
pub fn is_inversion(piece: &str) -> bool {
    piece.contains("loop") || piece.contains("corkscrew") || piece.contains("barrel")
}

pub fn is_speed_piece(piece: &str) -> bool {
    piece == "brakes" || piece == "booster"
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn climbs_match_the_game() {
        assert_eq!(climb("up_25"), 16);
        assert_eq!(climb("up_60"), 64);
        assert_eq!(climb("down_60"), -64);
        assert_eq!(climb("half_loop_up"), 152);
        assert_eq!(climb("flat"), 0);
        assert_eq!(climb("left_turn_5"), 0);
        assert_eq!(climb("not_a_piece"), 0);
    }

    #[test]
    fn inversions_are_the_pieces_that_invert() {
        assert!(is_inversion("left_vertical_loop"));
        assert!(is_inversion("right_corkscrew_up"));
        assert!(is_inversion("half_loop_up"));
        assert!(!is_inversion("left_helix_up_small"));
        assert!(!is_inversion("up_60"));
    }
}
