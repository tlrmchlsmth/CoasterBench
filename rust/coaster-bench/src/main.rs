//! CoasterBench orchestrator: runs a head-to-head eval where each contender is
//! a Claude Code session inside an OpenShell sandbox (personal-sub auth),
//! building interactively against the game's MCP server on this host.
//!
//!   coaster-bench --models claude-fable-5 claude-sonnet-5 --rounds 4 \
//!       --ride-type 51 --name sub-twister-1
//!
//! The game server is spawned as a child (openrct2-cli eval --serve) and the
//! orchestrator collects per-round artifacts (report.json, program.json,
//! park.png) in the same evals/runs/<name>/ layout driver.py produces, so
//! site.py needs no changes.

mod mcp;
mod prompt;
mod trace;

use std::fs::File;
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use clap::Parser;
use serde_json::{json, Value};

#[derive(Parser, Debug)]
#[command(version, about)]
struct Args {
    /// Contender model ids, one Claude Code session per round each.
    #[arg(long, num_args = 1.., default_values_t = vec!["claude-sonnet-5".to_string()])]
    models: Vec<String>,

    #[arg(long, default_value_t = 4)]
    rounds: u32,

    /// Competition ride type (51 steel twister, 52 wooden).
    #[arg(long, default_value_t = 51)]
    ride_type: u16,

    /// Test simulation budget passed to finish_and_test.
    #[arg(long, default_value_t = 25000)]
    ticks: u32,

    /// Run dir suffix: evals/runs/<yyyymmdd>-<name>.
    #[arg(long)]
    name: Option<String>,

    /// OpenShell sandbox that runs Claude Code (must reach this host's MCP port).
    #[arg(long, default_value = "coaster-sub")]
    sandbox: String,

    /// OpenShell sandbox for opencode contenders (OpenRouter lane).
    #[arg(long, default_value = "coaster-or")]
    opencode_sandbox: String,

    /// MCP port; must be allowed by the sandbox policy.
    #[arg(long, default_value_t = 8791)]
    port: u16,

    /// Max agentic turns per round session. Claude Code only; opencode has no
    /// equivalent, so --session-timeout is what bounds it.
    #[arg(long, default_value_t = 120)]
    max_turns: u32,

    /// Wall-clock budget per round session, in seconds. A session that runs
    /// past it is killed and the round is scored on whatever stands in the
    /// park. Without this an opencode session can iterate indefinitely.
    #[arg(long, default_value_t = 1800)]
    session_timeout: u64,

    #[arg(
        long,
        default_value = "~/rct2-assets/Scenarios/Build your own Six Flags Park.SC6"
    )]
    scenario: String,

    #[arg(long, default_value = "~/rct2-assets")]
    rct2_data: String,

    /// Reuse an already-running MCP server instead of spawning one.
    #[arg(long)]
    attach: bool,
}

fn expand_home(path: &str) -> PathBuf {
    if let Some(rest) = path.strip_prefix("~/") {
        if let Some(home) = std::env::var_os("HOME") {
            return PathBuf::from(home).join(rest);
        }
    }
    PathBuf::from(path)
}

fn repo_root() -> PathBuf {
    // target/{debug,release}/coaster-bench -> rust/coaster-bench -> repo
    let exe = std::env::current_exe().unwrap_or_default();
    exe.ancestors()
        .find(|p| p.join("src/openrct2").is_dir())
        .map(Path::to_path_buf)
        .unwrap_or_else(|| PathBuf::from("."))
}

struct GameServer {
    child: Option<Child>,
}

impl GameServer {
    fn spawn(args: &Args, root: &Path) -> Result<GameServer, String> {
        let cli = root.join("build/openrct2-cli");
        if !cli.is_file() {
            return Err(format!("{} not built", cli.display()));
        }
        let child = Command::new(cli)
            .arg("eval")
            .arg(expand_home(&args.scenario))
            .arg("--rct2-data-path")
            .arg(expand_home(&args.rct2_data))
            .arg("--serve")
            .arg(args.port.to_string())
            .arg("--serve-bind")
            .arg("0.0.0.0")
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .spawn()
            .map_err(|e| format!("spawn game server: {e}"))?;
        Ok(GameServer { child: Some(child) })
    }
}

