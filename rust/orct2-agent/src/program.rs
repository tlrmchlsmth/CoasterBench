//! Track program: the declarative JSON artifact an agent submits.
//!
//! A program is a ride type, a start tile, and an ordered list of pieces.
//! Pieces reference the catalog by name (see pieces.rs) or raw numeric id.
//! Execution is sequential: each accepted piece advances the cursor, the
//! first rejected piece aborts the run (the rest of the track would attach
//! to a piece that does not exist).

use serde::{Deserialize, Serialize};

use crate::host;
use crate::pieces;

#[derive(Debug, Deserialize)]
pub struct TrackProgram {
    /// Game ride type, e.g. 52 = wooden roller coaster.
    pub ride_type: u16,
    pub start: Start,
    pub pieces: Vec<Piece>,
}

#[derive(Debug, Deserialize)]
pub struct Start {
    /// Tile coordinates (1 tile = 32 world units).
    pub x: i32,
    pub y: i32,
    /// 0-3; 0 faces -x, 1 faces +y, 2 faces +x, 3 faces -y.
    pub dir: u8,
}

#[derive(Debug, Deserialize)]
#[serde(untagged)]
pub enum PieceRef {
    Name(String),
    Raw(u16),
}

#[derive(Debug, Deserialize)]
#[serde(untagged)]
pub enum Piece {
    /// Bare piece: "flat" or 0
    Simple(PieceRef),
    /// Piece with options: {"t": "up_25", "chain": true}
    Full {
        t: PieceRef,
        #[serde(default)]
        chain: bool,
    },
}

impl Piece {
    fn parts(&self) -> (&PieceRef, bool) {
        match self {
            Piece::Simple(r) => (r, false),
            Piece::Full { t, chain } => (t, *chain),
        }
    }
}

fn resolve(piece: &PieceRef) -> Result<u16, String> {
    match piece {
        PieceRef::Raw(id) => Ok(*id),
        PieceRef::Name(name) => {
            pieces::lookup(name).ok_or_else(|| format!("unknown piece name: {name}"))
        }
    }
}

fn describe(track_type: u16) -> String {
    match pieces::name_of(track_type) {
        Some(name) => name.to_string(),
        None => format!("#{track_type}"),
    }
}

/// Rejection text for a piece the game refused. Names the piece as well as the
/// cursor: a program is a long list of pieces, and a bare cursor leaves the
/// model to work out which entry the game meant.
fn rejection_message(track_type: u16, cursor: &host::TrackCursor, error: &str) -> String {
    format!(
        "piece '{}' rejected at cursor (x={}, y={}, z={}, dir={}): {error}",
        describe(track_type),
        cursor.x / 32,
        cursor.y / 32,
        cursor.z,
        cursor.direction
    )
}

/// Result of executing a track program, later folded into the eval report.
#[derive(Debug, Serialize, Default)]
pub struct ProgramOutcome {
    pub ok: bool,
    pub ride_id: Option<u16>,
    pub pieces_placed: usize,
    pub pieces_total: usize,
    pub total_cost: i64,
    pub error: Option<ProgramError>,
    /// Track types actually placed, in order, for the similarity check. Not
    /// serialised: the program JSON already spells out the pieces.
    #[serde(skip)]
    pub placed_types: Vec<u16>,
    /// Cursor after each placed piece (tile coords), preceded by the start
    /// cursor. Lets a renderer draw the layout schematically with no game
    /// assets: point i to i+1 is the chord of piece i.
    pub trace: Vec<TracePoint>,
}

#[derive(Debug, Serialize)]
pub struct TracePoint {
    /// Piece whose placement ended at this cursor; "start" for the first.
    pub piece: String,
    pub chain: bool,
    pub x: i32,
    pub y: i32,
    pub z: i32,
    pub dir: u8,
}

#[derive(Debug, Serialize)]
pub struct ProgramError {
    /// Index into pieces[] of the rejected piece; None for setup errors.
    pub piece_index: Option<usize>,
    pub piece: Option<String>,
    pub message: String,
}

