//! The eval-run domain: what `evals/runs/<run>/<model>/round_N/` holds, and
//! the scoring rules the site reports on.

use std::path::{Path, PathBuf};

use anyhow::{Context, Result};
use serde::Deserialize;

use crate::art::{Art, ArtStore};

/// The driver's copy penalty: similarity to a stock library design up to the
/// grace threshold is free, then the score scales linearly to zero at 1.0.
/// Each run records its threshold in run.json (the driver is the source of
/// truth); this default only covers runs from before it was recorded.
pub const DEFAULT_SIMILARITY_GRACE: f64 = 0.5;

pub fn similarity_multiplier(similarity: f64, grace: f64) -> f64 {
    if similarity <= grace {
        return 1.0;
    }
    ((1.0 - similarity) / (1.0 - grace)).max(0.0)
}

/// Keep in sync with RenderTrackLibrary in src/openrct2/rustbridge/RustBridge.cpp.
pub fn sanitise_name(name: &str) -> String {
    name.chars()
        .map(|c| {
            if c.is_ascii_alphanumeric() || matches!(c, '.' | '_' | '-') {
                c
            } else {
                '_'
            }
        })
        .collect()
}

#[derive(Debug, Deserialize, Default)]
struct RunMeta {
    mode: Option<String>,
    similarity_grace: Option<f64>,
    ride_type: Option<i64>,
    harness: Option<String>,
    /// What the orchestrator set out to run. Present since coaster-bench;
    /// used to tell a finished run from one that died (or is still going)
    /// half way through.
    #[serde(default)]
    models: Vec<String>,
    rounds: Option<u32>,
    /// Multi-scenario runs (driver --scenarios): the generated parks each
    /// model competed on, e.g. ["seed_1", "seed_2"]. Empty for single-park
    /// runs.
    #[serde(default)]
    scenarios: Vec<String>,
    /// Open note: the agents could read the engine source they are scored by.
    #[serde(default)]
    open_note: bool,
    /// Set false to keep a run out of the published site. Absent means yes: a
    /// run has to be deliberately withdrawn, never accidentally.
    published: Option<bool>,
}

#[derive(Debug, Deserialize)]
pub struct Ride {
    pub excitement: Option<f64>,
    #[serde(default)]
    pub intensity: Option<f64>,
    #[serde(default)]
    pub nausea: Option<f64>,
    #[serde(default)]
    pub ride_length: i64,
    #[serde(default)]
    pub num_drops: i64,
    #[serde(default)]
    pub total_air_time: i64,
    #[serde(default)]
    pub crashed: bool,
    #[serde(default)]
    pub circuit: Option<Circuit>,
}

/// Non-scoring name and colours the model chose for its banked winner.
#[derive(Debug, Deserialize)]
pub struct Presentation {
    pub name: String,
    pub track_color: String,
    pub rail_color: String,
    pub support_color: String,
}

/// The game's own walk of the ride's track. Absent from runs made before the
/// audit existed, which is why the whole field is optional rather than zeroed.
#[derive(Debug, Deserialize)]
pub struct Circuit {
    #[serde(default)]
    pub walked_pieces: u32,
    #[serde(default)]
    pub total_pieces: u32,
    #[serde(default)]
    pub orphan_pieces: u32,
    #[serde(default)]
    pub looped: bool,
}

/// The sidecar filming leaves beside a clip.
#[derive(Debug, Deserialize)]
struct ReplayMeta {
    #[serde(default)]
    looped: bool,
}

#[derive(Debug, Deserialize)]
pub struct Similarity {
    #[serde(default)]
    pub similarity: f64,
    #[serde(default)]
    pub nearest_design: String,
}

#[derive(Debug, Deserialize)]
struct ProgramError {
    piece_index: Option<i64>,
    message: Option<String>,
}