impl Drop for GameServer {
    fn drop(&mut self) {
        if let Some(mut child) = self.child.take() {
            let _ = child.kill();
            let _ = child.wait();
        }
    }
}

fn wait_for_server(client: &mut mcp::McpClient, budget: Duration) -> Result<(), String> {
    let start = Instant::now();
    loop {
        match client.initialize() {
            Ok(()) => return Ok(()),
            Err(e) if start.elapsed() > budget => return Err(format!("server not ready: {e}")),
            Err(_) => std::thread::sleep(Duration::from_millis(500)),
        }
    }
}

/// A contender: which agent harness runs it and the model id that harness
/// understands. Spec syntax: bare name = Claude Code on the sub lane;
/// "opencode:<provider/model>" = opencode in the OpenRouter sandbox.
#[derive(Debug, Clone, PartialEq)]
struct Contender {
    harness: Harness,
    model: String,
    /// What the model can actually be fed. The MCP server hides tools that
    /// answer outside this set, because a single image content block sent to a
    /// text-only model fails the whole request upstream.
    modalities: Modalities,
}

/// Input modalities a model accepts, named after the OpenRouter catalogue's
/// `input_modalities` field, which is also the wire format the game's MCP
/// server parses out of `/mcp?modalities=...`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct Modalities(u8);

impl Modalities {
    const TEXT: Modalities = Modalities(1 << 0);
    const IMAGE: Modalities = Modalities(1 << 1);
    const AUDIO: Modalities = Modalities(1 << 2);
    const FILE: Modalities = Modalities(1 << 3);

    fn from_catalog_name(name: &str) -> Modalities {
        match name {
            "text" => Modalities::TEXT,
            "image" => Modalities::IMAGE,
            "audio" => Modalities::AUDIO,
            "file" => Modalities::FILE,
            _ => Modalities(0),
        }
    }

    fn contains(self, other: Modalities) -> bool {
        self.0 & other.0 == other.0
    }

    /// Wire form for the MCP query string, e.g. "text,image".
    fn as_query_value(self) -> String {
        [
            (Modalities::TEXT, "text"),
            (Modalities::IMAGE, "image"),
            (Modalities::AUDIO, "audio"),
            (Modalities::FILE, "file"),
        ]
        .iter()
        .filter(|(flag, _)| self.contains(*flag))
        .map(|(_, name)| *name)
        .collect::<Vec<_>>()
        .join(",")
    }
}

impl std::ops::BitOr for Modalities {
    type Output = Modalities;
    fn bitor(self, rhs: Modalities) -> Modalities {
        Modalities(self.0 | rhs.0)
    }
}

#[derive(Debug, Clone, Copy, PartialEq)]
enum Harness {
    ClaudeCode,
    Opencode,
}

impl Harness {
    fn name(self) -> &'static str {
        match self {
            Harness::ClaudeCode => "claude-code",
            Harness::Opencode => "opencode",
        }
    }
}

impl Contender {
    fn parse(spec: &str) -> Contender {
        match spec.split_once(':') {
            Some(("opencode", model)) => Contender {
                harness: Harness::Opencode,
                // Claude models are all multimodal; OpenRouter ones are not,
                // so ask the catalogue instead of assuming.
                modalities: openrouter_modalities(model)
                    .unwrap_or(Modalities::TEXT | Modalities::IMAGE),
                model: model.to_string(),
            },
            _ => Contender {
                harness: Harness::ClaudeCode,
                model: spec.to_string(),
                modalities: Modalities::TEXT | Modalities::IMAGE,
            },
        }
    }

    /// Directory / display name: the model id, filesystem-safe.
    fn display(&self) -> String {
        self.model.replace('/', "_")
    }

