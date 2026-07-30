//! View models: everything the templates need, already formatted.
//!
//! Templates stay dumb on purpose (loops and layout only), so number
//! formatting, ranking and asset resolution all happen here.

use askama::Template;
use serde::Serialize;

pub const TAGLINE: &str = "A benchmark in which language models design roller coasters that RollerCoaster Tycoon 2 builds, tests, and rates.";

pub const CSS: &str = include_str!("../static/site.css");
pub const JS: &str = include_str!("../static/site.js");

pub const MODE_TAGLINES: [(&str, &str); 3] = [
    ("design", "design mode — models build from scratch"),
    (
        "library",
        "library mode — models may search the stock track design library, which measures retrieval and adaptation; copies score zero",
    ),
    (
        "open note",
        "open-note condition — the agent is given the engine source it is scored by, so it can read the ratings code instead of guessing; scores are not comparable with black-box runs",
    ),
];

pub fn mode_tagline(mode: &str) -> String {
    MODE_TAGLINES
        .iter()
        .find(|(name, _)| *name == mode)
        .map(|(_, tagline)| (*tagline).to_string())
        .unwrap_or_else(|| mode.to_string())
}

pub fn fmt_tokens(n: f64) -> String {
    if n >= 1_000_000.0 {
        format!("{:.1}M", n / 1_000_000.0)
    } else if n >= 1_000.0 {
        format!("{:.0}k", n / 1_000.0)
    } else {
        format!("{n:.0}")
    }
}

/// Page chrome: title bar, unfurl tags, layout width.
pub struct Chrome {
    pub title: String,
    pub titlebar: String,
    /// Page path relative to the site root, for og:url.
    pub path: String,
    /// Public URL the site deploys to. Slack ignores relative og:image/og:url,
    /// so those tags only appear when a base URL is known.
    pub base_url: Option<String>,
    /// Page width: prose by default, `Mid` for the leaderboard, `Wide` for
    /// the multi-column run and round grids.
    pub width: Width,
    /// The unfurl image filename for this page, relative to the site root.
    /// Defaults to the shared `og-card.jpg`; run/model/compare pages set their
    /// own so the preview shows the coaster the page is actually about.
    pub og_card: String,
    /// Per-page unfurl description; falls back to the site tagline. Matchup
    /// permalinks set the specific "A vs B" line here.
    pub description: Option<String>,
}

#[derive(Clone, Copy, PartialEq, Eq)]
pub enum Width {
    Prose,
    Mid,
    Wide,
}

impl Chrome {
    pub fn new(title: &str, titlebar: &str, path: &str, base_url: Option<&str>) -> Self {
        Self {
            title: title.to_string(),
            titlebar: titlebar.to_string(),
            path: path.to_string(),
            base_url: base_url.map(|b| b.trim_end_matches('/').to_string()),
            width: Width::Prose,
            og_card: "og-card.jpg".to_string(),
            description: None,
        }
    }

    pub fn width(mut self, width: Width) -> Self {
        self.width = width;
        self
    }

    /// Point this page's unfurl at its own card, when one could be drawn;
    /// `None` leaves it on the shared site card (see `og_card`).
    pub fn maybe_og_card(mut self, card: Option<&str>) -> Self {
        if let Some(card) = card {
            self.og_card = card.to_string();
        }
        self
    }

    /// Override the unfurl description (og:description) for this page.
    pub fn description(mut self, desc: &str) -> Self {
        self.description = Some(desc.to_string());
        self
    }

    /// The unfurl description: per-page override, else the site tagline.
    pub fn og_description(&self) -> &str {
        self.description.as_deref().unwrap_or(TAGLINE)
    }

    pub fn tagline(&self) -> &'static str {
        TAGLINE
    }

    pub fn css(&self) -> &'static str {
        CSS
    }

    pub fn js(&self) -> &'static str {
        JS
    }

    /// The one face the first screen needs; the rest load normally.
    pub fn font_preload(&self) -> &'static str {
        crate::fonts::PRELOAD
    }

    pub fn wrap_class(&self) -> &'static str {
        match self.width {
            Width::Prose => "wrap",
            Width::Mid => "wrap wrap-mid",
            Width::Wide => "wrap wrap-wide",
        }
    }

    pub fn og_url(&self) -> Option<String> {
        // The home page is the bare directory, not /index.html: this is the
        // canonical URL as well as the unfurl one.
        let path = self.path.strip_suffix("index.html").unwrap_or(&self.path);
        self.base_url.as_ref().map(|base| format!("{base}/{path}"))
    }

    pub fn og_image(&self) -> Option<String> {
        self.base_url
            .as_ref()
            .map(|base| format!("{base}/{}", self.og_card))
    }
}

/// One preview tile in a track design gallery.
pub struct Figure {
    pub name: String,
    pub caption: String,
    pub src: Option<String>,
}

