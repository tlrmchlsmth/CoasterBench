//! Static site generator for CoasterBench eval runs.
//!
//! Renders evals/runs/ into a self-contained static site styled after the
//! OpenRCT2 in-game window chrome: an index leaderboard (one row per model per
//! run) plus one page per run with every model's rounds, ratings, track
//! programs, and park screenshots.

mod art;
mod chart;
mod dna;
mod fonts;
mod geometry;
mod images;
mod model;
mod view;

use std::path::{Path, PathBuf};

use anyhow::{Context, Result};
use askama::Template;
use clap::Parser;

use art::{Art, ArtStore};
use model::{Circuit, EvalRun, ModelRun, Round};
use view::{
    Badge, Chrome, Facet, Figure, IndexBoard, IndexGroup, IndexPage, IndexRow, LibraryPage,
    ModelPage, ModelView, RoundStats, RoundView, RunPage, Shot, StandingRow, Stat, Swatch,
};

#[derive(Parser)]
#[command(about, long_about = None)]
struct Args {
    /// Runs directory.
    #[arg(long, default_value = "evals/runs")]
    runs: PathBuf,
    /// Output directory.
    #[arg(long, default_value = "evals/site")]
    out: PathBuf,
    /// Rendered track design previews.
    #[arg(long, default_value = "evals/library-previews")]
    previews: PathBuf,
    /// Manifest of previews uploaded to the artifact store.
    #[arg(long, default_value = "evals/library-previews.json")]
    previews_manifest: PathBuf,
    /// Canonical public URL. The site is published to two hosts (Cloudflare
    /// Pages and GitHub Pages); this is the one they both point at, and the one
    /// Slack unfurls resolve against. Pass an empty string to omit both.
    #[arg(long, default_value = "https://coasterbench.wseaton.com")]
    base_url: String,
    /// Public URL of the R2 artifact store holding screenshots not present
    /// locally (see evals/publish.py).
    #[arg(long, default_value = "https://artifacts.wseaton.com")]
    artifact_base: String,
    /// Render runs that never finished (some model short of its rounds).
    #[arg(long)]
    include_partial: bool,
    /// Render runs withdrawn with `"published": false` in run.json.
    #[arg(long)]
    include_unpublished: bool,
    /// Ask Cloudflare to resize artifact-store images at the edge instead of
    /// serving them whole (`/cdn-cgi/image/...`). Needs image transformations
    /// enabled on the zone; without that, every transformed URL 404s.
    #[arg(long)]
    cf_images: bool,
    /// Point every published artifact at the artifact store instead of copying
    /// the local file in. Keeps a preview deploy to HTML (the artifacts run to
    /// hundreds of megabytes) and matches what CI builds.
    #[arg(long)]
    remote_artifacts: bool,
}

fn place_label(place: usize) -> String {
    match place {
        1 => "1st".to_string(),
        2 => "2nd".to_string(),
        3 => "3rd".to_string(),
        other => format!("{other}th"),
    }
}

fn usage_cell(model: &ModelRun) -> String {
    let totals = model.usage_totals();
    let mut parts = Vec::new();
    if totals.tokens_in > 0.0 || totals.tokens_out > 0.0 {
        parts.push(format!(
            "{} in / {} out",
            view::fmt_tokens(totals.tokens_in),
            view::fmt_tokens(totals.tokens_out)
        ));
    }
    if let Some(cost) = totals.cost_usd {
        parts.push(format!("${cost:.2}"));
    }
    if parts.is_empty() {
        "—".to_string()
    } else {
        parts.join(" · ")
    }
}

fn fmt_opt(value: Option<f64>) -> String {
    value.map_or_else(|| "—".to_string(), |v| format!("{v:.2}"))
}

/// Copies a local artifact into the site and returns its src, or hands back
/// the artifact store URL for remote-only ones.
fn place_art(art: &Art, out: &Path, rel: &Path) -> Result<Option<String>> {
    let Some(local) = &art.local else {
        return Ok(art.url.clone());
    };
    let dest = out.join(rel);
    if let Some(parent) = dest.parent() {
        std::fs::create_dir_all(parent)?;
    }
    std::fs::copy(local, &dest)
        .with_context(|| format!("copying {} to {}", local.display(), dest.display()))?;
    Ok(Some(rel.to_string_lossy().replace('\\', "/")))
}

/// Preview tiles for a list of (design name, caption) pairs.
fn figures(designs: &[(String, String)], store: &ArtStore, out: &Path) -> Result<Vec<Figure>> {
    let mut figures = Vec::with_capacity(designs.len());
    for (name, caption) in designs {
        let src = match store.preview(name) {
            Some(art) => {
                let rel = Path::new("assets").join("library").join(&art.name);
                place_art(&art, out, &rel)?
            }
            None => None,
        };
        figures.push(Figure {
            name: name.clone(),
            caption: caption.clone(),
            src,
        });
    }
    Ok(figures)
}

fn round_stats(round: &Round) -> Option<RoundStats> {
    let ride = round.ride.as_ref()?;
    let excitement = ride.excitement?;
    let similarity = round.similarity.as_ref().map(|sim| {
        format!(
            "similarity {:.2} (nearest: {})",
            sim.similarity, sim.nearest_design
        )
    });
    let penalized = (round.excitement() < excitement)
        .then(|| format!("penalized score {:.2}", round.excitement()));
    Some(RoundStats {
        excitement: format!("{excitement:.2}"),
        intensity: fmt_opt(ride.intensity),
        nausea: fmt_opt(ride.nausea),
        ride_length: ride.ride_length,
        num_drops: ride.num_drops,
        total_air_time: ride.total_air_time,
        crashed: ride.crashed,
        similarity,
        penalized,
    })
}

/// The chip beside a round heading. Only outcomes that need explaining get
/// one; a round that built, tested and rated cleanly says nothing.
fn round_badge(round: &Round, stats: Option<&RoundStats>) -> Option<Badge> {
    let badge = |text: &str, class: &str| {
        Some(Badge {
            text: text.to_string(),
            class: class.to_string(),
        })
    };
    if round.build_error.is_some() {
        return badge("build failed", "badge-fail");
    }
    match stats {
        Some(stats) if stats.crashed => badge("crashed", "badge-fail"),
        Some(_) => None,
        None => badge("not rated", "badge-warn"),
    }
}

/// Chip reporting the circuit audit. Ratings already prove the ridden loop is
/// closed and completes, so what this adds is the other half: whether anything
/// in the screenshot is track no train ever touches.
fn circuit_badge(circuit: Option<&Circuit>) -> Option<Badge> {
    let circuit = circuit?;
    let badge = |text: String, class: &str| {
        Some(Badge {
            text,
            class: class.to_string(),
        })
    };
    if circuit.orphan_pieces > 0 {
        return badge(
            format!(
                "{} of {} pieces off the circuit",
                circuit.orphan_pieces, circuit.total_pieces
            ),
            "badge-warn",
        );
    }
    if !circuit.looped {
        return badge("circuit never closed".to_string(), "badge-warn");
    }
    badge(
        format!("verified circuit ({} pieces)", circuit.walked_pieces),
        "badge-ok",
    )
}