impl ProgramOutcome {
    pub fn failure(message: String) -> ProgramOutcome {
        ProgramOutcome {
            error: Some(ProgramError {
                piece_index: None,
                piece: None,
                message,
            }),
            ..ProgramOutcome::default()
        }
    }
}

/// Brute-force entrance and exit placement around the station tiles. The game
/// rejects invalid tile/direction combos, so trying all of them is cheap and
/// always agrees with the real rules.
pub fn place_entrance_and_exit(ride_id: u16, station_tiles: &[(i32, i32)]) -> Result<(), String> {
    if station_tiles.is_empty() {
        return Err("track has no station pieces; entrance cannot be placed".into());
    }
    let mut used_tile: Option<(i32, i32)> = None;
    let mut last_err = String::new();
    for is_exit in [false, true] {
        let mut placed = false;
        'search: for &(sx, sy) in station_tiles {
            for (dx, dy) in [(0, 1), (0, -1), (1, 0), (-1, 0)] {
                let tile = (sx + dx, sy + dy);
                if used_tile == Some(tile) {
                    continue;
                }
                for direction in 0..4u8 {
                    match host::entrance_place(ride_id, tile.0, tile.1, direction, is_exit) {
                        Ok(()) => {
                            used_tile = Some(tile);
                            placed = true;
                            break 'search;
                        }
                        Err(e) => last_err = e,
                    }
                }
            }
        }
        if !placed {
            let what = if is_exit { "exit" } else { "entrance" };
            return Err(format!(
                "could not place {what} next to any station tile (last error: {last_err})"
            ));
        }
    }
    Ok(())
}