/// One row of the index table: a single model's collection of rounds within a
/// run (runs are pivoted, so a three-model run is three rows).
#[derive(Clone)]
pub struct IndexRow {
    pub run_name: String,
    pub run_href: String,
    /// The model's own detail page.
    pub model_href: String,
    pub date: String,
    pub mode: String,
    pub coaster: String,
    pub harness: String,
    /// "single" or "N parks" — the scenario-count facet value.
    pub parks: String,
    pub model: String,
    pub thumb: Option<String>,
    pub place: String,
    pub is_winner: bool,
    /// Sort keys for the client-side column sorter; unrated rows sort last.
    pub sort_score: f64,
    pub sort_intensity: f64,
    pub sort_nausea: f64,
    pub score: Option<String>,
    /// This row's score as a percentage of the best on the board, for the meter
    /// under the number. 0 when nothing rated.
    pub score_pct: String,
    pub intensity: String,
    pub nausea: String,
    pub best_round: String,
    pub usage: String,
}

/// One board: a condition, and its rows split by coaster so nothing is ever
/// ranked across ride types.
pub struct IndexBoard {
    pub condition: String,
    pub tagline: String,
    /// True for the black-box design board, the benchmark proper.
    pub headline: bool,
    pub groups: Vec<IndexGroup>,
}

pub struct IndexGroup {
    pub coaster: String,
    /// Shown only when a board holds more than one coaster.
    pub labelled: bool,
    pub rows: Vec<IndexRow>,
}

pub struct Facet {
    pub name: String,
    pub values: Vec<String>,
}

/// The record coaster, headlining the index: footage rather than a still,
/// unlabelled beyond its caption.
pub struct Featured {
    /// Model-authored ride name, when the run recorded one.
    pub name: Option<String>,
    pub model: String,
    pub model_href: String,
    pub replay: String,
    /// The replay's own first frame, so the still looks like the clip.
    pub poster: Option<String>,
    /// Whether the clip runs a whole cycle, and so can be looped.
    pub loops: bool,
    /// For anyone who cannot watch it.
    pub alt: String,
    pub stats: Vec<Stat>,
}

#[derive(Template)]
#[template(path = "index.html")]
pub struct IndexPage {
    pub chrome: Chrome,
    /// None when no leaderboard round has a video yet.
    pub featured: Option<Featured>,
    pub facets: Vec<Facet>,
    /// One per condition, hardest-earned first.
    pub boards: Vec<IndexBoard>,
    pub row_count: usize,
    pub have_previews: bool,
    pub mode_taglines: Vec<(String, String)>,
    /// Runs left out because they never finished, newest first.
    pub skipped: Vec<String>,
    /// Runs deliberately withdrawn from the site, named rather than vanished.
    pub withdrawn: Vec<String>,
}

pub struct StandingRow {
    pub place: String,
    pub model: String,
    pub is_winner: bool,
    pub score: Option<String>,
    pub intensity: String,
    pub nausea: String,
    pub similarity: String,
    pub best_round: String,
    pub usage: String,
}

pub struct Shot {
    /// What the card shows: downscaled and re-encoded.
    pub src: String,
    /// The original capture, loaded only when someone opens the lightbox.
    pub full: String,
    pub label: String,
    /// Intrinsic size, when the file was local enough to measure. Emitted as
    /// width/height so the browser reserves the space before the image lands.
    pub size: Option<(u32, u32)>,
}

/// How a round ended, as a chip next to its heading: only the outcomes worth
/// flagging get one, so a clean round stays quiet.
pub struct Badge {
    pub text: String,
    pub class: String,
}

pub struct Swatch {
    pub name: String,
    pub class: String,
}

pub struct Presentation {
    pub name: String,
    pub track: Swatch,
    pub rail: Swatch,
    pub support: Swatch,
}

pub struct RoundView {
    pub number: u32,
    /// The generated park this round played on; shown as a chip on the round
    /// heading in multi-scenario runs.
    pub scenario: Option<String>,
    /// Link to this round's trace page, when a trace was recorded.
    pub trace_href: Option<String>,
    pub trace_events: usize,
    pub trace_rejections: usize,
    pub badge: Option<Badge>,
    /// Verdict of the circuit audit: whether every piece standing in the park
    /// is track a train actually rides. Absent on rounds recorded before the
    /// audit existed, which is different from a round that has no orphans.
    pub circuit: Option<Badge>,
    /// Model-authored name and colours; absent on runs made before the tool.
    pub presentation: Option<Presentation>,
    /// The build failure, kept to one clamped line (full text in the tooltip).
    pub build_error: Option<String>,
    /// Rating line, absent when the round produced no rated ride.
    pub stats: Option<RoundStats>,
    pub unrated_note: bool,
    pub lookups: Option<String>,
    pub program_json: Option<String>,
    pub program_pieces: usize,
    /// Elevation profile as inline SVG; empty without a program to walk.
    pub dna: String,
    /// The profile's shape, and what changed since the round before.
    pub dna_caption: String,
    /// Replay video source, when the round recorded one.
    pub replay: Option<String>,
    /// Download URL for the exact scored .park save.
    pub park: Option<String>,
    /// Poster still framed like the replay, written beside it while filming.
    pub replay_poster: Option<String>,
    /// Whether the clip runs a whole cycle, and so can be looped.
    pub replay_loops: bool,
    pub shots: Vec<Shot>,
    /// The rotator's shot list / labels as JSON, for the client-side flipper.
    pub shots_json: String,
    /// The same shots at full resolution, for the lightbox.
    pub fulls_json: String,
    pub labels_json: String,
}