    /// Which OpenShell sandbox this contender's harness runs in.
    fn sandbox<'a>(&self, args: &'a Args) -> &'a str {
        match self.harness {
            Harness::ClaudeCode => &args.sandbox,
            Harness::Opencode => &args.opencode_sandbox,
        }
    }

    /// MCP endpoint this contender's harness should connect to. The server
    /// hides tools answering outside the advertised modality set, and refuses
    /// anyone whose lease is not the one that currently owns the park.
    fn mcp_url(&self, port: u16, lease: &str) -> String {
        format!(
            "http://host.containers.internal:{port}/mcp?modalities={}&lease={lease}",
            self.modalities.as_query_value()
        )
    }
}

/// Input modalities an OpenRouter model declares. `None` when the catalogue
/// can't be reached or doesn't list the model, so callers pick their own
/// default. The opencode model id carries a provider prefix
/// ("openrouter/author/model") that the catalogue ids don't have.
fn openrouter_modalities(model: &str) -> Option<Modalities> {
    let id = model.strip_prefix("openrouter/").unwrap_or(model);
    let catalog: Value = ureq::get("https://openrouter.ai/api/v1/models")
        .timeout(Duration::from_secs(30))
        .call()
        .ok()?
        .into_json()
        .ok()?;
    let declared = catalog
        .get("data")?
        .as_array()?
        .iter()
        .find(|m| m.get("id").and_then(Value::as_str) == Some(id))?
        .pointer("/architecture/input_modalities")?
        .as_array()?
        .iter()
        .filter_map(Value::as_str)
        .fold(Modalities(0), |set, name| {
            set | Modalities::from_catalog_name(name)
        });
    Some(declared)
}

/// Token/cost accounting for one agent session; every field best-effort.
struct SessionResult {
    usage: Value,
    error: Option<String>,
}

/// Current spend (USD) on the OpenRouter key, for cost-by-delta accounting.
fn openrouter_spend() -> Option<f64> {
    let key = Command::new("security")
        .args(["find-generic-password", "-s", "openrouter-api-key", "-w"])
        .output()
        .ok()?;
    let key = String::from_utf8_lossy(&key.stdout).trim().to_string();
    if key.is_empty() {
        return None;
    }
    let response: Value = ureq::get("https://openrouter.ai/api/v1/key")
        .set("Authorization", &format!("Bearer {key}"))
        .timeout(Duration::from_secs(30))
        .call()
        .ok()?
        .into_json()
        .ok()?;
    response.pointer("/data/usage").and_then(Value::as_f64)
}

/// Point the opencode sandbox at this contender's MCP endpoint. Written fresh
/// per session so a mixed vision/text-only lineup can share one sandbox.
fn write_opencode_config(args: &Args, contender: &Contender, lease: &str) -> Result<(), String> {
    let config = json!({
        "$schema": "https://opencode.ai/config.json",
        "model": contender.model,
        "mcp": {
            "coaster": {"type": "remote", "url": contender.mcp_url(args.port, lease), "enabled": true}
        }
    })
    .to_string();
    let script = format!(
        "mkdir -p ~/.config/opencode && cat > ~/.config/opencode/opencode.json <<'OPENCODE_EOF'\n{config}\nOPENCODE_EOF"
    );
    let output = Command::new("openshell")
        .args(["sandbox", "exec", "-n", &args.opencode_sandbox, "--"])
        .args(["sh", "-c", &script])
        .output()
        .map_err(|e| format!("write opencode config: {e}"))?;
    if !output.status.success() {
        return Err(format!(
            "write opencode config ({}): {}",
            output.status,
            String::from_utf8_lossy(&output.stderr).trim()
        ));
    }
    Ok(())
}

