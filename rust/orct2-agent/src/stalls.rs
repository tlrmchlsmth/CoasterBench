//! Stall plan: the guest-services goal's submission artifact.
//!
//! A plan is a list of stalls, each a stall ride type, a tile, a facing, and
//! an optional price in dollars. Execution mirrors program.rs: sequential,
//! first rejection aborts (the model gets the exact index back), everything
//! through the same GameActions validation the construction window uses.
//! Placed stalls are opened immediately so guests can use them.

use serde::Deserialize;

use crate::host;
use crate::program::{ProgramError, ProgramOutcome};

#[derive(Debug, Deserialize)]
pub struct StallPlan {
    pub stalls: Vec<StallSpec>,
}

#[derive(Debug, Deserialize)]
pub struct StallSpec {
    /// Catalog name ("food", "toilets") or a raw game ride type number.
    pub stall_type: StallType,
    /// Tile coordinates.
    pub x: i32,
    pub y: i32,
    /// Facing 0-3 (the side guests enter from must touch a path).
    pub dir: u8,
    /// Primary item price in dollars, e.g. 1.5 = $1.50. Omit for the default.
    #[serde(default)]
    pub price: Option<f64>,
}

#[derive(Debug, Deserialize)]
#[serde(untagged)]
pub enum StallType {
    Name(String),
    Raw(u16),
}

/// The stall ride types a plan can name. All have
/// `RatingsCalculationType::Stall`; anything else is refused by the host.
pub const STALL_CATALOG: &[(&str, u16)] = &[
    ("food", 28),
    ("drink", 30),
    ("shop", 32),
    ("information_kiosk", 35),
    ("toilets", 36),
    ("cash_machine", 45),
    ("first_aid", 48),
];

fn resolve(stall_type: &StallType) -> Result<u16, String> {
    match stall_type {
        StallType::Raw(id) => Ok(*id),
        StallType::Name(name) => STALL_CATALOG
            .iter()
            .find(|(n, _)| n == name)
            .map(|&(_, id)| id)
            .ok_or_else(|| {
                let names: Vec<&str> = STALL_CATALOG.iter().map(|&(n, _)| n).collect();
                format!("unknown stall type: {name} (known: {})", names.join(", "))
            }),
    }
}

/// Dollars to the game's fixed-point money64 (10 units = $1.00). Negative
/// means "keep the ride's default price" to the host.
fn to_money(price: Option<f64>) -> i64 {
    match price {
        Some(dollars) => (dollars * 10.0).round() as i64,
        None => -1,
    }
}

/// Parses and executes a stall plan against the live game. Reuses
/// ProgramOutcome so the report and drivers handle both program kinds the
/// same way: pieces_placed/pieces_total count stalls here.
pub fn run(json: &str) -> ProgramOutcome {
    let plan: StallPlan = match serde_json::from_str(json) {
        Ok(p) => p,
        Err(e) => return ProgramOutcome::failure(format!("invalid stall plan JSON: {e}")),
    };

    if !host::enable_sandbox() {
        host::log("orct2-agent: warning: sandbox cheat failed, ownership checks apply");
    }

    let mut outcome = ProgramOutcome {
        pieces_total: plan.stalls.len(),
        park_before: host::park_stats(),
        ..ProgramOutcome::default()
    };

    for (index, spec) in plan.stalls.iter().enumerate() {
        let ride_type = match resolve(&spec.stall_type) {
            Ok(t) => t,
            Err(message) => {
                outcome.error = Some(ProgramError {
                    piece_index: Some(index),
                    piece: None,
                    message,
                });
                return outcome;
            }
        };
        match host::stall_place(
            ride_type,
            spec.x,
            spec.y,
            spec.dir & 3,
            to_money(spec.price),
        ) {
            Ok(ride_id) => {
                outcome.pieces_placed += 1;
                outcome.stall_ids.push(ride_id);
            }
            Err(message) => {
                outcome.error = Some(ProgramError {
                    piece_index: Some(index),
                    piece: Some(describe(&spec.stall_type, ride_type)),
                    message: format!(
                        "stall '{}' rejected at tile ({}, {}, dir={}): {message}",
                        describe(&spec.stall_type, ride_type),
                        spec.x,
                        spec.y,
                        spec.dir & 3
                    ),
                });
                return outcome;
            }
        }
    }

    outcome.ok = true;
    outcome
}