#[derive(Debug, Deserialize)]
struct ProgramResult {
    #[serde(default)]
    ok: bool,
    #[serde(default)]
    pieces_placed: i64,
    #[serde(default)]
    pieces_total: i64,
    #[serde(default)]
    error: Option<ProgramError>,
}

#[derive(Debug, Deserialize)]
struct Report {
    #[serde(default)]
    program: Option<ProgramResult>,
    #[serde(default)]
    rides: Vec<Ride>,
    #[serde(default)]
    similarity: Option<Similarity>,
    #[serde(default)]
    presentation: Option<Presentation>,
}

/// One library tool call a model made before submitting a round.
#[derive(Debug, Deserialize)]
pub struct Lookup {
    #[serde(default)]
    pub tool: String,
    #[serde(default)]
    pub name: Option<String>,
    #[serde(default)]
    pub found: bool,
}

#[derive(Debug, Deserialize, Default)]
pub struct Usage {
    #[serde(default)]
    pub input_tokens: Option<f64>,
    #[serde(default)]
    pub output_tokens: Option<f64>,
    #[serde(default)]
    pub cost_usd: Option<f64>,
}

/// A design in the stock track library, as dumped by `--dump-library`.
#[derive(Debug, Deserialize)]
pub struct LibraryDesign {
    pub name: String,
    #[serde(default)]
    pub ride_type: i64,
    #[serde(default)]
    pub piece_count: i64,
}

#[derive(Debug)]
pub struct Round {
    pub number: u32,
    /// Which generated park this round played on (multi-scenario runs nest
    /// rounds as `<model>/<scenario>/round_N`); None for single-park runs.
    pub scenario: Option<String>,
    /// The rated ride of the round, when the test circuit completed.
    pub ride: Option<Ride>,
    pub similarity: Option<Similarity>,
    pub presentation: Option<Presentation>,
    pub build_error: Option<String>,
    /// Pretty-printed program.json, shown in a <details> block.
    pub program_json: Option<String>,
    pub program_pieces: usize,
    /// Parsed, for the elevation profile.
    pub pieces: Vec<crate::dna::Piece>,
    pub screenshot: Option<Art>,
    /// Exact scored park save, reopenable in OpenRCT2.
    pub park: Option<Art>,
    /// Replay of the finished ride, when one was recorded.
    pub replay: Option<Art>,
    /// Still from the replay's own camera; park screenshots frame the whole map.
    pub replay_poster: Option<Art>,
    /// A whole station-to-station cycle, and so loops. False when the lap
    /// outran the cap; None for clips filmed before this was recorded.
    pub replay_looped: Option<bool>,
    /// Additional view rotations of the same capture (park-r1/2/3.png).
    pub rotation_shots: Vec<Art>,
    /// See-through verification capture (park-x.png): terrain and supports
    /// hidden so tunnelled track is visible. The upstream sprite-sort glitch
    /// that hides track near track crossings at some rotations still applies.
    pub xray_shot: Option<Art>,
    /// Library tool calls the model made before submitting (library mode).
    pub lookups: Vec<Lookup>,
    /// Token/cost accounting for the round (usage.json), when recorded.
    pub usage: Option<Usage>,
    /// Normalised agent session events (trace.jsonl), oldest first. Only rounds
    /// driven by coaster-bench have one.
    pub trace: Vec<TraceEvent>,
    grace: f64,
}

/// One event from a round's trace.jsonl. Field meanings are set by
/// coaster-bench's trace module; everything past `kind` is optional because a
/// killed session leaves half-written events behind.
#[derive(Debug, Deserialize)]
pub struct TraceEvent {
    pub kind: String,
    #[serde(default)]
    pub ms: Option<i64>,
    #[serde(default)]
    pub text: Option<String>,
    #[serde(default)]
    pub name: Option<String>,
    #[serde(default)]
    pub input: Option<serde_json::Value>,
    #[serde(default)]
    pub output: Option<serde_json::Value>,
    #[serde(default)]
    pub status: Option<String>,
    #[serde(default)]
    pub dur_ms: Option<i64>,
    #[serde(default)]
    pub cost_usd: Option<f64>,
}

