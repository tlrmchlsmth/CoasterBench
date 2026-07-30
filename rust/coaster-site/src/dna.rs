//! A round's track program drawn as the ride's elevation, plus a diff
//! against the round before it. See CLAUDE.local.md for the encoding.

use std::fmt::Write as _;

use crate::geometry;

const WIDTH: f64 = 640.0;
const HEIGHT: f64 = 96.0;
const PAD_X: f64 = 6.0;
const PAD_Y: f64 = 14.0;

/// One placed piece, as a program records it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Piece {
    pub name: String,
    pub chain: bool,
}

/// Reads a program's piece list, in either form the harness writes.
pub fn parse_pieces(program: &serde_json::Value) -> Vec<Piece> {
    program
        .get("pieces")
        .and_then(|p| p.as_array())
        .map(|pieces| {
            pieces
                .iter()
                .filter_map(|piece| match piece {
                    serde_json::Value::String(name) => Some(Piece {
                        name: name.clone(),
                        chain: false,
                    }),
                    serde_json::Value::Object(map) => {
                        let name = map
                            .get("t")
                            .or_else(|| map.get("piece"))
                            .and_then(|n| n.as_str())?;
                        Some(Piece {
                            name: name.to_string(),
                            chain: map
                                .get("chain")
                                .and_then(serde_json::Value::as_bool)
                                .unwrap_or(false),
                        })
                    }
                    _ => None,
                })
                .collect()
        })
        .unwrap_or_default()
}

/// Running altitude after each piece, in the game's z units, starting at 0.
fn altitudes(pieces: &[Piece]) -> Vec<f64> {
    let mut z = 0.0;
    pieces
        .iter()
        .map(|piece| {
            z += f64::from(geometry::climb(&piece.name));
            z
        })
        .collect()
}

/// What the caption says about a round's shape.
pub struct Summary {
    pub pieces: usize,
    pub inversions: usize,
    pub lift_pieces: usize,
}

pub fn summarise(pieces: &[Piece]) -> Summary {
    Summary {
        pieces: pieces.len(),
        inversions: pieces
            .iter()
            .filter(|p| geometry::is_inversion(&p.name))
            .count(),
        lift_pieces: pieces.iter().filter(|p| p.chain).count(),
    }
}

/// How this round's piece list differs from the one before it, as a sentence.
/// None when there is no previous round to compare against.
pub fn diff_line(pieces: &[Piece], previous: Option<&[Piece]>) -> Option<String> {
    let previous = previous?;
    if previous.is_empty() {
        return None;
    }
    if pieces == previous {
        return Some("identical to the round before".to_string());
    }
    let kept = longest_common_subsequence(previous, pieces);
    let added = pieces.len() - kept;
    let removed = previous.len() - kept;
    let mut parts = Vec::new();
    if added > 0 {
        parts.push(format!("{added} added"));
    }
    if removed > 0 {
        parts.push(format!("{removed} removed"));
    }
    // Nothing kept is a new coaster, not an edit.
    if kept == 0 {
        return Some("rebuilt from scratch".to_string());
    }
    Some(format!(
        "{} vs the round before ({kept} of {} pieces kept)",
        parts.join(", "),
        previous.len()
    ))
}

/// How much of the old track survived into the new one, in order.
fn longest_common_subsequence(a: &[Piece], b: &[Piece]) -> usize {
    let mut previous = vec![0usize; b.len() + 1];
    let mut current = vec![0usize; b.len() + 1];
    for x in a {
        for (j, y) in b.iter().enumerate() {
            current[j + 1] = if x == y {
                previous[j] + 1
            } else {
                current[j].max(previous[j + 1])
            };
        }
        std::mem::swap(&mut previous, &mut current);
    }
    previous[b.len()]
}