/// Kill agent processes left behind in a sandbox. Killing the local openshell
/// client does NOT stop the process it started inside the sandbox, so a run
/// that dies (Ctrl-C, `pkill`, a timeout) strands an agent that keeps building
/// in the shared park and keeps spending. Two of those once fought over the
/// same ride for an hour, each demolishing the other's track.
///
/// The sandbox image has no ps/pkill, so this walks /proc and matches the
/// executable rather than the command line: the command line of this very
/// script contains "opencode", and matching that would kill the cleanup shell.
/// It must stay a single line; openshell's exec drops multi-line scripts.
fn kill_stray_agents(sandbox: &str) {
    let script = "for pid in $(ls /proc | grep \"^[0-9]\"); do [ \"$pid\" = \"$$\" ] && continue; \
                  exe=$(readlink /proc/$pid/exe 2>/dev/null); \
                  case \"$exe\" in *opencode*|*claude*) kill -TERM \"$pid\" ;; esac; done";
    let _ = Command::new("openshell")
        .args(["sandbox", "exec", "-n", sandbox, "--"])
        .args(["sh", "-c", script])
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .status();
}

/// One agent session in the harness's sandbox. Success is judged by game
/// state, not the transcript; usage is captured best-effort either way.
///
/// Output streams to `session.log` / `session.err` in the round directory as
/// the session runs, so a round in progress can be tailed, and a round that
/// went wrong can be read afterwards. Files, not pipes: a session can outrun a
/// pipe buffer and deadlock while nobody is reading it.
fn run_agent_session(
    args: &Args,
    contender: &Contender,
    prompt: &str,
    round_dir: &Path,
    lease: &str,
) -> SessionResult {
    let mut cmd = Command::new("openshell");
    let spend_before = match contender.harness {
        Harness::Opencode => openrouter_spend(),
        Harness::ClaudeCode => None,
    };
    match contender.harness {
        Harness::ClaudeCode => {
            let mcp_config = json!({
                "mcpServers": {
                    "coaster": {"type": "http", "url": contender.mcp_url(args.port, lease)}
                }
            });
            cmd.args(["sandbox", "exec", "-n", &args.sandbox, "--"])
                .args(["claude", "--model", &contender.model, "-p", prompt])
                .args(["--mcp-config", &mcp_config.to_string()])
                .args(["--allowedTools", "mcp__coaster__*"])
                .args(["--max-turns", &args.max_turns.to_string()])
                // Event stream, not a summary: the round's trace is built from it.
                .args(["--output-format", "stream-json", "--verbose"]);
        }
        Harness::Opencode => {
            // opencode reads its MCP server list from a config file inside the
            // sandbox, so the per-contender endpoint has to be written there
            // before the session starts. Model comes fully qualified on the
            // command line (e.g. openrouter/poolside/...).
            if let Err(e) = write_opencode_config(args, contender, lease) {
                return SessionResult {
                    usage: Value::Null,
                    error: Some(e),
                };
            }
            cmd.args(["sandbox", "exec", "-n", &args.opencode_sandbox, "--"])
                .args(["opencode", "run", prompt, "-m", &contender.model])
                .args(["--format", "json"]);
        }
    }
    let log_path = round_dir.join("session.log");
    let err_path = round_dir.join("session.err");
    let (log, errlog) = match (File::create(&log_path), File::create(&err_path)) {
        (Ok(a), Ok(b)) => (a, b),
        _ => {
            return SessionResult {
                usage: Value::Null,
                error: Some(format!(
                    "cannot open session logs in {}",
                    round_dir.display()
                )),
            }
        }
    };
    cmd.stdout(Stdio::from(log)).stderr(Stdio::from(errlog));

    let mut child = match cmd.spawn() {
        Ok(c) => c,
        Err(e) => {
            return SessionResult {
                usage: Value::Null,
                error: Some(format!("openshell exec: {e}")),
            }
        }
    };
    let budget = Duration::from_secs(args.session_timeout);
    let started = Instant::now();
    let mut timed_out = false;
    let status = loop {
        match child.try_wait() {
            Ok(Some(status)) => break Some(status),
            Ok(None) if started.elapsed() >= budget => {
                timed_out = true;
                let _ = child.kill();
                let status = child.wait().ok();
                // The local client is gone; the agent inside the sandbox is not.
                kill_stray_agents(contender.sandbox(args));
                break status;
            }
            Ok(None) => std::thread::sleep(Duration::from_secs(1)),
            Err(e) => {
                return SessionResult {
                    usage: Value::Null,
                    error: Some(format!("waiting on agent session: {e}")),
                }
            }
        }
    };

    // Built even when the session failed or was killed: a partial event stream
    // still explains what the model was doing when it stopped.
    let stdout = std::fs::read_to_string(&log_path).unwrap_or_default();
    let trace = trace::parse(contender.harness, &stdout);
    if let Err(e) = std::fs::write(round_dir.join("trace.jsonl"), trace.to_jsonl()) {
        eprintln!("trace write failed: {e}");
    }
    let mut usage = trace.usage.clone();
    // opencode reports per-step cost, but only when the provider sends it back.
    // Fall back to the key's spend delta, which is right for a run of one.
    if contender.harness == Harness::Opencode
        && usage.get("cost_usd").and_then(Value::as_f64).unwrap_or(0.0) == 0.0
    {
        if let (Some(before), Some(after)) = (spend_before, openrouter_spend()) {
            if after >= before {
                usage["cost_usd"] = json!(after - before);
            }
        }
    }
    println!(
        "  {} trace events, {} tool calls",
        trace.events.len(),
        trace.tool_calls()
    );
    let error = if timed_out {
        Some(format!(
            "agent session hit the {}s timeout and was killed; scoring whatever it built",
            args.session_timeout
        ))
    } else if status.map(|s| s.success()).unwrap_or(false) {
        None
    } else {
        let stderr = std::fs::read_to_string(&err_path).unwrap_or_default();
        let stdout = stdout.as_str();
        let mut tail = format!("{} | {}", stderr.trim(), stdout.trim());
        tail.truncate(500);
        let code = status
            .map(|s| s.to_string())
            .unwrap_or_else(|| "killed".into());
        Some(format!("agent session failed ({code}): {tail}"))
    };
    SessionResult { usage, error }
}