impl TraceEvent {
    pub fn is_tool(&self) -> bool {
        self.kind == "tool"
    }

    /// A tool call the game refused: the rejections are the substance of a
    /// trace, so they get counted and highlighted separately.
    pub fn is_rejection(&self) -> bool {
        self.is_tool() && self.status.as_deref() == Some("error")
    }
}

/// Reads trace.jsonl, skipping any line that fails to parse: a session killed
/// mid-write can leave a truncated last line, and a partial trace still
/// explains more than no trace.
fn read_trace(path: &Path) -> Vec<TraceEvent> {
    let Ok(raw) = std::fs::read_to_string(path) else {
        return Vec::new();
    };
    raw.lines()
        .filter(|l| l.starts_with('{'))
        .filter_map(|l| serde_json::from_str(l).ok())
        .collect()
}

impl Round {
    /// Raw excitement scaled down for copying a stock library design.
    pub fn excitement(&self) -> f64 {
        let Some(raw) = self.ride.as_ref().and_then(|r| r.excitement) else {
            return 0.0;
        };
        let sim = self.similarity.as_ref().map_or(0.0, |s| s.similarity);
        raw * similarity_multiplier(sim, self.grace)
    }

    pub fn fetched_designs(&self) -> Vec<&str> {
        self.lookups
            .iter()
            .filter(|l| l.tool == "get" && l.found)
            .filter_map(|l| l.name.as_deref())
            .collect()
    }
}

#[derive(Debug)]
pub struct ModelRun {
    pub model: String,
    pub rounds: Vec<Round>,
}

/// (input tokens, output tokens, cost USD) summed over rounds; the cost is
/// None when no round recorded one.
#[derive(Debug, Default, Clone, Copy)]
pub struct UsageTotals {
    pub tokens_in: f64,
    pub tokens_out: f64,
    pub cost_usd: Option<f64>,
}

impl ModelRun {
    pub fn best(&self) -> Option<&Round> {
        self.rounds
            .iter()
            .filter(|r| r.excitement() > 0.0)
            .max_by(|a, b| a.excitement().total_cmp(&b.excitement()))
    }

    /// The best rated round on one scenario park.
    pub fn best_in(&self, scenario: &str) -> Option<&Round> {
        self.rounds
            .iter()
            .filter(|r| r.scenario.as_deref() == Some(scenario) && r.excitement() > 0.0)
            .max_by(|a, b| a.excitement().total_cmp(&b.excitement()))
    }

    /// Aggregate score across scenario parks: the mean of each park's best
    /// excitement, counting a park with no rated coaster as zero — the
    /// aggregate measures reliability across parks, not just the good ones.
    pub fn aggregate_score(&self, scenarios: &[String]) -> f64 {
        if scenarios.is_empty() {
            return self.best().map_or(0.0, |r| r.excitement());
        }
        let total: f64 = scenarios
            .iter()
            .map(|s| self.best_in(s).map_or(0.0, |r| r.excitement()))
            .sum();
        total / scenarios.len() as f64
    }

    /// How many scenario parks got at least one rated coaster.
    pub fn scenarios_scored(&self, scenarios: &[String]) -> usize {
        scenarios
            .iter()
            .filter(|s| self.best_in(s).is_some())
            .count()
    }

    pub fn usage_totals(&self) -> UsageTotals {
        let mut totals = UsageTotals::default();
        for usage in self.rounds.iter().filter_map(|r| r.usage.as_ref()) {
            totals.tokens_in += usage.input_tokens.unwrap_or(0.0);
            totals.tokens_out += usage.output_tokens.unwrap_or(0.0);
            if let Some(cost) = usage.cost_usd {
                totals.cost_usd = Some(totals.cost_usd.unwrap_or(0.0) + cost);
            }
        }
        totals
    }