fn presentation_view(presentation: Option<&model::Presentation>) -> Option<view::Presentation> {
    let presentation = presentation?;
    let swatch = |name: &str| Swatch {
        name: name.replace('_', " "),
        class: format!(
            "swatch-{}",
            name.chars()
                .map(|c| if c.is_ascii_alphanumeric() { c } else { '-' })
                .collect::<String>()
        ),
    };
    Some(view::Presentation {
        name: presentation.name.clone(),
        track: swatch(&presentation.track_color),
        rail: swatch(&presentation.rail_color),
        support: swatch(&presentation.support_color),
    })
}

fn lookups_line(round: &Round) -> Option<String> {
    if round.lookups.is_empty() {
        return None;
    }
    let searches = round.lookups.iter().filter(|l| l.tool == "search").count();
    let mut line = if searches > 0 {
        format!("library: {searches} search(es)")
    } else {
        "library:".to_string()
    };
    let studied = round.fetched_designs();
    if !studied.is_empty() {
        line.push_str(" studied ");
        line.push_str(&studied.join(", "));
    }
    Some(line)
}

/// Any published artifact URL, for the edge-resize preflight.
fn first_artifact_url(runs: &[EvalRun]) -> Option<String> {
    runs.iter()
        .flat_map(|run| run.models.iter())
        .flat_map(|model| model.rounds.iter())
        .filter_map(|round| round.screenshot.as_ref())
        .find_map(|art| art.url.clone())
}

/// Where a derived display copy lives. Used by both the pre-pass and the render,
/// so they cannot disagree about the path.
fn display_rel(run: &EvalRun, model: &str, round: u32, art_name: &str) -> PathBuf {
    let stem = art_name.rsplit_once('.').map_or(art_name, |(stem, _)| stem);
    Path::new("assets")
        .join(&run.name)
        .join(model)
        .join(format!("round_{round}_{stem}.jpg"))
}

/// Every display copy the site needs: each round's first capture and each
/// replay's poster. Everything else is behind a click, at full resolution.
/// Cards render at ~500 CSS px, so this covers a 2x screen.
const DISPLAY_WIDTH: u32 = 1200;

fn derive_jobs(runs: &[EvalRun], out: &Path) -> Vec<(PathBuf, PathBuf, u32)> {
    let mut jobs = Vec::new();
    for run in runs {
        for model in &run.models {
            for round in &model.rounds {
                let mut want = Vec::new();
                if let Some(art) = round.screenshot.as_ref() {
                    want.push(art);
                }
                if let Some(art) = round.replay_poster.as_ref() {
                    want.push(art);
                }
                for art in want {
                    // Only what is on disk: downloading 300 artifacts just to
                    // shrink them took the build from seconds to ten minutes.
                    // Remote ones resize at the edge (--cf-images) instead.
                    let Some(src) = art.local.clone() else {
                        continue;
                    };
                    let rel = display_rel(run, &model.model, round.number, &art.name);
                    jobs.push((src, out.join(rel), DISPLAY_WIDTH));
                }
            }
        }
    }
    jobs
}

/// A round's poster frame, downscaled and re-encoded into the site. None when
/// the round has no replay still, or when its pixels could not be fetched.
fn poster(
    round: &Round,
    out: &Path,
    run: &EvalRun,
    model: &str,
    store: &ArtStore,
) -> Result<Option<String>> {
    let Some(art) = round.replay_poster.as_ref() else {
        return Ok(None);
    };
    Ok(derived_or_edge(art, run, model, round.number, out, store))
}

/// The derived copy, else an edge-resized URL, else the artifact itself. That
/// last fallback matters: a CI build has no local artifacts to derive from, and
/// returning nothing there left the hero with no poster at all.
fn derived_or_edge(
    art: &Art,
    run: &EvalRun,
    model: &str,
    round: u32,
    out: &Path,
    store: &ArtStore,
) -> Option<String> {
    let rel = display_rel(run, model, round, &art.name);
    if out.join(&rel).is_file() {
        return Some(rel.to_string_lossy().replace('\\', "/"));
    }
    store
        .resized_url(art, DISPLAY_WIDTH)
        .or_else(|| art.url.clone())
}

/// Copies a round artifact into the site under a run/model/round path. Shared by
/// the model view and the index hero, so both point at one file.
fn round_asset(
    art: Option<&Art>,
    out: &Path,
    run: &EvalRun,
    model: &str,
    round: u32,
) -> Result<Option<String>> {
    let Some(art) = art else {
        return Ok(None);
    };
    let rel = Path::new("assets")
        .join(&run.name)
        .join(model)
        .join(format!("round_{round}_{}", art.name));
    place_art(art, out, &rel)
}

/// Every capture of a round, in rotator order: the main view, extra rotations,
/// then the x-ray.
fn round_shots(
    round: &Round,
    out: &Path,
    run: &EvalRun,
    model: &str,
    store: &ArtStore,
) -> Result<Vec<Shot>> {
    let mut arts: Vec<(&Art, String)> = Vec::new();
    if let Some(shot) = &round.screenshot {
        arts.push((shot, "view 1".to_string()));
    }
    for (i, shot) in round.rotation_shots.iter().enumerate() {
        arts.push((shot, format!("view {}", i + 2)));
    }
    if let Some(shot) = &round.xray_shot {
        arts.push((shot, "x-ray".to_string()));
    }
    let mut shots = Vec::with_capacity(arts.len());
    // Scenario-nested rounds reuse round numbers, so the copied capture name
    // needs the scenario to stay unique within the model's asset dir.
    let scenario_prefix = round
        .scenario
        .as_deref()
        .map(|s| format!("{}_", model::sanitise_name(s)))
        .unwrap_or_default();
    for (art, label) in arts {
        let rel = Path::new("assets")
            .join(&run.name)
            .join(model)
            .join(format!(
                "{scenario_prefix}round_{}_{}",
                round.number, art.name
            ));
        let Some(src) = place_art(art, out, &rel)? else {
            continue;
        };
        // Reads the header only, not the pixels.
        let size = art
            .local
            .as_ref()
            .and_then(|path| image::image_dimensions(path).ok());
        // The first shot shows a derived copy and keeps the original one click
        // away in the lightbox. Later shots are already behind a click.
        let display = if shots.is_empty() {
            derived_or_edge(art, run, model, round.number, out, store)
                .unwrap_or_else(|| src.clone())
        } else {
            src.clone()
        };
        shots.push(Shot {
            src: display,
            full: src,
            label,
            size,
        });
    }
    Ok(shots)
}