/// Post-round artifact collection over MCP; agent may or may not have called
/// finish_and_test itself (calling it again only adds test ticks).
fn collect_round(
    client: &mut mcp::McpClient,
    args: &Args,
    round_dir: &Path,
) -> Result<Value, String> {
    std::fs::create_dir_all(round_dir).map_err(|e| e.to_string())?;

    // Both calls can legitimately fail (no ride built, open circuit); the
    // round record must exist either way so the site can show what happened.
    let state = client.call("get_state", json!({})).unwrap_or(json!({}));
    let placed = state.get("pieces_placed").cloned().unwrap_or(json!(0));
    // Test the current park: catches a round where the agent never called
    // finish_and_test itself, and updates the server's best-so-far.
    let final_report = client
        .call("finish_and_test", json!({"ticks": args.ticks}))
        .unwrap_or_else(|e| {
            json!({
                "program": {
                    "ok": false,
                    "pieces_placed": placed,
                    "pieces_total": placed,
                    "error": {"piece_index": null, "message": e},
                },
                "rides": [],
                "similarity": null,
            })
        });
    // Score the best tested coaster of the round, not whatever happens to be
    // standing now: a model that demolished a good ride to try a better one
    // should keep the good score. best_result errors when nothing rated, in
    // which case the final-park report (with its error) is what we record.
    let (report, scored_from_best) = match client.call("best_result", json!({})) {
        Ok(best) if best_excitement(&best) >= best_excitement(&final_report) => (best, true),
        _ => (final_report, false),
    };

    // Reconstruct program.json for the piece listing. The scored report
    // carries its own placed_pieces/start (finish_and_test snapshots them), so
    // this describes the scored coaster even when it has since been rebuilt;
    // fall back to the live state for pre-snapshot reports.
    let pieces = report
        .get("placed_pieces")
        .or_else(|| state.get("placed_pieces"))
        .cloned()
        .unwrap_or(json!([]));
    let start = report
        .get("start")
        .or_else(|| state.get("start"))
        .cloned()
        .unwrap_or(Value::Null);
    // start already comes from cursor_json, which reports tile coordinates
    // (it divides raw game units by 32); the program format wants tiles, so
    // pass them straight through — dividing again put every start at (1, 1).
    let program = json!({
        "ride_type": args.ride_type,
        "start": {
            "x": start.get("x").and_then(Value::as_i64).unwrap_or(0),
            "y": start.get("y").and_then(Value::as_i64).unwrap_or(0),
            "dir": start.get("dir").and_then(Value::as_i64).unwrap_or(0),
        },
        "pieces": pieces,
    });
    write_json(&round_dir.join("program.json"), &program)?;
    write_json(&round_dir.join("report.json"), &report)?;

    // Picture the scored coaster: best_screenshot when the score came from an
    // earlier (possibly demolished) build, otherwise the current park.
    let shot_tool = if scored_from_best {
        "best_screenshot"
    } else {
        "screenshot"
    };
    match client.call_image(shot_tool, json!({})) {
        Ok(png) => std::fs::write(round_dir.join("park.png"), png).map_err(|e| e.to_string())?,
        Err(e) => eprintln!("  {shot_tool} failed: {e}"),
    }
    Ok(report)
}