    /// The stock designs this model studied, in fetch order, deduplicated.
    pub fn studied_designs(&self) -> Vec<String> {
        let mut studied: Vec<String> = Vec::new();
        for round in &self.rounds {
            for name in round.fetched_designs() {
                if !studied.iter().any(|s| s == name) {
                    studied.push(name.to_string());
                }
            }
        }
        studied
    }
}

#[derive(Debug)]
pub struct EvalRun {
    pub name: String,
    /// Raw mode. Private: display and comparison keys must go through
    /// `mode_label()`, which folds in modifiers like open note.
    mode: String,
    pub grace: f64,
    pub models: Vec<ModelRun>,
    pub ride_type: i64,
    pub harness: String,
    /// Multi-scenario runs: the generated parks every model competed on, in
    /// run.json order. Empty for classic single-park runs.
    pub scenarios: Vec<String>,
    pub open_note: bool,
    /// Whether this run belongs on the published site; `published: false` in
    /// run.json withdraws it without deleting the evidence.
    pub published: bool,
    /// Models and round count run.json promised, when it recorded them.
    expected_models: Vec<String>,
    expected_rounds: Option<u32>,
}

impl EvalRun {
    pub fn ride_name(&self) -> String {
        match self.ride_type {
            51 => "steel twister".to_string(),
            52 => "wooden".to_string(),
            other => format!("type {other}"),
        }
    }

    /// The YYYYMMDD prefix of the run name as an ISO date, when it has one.
    pub fn date(&self) -> String {
        let digits: String = self.name.chars().take(8).collect();
        if digits.len() == 8 && digits.chars().all(|c| c.is_ascii_digit()) {
            format!("{}-{}-{}", &digits[0..4], &digits[4..6], &digits[6..8])
        } else {
            String::new()
        }
    }

    /// Why this run is not finished, or None when it is.
    ///
    /// A run that died (or is still in flight) half way through would
    /// otherwise show up on the leaderboard as a model losing badly, which is
    /// worse than not showing it at all. run.json says what was promised;
    /// runs from before it recorded that fall back to "every model ran the
    /// same number of rounds".
    pub fn incomplete_reason(&self) -> Option<String> {
        // run.json records raw model ids ("poolside/Laguna-S-2.1"); model
        // dirs on disk are sanitised, so compare in sanitised space.
        let have = |name: &str| {
            self.models
                .iter()
                .find(|m| m.model == name || m.model == sanitise_name(name))
        };
        let missing: Vec<&str> = self
            .expected_models
            .iter()
            .filter(|name| have(name).is_none())
            .map(String::as_str)
            .collect();
        if !missing.is_empty() {
            return Some(format!("no rounds for {}", missing.join(", ")));
        }
        let expected = self.expected_rounds.unwrap_or_else(|| {
            self.models
                .iter()
                .map(|m| m.rounds.len() as u32)
                .max()
                .unwrap_or(0)
        });
        let short: Vec<String> = self
            .models
            .iter()
            .filter(|m| (m.rounds.len() as u32) < expected)
            .map(|m| format!("{} {}/{expected}", m.model, m.rounds.len()))
            .collect();
        if short.is_empty() {
            None
        } else {
            Some(format!("short rounds: {}", short.join(", ")))
        }
    }

    /// Mode alone, for tagline lookup. Not for display: see `mode_label()`.
    pub fn base_mode(&self) -> &str {
        &self.mode
    }

    /// Mode as shown and faceted. Open note is a modifier rather than a mode,
    /// but its scores are not comparable with black-box ones, so the label has
    /// to say so wherever a run is identified.
    pub fn mode_label(&self) -> String {
        if self.open_note {
            format!("{} + open note", self.mode)
        } else {
            self.mode.clone()
        }
    }