/// "round 3/6", or "seed_4 round 3 · 5/8 parks" on a multi-scenario run: the
/// best-round cell also says where the best came from and how reliably the
/// model scored across parks.
fn best_label(run: &EvalRun, model: &ModelRun) -> String {
    let Some(best) = model.best() else {
        return String::new();
    };
    match &best.scenario {
        Some(scenario) => format!(
            "{scenario} round {} · {}/{} parks",
            best.number,
            model.scenarios_scored(&run.scenarios),
            run.scenarios.len()
        ),
        None => format!("round {}/{}", best.number, model.rounds.len()),
    }
}

fn standings(run: &EvalRun) -> Vec<StandingRow> {
    run.ranked()
        .iter()
        .enumerate()
        .map(|(i, model)| {
            let best = model.best();
            let ride = best.and_then(|r| r.ride.as_ref());
            let score = if run.scenarios.is_empty() {
                best.map(|r| format!("{:.2}", r.excitement()))
            } else {
                best.map(|_| format!("{:.2}", run.score_of(model)))
            };
            StandingRow {
                place: place_label(i + 1),
                model: model.model.clone(),
                is_winner: i == 0 && best.is_some(),
                score,
                intensity: fmt_opt(ride.and_then(|r| r.intensity)),
                nausea: fmt_opt(ride.and_then(|r| r.nausea)),
                similarity: fmt_opt(
                    best.and_then(|r| r.similarity.as_ref())
                        .map(|s| s.similarity),
                ),
                best_round: best_label(run, model),
                usage: usage_cell(model),
            }
        })
        .collect()
}

/// The page a run's model links to: `run-<run>-<model>.html`.
fn model_href(run: &EvalRun, model: &str) -> String {
    format!("run-{}-{}.html", run.name, model::sanitise_name(model))
}

/// One round's session trace lives on its own page: a long round runs to
/// hundreds of events, which would bury the ratings on the model page.
fn trace_href(run: &EvalRun, model: &str, round: &Round) -> String {
    let scenario = round
        .scenario
        .as_deref()
        .map(|s| format!("{}-", model::sanitise_name(s)))
        .unwrap_or_default();
    format!(
        "trace-{}-{}-{scenario}r{}.html",
        run.name,
        model::sanitise_name(model),
        round.number
    )
}

fn fmt_elapsed(ms: i64) -> String {
    let secs = ms.max(0) / 1000;
    format!("{}:{:02}", secs / 60, secs % 60)
}

fn fmt_json_compact(value: &serde_json::Value, limit: usize) -> Option<String> {
    let text = match value {
        serde_json::Value::Null => return None,
        serde_json::Value::String(s) => s.clone(),
        other => other.to_string(),
    };
    let text = text.trim();
    if text.is_empty() || text == "{}" {
        return None;
    }
    Some(clamp(text, limit))
}

/// Long tool payloads are truncated: a `valid_next_pieces` result runs to
/// kilobytes and the interesting part is always at the front.
fn clamp(text: &str, limit: usize) -> String {
    if text.chars().count() <= limit {
        return text.to_string();
    }
    let head: String = text.chars().take(limit).collect();
    format!("{head}… ({} chars)", text.chars().count())
}

fn build_trace_page(
    run: &EvalRun,
    model: &ModelRun,
    round: &Round,
    base_url: Option<&str>,
    out: &Path,
) -> Result<()> {
    let tools = round.trace.iter().filter(|e| e.is_tool()).count();
    let rejected = round.trace.iter().filter(|e| e.is_rejection()).count();
    let elapsed = round.trace.iter().filter_map(|e| e.ms).max().unwrap_or(0);
    let cost: f64 = round.trace.iter().filter_map(|e| e.cost_usd).sum();

    let mut summary = vec![
        view::TraceSummary {
            label: "events".into(),
            value: round.trace.len().to_string(),
            class: String::new(),
        },
        view::TraceSummary {
            label: "tool calls".into(),
            value: tools.to_string(),
            class: String::new(),
        },
        view::TraceSummary {
            label: "rejected".into(),
            value: rejected.to_string(),
            class: if rejected > 0 { "fail" } else { "dim" }.into(),
        },
        view::TraceSummary {
            label: "session".into(),
            value: fmt_elapsed(elapsed),
            class: String::new(),
        },
    ];
    if cost > 0.0 {
        summary.push(view::TraceSummary {
            label: "cost".into(),
            value: format!("${cost:.2}"),
            class: "usage".into(),
        });
    }

    let rows = round
        .trace
        .iter()
        .map(|event| view::TraceRow {
            kind: event.kind.clone(),
            at: event.ms.map(fmt_elapsed).unwrap_or_default(),
            text: event.text.as_ref().map(|t| clamp(t, 4000)),
            tool: event.name.clone(),
            input: event.input.as_ref().and_then(|v| fmt_json_compact(v, 1200)),
            output: event
                .output
                .as_ref()
                .and_then(|v| fmt_json_compact(v, 1200)),
            failed: event.is_rejection(),
            duration: event.dur_ms.map(|d| format!("{d} ms")),
            cost: event
                .cost_usd
                .filter(|c| *c > 0.0)
                .map(|c| format!("${c:.4}")),
        })
        .collect();

    let path = trace_href(run, &model.model, round);
    let page = view::TracePage {
        chrome: Chrome::new(
            &format!(
                "CoasterBench — {} · {} · round {}",
                run.name, model.model, round.number
            ),
            &format!("TRACE · ROUND {}", round.number),
            &path,
            base_url,
        )
        .width(view::Width::Mid),
        run_name: run.name.clone(),
        run_href: format!("run-{}.html", run.name),
        model: model.model.clone(),
        model_href: model_href(run, &model.model),
        round: round.number,
        summary,
        rows,
    };
    write_page(&out.join(&path), &page.render()?)
}