/// Folds a stall-plan outcome into an already-successful track outcome (the
/// combined pieces+stalls program). Counters accumulate; a stall rejection
/// fails the whole program with its piece_index continuing past the track
/// pieces, so "piece 43" in a 40-piece program unambiguously means stall 3.
/// The track outcome's park_before (snapshotted before anything was built)
/// is kept.
pub fn merge(track: &mut ProgramOutcome, stall_outcome: ProgramOutcome) {
    let track_pieces = track.pieces_total;
    track.pieces_placed += stall_outcome.pieces_placed;
    track.pieces_total += stall_outcome.pieces_total;
    track.total_cost += stall_outcome.total_cost;
    track.stall_ids = stall_outcome.stall_ids;
    if let Some(mut error) = stall_outcome.error {
        error.piece_index = error.piece_index.map(|i| i + track_pieces);
        track.error = Some(error);
        track.ok = false;
    }
}

fn describe(stall_type: &StallType, ride_type: u16) -> String {
    match stall_type {
        StallType::Name(name) => name.clone(),
        StallType::Raw(_) => STALL_CATALOG
            .iter()
            .find(|&&(_, id)| id == ride_type)
            .map(|&(n, _)| n.to_string())
            .unwrap_or_else(|| format!("#{ride_type}")),
    }
}

#[cfg(test)]
mod tests {
    use crate::stalls::*;

    #[test]
    fn parses_names_raw_types_and_prices() {
        let json = r#"{"stalls": [
            {"stall_type": "food", "x": 10, "y": 12, "dir": 1, "price": 1.5},
            {"stall_type": 36, "x": 11, "y": 12, "dir": 2}
        ]}"#;
        let plan: StallPlan = serde_json::from_str(json).expect("parse");
        assert_eq!(plan.stalls.len(), 2);
        assert_eq!(resolve(&plan.stalls[0].stall_type).expect("resolve"), 28);
        assert_eq!(to_money(plan.stalls[0].price), 15);
        assert_eq!(resolve(&plan.stalls[1].stall_type).expect("resolve"), 36);
        assert_eq!(to_money(plan.stalls[1].price), -1, "no price keeps default");
    }

    #[test]
    fn unknown_stall_name_lists_the_catalog() {
        let err = resolve(&StallType::Name("burgers".into())).expect_err("unknown");
        assert!(err.contains("unknown stall type: burgers"));
        assert!(err.contains("toilets"));
    }

    #[test]
    fn raw_types_describe_by_catalog_name_when_known() {
        assert_eq!(describe(&StallType::Raw(36), 36), "toilets");
        assert_eq!(describe(&StallType::Raw(99), 99), "#99");
    }

    #[test]
    fn merge_accumulates_counts_and_offsets_stall_errors() {
        let mut track = ProgramOutcome {
            ok: true,
            ride_id: Some(7),
            pieces_placed: 40,
            pieces_total: 40,
            total_cost: 5000,
            ..ProgramOutcome::default()
        };
        let stall_fail = ProgramOutcome {
            ok: false,
            pieces_placed: 2,
            pieces_total: 4,
            total_cost: 300,
            stall_ids: vec![8, 9],
            error: Some(ProgramError {
                piece_index: Some(2),
                piece: Some("food".into()),
                message: "stall 'food' rejected at tile (5, 6, dir=0): no clearance".into(),
            }),
            ..ProgramOutcome::default()
        };
        merge(&mut track, stall_fail);
        assert!(!track.ok, "a stall rejection fails the combined program");
        assert_eq!(track.pieces_placed, 42);
        assert_eq!(track.pieces_total, 44);
        assert_eq!(track.total_cost, 5300);
        assert_eq!(track.stall_ids, vec![8, 9]);
        // Stall 2 of a 40-piece track reports as piece 42, not piece 2.
        assert_eq!(track.error.as_ref().expect("error").piece_index, Some(42));
        assert_eq!(track.ride_id, Some(7), "the track's ride is untouched");

        // A clean stall plan keeps the program ok.
        let mut track = ProgramOutcome {
            ok: true,
            pieces_placed: 40,
            pieces_total: 40,
            ..ProgramOutcome::default()
        };
        merge(
            &mut track,
            ProgramOutcome {
                ok: true,
                pieces_placed: 3,
                pieces_total: 3,
                stall_ids: vec![2],
                ..ProgramOutcome::default()
            },
        );
        assert!(track.ok);
        assert_eq!(track.pieces_total, 43);
        assert_eq!(track.stall_ids, vec![2]);
    }

    #[test]
    fn stub_host_rejects_the_first_stall_with_its_index() {
        // The test-stub host refuses stall_place, so execution must stop at
        // index 0 with the tile in the message and no stalls recorded.
        let outcome = run(r#"{"stalls": [{"stall_type": "drink", "x": 5, "y": 6, "dir": 0}]}"#);
        assert!(!outcome.ok);
        assert!(outcome.stall_ids.is_empty());
        let err = outcome.error.expect("error");
        assert_eq!(err.piece_index, Some(0));
        assert!(err.message.contains("tile (5, 6"));
    }
}
