//! Non-scoring authorship metadata for the coaster a round has banked.
//!
//! These are OpenRCT2's canonical colour strings (Drawing::Colour.cpp). The
//! choices stay within the original player-facing palette: invisible colours
//! and compatibility aliases would make poor eval artifacts.

use serde::Serialize;
use serde_json::{json, Value};

pub const COLOURS: [&str; 32] = [
    "black",
    "grey",
    "white",
    "dark_purple",
    "light_purple",
    "bright_purple",
    "dark_blue",
    "light_blue",
    "icy_blue",
    "teal",
    "aquamarine",
    "saturated_green",
    "dark_green",
    "moss_green",
    "bright_green",
    "olive_green",
    "dark_olive_green",
    "bright_yellow",
    "yellow",
    "dark_yellow",
    "light_orange",
    "dark_orange",
    "light_brown",
    "saturated_brown",
    "dark_brown",
    "salmon_pink",
    "bordeaux_red",
    "saturated_red",
    "bright_red",
    "dark_pink",
    "bright_pink",
    "light_pink",
];

#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
pub struct RidePresentation {
    pub name: String,
    pub track_color: String,
    pub rail_color: String,
    pub support_color: String,
}

impl RidePresentation {
    pub fn from_args(args: &Value) -> Result<Self, String> {
        let value = |key: &str| {
            args.get(key)
                .and_then(Value::as_str)
                .ok_or_else(|| format!("{key} required"))
        };
        let name = value("name")?.trim();
        if name.is_empty() {
            return Err("name cannot be empty".into());
        }
        if name.chars().count() > 32 {
            return Err("name cannot exceed 32 characters".into());
        }
        let colour = |key: &str| -> Result<String, String> {
            let choice = value(key)?;
            if COLOURS.contains(&choice) {
                Ok(choice.to_string())
            } else {
                Err(format!(
                    "unknown {key} '{choice}'; choose one of: {}",
                    COLOURS.join(", ")
                ))
            }
        };
        Ok(Self {
            name: name.to_string(),
            track_color: colour("track_color")?,
            rail_color: colour("rail_color")?,
            support_color: colour("support_color")?,
        })
    }

    /// Attach the presentation to the scored report without touching any
    /// rating, similarity measurement, or the already-computed score.
    pub fn annotate(&self, report: &mut Value) -> Result<(), String> {
        let object = report
            .as_object_mut()
            .ok_or("best result is not a JSON object")?;
        object.insert("presentation".into(), json!(self));
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_and_trims_a_complete_style() {
        let style = RidePresentation::from_args(&json!({
            "name": "  Lake Effect  ",
            "track_color": "bright_red",
            "rail_color": "white",
            "support_color": "dark_blue",
        }))
        .expect("valid presentation");
        assert_eq!(style.name, "Lake Effect");
        assert_eq!(style.rail_color, "white");
    }

    #[test]
    fn rejects_non_game_colours_and_long_names() {
        let bad_colour = RidePresentation::from_args(&json!({
            "name": "Lake Effect",
            "track_color": "chartreuse-ish",
            "rail_color": "white",
            "support_color": "black",
        }))
        .expect_err("invalid colour");
        assert!(bad_colour.contains("unknown track_color"));

        let long = "x".repeat(33);
        let bad_name = RidePresentation::from_args(&json!({
            "name": long,
            "track_color": "bright_red",
            "rail_color": "white",
            "support_color": "black",
        }))
        .expect_err("long name");
        assert!(bad_name.contains("32 characters"));
    }

    #[test]
    fn annotation_preserves_the_score() {
        let style = RidePresentation {
            name: "Lake Effect".into(),
            track_color: "bright_red".into(),
            rail_color: "white".into(),
            support_color: "black".into(),
        };
        let mut report = json!({"score": 7.81, "rides": [{"excitement": 7.81}]});
        style.annotate(&mut report).expect("annotate");
        assert_eq!(report["score"], json!(7.81));
        assert_eq!(report["rides"][0]["excitement"], json!(7.81));
        assert_eq!(report["presentation"]["name"], json!("Lake Effect"));
    }
}