/// Builds one model's view (chart, studied designs, every round). Copies the
/// round captures into the site as a side effect, so it runs once per model
/// and both the run comparison and the model detail page render from it.
fn build_model_view(
    run: &EvalRun,
    model: &ModelRun,
    store: &ArtStore,
    out: &Path,
) -> Result<ModelView> {
    let studied: Vec<(String, String)> = model
        .studied_designs()
        .into_iter()
        .map(|name| (name.clone(), name))
        .collect();
    let mut rounds = Vec::with_capacity(model.rounds.len());
    for (i, round) in model.rounds.iter().enumerate() {
        let previous = i.checked_sub(1).map(|p| model.rounds[p].pieces.as_slice());
        let shots = round_shots(round, out, run, &model.model, store)?;
        let stats = round_stats(round);
        rounds.push(RoundView {
            number: round.number,
            scenario: round.scenario.clone(),
            trace_href: (!round.trace.is_empty()).then(|| trace_href(run, &model.model, round)),
            trace_events: round.trace.len(),
            trace_rejections: round.trace.iter().filter(|e| e.is_rejection()).count(),
            badge: round_badge(round, stats.as_ref()),
            circuit: circuit_badge(round.ride.as_ref().and_then(|r| r.circuit.as_ref())),
            presentation: presentation_view(round.presentation.as_ref()),
            build_error: round.build_error.clone(),
            unrated_note: stats.is_none() && round.build_error.is_none(),
            stats,
            lookups: lookups_line(round),
            program_json: round.program_json.clone(),
            program_pieces: round.program_pieces,
            dna: dna::profile_svg(
                &round.pieces,
                previous,
                &format!(
                    "Elevation profile of {}'s round {} track, {} pieces",
                    model.model,
                    round.number,
                    round.pieces.len()
                ),
            ),
            dna_caption: dna_caption(&round.pieces, previous),
            replay: round_asset(round.replay.as_ref(), out, run, &model.model, round.number)?,
            park: round_asset(round.park.as_ref(), out, run, &model.model, round.number)?,
            replay_poster: poster(round, out, run, &model.model, store)?,
            // A clip cut at the cap jumps rather than loops.
            replay_loops: round.replay_looped.unwrap_or(true),
            shots_json: serde_json::to_string(&shots.iter().map(|s| &s.src).collect::<Vec<_>>())?,
            fulls_json: serde_json::to_string(&shots.iter().map(|s| &s.full).collect::<Vec<_>>())?,
            labels_json: serde_json::to_string(
                &shots.iter().map(|s| &s.label).collect::<Vec<_>>(),
            )?,
            shots,
        });
    }
    Ok(ModelView {
        model: model.model.clone(),
        href: model_href(run, &model.model),
        chart_svg: chart::round_chart(model),
        studied: figures(&studied, store, out)?,
        rounds,
    })
}

/// The line under a profile: what the track is, then what changed.
fn dna_caption(pieces: &[dna::Piece], previous: Option<&[dna::Piece]>) -> String {
    if pieces.is_empty() {
        return String::new();
    }
    let summary = dna::summarise(pieces);
    let mut parts = vec![format!("{} pieces", summary.pieces)];
    if summary.lift_pieces > 0 {
        parts.push(format!("{} on the lift", summary.lift_pieces));
    }
    if summary.inversions > 0 {
        parts.push(format!(
            "{} inversion{}",
            summary.inversions,
            if summary.inversions == 1 { "" } else { "s" }
        ));
    }
    if let Some(diff) = dna::diff_line(pieces, previous) {
        parts.push(diff);
    }
    parts.join(" · ")
}

/// The headline numbers at the top of a model detail page.
fn model_stats(run: &EvalRun, model: &ModelRun) -> Vec<Stat> {
    let stat = |label: &str, value: String, class: &str| Stat {
        label: label.to_string(),
        value,
        class: class.to_string(),
    };
    let best = model.best();
    let ride = best.and_then(|r| r.ride.as_ref());
    let rated = model.rounds.iter().filter(|r| r.excitement() > 0.0).count();
    let multi = !run.scenarios.is_empty();
    let mut stats = vec![
        stat(
            if multi { "mean best score" } else { "score" },
            best.map_or_else(
                || "—".to_string(),
                |_| format!("{:.2}", run.score_of(model)),
            ),
            "rating-excitement",
        ),
        stat(
            "intensity",
            fmt_opt(ride.and_then(|r| r.intensity)),
            "rating-intensity",
        ),
        stat(
            "nausea",
            fmt_opt(ride.and_then(|r| r.nausea)),
            "rating-nausea",
        ),
        stat(
            "best round",
            best.map_or_else(
                || "—".to_string(),
                |r| match &r.scenario {
                    Some(scenario) => format!("{scenario} r{}", r.number),
                    None => format!("{}", r.number),
                },
            ),
            "",
        ),
        stat(
            "rated rounds",
            format!("{rated}/{}", model.rounds.len()),
            "",
        ),
    ];
    if multi {
        stats.push(stat(
            "parks scored",
            format!(
                "{}/{}",
                model.scenarios_scored(&run.scenarios),
                run.scenarios.len()
            ),
            "",
        ));
        if let Some(best) = best {
            stats.push(stat(
                "best park score",
                format!("{:.2}", best.excitement()),
                "rating-excitement",
            ));
        }
    }
    if let Some(sim) = best.and_then(|r| r.similarity.as_ref()) {
        stats.push(stat("similarity", format!("{:.2}", sim.similarity), "dim"));
    }
    stats.push(stat("tokens / cost", usage_cell(model), "usage"));
    stats
}

fn build_model_page(
    run: &EvalRun,
    view: &ModelView,
    place: usize,
    base_url: Option<&str>,
    store: &ArtStore,
    out: &Path,
) -> Result<()> {
    let model = run
        .models
        .iter()
        .find(|m| m.model == view.model)
        .context("model view without a model")?;
    let path = model_href(run, &view.model);
    let card = write_page_card(
        model_best_shot(model),
        store,
        out,
        &format!("og-{}", path.replace(".html", ".jpg")),
    )?;
    let chrome = Chrome::new(
        &format!("CoasterBench — {} · {}", run.name, view.model),
        &format!("{} · {}", view.model.to_uppercase(), run.name),
        &path,
        base_url,
    )
    .width(view::Width::Wide)
    .maybe_og_card(card.as_deref());
    let page = ModelPage {
        chrome,
        run_name: run.name.clone(),
        run_href: format!("run-{}.html", run.name),
        place: place_label(place),
        of_models: run.models.len(),
        context: format!(
            "{} · {} · {}{} · {}",
            run.mode_label(),
            run.ride_name(),
            run.harness,
            if run.scenarios.is_empty() {
                String::new()
            } else {
                format!(" · {} parks", run.scenarios.len())
            },
            view::mode_tagline(if run.open_note {
                "open note"
            } else {
                run.base_mode()
            })
        ),
        stats: model_stats(run, model),
        model: view,
    };
    write_page(&out.join(&path), &page.render()?)
}

fn build_run_page(
    run: &EvalRun,
    store: &ArtStore,
    out: &Path,
    base_url: Option<&str>,
) -> Result<()> {
    let mut models = Vec::with_capacity(run.models.len());
    for model in run.ranked() {
        models.push(build_model_view(run, model, store, out)?);
    }
    for (i, view) in models.iter().enumerate() {
        build_model_page(run, view, i + 1, base_url, store, out)?;
    }
    for model in &run.models {
        for round in model.rounds.iter().filter(|r| !r.trace.is_empty()) {
            build_trace_page(run, model, round, base_url, out)?;
        }
    }

    let path = format!("run-{}.html", run.name);
    let card = write_page_card(
        run_best_shot(run).and_then(model_best_shot),
        store,
        out,
        &format!("og-run-{}.jpg", run.name),
    )?;
    let chrome = Chrome::new(
        &format!("CoasterBench — {}", run.name),
        &format!("RUN {} ({})", run.name, run.mode_label()),
        &path,
        base_url,
    )
    .width(view::Width::Wide)
    .maybe_og_card(card.as_deref());
    let page = RunPage {
        chrome,
        mode_tagline: view::mode_tagline(if run.open_note {
            "open note"
        } else {
            run.base_mode()
        }),
        grace: format!("{}", run.grace),
        standings: standings(run),
        models,
    };
    write_page(&out.join(&path), &page.render()?)
}