fn write_json(path: &Path, value: &Value) -> Result<(), String> {
    let text = serde_json::to_string_pretty(value).map_err(|e| e.to_string())?;
    std::fs::write(path, text).map_err(|e| format!("{}: {e}", path.display()))
}

fn best_excitement(report: &Value) -> Option<f64> {
    let rides = report.get("rides")?.as_array()?;
    let raw = rides
        .iter()
        .filter_map(|r| r.get("excitement").and_then(Value::as_f64))
        .reduce(f64::max)?;
    // Same penalty math as driver.py / site.py (SIMILARITY_GRACE = 0.5).
    let similarity = report
        .pointer("/similarity/similarity")
        .and_then(Value::as_f64)
        .unwrap_or(0.0);
    let multiplier = if similarity <= 0.5 {
        1.0
    } else {
        ((1.0 - similarity) / 0.5).max(0.0)
    };
    Some(raw * multiplier)
}

/// Wall-clock seconds, used to make each round's lease unique across restarts.
fn epoch_secs() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs()
}

fn today() -> String {
    // No chrono dep for one date string: days since epoch -> civil date.
    let secs = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs();
    let days = (secs / 86400) as i64;
    let (y, m, d) = civil_from_days(days);
    format!("{y:04}{m:02}{d:02}")
}

/// Howard Hinnant's days-from-civil inverse, public domain algorithm.
fn civil_from_days(z: i64) -> (i64, u32, u32) {
    let z = z + 719_468;
    let era = z.div_euclid(146_097);
    let doe = z.rem_euclid(146_097);
    let yoe = (doe - doe / 1460 + doe / 36524 - doe / 146_096) / 365;
    let y = yoe + era * 400;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = (doy - (153 * mp + 2) / 5 + 1) as u32;
    let m = if mp < 10 { mp + 3 } else { mp - 9 } as u32;
    (if m <= 2 { y + 1 } else { y }, m, d)
}