    /// Rounds each model was expected to run: run.json's promise (times the
    /// scenario count on multi-scenario runs, where `rounds` is per park), or
    /// the run's own longest model when it predates that field.
    pub fn expected_round_count(&self) -> u32 {
        self.expected_rounds
            .map(|r| r * self.scenarios.len().max(1) as u32)
            .unwrap_or_else(|| {
                self.models
                    .iter()
                    .map(|m| m.rounds.len() as u32)
                    .max()
                    .unwrap_or(0)
            })
    }

    /// A model's headline score: best excitement on single-park runs, the
    /// across-parks aggregate on multi-scenario runs.
    pub fn score_of(&self, model: &ModelRun) -> f64 {
        if self.scenarios.is_empty() {
            model.best().map_or(0.0, |r| r.excitement())
        } else {
            model.aggregate_score(&self.scenarios)
        }
    }

    /// True when this model ran its full expected round count. Unlike
    /// `incomplete_reason`, this is per-model: a finished model inside an
    /// otherwise-abandoned run (a solo model whose promised rivals never ran)
    /// still counts, which is what the head-to-head compares.
    pub fn model_completed(&self, model: &ModelRun) -> bool {
        model.rounds.len() as u32 >= self.expected_round_count() && self.expected_round_count() > 0
    }

    /// Models best-first, so index one is the winner.
    pub fn ranked(&self) -> Vec<&ModelRun> {
        let mut models: Vec<&ModelRun> = self.models.iter().collect();
        models.sort_by(|a, b| self.score_of(b).total_cmp(&self.score_of(a)));
        models
    }
}

fn read_json<T: for<'de> Deserialize<'de>>(path: &Path) -> Result<T> {
    let text =
        std::fs::read_to_string(path).with_context(|| format!("reading {}", path.display()))?;
    serde_json::from_str(&text).with_context(|| format!("parsing {}", path.display()))
}

fn read_json_opt<T: for<'de> Deserialize<'de>>(path: &Path) -> Result<Option<T>> {
    if path.is_file() {
        read_json(path).map(Some)
    } else {
        Ok(None)
    }
}

/// Sorted entries of a directory (missing directory reads as empty, so the
/// site still builds against a fresh checkout with no runs).
fn sorted_entries(dir: &Path) -> Result<Vec<PathBuf>> {
    if !dir.is_dir() {
        return Ok(Vec::new());
    }
    let mut entries: Vec<PathBuf> = std::fs::read_dir(dir)
        .with_context(|| format!("listing {}", dir.display()))?
        .collect::<std::io::Result<Vec<_>>>()?
        .into_iter()
        .map(|e| e.path())
        .collect();
    entries.sort();
    Ok(entries)
}

fn build_error(program: Option<&ProgramResult>) -> Option<String> {
    let program = program?;
    if program.ok {
        return None;
    }
    let error = program.error.as_ref();
    let message = error
        .and_then(|e| e.message.clone())
        .unwrap_or_else(|| "unknown error".to_string());
    let where_ = match error.and_then(|e| e.piece_index) {
        Some(idx) => format!(" at piece {idx}"),
        None => String::new(),
    };
    Some(format!(
        "build failed{where_} ({}/{} pieces placed): {message}",
        program.pieces_placed, program.pieces_total
    ))
}