fn write_page(path: &Path, html: &str) -> Result<()> {
    std::fs::write(path, html).with_context(|| format!("writing {}", path.display()))
}

/// Writes the thumbnail standing for a model's whole run (its best-rated
/// coaster) into the site, and returns the src. None when it has no art.
fn model_thumb(
    model: &ModelRun,
    store: &ArtStore,
    out: &Path,
    run: &EvalRun,
) -> Result<Option<String>> {
    let Some(src) = model_best_shot(model).and_then(|shot| store.pixels(shot)) else {
        return Ok(None);
    };
    let rel = Path::new("assets").join("thumbs").join(format!(
        "{}-{}.jpg",
        run.name,
        model::sanitise_name(&model.model)
    ));
    images::write_thumbnail(&src, &out.join(&rel))?;
    Ok(Some(rel.to_string_lossy().replace('\\', "/")))
}

/// The scenario-count facet value: "single", or "N parks" for a
/// multi-scenario run.
fn parks_label(run: &EvalRun) -> String {
    if run.scenarios.is_empty() {
        "single".to_string()
    } else {
        format!("{} parks", run.scenarios.len())
    }
}

/// Index rows: one per model per run, best score first (the leaderboard
/// question is "who built the best coaster", not "what ran most recently").
/// The client-side sorter can reorder by any column from here.
fn index_rows(runs: &[EvalRun], store: &ArtStore, out: &Path) -> Result<Vec<IndexRow>> {
    let mut rows = Vec::new();
    for run in runs {
        for (i, model) in run.ranked().iter().enumerate() {
            let best = model.best();
            let ride = best.and_then(|r| r.ride.as_ref());
            rows.push(IndexRow {
                run_name: run.name.clone(),
                run_href: format!("run-{}.html", run.name),
                model_href: model_href(run, &model.model),
                date: run.date(),
                mode: run.mode_label(),
                coaster: run.ride_name(),
                harness: run.harness.clone(),
                parks: parks_label(run),
                model: model.model.clone(),
                thumb: model_thumb(model, store, out, run)?,
                place: place_label(i + 1),
                is_winner: i == 0 && best.is_some(),
                sort_score: run.score_of(model),
                sort_intensity: ride.and_then(|r| r.intensity).unwrap_or(0.0),
                sort_nausea: ride.and_then(|r| r.nausea).unwrap_or(0.0),
                score: best.map(|_| format!("{:.2}", run.score_of(model))),
                score_pct: String::new(), // filled in below, once the field's best is known

                intensity: fmt_opt(ride.and_then(|r| r.intensity)),
                nausea: fmt_opt(ride.and_then(|r| r.nausea)),
                best_round: best_label(run, model),
                usage: usage_cell(model),
            });
        }
    }
    rows.sort_by(|a, b| b.sort_score.total_cmp(&a.sort_score));
    Ok(rows)
}

/// The index's headline: the best-scoring steel twister with a replay. Twister
/// only, because scores across ride types are not one league (a library-mode
/// wooden coaster outrates every twister without answering the same question).
fn featured(runs: &[EvalRun], store: &ArtStore, out: &Path) -> Result<Option<view::Featured>> {
    const TWISTER: i64 = 51;
    let best = runs
        .iter()
        .filter(|run| run.ride_type == TWISTER)
        .flat_map(|run| run.models.iter().map(move |model| (run, model)))
        .flat_map(|(run, model)| {
            model
                .rounds
                .iter()
                .filter(|round| round.replay.is_some() && round.excitement() > 0.0)
                .map(move |round| (run, model, round))
        })
        .max_by(|a, b| a.2.excitement().total_cmp(&b.2.excitement()));
    let Some((run, model, round)) = best else {
        return Ok(None);
    };

    // The same paths the model page uses, so the hero shares its files.
    let asset = |art: Option<&Art>| round_asset(art, out, run, &model.model, round.number);
    let Some(replay) = asset(round.replay.as_ref())? else {
        return Ok(None);
    };
    // The replay's own still: park screenshots frame the whole map.
    let poster = poster(round, out, run, &model.model, store)?;

    let ride = round.ride.as_ref();
    let stat = |label: &str, value: String, class: &str| Stat {
        label: label.to_string(),
        value,
        class: class.to_string(),
    };
    let stats = vec![
        stat(
            "excitement",
            format!("{:.2}", round.excitement()),
            "rating-excitement",
        ),
        stat(
            "intensity",
            fmt_opt(ride.and_then(|r| r.intensity)),
            "rating-intensity",
        ),
        stat(
            "nausea",
            fmt_opt(ride.and_then(|r| r.nausea)),
            "rating-nausea",
        ),
        stat(
            "length",
            ride.map_or_else(|| "—".to_string(), |r| format!("{} m", r.ride_length)),
            "",
        ),
        stat(
            "drops",
            ride.map_or_else(|| "—".to_string(), |r| r.num_drops.to_string()),
            "",
        ),
        stat(
            "airtime",
            ride.map_or_else(|| "—".to_string(), |r| r.total_air_time.to_string()),
            "",
        ),
        stat("pieces", round.program_pieces.to_string(), ""),
    ];

    Ok(Some(view::Featured {
        name: round.presentation.as_ref().map(|p| p.name.clone()),
        model: model.model.clone(),
        model_href: model_href(run, &model.model),
        replay,
        poster,
        loops: round.replay_looped.unwrap_or(true),
        alt: format!(
            "A lap of {}the {} {} designed by {} in round {} of {}, rated {:.2} excitement by the game.",
            round
                .presentation
                .as_ref()
                .map(|p| format!("{} — ", p.name))
                .unwrap_or_default(),
            run.mode_label(),
            run.ride_name(),
            model.model,
            round.number,
            run.name,
            round.excitement(),
        ),
        stats,
    }))
}