fn main() -> Result<(), String> {
    let args = Args::parse();
    let root = repo_root();

    // Debug aid: print the round-1 prompt verbatim and exit.
    if std::env::var_os("COASTER_BENCH_PRINT_PROMPT").is_some() {
        print!(
            "{}",
            prompt::round_prompt(
                args.ride_type,
                1,
                args.rounds,
                None,
                Modalities::TEXT | Modalities::IMAGE,
                args.session_timeout,
            )
        );
        return Ok(());
    }

    let suffix = args.name.clone().unwrap_or_else(|| "bench".into());
    let run_dir = root
        .join("evals/runs")
        .join(format!("{}-{}", today(), suffix));
    std::fs::create_dir_all(&run_dir).map_err(|e| e.to_string())?;
    println!(
        "run dir: {} (ride_type {})",
        run_dir.display(),
        args.ride_type
    );

    let _server = if args.attach {
        None
    } else {
        Some(GameServer::spawn(&args, &root)?)
    };
    let mut client = mcp::McpClient::new("127.0.0.1", args.port);
    wait_for_server(&mut client, Duration::from_secs(120))?;
    println!("game server ready on port {}", args.port);

    let contenders: Vec<Contender> = args.models.iter().map(|s| Contender::parse(s)).collect();
    // A previous run killed from the outside can leave an agent alive in its
    // sandbox, still building in the park this run is about to use.
    for sandbox in contenders
        .iter()
        .map(|c| c.sandbox(&args))
        .collect::<std::collections::BTreeSet<_>>()
    {
        kill_stray_agents(sandbox);
    }
    for c in &contenders {
        if !c.modalities.contains(Modalities::IMAGE) {
            println!(
                "[{}] modalities {}: screenshot tool hidden",
                c.display(),
                c.modalities.as_query_value()
            );
        }
    }
    let modalities: Value = contenders
        .iter()
        .map(|c| (c.display(), json!(c.modalities.as_query_value())))
        .collect::<serde_json::Map<_, _>>()
        .into();
    let harnesses: Value = contenders
        .iter()
        .map(|c| (c.display(), json!(c.harness.name())))
        .collect::<serde_json::Map<_, _>>()
        .into();
    let run_harness = if contenders
        .iter()
        .all(|c| c.harness == contenders[0].harness)
    {
        contenders[0].harness.name().to_string()
    } else {
        "mixed".to_string()
    };
    write_json(
        &run_dir.join("run.json"),
        &json!({
            // coaster-bench only orchestrates the flagship goal today; the
            // site refuses to rank runs of different goals together.
            "goal": "best-coaster",
            "mode": "design",
            "orchestrator": "coaster-bench",
            "harness": run_harness,
            "harnesses": harnesses,
            "models": contenders.iter().map(Contender::display).collect::<Vec<_>>(),
            "modalities": modalities,
            "rounds": args.rounds,
            "ticks": args.ticks,
            "ride_type": args.ride_type,
            "similarity_grace": 0.5,
        }),
    )?;

    let mut standings: Vec<Value> = Vec::new();
    for contender in &contenders {
        let model = contender.display();
        let mut best: Option<(u32, f64)> = None;
        let mut feedback: Option<String> = None;
        for round in 1..=args.rounds {
            // One lease per round: claiming it evicts any agent still alive
            // from an earlier round, which would otherwise build in this park.
            let lease = format!("{}-r{round}-{}", model, epoch_secs());
            client.claim("127.0.0.1", args.port, &lease);
            // A fresh park state for every round.
            let _ = client.call("demolish", json!({}));
            let prompt = prompt::round_prompt(
                args.ride_type,
                round,
                args.rounds,
                feedback.as_deref(),
                contender.modalities,
                args.session_timeout,
            );
            // Created up front: the session streams its logs in here while it
            // runs, so `tail -f` works on a round in progress.
            let round_dir = run_dir.join(&model).join(format!("round_{round}"));
            if let Err(e) = std::fs::create_dir_all(&round_dir) {
                return Err(format!("create {}: {e}", round_dir.display()));
            }
            println!(
                "[{model}] round {round}: agent session starting (log: {})",
                round_dir.join("session.log").display()
            );
            let session = run_agent_session(&args, contender, &prompt, &round_dir, &lease);
            if let Some(e) = &session.error {
                eprintln!("[{model}] round {round}: {e}");
            }
            if !session.usage.is_null() {
                let usage = json!({
                    "harness": contender.harness.name(),
                    "model": contender.model,
                    "input_tokens": session.usage.get("input_tokens"),
                    "output_tokens": session.usage.get("output_tokens"),
                    "cache_read_tokens": session.usage.get("cache_read_tokens"),
                    "cost_usd": session.usage.get("cost_usd"),
                    "num_turns": session.usage.get("num_turns"),
                });
                let _ = std::fs::create_dir_all(&round_dir);
                if let Err(e) = write_json(&round_dir.join("usage.json"), &usage) {
                    eprintln!("[{model}] round {round}: usage write failed: {e}");
                }
            }
            match collect_round(&mut client, &args, &round_dir) {
                Ok(report) => {
                    let score = best_excitement(&report);
                    println!(
                        "[{model}] round {round}: excitement {}",
                        score.map_or("unrated".into(), |s| format!("{s:.2}"))
                    );
                    if let Some(s) = score {
                        if best.is_none_or(|(_, b)| s > b) {
                            best = Some((round, s));
                        }
                    }
                    feedback = serde_json::to_string(&report).ok();
                }
                Err(e) => {
                    eprintln!("[{model}] round {round}: collect failed: {e}");
                    feedback = Some(format!(
                        "{{\"error\": \"round produced no rated ride: {e}\"}}"
                    ));
                }
            }
        }
        standings.push(json!({
            "model": model,
            "harness": contender.harness.name(),
            "best_excitement": best.map(|(_, s)| s),
            "best_round": best.map(|(r, _)| r),
        }));
    }

    standings.sort_by(|a, b| {
        let score = |v: &Value| {
            v.get("best_excitement")
                .and_then(Value::as_f64)
                .unwrap_or(-1.0)
        };
        score(b)
            .partial_cmp(&score(a))
            .unwrap_or(std::cmp::Ordering::Equal)
    });
    write_json(
        &run_dir.join("standings.json"),
        &json!({"goal": "best-coaster", "standings": standings}),
    )?;

    println!("\n=== FINAL STANDINGS ===");
    for (place, entry) in standings.iter().enumerate() {
        let model = entry.get("model").and_then(Value::as_str).unwrap_or("?");
        match entry.get("best_excitement").and_then(Value::as_f64) {
            Some(s) => println!("{}. {model}: excitement {s:.2}", place + 1),
            None => println!("{}. {model}: no successful coaster", place + 1),
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn contenders_advertise_their_modalities_to_the_mcp_server() {
        let text_only = Contender {
            harness: Harness::Opencode,
            model: "openrouter/poolside/laguna-s-2.1".into(),
            modalities: Modalities::TEXT,
        };
        let multimodal = Contender {
            harness: Harness::ClaudeCode,
            model: "claude-sonnet-5".into(),
            modalities: Modalities::TEXT | Modalities::IMAGE,
        };
        assert!(text_only
            .mcp_url(8791, "lease-1")
            .ends_with("/mcp?modalities=text&lease=lease-1"));
        assert!(multimodal
            .mcp_url(8791, "lease-1")
            .ends_with("/mcp?modalities=text,image&lease=lease-1"));
    }

    #[test]
    fn each_harness_resolves_to_its_own_sandbox() {
        let args = Args::parse_from(["coaster-bench"]);
        let claude = Contender::parse("claude-sonnet-5");
        let opencode = Contender {
            harness: Harness::Opencode,
            model: "openrouter/moonshotai/kimi-k3".into(),
            modalities: Modalities::TEXT | Modalities::IMAGE,
        };
        assert_eq!(claude.sandbox(&args), "coaster-sub");
        assert_eq!(opencode.sandbox(&args), "coaster-or");
    }

    #[test]
    fn modality_sets_render_in_catalogue_order() {
        assert_eq!(Modalities::TEXT.as_query_value(), "text");
        assert_eq!(
            (Modalities::FILE | Modalities::TEXT).as_query_value(),
            "text,file"
        );
        assert!(!Modalities::TEXT.contains(Modalities::IMAGE));
    }

    #[test]
    fn civil_date_matches_known_epochs() {
        assert_eq!(civil_from_days(0), (1970, 1, 1));
        assert_eq!(civil_from_days(20_292), (2025, 7, 23));
    }

    #[test]
    fn penalty_math_matches_driver() {
        let report = json!({"rides": [{"excitement": 8.0}], "similarity": {"similarity": 0.75}});
        let score = best_excitement(&report).expect("scored");
        assert!((score - 4.0).abs() < 1e-9);
        let free = json!({"rides": [{"excitement": 8.0}], "similarity": {"similarity": 0.4}});
        assert!((best_excitement(&free).expect("scored") - 8.0).abs() < 1e-9);
    }

    #[test]
    fn unrated_ride_scores_none() {
        let report = json!({"rides": [{"excitement": null}]});
        assert!(best_excitement(&report).is_none());
    }
}