/// Parses and executes a track program against the live game. Never panics;
/// all failures land in the returned outcome.
pub fn run(json: &str) -> ProgramOutcome {
    let program: TrackProgram = match serde_json::from_str(json) {
        Ok(p) => p,
        Err(e) => return ProgramOutcome::failure(format!("invalid program JSON: {e}")),
    };

    if !host::enable_sandbox() {
        host::log("orct2-agent: warning: sandbox cheat failed, ownership checks apply");
    }

    let ride_id = match host::ride_create(program.ride_type) {
        Ok(id) => id,
        Err(e) => {
            return ProgramOutcome::failure(format!("ride_create({}): {e}", program.ride_type))
        }
    };

    // Snap the start height to the surface, rounded up to the 16-unit step
    // TrackPlaceAction requires.
    let ground = host::surface_z(program.start.x, program.start.y);
    let z = (ground + 15) & !15;
    let mut cursor = host::TrackCursor {
        x: program.start.x * 32,
        y: program.start.y * 32,
        z,
        direction: program.start.dir & 3,
        bank: 0,  // TrackRoll::none
        slope: 0, // TrackPitch::none
    };

    let mut outcome = ProgramOutcome {
        ride_id: Some(ride_id),
        pieces_total: program.pieces.len(),
        ..ProgramOutcome::default()
    };

    // Tiles occupied by station pieces, for entrance/exit placement.
    let mut station_tiles: Vec<(i32, i32)> = Vec::new();

    outcome.trace.push(TracePoint {
        piece: "start".into(),
        chain: false,
        x: cursor.x / 32,
        y: cursor.y / 32,
        z: cursor.z,
        dir: cursor.direction,
    });

    for (index, piece) in program.pieces.iter().enumerate() {
        let (piece_ref, chain) = piece.parts();
        let track_type = match resolve(piece_ref) {
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
        let piece_tile = (cursor.x / 32, cursor.y / 32);
        match host::track_place(ride_id, track_type, chain, &mut cursor) {
            Ok(cost) => {
                outcome.pieces_placed += 1;
                outcome.total_cost += cost;
                outcome.placed_types.push(track_type);
                outcome.trace.push(TracePoint {
                    piece: describe(track_type),
                    chain,
                    x: cursor.x / 32,
                    y: cursor.y / 32,
                    z: cursor.z,
                    dir: cursor.direction,
                });
                // TrackElemType 1-3 are the station pieces.
                if (1..=3).contains(&track_type) {
                    station_tiles.push(piece_tile);
                }
            }
            Err(message) => {
                outcome.error = Some(ProgramError {
                    piece_index: Some(index),
                    piece: Some(describe(track_type)),
                    message: rejection_message(track_type, &cursor, &message),
                });
                return outcome;
            }
        }
    }

    // Testing requires an entrance and an exit next to the station. Placement
    // direction rules are fiddly, so lean on the game's own validation: try
    // every neighbour tile and direction until one sticks.
    if let Err(message) = place_entrance_and_exit(ride_id, &station_tiles) {
        outcome.error = Some(ProgramError {
            piece_index: None,
            piece: None,
            message,
        });
        return outcome;
    }

    // Flip to testing so trains spawn and the game measures the layout.
    if let Err(message) = host::ride_set_status(ride_id, 2) {
        // A circuit gap is invisible from the game's error alone; report where
        // the track ended relative to where it started so the agent can close it.
        let start_tile = (program.start.x, program.start.y);
        let end_tile = (cursor.x / 32, cursor.y / 32);
        outcome.error = Some(ProgramError {
            piece_index: None,
            piece: None,
            message: format!(
                "set_status(testing): {message} \
                 [track starts at tile ({}, {}, z={}, dir={}, bank=0, slope=0) and ends at tile ({}, {}, z={}, dir={}, bank={}, slope={}); \
                 a closed circuit requires these to be identical]",
                start_tile.0, start_tile.1, z, program.start.dir & 3, end_tile.0, end_tile.1, cursor.z, cursor.direction, cursor.bank, cursor.slope
            ),
        });
        return outcome;
    }

    outcome.ok = true;
    outcome
}

#[cfg(test)]
mod tests {
    use crate::program::*;

    #[test]
    fn rejection_names_the_piece_and_the_tile() {
        let cursor = host::TrackCursor {
            x: 64,
            y: 96,
            z: 112,
            direction: 2,
            bank: 0,
            slope: 0,
        };
        // flat_to_up_60: the piece a wooden coaster builds but cannot draw.
        let message = rejection_message(62, &cursor, "ride type 52 has no artwork for this piece");
        assert_eq!(
            message,
            "piece 'flat_to_up_60' rejected at cursor (x=2, y=3, z=112, dir=2): \
             ride type 52 has no artwork for this piece"
        );
        // Pieces outside the catalog still identify themselves by raw id.
        assert!(rejection_message(9999, &cursor, "nope").starts_with("piece '#9999'"));
    }

    #[test]
    fn parses_name_and_raw_and_chain_pieces() {
        let json = r#"{
            "ride_type": 52,
            "start": {"x": 10, "y": 12, "dir": 1},
            "pieces": ["begin_station", 3, {"t": "up_25", "chain": true}, {"t": 42}]
        }"#;
        let p: TrackProgram = serde_json::from_str(json).expect("parse");
        assert_eq!(p.ride_type, 52);
        assert_eq!(p.start.dir, 1);
        assert_eq!(p.pieces.len(), 4);
        let (r0, chain0) = p.pieces[0].parts();
        assert_eq!(resolve(r0).expect("resolve"), 2);
        assert!(!chain0);
        let (r2, chain2) = p.pieces[2].parts();
        assert_eq!(resolve(r2).expect("resolve"), 4);
        assert!(chain2);
    }

    #[test]
    fn unknown_name_resolves_to_error() {
        assert!(resolve(&PieceRef::Name("loop_de_loop".into())).is_err());
    }

    #[test]
    fn bad_json_is_a_setup_failure_not_a_panic() {
        let outcome = ProgramOutcome::failure("invalid program JSON: x".into());
        assert!(!outcome.ok);
        let err = outcome.error.expect("error present");
        assert!(err.piece_index.is_none());
        assert!(err.message.contains("invalid program JSON"));
    }
}