fn load_round(
    round_dir: &Path,
    run_dir: &Path,
    store: &ArtStore,
    grace: f64,
    scenario: Option<&str>,
) -> Result<Option<Round>> {
    let report_path = round_dir.join("report.json");
    if !report_path.is_file() {
        return Ok(None);
    }
    let Some(number) = round_dir
        .file_name()
        .and_then(|n| n.to_str())
        .and_then(|n| n.rsplit('_').next())
        .and_then(|n| n.parse::<u32>().ok())
    else {
        return Ok(None);
    };
    let report: Report = read_json(&report_path)?;
    let program: Option<serde_json::Value> = read_json_opt(&round_dir.join("program.json"))?;
    let program_pieces = program
        .as_ref()
        .and_then(|p| p.get("pieces"))
        .and_then(|p| p.as_array())
        .map_or(0, |p| p.len());
    let program_json = program
        .as_ref()
        .map(serde_json::to_string_pretty)
        .transpose()?;
    let pieces = program
        .as_ref()
        .map(crate::dna::parse_pieces)
        .unwrap_or_default();

    // Paths in the artifact manifest are relative to the run directory.
    let rel = round_dir.strip_prefix(run_dir).unwrap_or(round_dir);
    let art = |name: &str| store.art(run_dir, &rel.join(name));
    let screenshot = art("park_small.png").or_else(|| art("park.png"));
    let rotation_shots = (1..=3)
        .filter_map(|i| art(&format!("park-r{i}.png")))
        .collect();

    Ok(Some(Round {
        number,
        scenario: scenario.map(str::to_string),
        ride: report.rides.into_iter().find(|r| r.excitement.is_some()),
        similarity: report.similarity,
        presentation: report.presentation,
        build_error: build_error(report.program.as_ref()),
        program_json,
        program_pieces,
        pieces,
        screenshot,
        park: art("park.park"),
        replay: art("replay.mp4"),
        replay_poster: art("replay.png"),
        replay_looped: read_json_opt::<ReplayMeta>(&round_dir.join("replay.json"))?
            .map(|meta| meta.looped),
        rotation_shots,
        xray_shot: art("park-x.png"),
        lookups: read_json_opt(&round_dir.join("lookups.json"))?.unwrap_or_default(),
        usage: read_json_opt(&round_dir.join("usage.json"))?,
        trace: read_trace(&round_dir.join("trace.jsonl")),
        grace,
    }))
}

/// Loads every run under `runs_dir`, newest first. A run directory needs at
/// least one model with one round report to appear.
pub fn load_runs(runs_dir: &Path, store: &ArtStore) -> Result<Vec<EvalRun>> {
    let mut runs = Vec::new();
    for run_dir in sorted_entries(runs_dir)?.into_iter().rev() {
        if !run_dir.is_dir() {
            continue;
        }
        // Mode and penalty parameters live in run.json (newer runs); older
        // runs are design mode with the default grace.
        let meta: RunMeta = read_json_opt(&run_dir.join("run.json"))?.unwrap_or_default();
        let grace = meta.similarity_grace.unwrap_or(DEFAULT_SIMILARITY_GRACE);

        let dir_name = |path: &Path| {
            path.file_name()
                .and_then(|n| n.to_str())
                .unwrap_or_default()
                .to_string()
        };
        let mut models = Vec::new();
        let mut seen_scenarios: Vec<String> = Vec::new();
        for model_dir in sorted_entries(&run_dir)? {
            if !model_dir.is_dir() {
                continue;
            }
            let mut rounds = Vec::new();
            for entry in sorted_entries(&model_dir)? {
                if !entry.is_dir() {
                    continue;
                }
                let name = dir_name(&entry);
                if name.starts_with("round_") {
                    if let Some(round) = load_round(&entry, &run_dir, store, grace, None)? {
                        rounds.push(round);
                    }
                } else {
                    // Multi-scenario layout: <model>/<scenario>/round_N.
                    for round_dir in sorted_entries(&entry)? {
                        let is_round =
                            round_dir.is_dir() && dir_name(&round_dir).starts_with("round_");
                        if !is_round {
                            continue;
                        }
                        if let Some(round) =
                            load_round(&round_dir, &run_dir, store, grace, Some(&name))?
                        {
                            if !seen_scenarios.contains(&name) {
                                seen_scenarios.push(name.clone());
                            }
                            rounds.push(round);
                        }
                    }
                }
            }
            rounds.sort_by(|a, b| (&a.scenario, a.number).cmp(&(&b.scenario, b.number)));
            if !rounds.is_empty() {
                models.push(ModelRun {
                    model: dir_name(&model_dir),
                    rounds,
                });
            }
        }
        if models.is_empty() {
            continue;
        }
        // run.json's list is authoritative (it includes parks nobody scored
        // on); the directory scan covers harnesses that nest rounds without
        // recording the field.
        let scenarios = if meta.scenarios.is_empty() {
            seen_scenarios
        } else {
            meta.scenarios.clone()
        };
        runs.push(EvalRun {
            name: run_dir
                .file_name()
                .and_then(|n| n.to_str())
                .unwrap_or_default()
                .to_string(),
            mode: meta.mode.unwrap_or_else(|| "design".to_string()),
            grace,
            models,
            ride_type: meta.ride_type.unwrap_or(52),
            harness: meta.harness.unwrap_or_else(|| "driver-api".to_string()),
            scenarios,
            open_note: meta.open_note,
            published: meta.published.unwrap_or(true),
            expected_models: meta.models,
            expected_rounds: meta.rounds,
        });
    }
    Ok(runs)
}