/// The leaderboard, split by what the agent was allowed to do and then by
/// coaster.
///
/// One table would rank a model that searched the stock design library, and one
/// that read the ratings source, above every model that worked blind — and rank
/// a 6 km wooden coaster against a twister while it was at it. Neither is a
/// comparison anyone should draw, so the page does not offer it.
fn index_boards(rows: Vec<IndexRow>) -> Vec<IndexBoard> {
    // Blind design first: it is the benchmark. The rest are their own
    // experiments, and say so.
    let order = [
        "design",
        "library",
        "design + open note",
        "library + open note",
    ];
    let mut boards: Vec<IndexBoard> = Vec::new();
    let mut conditions: Vec<String> = rows.iter().map(|r| r.mode.clone()).collect();
    conditions.sort_by_key(|c| order.iter().position(|o| o == c).unwrap_or(usize::MAX));
    conditions.dedup();

    for condition in conditions {
        let mine: Vec<&IndexRow> = rows.iter().filter(|r| r.mode == condition).collect();
        let mut coasters: Vec<String> = mine.iter().map(|r| r.coaster.clone()).collect();
        coasters.sort();
        coasters.dedup();
        let labelled = coasters.len() > 1;
        let groups = coasters
            .into_iter()
            .map(|coaster| {
                let mut rows: Vec<IndexRow> = mine
                    .iter()
                    .filter(|r| r.coaster == coaster)
                    .map(|r| (*r).clone())
                    .collect();
                // The meter is magnitude within its own table: scaled across
                // boards, a board's own leader would show a part-full bar
                // against a score nobody here was competing with.
                let top = rows.first().map_or(0.0, |r| r.sort_score);
                for row in &mut rows {
                    row.score_pct = if top > 0.0 {
                        format!("{:.1}", (row.sort_score / top * 100.0).clamp(0.0, 100.0))
                    } else {
                        "0".to_string()
                    };
                }
                IndexGroup {
                    rows,
                    coaster,
                    labelled,
                }
            })
            .collect();
        boards.push(IndexBoard {
            headline: condition == "design",
            tagline: view::mode_tagline(if condition.contains("open note") {
                "open note"
            } else {
                condition.split(" + ").next().unwrap_or(&condition)
            }),
            condition,
            groups,
        });
    }
    boards
}

/// Every finished model run, as head-to-head contenders. Draws from all runs,
/// not just the ones shown on the leaderboard: a model that ran its full round
/// count inside an otherwise-abandoned run is a valid opponent (that is the
/// only way "kimi vs fable" works, since fable's twister run was hidden by its
/// missing rivals). Generates each contender's thumbnail as a side effect.
fn contenders(runs: &[EvalRun], store: &ArtStore, out: &Path) -> Result<Vec<view::Contender>> {
    let mut contenders = Vec::new();
    for run in runs {
        for model in &run.models {
            if !run.model_completed(model) {
                continue;
            }
            let best = model.best();
            let ride = best.and_then(|r| r.ride.as_ref());
            let totals = model.usage_totals();
            let href = model_href(run, &model.model);
            contenders.push(view::Contender {
                id: href.trim_end_matches(".html").to_string(),
                href,
                run: run.name.clone(),
                date: run.date(),
                model: model.model.clone(),
                coaster: run.ride_name(),
                // Half the compare scenario key: open note must not fight black box.
                mode: run.mode_label(),
                harness: run.harness.clone(),
                thumb: model_thumb(model, store, out, run)?,
                score: best.map(|_| run.score_of(model)),
                intensity: ride.and_then(|r| r.intensity),
                nausea: ride.and_then(|r| r.nausea),
                similarity: best
                    .and_then(|r| r.similarity.as_ref())
                    .map(|s| s.similarity),
                ride_length: ride.map(|r| r.ride_length),
                airtime: ride.map(|r| r.total_air_time),
                drops: ride.map(|r| r.num_drops),
                best_round: best.map(|r| r.number),
                rounds: model.rounds.len(),
                rated_rounds: model.rounds.iter().filter(|r| r.excitement() > 0.0).count(),
                tokens: totals.tokens_in + totals.tokens_out,
                cost: totals.cost_usd,
                round_scores: model.rounds.iter().map(|r| r.excitement()).collect(),
            });
        }
    }
    // Group by coaster then best-first, so the dropdown reads top-down within
    // each coaster and the default pairing is the tightest fight available.
    contenders.sort_by(|a, b| {
        a.coaster.cmp(&b.coaster).then(
            b.score
                .partial_cmp(&a.score)
                .unwrap_or(std::cmp::Ordering::Equal),
        )
    });
    Ok(contenders)
}

/// Default matchup: the top two contenders of the most-contested matchup
/// class, so the page opens on a real fight rather than an empty picker. A
/// matchup class is a (coaster, mode) pair — the picker's same-class rule.
/// (Not to be confused with scenario parks, which multi-scenario runs play
/// on; the head-to-head compares like coaster types regardless of park.)
fn default_pair(contenders: &[view::Contender]) -> (String, String) {
    let mut by_matchup: std::collections::HashMap<(&str, &str), Vec<&view::Contender>> =
        std::collections::HashMap::new();
    for c in contenders {
        by_matchup.entry((&c.coaster, &c.mode)).or_default().push(c);
    }
    let best = by_matchup
        .values()
        .filter(|group| group.len() >= 2)
        .max_by_key(|group| group.len());
    match best {
        // Groups are already score-sorted within a class by contenders().
        Some(group) => (group[0].id.clone(), group[1].id.clone()),
        None => {
            let id = |i: usize| contenders.get(i).map(|c| c.id.clone()).unwrap_or_default();
            (id(0), id(1))
        }
    }
}

/// The filename stem shared by a matchup's permalink page and its card, so the
/// Rust side and site.js agree on the URL. Ids are already file-safe.
fn pair_slug(a_id: &str, b_id: &str) -> String {
    format!("{a_id}.vs.{b_id}")
}

/// Draws a matchup card (a on the left, b on the right, winner badged gold) to
/// `filename`, reusing the contenders' thumbnails. None when either lacks art.
fn write_matchup_card(
    a: &view::Contender,
    b: &view::Contender,
    out: &Path,
    filename: &str,
) -> Result<Option<String>> {
    let (Some(la), Some(rp)) = (a.thumb.as_ref(), b.thumb.as_ref()) else {
        return Ok(None);
    };
    // Crown the higher score. A tie, or two unrated coasters, crowns nobody.
    let (a_wins, b_wins) = match (a.score, b.score) {
        (Some(x), Some(y)) if x > y => (true, false),
        (Some(x), Some(y)) if y > x => (false, true),
        (Some(_), None) => (true, false),
        (None, Some(_)) => (false, true),
        _ => (false, false),
    };
    images::write_compare_card(
        &a.coaster,
        &a.mode,
        &images::CardSide {
            shot: &out.join(la),
            model: &a.model,
            score: a.score,
            winner: a_wins,
        },
        &images::CardSide {
            shot: &out.join(rp),
            model: &b.model,
            score: b.score,
            winner: b_wins,
        },
        &out.join(filename),
    )?;
    Ok(Some(filename.to_string()))
}