pub struct RoundStats {
    pub excitement: String,
    pub intensity: String,
    pub nausea: String,
    pub ride_length: i64,
    pub num_drops: i64,
    pub total_air_time: i64,
    pub crashed: bool,
    pub similarity: Option<String>,
    /// Set when the copy penalty actually cut the score.
    pub penalized: Option<String>,
}

pub struct ModelView {
    pub model: String,
    /// This model's detail page, linked from the run comparison.
    pub href: String,
    pub chart_svg: String,
    pub studied: Vec<Figure>,
    pub rounds: Vec<RoundView>,
}

#[derive(Template)]
#[template(path = "run.html")]
pub struct RunPage {
    pub chrome: Chrome,
    pub mode_tagline: String,
    pub grace: String,
    pub standings: Vec<StandingRow>,
    pub models: Vec<ModelView>,
}

/// A headline number on the model detail page.
pub struct Stat {
    pub label: String,
    pub value: String,
    /// CSS class for the value, e.g. "rating-excitement".
    pub class: String,
}

/// One model's whole run: the detail view an index row links to.
#[derive(Template)]
#[template(path = "model.html")]
pub struct ModelPage<'a> {
    pub chrome: Chrome,
    pub run_name: String,
    pub run_href: String,
    pub place: String,
    pub of_models: usize,
    pub context: String,
    pub stats: Vec<Stat>,
    pub model: &'a ModelView,
}

#[derive(Template)]
#[template(path = "library.html")]
pub struct LibraryPage {
    pub chrome: Chrome,
    pub figures: Vec<Figure>,
}

/// One row of a session trace: what the model said, or a tool call and how the
/// game answered.
pub struct TraceRow {
    /// "text", "thinking", "tool" or "step".
    pub kind: String,
    /// Elapsed session time, e.g. "1:42".
    pub at: String,
    pub text: Option<String>,
    pub tool: Option<String>,
    pub input: Option<String>,
    pub output: Option<String>,
    /// Set when the game refused the call, so the row can be flagged.
    pub failed: bool,
    pub duration: Option<String>,
    pub cost: Option<String>,
}

/// Summary strip above a trace: enough to see the shape of a round without
/// reading it.
pub struct TraceSummary {
    pub label: String,
    pub value: String,
    pub class: String,
}

/// One finished model run entered in the head-to-head. Everything the compare
/// page needs is here so the client can render any pairing without a round
/// trip; serialised to a JSON blob embedded in the page.
#[derive(Serialize)]
pub struct Contender {
    /// Stable URL-safe id (the model page path without ".html"), used in the
    /// `?a=&b=` deep link.
    pub id: String,
    /// The model's detail page.
    pub href: String,
    pub run: String,
    pub date: String,
    pub model: String,
    pub coaster: String,
    pub mode: String,
    pub harness: String,
    pub thumb: Option<String>,
    pub score: Option<f64>,
    pub intensity: Option<f64>,
    pub nausea: Option<f64>,
    pub similarity: Option<f64>,
    pub ride_length: Option<i64>,
    pub airtime: Option<i64>,
    pub drops: Option<i64>,
    pub best_round: Option<u32>,
    pub rounds: usize,
    pub rated_rounds: usize,
    pub tokens: f64,
    pub cost: Option<f64>,
    /// Per-round excitement (0 for unrated), oldest first, for the sparkline.
    pub round_scores: Vec<f64>,
}

#[derive(Template)]
#[template(path = "compare.html")]
pub struct ComparePage {
    pub chrome: Chrome,
    /// JSON array of `Contender`, embedded and read by the client.
    pub contenders_json: String,
    pub default_a: String,
    pub default_b: String,
    pub count: usize,
}

#[derive(Template)]
#[template(path = "trace.html")]
pub struct TracePage {
    pub chrome: Chrome,
    pub run_name: String,
    pub run_href: String,
    pub model: String,
    pub model_href: String,
    pub round: u32,
    pub summary: Vec<TraceSummary>,
    pub rows: Vec<TraceRow>,
}