/// The most recent run's library.json: name/ride_type/piece_count per design,
/// used to caption and order the full gallery page.
pub fn latest_library_index(runs_dir: &Path) -> Result<Vec<LibraryDesign>> {
    for run_dir in sorted_entries(runs_dir)?.into_iter().rev() {
        let library = run_dir.join("library.json");
        if library.is_file() {
            return read_json(&library);
        }
    }
    Ok(Vec::new())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn similarity_penalty_scales_to_zero_above_grace() {
        assert_eq!(similarity_multiplier(0.4, 0.5), 1.0);
        assert_eq!(similarity_multiplier(0.5, 0.5), 1.0);
        assert_eq!(similarity_multiplier(0.75, 0.5), 0.5);
        assert_eq!(similarity_multiplier(1.0, 0.5), 0.0);
    }

    #[test]
    fn sanitise_matches_the_game_side_rule() {
        assert_eq!(sanitise_name("Judge Roy Scream"), "Judge_Roy_Scream");
        assert_eq!(sanitise_name("Colossus (Track 1)"), "Colossus__Track_1_");
        assert_eq!(sanitise_name("a.b-c_d"), "a.b-c_d");
    }

    #[test]
    fn build_error_reports_placement_progress() {
        let program = ProgramResult {
            ok: false,
            pieces_placed: 12,
            pieces_total: 57,
            error: Some(ProgramError {
                piece_index: Some(12),
                message: Some("no room".to_string()),
            }),
        };
        assert_eq!(
            build_error(Some(&program)).as_deref(),
            Some("build failed at piece 12 (12/57 pieces placed): no room")
        );
        let ok = ProgramResult {
            ok: true,
            pieces_placed: 57,
            pieces_total: 57,
            error: None,
        };
        assert_eq!(build_error(Some(&ok)), None);
    }

    /// A round with a rated ride, enough to count towards completeness.
    fn round(number: u32, excitement: f64) -> Round {
        Round {
            number,
            scenario: None,
            ride: Some(Ride {
                excitement: Some(excitement),
                intensity: Some(6.0),
                nausea: Some(3.0),
                ride_length: 0,
                num_drops: 0,
                total_air_time: 0,
                crashed: false,
                circuit: None,
            }),
            similarity: None,
            presentation: None,
            build_error: None,
            program_json: None,
            program_pieces: 0,
            pieces: Vec::new(),
            screenshot: None,
            park: None,
            replay: None,
            replay_poster: None,
            replay_looped: None,
            rotation_shots: Vec::new(),
            xray_shot: None,
            lookups: Vec::new(),
            usage: None,
            trace: Vec::new(),
            grace: DEFAULT_SIMILARITY_GRACE,
        }
    }

    fn run(models: Vec<(&str, u32)>, expected: &[&str], rounds: Option<u32>) -> EvalRun {
        EvalRun {
            name: "20260101-test".to_string(),
            mode: "design".to_string(),
            grace: DEFAULT_SIMILARITY_GRACE,
            models: models
                .into_iter()
                .map(|(name, count)| ModelRun {
                    model: name.to_string(),
                    rounds: (1..=count).map(|n| round(n, 5.0)).collect(),
                })
                .collect(),
            ride_type: 52,
            harness: "test".to_string(),
            scenarios: Vec::new(),
            open_note: false,
            published: true,
            expected_models: expected.iter().map(|m| m.to_string()).collect(),
            expected_rounds: rounds,
        }
    }

    #[test]
    fn open_note_runs_are_labelled_apart_from_black_box_ones() {
        let mut r = run(vec![("m", 3)], &["m"], Some(3));
        assert_eq!(r.mode_label(), "design");
        r.open_note = true;
        assert_eq!(r.mode_label(), "design + open note");
    }

    #[test]
    fn a_run_is_complete_when_every_model_ran_every_round() {
        let complete = run(vec![("a", 6), ("b", 6)], &["a", "b"], Some(6));
        assert_eq!(complete.incomplete_reason(), None);
    }

    #[test]
    fn a_model_that_never_started_makes_the_run_partial() {
        let partial = run(vec![("a", 6)], &["a", "b"], Some(6));
        assert_eq!(
            partial.incomplete_reason().as_deref(),
            Some("no rounds for b")
        );
    }

    #[test]
    fn a_model_short_of_its_rounds_makes_the_run_partial() {
        let partial = run(vec![("a", 6), ("b", 4)], &["a", "b"], Some(6));
        assert_eq!(
            partial.incomplete_reason().as_deref(),
            Some("short rounds: b 4/6")
        );
    }

    #[test]
    fn runs_from_before_run_json_fall_back_to_an_even_round_count() {
        // No promise recorded: the model that ran the most rounds sets the bar.
        let even = run(vec![("a", 3), ("b", 3)], &[], None);
        assert_eq!(even.incomplete_reason(), None);
        let uneven = run(vec![("a", 3), ("b", 1)], &[], None);
        assert_eq!(
            uneven.incomplete_reason().as_deref(),
            Some("short rounds: b 1/3")
        );
    }

    #[test]
    fn ranking_puts_the_best_score_first() {
        let run = run(vec![("a", 1), ("b", 1)], &[], None);
        assert_eq!(run.ranked()[0].model, "a");
        assert_eq!(run.date(), "2026-01-01");
    }

    fn scenario_round(scenario: &str, number: u32, excitement: f64) -> Round {
        let mut r = round(number, excitement);
        r.scenario = Some(scenario.to_string());
        r
    }

    #[test]
    fn multi_scenario_aggregates_mean_of_per_park_bests() {
        let scenarios = vec![
            "seed_1".to_string(),
            "seed_2".to_string(),
            "seed_3".to_string(),
        ];
        let model = ModelRun {
            model: "a".to_string(),
            rounds: vec![
                scenario_round("seed_1", 1, 4.0),
                scenario_round("seed_1", 2, 6.0),
                scenario_round("seed_2", 1, 3.0),
                // seed_3 never scored: counts as zero in the aggregate.
            ],
        };
        assert_eq!(model.best_in("seed_1").unwrap().excitement(), 6.0);
        assert_eq!(model.aggregate_score(&scenarios), 3.0);
        assert_eq!(model.scenarios_scored(&scenarios), 2);
    }

    #[test]
    fn multi_scenario_expected_rounds_multiply_by_park_count() {
        let mut multi = run(vec![("a", 6)], &["a"], Some(2));
        multi.scenarios = vec![
            "seed_1".to_string(),
            "seed_2".to_string(),
            "seed_3".to_string(),
        ];
        // 2 rounds per park x 3 parks; the model's 6 flat rounds satisfy it.
        assert_eq!(multi.expected_round_count(), 6);
        assert_eq!(multi.incomplete_reason(), None);
    }
}