/// The profile as an inline SVG. `previous` draws the round before as a ghost.
pub fn profile_svg(pieces: &[Piece], previous: Option<&[Piece]>, label: &str) -> String {
    if pieces.len() < 2 {
        return String::new();
    }
    let z = altitudes(pieces);
    let ghost = previous.map(altitudes).unwrap_or_default();

    // One scale for both lines, so a taller lift looks taller.
    let low = z
        .iter()
        .chain(ghost.iter())
        .copied()
        .fold(0.0_f64, f64::min);
    let high = z
        .iter()
        .chain(ghost.iter())
        .copied()
        .fold(0.0_f64, f64::max);
    let span = (high - low).max(1.0);
    let plot_w = WIDTH - 2.0 * PAD_X;
    let plot_h = HEIGHT - 2.0 * PAD_Y;
    let x_of = |i: usize, n: usize| PAD_X + plot_w * (i as f64 / (n.max(2) - 1) as f64);
    let y_of = |value: f64| PAD_Y + plot_h * (1.0 - (value - low) / span);

    let mut svg = String::new();
    let ground = y_of(0.0);

    // Ghost first, so it sits behind everything.
    if ghost.len() > 1 {
        let points: Vec<String> = ghost
            .iter()
            .enumerate()
            .map(|(i, v)| format!("{:.1},{:.1}", x_of(i, ghost.len()), y_of(*v)))
            .collect();
        let _ = write!(
            svg,
            r#"<polyline class="dna-ghost" points="{}"/>"#,
            points.join(" ")
        );
    }

    let points: Vec<String> = z
        .iter()
        .enumerate()
        .map(|(i, v)| format!("{:.1},{:.1}", x_of(i, z.len()), y_of(*v)))
        .collect();
    let _ = write!(
        svg,
        r#"<path class="dna-fill" d="M{:.1},{ground:.1} L{} L{:.1},{ground:.1} Z"/>"#,
        PAD_X,
        points.join(" L"),
        WIDTH - PAD_X,
    );
    let _ = write!(
        svg,
        r#"<polyline class="dna-line" points="{}"/>"#,
        points.join(" ")
    );

    // The lift, over the stretch of line it follows.
    let mut run: Vec<String> = Vec::new();
    for (i, piece) in pieces.iter().enumerate() {
        if piece.chain {
            if run.is_empty() && i > 0 {
                run.push(format!("{:.1},{:.1}", x_of(i - 1, z.len()), y_of(z[i - 1])));
            }
            run.push(format!("{:.1},{:.1}", x_of(i, z.len()), y_of(z[i])));
        } else if !run.is_empty() {
            let _ = write!(
                svg,
                r#"<polyline class="dna-lift" points="{}"/>"#,
                run.join(" ")
            );
            run.clear();
        }
    }
    if !run.is_empty() {
        let _ = write!(
            svg,
            r#"<polyline class="dna-lift" points="{}"/>"#,
            run.join(" ")
        );
    }

    for (i, piece) in pieces.iter().enumerate() {
        let (x, y) = (x_of(i, z.len()), y_of(z[i]));
        if geometry::is_inversion(&piece.name) {
            let _ = write!(
                svg,
                r#"<path class="dna-inversion" d="M{x:.1},{:.1} l3.4,3.4 l-3.4,3.4 l-3.4,-3.4 Z"><title>{}</title></path>"#,
                y - 10.4,
                escape(&piece.name)
            );
        } else if geometry::is_speed_piece(&piece.name) {
            let _ = write!(
                svg,
                r#"<line class="dna-speed" x1="{x:.1}" y1="{:.1}" x2="{x:.1}" y2="{:.1}"><title>{}</title></line>"#,
                HEIGHT - 4.0,
                HEIGHT - 10.0,
                escape(&piece.name)
            );
        }
    }

    format!(
        r#"<svg class="dna" viewBox="0 0 {WIDTH:.0} {HEIGHT:.0}" role="img" aria-label="{}">{svg}</svg>"#,
        escape(label)
    )
}

fn escape(text: &str) -> String {
    askama::filters::escape(text, askama::filters::Html)
        .map(|e| e.to_string())
        .unwrap_or_default()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn pieces(names: &[&str]) -> Vec<Piece> {
        names
            .iter()
            .map(|n| Piece {
                name: (*n).to_string(),
                chain: false,
            })
            .collect()
    }

    #[test]
    fn parses_both_piece_forms() {
        let program = serde_json::json!({"pieces": [
            "flat",
            {"t": "up_25", "chain": true},
            {"piece": "booster", "speed": 20},
            42,
        ]});
        let parsed = parse_pieces(&program);
        assert_eq!(parsed.len(), 3, "the unparseable entry is dropped");
        assert_eq!(parsed[0].name, "flat");
        assert!(parsed[1].chain, "chain survives, it is the lift");
        assert_eq!(parsed[2].name, "booster");
    }

    #[test]
    fn altitude_follows_the_game_geometry() {
        let z = altitudes(&pieces(&["up_25", "down_25"]));
        assert_eq!(z, vec![16.0, 0.0]);
        let summary = summarise(&pieces(&["up_25", "up_25", "down_60"]));
        assert_eq!(summary.pieces, 3);
    }

    #[test]
    fn diff_counts_what_survived() {
        let before = pieces(&["flat", "up_25", "up_25", "down_25"]);
        let after = pieces(&["flat", "up_25", "up_25", "up_25", "down_25"]);
        let line = diff_line(&after, Some(&before)).expect("a diff");
        assert!(line.starts_with("1 added"), "{line}");
        assert!(line.contains("4 of 4 pieces kept"), "{line}");

        assert_eq!(
            diff_line(&before, Some(&before)).as_deref(),
            Some("identical to the round before")
        );
        assert_eq!(
            diff_line(&pieces(&["booster", "brakes"]), Some(&before)).as_deref(),
            Some("rebuilt from scratch")
        );
        assert_eq!(diff_line(&before, None), None);
    }

    #[test]
    fn a_one_piece_track_has_no_profile() {
        assert!(profile_svg(&pieces(&["flat"]), None, "x").is_empty());
        assert!(!profile_svg(&pieces(&["up_25", "down_25"]), None, "x").is_empty());
    }
}