/// One compare page defaulted to a pair, with its own card so a shared link
/// previews that fight. Every variant embeds the same contender list, so the
/// picker works identically from any of them.
fn write_compare_variant(
    contenders: &[view::Contender],
    json: &str,
    a: &view::Contender,
    b: &view::Contender,
    page_path: &str,
    base_url: Option<&str>,
    out: &Path,
) -> Result<()> {
    let card = write_matchup_card(
        a,
        b,
        out,
        &format!("og-{page_path}").replace(".html", ".jpg"),
    )?;
    let chrome = Chrome::new(
        &format!("CoasterBench — {} vs {}", a.model, b.model),
        "HEAD TO HEAD",
        page_path,
        base_url,
    )
    .width(view::Width::Mid)
    .description(&format!(
        "{} vs {} — {} · {} mode. Head-to-head on CoasterBench.",
        a.model, b.model, a.coaster, a.mode
    ))
    .maybe_og_card(card.as_deref());
    write_page(
        &out.join(page_path),
        &view::ComparePage {
            chrome,
            contenders_json: json.to_string(),
            default_a: a.id.clone(),
            default_b: b.id.clone(),
            count: contenders.len(),
        }
        .render()?,
    )
}

fn build_compare_page(
    contenders: &[view::Contender],
    base_url: Option<&str>,
    out: &Path,
) -> Result<()> {
    let json = serde_json::to_string(contenders)?;
    let by_id = |id: &str| contenders.iter().find(|c| c.id == id);

    // The hub page: the picker itself, defaulted to the best matchup available.
    // Titled generically (it is the entry point, not one fight) but carded with
    // its default pair.
    let (default_a, default_b) = default_pair(contenders);
    let hub_card = match (by_id(&default_a), by_id(&default_b)) {
        (Some(a), Some(b)) => write_matchup_card(a, b, out, "og-compare.jpg")?,
        _ => None,
    };
    write_page(
        &out.join("compare.html"),
        &view::ComparePage {
            chrome: Chrome::new(
                "CoasterBench — Head to Head",
                "HEAD TO HEAD",
                "compare.html",
                base_url,
            )
            .width(view::Width::Mid)
            .maybe_og_card(hub_card.as_deref()),
            contenders_json: json.clone(),
            default_a,
            default_b,
            count: contenders.len(),
        }
        .render()?,
    )?;

    // A permalink page + card for every ordered same-class matchup (same
    // coaster and mode), so a shared `compare-<a>.vs.<b>.html` unfurls with
    // that exact pairing. The picker only allows same-class fights, so those
    // are the only reachable deep links. Ordered (a left, b right) to match
    // the URL and the swap.
    let matchup_key = |c: &view::Contender| (c.coaster.clone(), c.mode.clone());
    for a in contenders {
        for b in contenders {
            if a.id == b.id || matchup_key(a) != matchup_key(b) {
                continue;
            }
            let path = format!("compare-{}.html", pair_slug(&a.id, &b.id));
            write_compare_variant(contenders, &json, a, b, &path, base_url, out)?;
        }
    }
    Ok(())
}

fn facets(runs: &[EvalRun]) -> Vec<Facet> {
    let collect = |name: &str, mut values: Vec<String>| {
        values.sort();
        values.dedup();
        Facet {
            name: name.to_string(),
            values,
        }
    };
    vec![
        collect("coaster", runs.iter().map(|r| r.ride_name()).collect()),
        collect("harness", runs.iter().map(|r| r.harness.clone()).collect()),
        collect("parks", runs.iter().map(parks_label).collect()),
        collect(
            "model",
            runs.iter()
                .flat_map(|r| r.models.iter().map(|m| m.model.clone()))
                .collect(),
        ),
    ]
}

/// The best-rated screenshot across every run, for the shared unfurl card.
fn og_shot(runs: &[EvalRun]) -> Option<&Art> {
    runs.iter()
        .filter_map(run_best_shot)
        .max_by(|a, b| shot_score(a).total_cmp(&shot_score(b)))
        .and_then(model_best_shot)
}

/// A model's best-rated screenshot, falling back to any it produced so an
/// all-unrated run still gets a preview.
fn model_best_shot(model: &model::ModelRun) -> Option<&Art> {
    model
        .best()
        .and_then(|r| r.screenshot.as_ref())
        .or_else(|| model.rounds.iter().find_map(|r| r.screenshot.as_ref()))
}

/// The excitement behind a model's best shot, for ranking runs against each
/// other; 0 when nothing rated.
fn shot_score(model: &model::ModelRun) -> f64 {
    model.best().map_or(0.0, |r| r.excitement())
}

/// A run's headline coaster: the highest-rated screenshot across its models.
fn run_best_shot(run: &EvalRun) -> Option<&model::ModelRun> {
    run.models
        .iter()
        .filter(|m| model_best_shot(m).is_some())
        .max_by(|a, b| shot_score(a).total_cmp(&shot_score(b)))
}

/// Renders `shot` into a per-page unfurl card and returns its site-relative
/// filename, or None when there is nothing to draw.
fn write_page_card(
    art: Option<&Art>,
    store: &ArtStore,
    out: &Path,
    name: &str,
) -> Result<Option<String>> {
    let Some(shot) = art.and_then(|a| store.pixels(a)) else {
        return Ok(None);
    };
    images::write_og_card(&shot, &out.join(name))?;
    Ok(Some(name.to_string()))
}

fn build_library_page(
    runs_dir: &Path,
    store: &ArtStore,
    out: &Path,
    base_url: Option<&str>,
) -> Result<()> {
    let mut index = model::latest_library_index(runs_dir)?;
    index.sort_by(|a, b| (a.ride_type, &a.name).cmp(&(b.ride_type, &b.name)));
    let designs: Vec<(String, String)> = if index.is_empty() {
        // No library.json yet: caption with the (sanitised) file names.
        store
            .preview_names()
            .into_iter()
            .map(|name| (name.clone(), name))
            .collect()
    } else {
        index
            .iter()
            .map(|d| {
                let caption = format!(
                    "{} · type {} · {} pieces",
                    d.name, d.ride_type, d.piece_count
                );
                (d.name.clone(), caption)
            })
            .collect()
    };
    let page = LibraryPage {
        chrome: Chrome::new(
            "CoasterBench — Track Design Library",
            "TRACK DESIGN LIBRARY",
            "library.html",
            base_url,
        ),
        figures: figures(&designs, store, out)?,
    };
    write_page(&out.join("library.html"), &page.render()?)
}

/// Empties the previous build: stale run pages and copied assets would
/// otherwise survive a renamed or deleted run.
fn clean_output(out: &Path) -> Result<()> {
    std::fs::create_dir_all(out)?;
    let assets = out.join("assets");
    if assets.is_dir() {
        std::fs::remove_dir_all(&assets)?;
    }
    // Drop every .html and every generated og-* card. The per-matchup
    // cards/pages are keyed on contender ids, so a removed run would otherwise
    // leave orphans; all live ones are rewritten this build.
    for entry in std::fs::read_dir(out)?.flatten() {
        let path = entry.path();
        let is_html = path.extension().is_some_and(|e| e == "html");
        let is_og_card = path
            .file_name()
            .and_then(|n| n.to_str())
            .is_some_and(|n| n.starts_with("og-") && (n.ends_with(".png") || n.ends_with(".jpg")));
        if is_html || is_og_card {
            std::fs::remove_file(path)?;
        }
    }
    Ok(())
}

fn main() -> Result<()> {
    let args = Args::parse();
    let base_url = (!args.base_url.is_empty()).then_some(args.base_url.as_str());

    let store = ArtStore::new(&args.artifact_base, &args.previews, &args.previews_manifest)?
        .remote_only(args.remote_artifacts)
        .edge_resize(args.cf_images);

    let all_runs = model::load_runs(&args.runs, &store)?;
    store.check_edge_resize(first_artifact_url(&all_runs).as_deref())?;
    let out = &args.out;
    clean_output(out)?;

    // The head-to-head draws from every finished model run, including complete
    // solo models inside runs the leaderboard hides, so build it before the
    // partial-run filter.
    // Image work first and in parallel: decoding is the expensive part.
    let jobs = derive_jobs(&all_runs, out);
    images::derive_all(&jobs)?;

    // A withdrawn run leaves the site entirely: no pages, no head-to-head, no
    // row. The record stays in the repo, which is the point of saying so in
    // run.json rather than deleting it.
    let (all_runs, withdrawn): (Vec<EvalRun>, Vec<EvalRun>) = all_runs
        .into_iter()
        .partition(|run| run.published || args.include_unpublished);
    for run in &withdrawn {
        eprintln!("run {} withdrawn (published: false)", run.name);
    }

    let contenders = contenders(&all_runs, &store, out)?;

    // A run that died (or is still going) half way through would show up as a
    // model losing badly; leave it out of the leaderboard and say so.
    let mut skipped = Vec::new();
    let mut runs = Vec::new();
    for run in all_runs {
        // Build every run's pages, so a head-to-head link into a run the
        // leaderboard hides still resolves instead of 404ing. The partial
        // filter only decides what the index lists, not what exists.
        build_run_page(&run, &store, out, base_url)?;
        match run.incomplete_reason() {
            Some(reason) if !args.include_partial => {
                eprintln!("partial run {} hidden from index ({reason})", run.name);
                skipped.push(run.name.clone());
            }
            _ => runs.push(run),
        }
    }

    let rows = index_rows(&runs, &store, out)?;
    let index = IndexPage {
        chrome: Chrome::new("CoasterBench", "COASTERBENCH", "index.html", base_url)
            .width(view::Width::Mid),
        featured: featured(&runs, &store, out)?,
        facets: facets(&runs),
        row_count: rows.len(),
        boards: index_boards(rows),
        have_previews: store.have_previews(),
        mode_taglines: view::MODE_TAGLINES
            .iter()
            .map(|(mode, tagline)| (mode.to_string(), tagline.to_string()))
            .collect(),
        skipped,
        withdrawn: withdrawn.iter().map(|run| run.name.clone()).collect(),
    };
    write_page(&out.join("index.html"), &index.render()?)?;

    if contenders.len() >= 2 {
        build_compare_page(&contenders, base_url, out)?;
    }
    if store.have_previews() {
        build_library_page(&args.runs, &store, out, base_url)?;
    }

    fonts::write(out)?;
    images::write_favicon(out)?;
    if let Some(shot) = og_shot(&runs).and_then(|art| store.pixels(art)) {
        images::write_og_card(&shot, &out.join("og-card.jpg"))?;
        println!("wrote og-card.jpg to {}", out.display());
    }
    if base_url.is_none() {
        eprintln!(
            "note: no --base-url given, og:image/og:url omitted (Slack unfurls will be text-only)"
        );
    }
    println!(
        "wrote {} page(s) for {} run(s) to {}",
        1 + runs.len(),
        runs.len(),
        out.display()
    );
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn contender(id: &str, mode: &str, score: f64) -> view::Contender {
        view::Contender {
            id: id.to_string(),
            href: format!("{id}.html"),
            run: "r".to_string(),
            date: "2026-07-25".to_string(),
            model: id.to_string(),
            coaster: "steel twister".to_string(),
            mode: mode.to_string(),
            harness: "claude-code".to_string(),
            thumb: None,
            score: Some(score),
            intensity: None,
            nausea: None,
            similarity: None,
            ride_length: None,
            airtime: None,
            drops: None,
            best_round: None,
            rounds: 6,
            rated_rounds: 6,
            tokens: 0.0,
            cost: Some(0.0),
            round_scores: vec![],
        }
    }

    #[test]
    fn open_note_never_headlines_a_fight_against_a_black_box_run() {
        // Higher scores, but open note reads the ratings code: not a fair fight.
        let cs = vec![
            contender("on-a", "design + open note", 9.9),
            contender("on-b", "design + open note", 9.8),
            contender("bb-a", "design", 7.0),
            contender("bb-b", "design", 6.9),
            contender("bb-c", "design", 6.8),
        ];
        let (a, b) = default_pair(&cs);
        let mode_of = |id: &String| {
            cs.iter()
                .find(|c| &c.id == id)
                .map(|c| c.mode.clone())
                .unwrap()
        };
        assert_eq!(
            mode_of(&a),
            mode_of(&b),
            "default matchup must stay inside one scenario"
        );
        assert_eq!(mode_of(&a), "design", "biggest scenario wins the headline");
    }

    #[test]
    fn circuit_badge_separates_clean_stranded_and_unclosed_track() {
        let circuit = |walked, total, orphans, looped| Circuit {
            walked_pieces: walked,
            total_pieces: total,
            orphan_pieces: orphans,
            looped,
        };

        let clean = circuit_badge(Some(&circuit(48, 48, 0, true))).expect("a verdict");
        assert_eq!(clean.class, "badge-ok");
        assert!(clean.text.contains("48"), "{}", clean.text);

        // The case the audit exists for: a rated ride whose screenshot also
        // shows track no train touches.
        let stranded = circuit_badge(Some(&circuit(40, 48, 8, true))).expect("a verdict");
        assert_eq!(stranded.class, "badge-warn");
        assert!(stranded.text.contains("8 of 48"), "{}", stranded.text);

        let unclosed = circuit_badge(Some(&circuit(14, 14, 0, false))).expect("a verdict");
        assert_eq!(unclosed.class, "badge-warn");
        assert!(unclosed.text.contains("never closed"), "{}", unclosed.text);

        assert!(
            circuit_badge(None).is_none(),
            "runs predating the audit claim nothing either way"
        );
    }

    #[test]
    fn presentation_becomes_safe_named_swatches() {
        let presentation = model::Presentation {
            name: "Retro Rocket".into(),
            track_color: "bright_red".into(),
            rail_color: "white".into(),
            support_color: "dark_blue".into(),
        };
        let view = presentation_view(Some(&presentation)).expect("presentation view");
        assert_eq!(view.name, "Retro Rocket");
        assert_eq!(view.track.name, "bright red");
        assert_eq!(view.track.class, "swatch-bright-red");
        assert_eq!(view.support.class, "swatch-dark-blue");
    }
}
