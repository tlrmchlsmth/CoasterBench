//! CoasterBench orchestrator: runs a head-to-head eval where each contender is
//! a Claude Code session inside an OpenShell sandbox (personal-sub auth),
//! building interactively against the game's MCP server on this host.
//!
//!   coaster-bench --models claude-fable-5 claude-sonnet-5 --rounds 4 \
//!       --ride-type 51 --name sub-twister-1
//!
//! The game server is spawned as a child (coasterbench-cli eval --serve) and the
//! orchestrator collects per-round artifacts (report.json, program.json,
//! park.png) in the same evals/runs/<name>/ layout driver.py produces, so the
//! site generator needs no changes.

mod mcp;
mod prompt;
mod trace;

use std::collections::BTreeSet;
use std::fs::File;
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use clap::{Parser, ValueEnum};
use serde_json::{json, Value};
use sha2::{Digest, Sha256};

const RUN_SCHEMA_VERSION: u32 = 2;
const SCORER_VERSION: &str = env!("CARGO_PKG_VERSION");

#[derive(Parser, Debug)]
#[command(version, about)]
struct Args {
    /// Contender model ids, one Claude Code session per round each.
    #[arg(long, num_args = 1.., default_values_t = vec!["claude-sonnet-5".to_string()])]
    models: Vec<String>,

    /// Reopen one archived round for a presentation-only model pass. The
    /// original model chooses a name and colours; score and layout must match.
    #[arg(long, value_name = "ROUND_DIR")]
    present_round: Option<PathBuf>,

    #[arg(long, default_value_t = 4)]
    rounds: u32,

    /// Competition ride type (51 steel twister, 52 wooden).
    #[arg(long, default_value_t = 51)]
    ride_type: u16,

    /// Benchmark condition. Design is MCP-only, library exposes stock-design
    /// lookup, and open-note stages the scorer's upstream engine source.
    #[arg(long, value_enum, default_value_t = BenchmarkMode::Design)]
    mode: BenchmarkMode,

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

    /// Loopback-only control port. The harness's own channel into the game
    /// (save_park), on a listener the sandboxes cannot route to, so nothing an
    /// agent can reach writes to this filesystem.
    #[arg(long, default_value_t = 8792)]
    control_port: u16,

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

    /// Run every contender across the first N generated scenario parks (seeds
    /// from evals/scenarios/seeds.json, regenerated deterministically on
    /// demand) and aggregate scores across parks. Overrides --scenario.
    #[arg(long, default_value_t = 0)]
    scenarios: u32,

    /// With --scenarios: how many parks run at once. Each concurrent park is
    /// its own game server on its own port (--port + slot), so the sandbox
    /// policy must allow this many consecutive ports. Contenders on the same
    /// park always run sequentially (they share its server).
    #[arg(long, default_value_t = 3)]
    concurrency: usize,

    #[arg(long, default_value = "~/rct2-assets")]
    rct2_data: String,

    /// Run the game servers without RCT2 assets (--no-graphics): the whole
    /// interactive build/test/rate loop works, but the MCP server drops the
    /// screenshot tool and park.png captures are skipped.
    #[arg(long)]
    no_graphics: bool,

    /// Reuse an already-running MCP server instead of spawning one.
    #[arg(long)]
    attach: bool,

    /// Create a throwaway sandbox for this run and delete it afterwards, so no
    /// state survives between runs. Claude Code lane only for now.
    #[arg(long)]
    fresh_sandbox: bool,

    #[arg(long, default_value = "localhost/coaster-sandbox")]
    sandbox_image: String,

    #[arg(long, default_value = "rust/coaster-bench/sandbox/policy.yaml")]
    sandbox_policy: String,

    #[arg(long, default_value = "claude-sub")]
    sandbox_provider: String,

    #[arg(long, default_value = "localhost/coaster-or")]
    opencode_sandbox_image: String,

    #[arg(
        long,
        default_value = "rust/coaster-bench/sandbox/opencode-policy.yaml"
    )]
    opencode_sandbox_policy: String,

    #[arg(long, default_value = "openrouter")]
    opencode_sandbox_provider: String,

    #[arg(long, default_value = "codex-arena")]
    codex_sandbox: String,

    #[arg(long, default_value = "localhost/codex-arena")]
    codex_sandbox_image: String,

    #[arg(long, default_value = "rust/coaster-bench/sandbox/codex-policy.yaml")]
    codex_sandbox_policy: String,

    #[arg(long, default_value = "codex-oauth")]
    codex_sandbox_provider: String,

    #[arg(long, default_value = "/home/sandbox/bin/codex")]
    codex_bin: String,

    /// Reasoning effort for Codex contenders. Pin this instead of inheriting
    /// the model catalogue default so public runs remain reproducible.
    #[arg(long, value_enum, default_value_t = CodexReasoningEffort::Medium)]
    codex_reasoning_effort: CodexReasoningEffort,

    /// Extra tools to allow the agent, comma separated (e.g. "Bash"). Open note
    /// implies the file tools; this grants them without staging any source.
    #[arg(long)]
    extra_tools: Option<String>,

    /// Cap on the replay recorded as round_N/replay.mp4 (needs ffmpeg on
    /// PATH). The length actually used is the lap time the game measured, so
    /// this only bounds a pathological ride. 0 records nothing.
    #[arg(long, default_value_t = 90)]
    replay_seconds: u32,

    /// Compatibility spelling for `--mode open-note`.
    #[arg(long)]
    open_note: bool,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, ValueEnum)]
enum BenchmarkMode {
    Design,
    Library,
    OpenNote,
    /// Internal mode selected by --present-round. It is a capability boundary,
    /// not a standalone benchmark condition.
    #[value(skip)]
    Presentation,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, ValueEnum)]
enum CodexReasoningEffort {
    Low,
    Medium,
    High,
    Xhigh,
    Max,
    Ultra,
}

impl CodexReasoningEffort {
    fn as_str(self) -> &'static str {
        match self {
            CodexReasoningEffort::Low => "low",
            CodexReasoningEffort::Medium => "medium",
            CodexReasoningEffort::High => "high",
            CodexReasoningEffort::Xhigh => "xhigh",
            CodexReasoningEffort::Max => "max",
            CodexReasoningEffort::Ultra => "ultra",
        }
    }
}

impl BenchmarkMode {
    /// Wire condition understood by the MCP server. Open note remains a
    /// from-scratch design condition; its extra capability is local source.
    fn mcp_condition(self) -> &'static str {
        match self {
            BenchmarkMode::Design | BenchmarkMode::OpenNote => "design",
            BenchmarkMode::Library => "library",
            BenchmarkMode::Presentation => "presentation",
        }
    }

    fn base_mode(self) -> &'static str {
        match self {
            BenchmarkMode::Library => "library",
            BenchmarkMode::Design | BenchmarkMode::OpenNote => "design",
            BenchmarkMode::Presentation => "presentation",
        }
    }

    fn condition_name(self) -> &'static str {
        match self {
            BenchmarkMode::Design => "design",
            BenchmarkMode::Library => "library",
            BenchmarkMode::OpenNote => "open-note",
            BenchmarkMode::Presentation => "presentation",
        }
    }
}

const OPEN_NOTE_DIR: &str = "/tmp/openrct2-src";

/// Longest name the gateway accepts.
const MAX_SANDBOX_NAME: usize = 19;

/// `<base>-<short id>`, trimming the base rather than the id so runs stay
/// distinguishable when the name has to be cut.
fn ephemeral_sandbox_name(base: &str, epoch: u64) -> String {
    let id = format!("{:x}", epoch % 0xffffff);
    let room = MAX_SANDBOX_NAME.saturating_sub(id.len() + 1);
    let base: String = base.chars().take(room).collect();
    format!("{base}-{id}")
}

/// A sandbox owned by this run: deleted on the way out, including when a signal
/// unwinds the run, because the flag path returns from main normally.
struct EphemeralSandbox {
    name: String,
}

impl Drop for EphemeralSandbox {
    fn drop(&mut self) {
        let out = Command::new("openshell")
            .args(["sandbox", "delete", &self.name])
            .stdout(Stdio::null())
            .stderr(Stdio::piped())
            .output();
        match out {
            Ok(o) if o.status.success() => println!("deleted sandbox {}", self.name),
            Ok(o) if String::from_utf8_lossy(&o.stderr).contains("not found") => {}
            _ => eprintln!(
                "could not delete sandbox {}; remove it with `openshell sandbox delete {}`",
                self.name, self.name
            ),
        }
    }
}

/// The policy file pins the MCP port, and a mismatch fails as a silent network
/// denial inside the sandbox. Parse the policy and touch only the coaster-game
/// endpoint; inference endpoints such as api.openai.com:443 are unrelated.
fn policy_with_port(policy: &str, port: u16) -> Result<String, String> {
    let mut document: serde_norway::Value =
        serde_norway::from_str(policy).map_err(|e| format!("parse sandbox policy: {e}"))?;
    let endpoints = document
        .get_mut("network_policies")
        .and_then(|v| v.get_mut("coaster_game"))
        .and_then(|v| v.get_mut("endpoints"))
        .and_then(serde_norway::Value::as_sequence_mut)
        .ok_or("sandbox policy has no network_policies.coaster_game.endpoints")?;
    let mut changed = false;
    for endpoint in endpoints {
        if endpoint.get("host").and_then(serde_norway::Value::as_str)
            == Some("host.containers.internal")
        {
            endpoint["port"] =
                serde_norway::to_value(port).map_err(|e| format!("serialize MCP port: {e}"))?;
            changed = true;
        }
    }
    if !changed {
        return Err("coaster_game policy has no host.containers.internal endpoint".into());
    }
    serde_norway::to_string(&document).map_err(|e| format!("serialize sandbox policy: {e}"))
}

struct SandboxRecipe<'a> {
    image: &'a str,
    policy: &'a str,
    provider: &'a str,
}

fn create_sandbox(
    recipe: &SandboxRecipe,
    port: u16,
    root: &Path,
    name: &str,
) -> Result<EphemeralSandbox, String> {
    let policy_path = root.join(recipe.policy);
    let policy = std::fs::read_to_string(&policy_path)
        .map_err(|e| format!("read {}: {e}", policy_path.display()))?;
    let staged = std::env::temp_dir().join(format!("coaster-policy-{name}.yaml"));
    std::fs::write(&staged, policy_with_port(&policy, port)?)
        .map_err(|e| format!("write {}: {e}", staged.display()))?;

    // `sandbox create` attaches and never returns, but the sandbox is up long
    // before that and outlives the client, so spawn it, wait for the sandbox to
    // answer, then drop the client.
    let mut cmd = Command::new("openshell");
    cmd.args(["sandbox", "create", "--name", name])
        .args(["--from", recipe.image])
        .arg("--policy")
        .arg(&staged);
    for provider in recipe.provider.split(',').filter(|p| !p.is_empty()) {
        cmd.args(["--provider", provider]);
    }
    let mut child = cmd
        .arg("--no-tty")
        .args(["--", "sleep", "infinity"])
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::piped())
        .spawn()
        .map_err(|e| format!("openshell create: {e}"))?;
    // Registered before the wait: a sandbox that comes up and then fails the
    // readiness check still has to be cleaned up.
    let guard = EphemeralSandbox {
        name: name.to_string(),
    };

    let deadline = Instant::now() + Duration::from_secs(180);
    loop {
        if sandbox_ready(name) {
            let _ = child.kill();
            let _ = child.wait();
            let _ = std::fs::remove_file(&staged);
            return Ok(guard);
        }
        match child.try_wait() {
            Ok(Some(status)) => {
                let mut stderr = String::new();
                if let Some(mut pipe) = child.stderr.take() {
                    use std::io::Read;
                    let _ = pipe.read_to_string(&mut stderr);
                }
                let _ = std::fs::remove_file(&staged);
                return Err(format!(
                    "creating sandbox {name} failed ({status}): {}",
                    stderr.trim()
                ));
            }
            Ok(None) if Instant::now() >= deadline => {
                let _ = child.kill();
                let _ = child.wait();
                let _ = std::fs::remove_file(&staged);
                return Err(format!("sandbox {name} never became ready"));
            }
            Ok(None) => std::thread::sleep(Duration::from_secs(2)),
            Err(e) => return Err(format!("waiting on sandbox create: {e}")),
        }
    }
}

/// Runs a command with a deadline, killing it if it overruns. `sandbox exec`
/// against a sandbox that is still coming up blocks instead of failing, so an
/// unbounded readiness probe hangs forever and never reaches its own deadline.
fn run_bounded(mut cmd: Command, budget: Duration) -> Option<bool> {
    let mut child = cmd
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()
        .ok()?;
    let deadline = Instant::now() + budget;
    loop {
        match child.try_wait() {
            Ok(Some(status)) => return Some(status.success()),
            Ok(None) if Instant::now() >= deadline => {
                let _ = child.kill();
                let _ = child.wait();
                return None;
            }
            Ok(None) => std::thread::sleep(Duration::from_millis(500)),
            Err(_) => return None,
        }
    }
}

fn sandbox_ready(name: &str) -> bool {
    let mut cmd = Command::new("openshell");
    cmd.args(["sandbox", "exec", "-n", name, "--"])
        .args(["true"]);
    run_bounded(cmd, Duration::from_secs(20)) == Some(true)
}

/// Set once on SIGINT/SIGTERM/SIGHUP. A handler cannot do the cleanup itself
/// (killing children and reaping them is not async-signal-safe, and process
/// exit from a handler skips every Drop), so it only raises this and the run
/// loop unwinds normally: agent killed, sandbox swept, game server dropped.
fn install_shutdown_flag() -> Result<Arc<AtomicBool>, String> {
    let flag = Arc::new(AtomicBool::new(false));
    for signal in [
        signal_hook::consts::SIGINT,
        signal_hook::consts::SIGTERM,
        signal_hook::consts::SIGHUP,
    ] {
        signal_hook::flag::register(signal, Arc::clone(&flag))
            .map_err(|e| format!("register signal {signal}: {e}"))?;
    }
    Ok(flag)
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
    fn spawn(
        args: &Args,
        root: &Path,
        scenario: &Path,
        port: u16,
        control_port: u16,
    ) -> Result<GameServer, String> {
        let cli = root.join("build/coasterbench-cli");
        if !cli.is_file() {
            return Err(format!("{} not built", cli.display()));
        }
        // Our own child would fail to bind and wait_for_server would then talk
        // happily to whatever is already there: a stale server holding a
        // different park, or another run in progress. Scoring against that is
        // worse than not running.
        if port_in_use(port) {
            return Err(format!(
                "port {port} is already serving; kill the stale server (lsof -ti:{port}) or pass --attach to use it deliberately"
            ));
        }
        if port_in_use(control_port) {
            return Err(format!(
                "control port {control_port} is already serving; kill the stale server (lsof -ti:{control_port}) or choose another --control-port"
            ));
        }
        let mut cmd = Command::new(cli);
        cmd.arg("eval").arg(scenario);
        if args.no_graphics {
            cmd.arg("--no-graphics");
        } else {
            cmd.arg("--rct2-data-path")
                .arg(expand_home(&args.rct2_data));
        }
        let child = cmd
            .arg("--serve")
            .arg(port.to_string())
            .arg("--serve-bind")
            .arg("0.0.0.0")
            .arg("--serve-control")
            .arg(control_port.to_string())
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

fn port_in_use(port: u16) -> bool {
    std::net::TcpStream::connect_timeout(
        &std::net::SocketAddr::from(([127, 0, 0, 1], port)),
        Duration::from_millis(500),
    )
    .is_ok()
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
    Codex,
}

impl Harness {
    fn name(self) -> &'static str {
        match self {
            Harness::ClaudeCode => "claude-code",
            Harness::Opencode => "opencode",
            Harness::Codex => "codex",
        }
    }

    fn single_turn(self) -> bool {
        match self {
            Harness::Codex => true,
            Harness::ClaudeCode | Harness::Opencode => false,
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
            Some(("codex", model)) => Contender {
                harness: Harness::Codex,
                modalities: match model.strip_prefix("openrouter/") {
                    Some(or) => {
                        openrouter_modalities(or).unwrap_or(Modalities::TEXT | Modalities::IMAGE)
                    }
                    None => Modalities::TEXT | Modalities::IMAGE,
                },
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
            Harness::Codex => &args.codex_sandbox,
        }
    }

    fn codex_openrouter_model(&self) -> Option<&str> {
        (self.harness == Harness::Codex).then(|| self.model.strip_prefix("openrouter/"))?
    }

    fn provider<'a>(&self, args: &'a Args) -> &'a str {
        match self.harness {
            Harness::ClaudeCode => &args.sandbox_provider,
            Harness::Opencode => &args.opencode_sandbox_provider,
            Harness::Codex if self.codex_openrouter_model().is_some() => "openrouter",
            Harness::Codex => &args.codex_sandbox_provider,
        }
    }

    /// MCP endpoint this contender's harness should connect to. The server
    /// hides tools answering outside the advertised modality set, and refuses
    /// anyone whose lease is not the one that currently owns the park.
    fn mcp_url(&self, port: u16, lease: &str, mode: BenchmarkMode) -> String {
        format!(
            "http://host.containers.internal:{port}/mcp?modalities={}&condition={}&lease={lease}",
            self.modalities.as_query_value(),
            mode.mcp_condition(),
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

impl SessionResult {
    /// The session never got far enough to report usage.
    fn failed(error: String) -> SessionResult {
        SessionResult {
            usage: Value::Null,
            error: Some(error),
        }
    }
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
/// opencode's tool permissions default to allow, so without this a blind run on
/// this lane is not blind at all: it keeps bash, read and grep, while the Claude
/// Code lane is held to MCP only by its allowlist. Named explicitly rather than
/// with a wildcard, because a wildcard would also catch the MCP tools.
fn opencode_permissions(args: &Args) -> Value {
    let verdict = if args.open_note { "allow" } else { "deny" };
    json!({
        "bash": verdict,
        "read": verdict,
        "grep": verdict,
        "glob": verdict,
        "edit": verdict,
        // Open note stages the source outside the project directory, which
        // otherwise prompts and stalls a non-interactive session.
        "external_directory": verdict,
        "webfetch": "deny",
        "websearch": "deny",
    })
}

fn write_opencode_config(
    args: &Args,
    contender: &Contender,
    port: u16,
    lease: &str,
) -> Result<(), String> {
    let config = json!({
        "$schema": "https://opencode.ai/config.json",
        "model": contender.model,
        "permission": opencode_permissions(args),
        "mcp": {
            "coaster": {"type": "remote", "url": contender.mcp_url(port, lease, args.mode), "enabled": true}
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

fn rfc3339_now() -> String {
    let secs = epoch_secs();
    let (y, m, d) = civil_from_days((secs / 86400) as i64);
    let (h, min, s) = (secs / 3600 % 24, secs / 60 % 60, secs % 60);
    format!("{y:04}-{m:02}-{d:02}T{h:02}:{min:02}:{s:02}.000Z")
}

fn host_codex_identity() -> Result<(String, String), String> {
    let path = expand_home("~/.codex/auth.json");
    let raw = std::fs::read_to_string(&path)
        .map_err(|e| format!("read {} (run `codex login` first): {e}", path.display()))?;
    let auth: Value = serde_json::from_str(&raw).map_err(|e| format!("parse codex auth: {e}"))?;
    let field = |name: &str| {
        auth.pointer(&format!("/tokens/{name}"))
            .and_then(Value::as_str)
            .filter(|v| !v.is_empty())
            .map(str::to_string)
            .ok_or_else(|| format!("codex auth.json has no tokens.{name}"))
    };
    Ok((field("id_token")?, field("account_id")?))
}

fn codex_sandbox_mode(args: &Args) -> &'static str {
    if args.open_note || allowed_tools(args).contains("Bash") {
        "danger-full-access"
    } else {
        "read-only"
    }
}

fn codex_config(args: &Args, contender: &Contender, lease: &str) -> String {
    let sandbox_mode = codex_sandbox_mode(args);
    let mut config = match contender.codex_openrouter_model() {
        Some(model) => format!(
            "model = \"{model}\"\n\
             model_provider = \"openrouter\"\n\
             # Codex has no metadata for an \"openai/...\"-prefixed id, and its\n\
             # fallback assumes the model cannot reason, which OpenRouter\n\
             # rejects for models where reasoning is mandatory.\n\
             model_reasoning_summary = \"auto\"\n"
        ),
        None => format!("model = \"{}\"\n", contender.model),
    };
    config.push_str(&format!(
        "model_reasoning_effort = \"{}\"\n\
         approval_policy = \"never\"\n\
         sandbox_mode = \"{sandbox_mode}\"\n",
        args.codex_reasoning_effort.as_str()
    ));
    if contender.codex_openrouter_model().is_some() {
        config.push_str(
            "\n[model_providers.openrouter]\n\
             name = \"OpenRouter\"\n\
             base_url = \"https://openrouter.ai/api/v1\"\n\
             env_key = \"OPENROUTER_API_KEY\"\n\
             wire_api = \"responses\"\n",
        );
    }
    format!(
        "{config}\n\
         [projects.\"$HOME\"]\n\
         trust_level = \"trusted\"\n\
         \n\
         [mcp_servers.coaster]\n\
         url = \"{}\"\n\
         # Headless `codex exec` has no user-input handler for MCP approval\n\
         # prompts. This server is the benchmark's deliberately trusted tool\n\
         # boundary, so pre-approve it while leaving the filesystem read-only.\n\
         default_tools_approval_mode = \"approve\"\n",
        contender.mcp_url(args.port, lease, args.mode)
    )
}

fn write_codex_config(args: &Args, contender: &Contender, lease: &str) -> Result<(), String> {
    let mut script = format!(
        "set -e\n\
         CODEX_HOME=\"${{CODEX_HOME:-$HOME/.codex}}\"\n\
         rm -rf \"$CODEX_HOME\"\n\
         mkdir -p \"$CODEX_HOME\"\n\
         cat > \"$CODEX_HOME/config.toml\" <<CODEX_CONFIG_EOF\n\
         {}\
         CODEX_CONFIG_EOF\n",
        codex_config(args, contender, lease)
    );
    if contender.codex_openrouter_model().is_none() {
        let (id_token, account_id) = host_codex_identity()?;
        script.push_str(&format!(
            "cat > \"$CODEX_HOME/auth.json\" <<CODEX_AUTH_EOF\n\
             {{\"auth_mode\":\"chatgpt\",\"OPENAI_API_KEY\":null,\"tokens\":{{\
             \"id_token\":\"{id_token}\",\
             \"access_token\":\"$CODEX_AUTH_ACCESS_TOKEN\",\
             \"refresh_token\":\"$CODEX_AUTH_REFRESH_TOKEN\",\
             \"account_id\":\"{account_id}\"}},\
             \"last_refresh\":\"{}\"}}\n\
             CODEX_AUTH_EOF\n\
             chmod 600 \"$CODEX_HOME/auth.json\"\n",
            rfc3339_now()
        ));
    }

    let mut cmd = Command::new("openshell");
    cmd.args(["sandbox", "exec", "-n", &args.codex_sandbox, "--"])
        .args(["sh", "-c", &script]);
    match run_bounded(cmd, Duration::from_secs(120)) {
        Some(true) => Ok(()),
        Some(false) => Err(format!(
            "writing codex config in {} failed",
            args.codex_sandbox
        )),
        None => Err(format!(
            "writing codex config in {} timed out",
            args.codex_sandbox
        )),
    }
}

/// Kill agent processes left behind in a sandbox. Killing the local openshell
/// client does NOT stop the process it started inside the sandbox, so a run
/// that dies (Ctrl-C, `pkill`, a timeout) strands an agent that keeps building
/// in the shared park and keeps spending. Two of those once fought over the
/// same ride for an hour, each demolishing the other's track.
/// The sandbox image has no ps/pkill, so this walks /proc and matches the
/// executable rather than the command line: the command line of this very
/// script contains "opencode", and matching that would kill the cleanup shell.
/// It must stay a single line; openshell's exec drops multi-line scripts.
fn kill_stray_agents(sandbox: &str) {
    let script = "for pid in $(ls /proc | grep \"^[0-9]\"); do [ \"$pid\" = \"$$\" ] && continue; \
                  exe=$(readlink /proc/$pid/exe 2>/dev/null); \
                  case \"$exe\" in *opencode*|*claude*|*codex*) kill -TERM \"$pid\" ;; esac; done";
    let _ = Command::new("openshell")
        .args(["sandbox", "exec", "-n", sandbox, "--"])
        .args(["sh", "-c", script])
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .status();
}

/// Wipes what a session can leave behind for the next one. The sandbox is
/// long-lived, and Claude Code's memory directory is writable even under a
/// tools allowlist, so without this a run inherits the previous run's playbook
/// (observed: a distilled tactics file naming the score to beat). Credentials
/// are injected by the provider at exec time, not stored here, so ~/.claude
/// goes in full.
fn reset_agent_state(sandbox: &str) -> Result<(), String> {
    let script = "rm -rf \"$HOME\"/.local/share/opencode \"$HOME\"/.config/opencode \
                  \"$HOME\"/.local/state/opencode \"$HOME\"/.cache \
                  \"$HOME\"/.claude/projects \"$HOME\"/.claude/sessions \
                  \"$HOME\"/.claude/shell-snapshots \"$HOME\"/.claude/backups \
                  \"$HOME\"/.claude/session-env \"$HOME\"/.claude/todos \
                  \"${CODEX_HOME:-$HOME/.codex}\"; \
                  chmod -R u+w /workspace /tmp 2>/dev/null; \
                  rm -rf /workspace/* /workspace/.* /tmp/* 2>/dev/null; true";
    let mut cmd = Command::new("openshell");
    cmd.args(["sandbox", "exec", "-n", sandbox, "--"])
        .args(["sh", "-c", script]);
    match run_bounded(cmd, Duration::from_secs(120)) {
        Some(true) => Ok(()),
        Some(false) => Err(format!("reset {sandbox} failed")),
        None => Err(format!("reset {sandbox} timed out")),
    }
}

/// The revision of upstream OpenRCT2 this fork is based on. That tree is the
/// engine we build and score with, minus the harness, so it is what open note
/// hands to the agents.
fn upstream_merge_base(root: &Path) -> Result<String, String> {
    for upstream in ["upstream/develop", "origin/develop"] {
        let out = Command::new("git")
            .current_dir(root)
            .args(["merge-base", "HEAD", upstream])
            .output()
            .map_err(|e| format!("git merge-base: {e}"))?;
        if out.status.success() {
            let sha = String::from_utf8_lossy(&out.stdout).trim().to_string();
            if !sha.is_empty() {
                return Ok(sha);
            }
        }
    }
    Err("no merge-base with upstream/develop or origin/develop".into())
}

/// Extracts upstream's tree with `git archive` (which cannot contain fork code:
/// that commit predates all of it) and uploads it into the sandbox. Cached per
/// revision on the host, so extra models and reruns skip the extract.
/// exec's stdin caps at 4 MiB, hence `sandbox upload`, which nests the local
/// directory under its destination: the staging directory is therefore named
/// after the sandbox path's last component and uploaded to its parent.
fn stage_open_note(root: &Path, sandbox: &str, sha: &str) -> Result<(), String> {
    let dest = Path::new(OPEN_NOTE_DIR);
    let (parent, name) = match (dest.parent(), dest.file_name()) {
        (Some(p), Some(n)) => (p, n),
        _ => return Err(format!("{OPEN_NOTE_DIR} is not a usable path")),
    };
    let stage = std::env::temp_dir()
        .join("coaster-open-note")
        .join(&sha[..12])
        .join(name);
    let probe = stage.join("src/openrct2/ride/RideRatings.cpp");
    if !probe.is_file() {
        let _ = std::fs::remove_dir_all(&stage);
        std::fs::create_dir_all(&stage).map_err(|e| format!("create {}: {e}", stage.display()))?;
        let mut archive = Command::new("git")
            .current_dir(root)
            .args(["archive", sha])
            .stdout(Stdio::piped())
            .stderr(Stdio::null())
            .spawn()
            .map_err(|e| format!("git archive: {e}"))?;
        let source = archive
            .stdout
            .take()
            .ok_or_else(|| "git archive produced no output".to_string())?;
        let untar = Command::new("tar")
            .args([
                std::ffi::OsStr::new("x"),
                std::ffi::OsStr::new("-C"),
                stage.as_os_str(),
            ])
            .stdin(Stdio::from(source))
            .status()
            .map_err(|e| format!("tar: {e}"))?;
        let _ = archive.wait();
        if !untar.success() || !probe.is_file() {
            return Err(format!("extracting upstream {} failed", &sha[..12]));
        }
    }

    // An earlier run left the tree unwritable, which would also block deleting
    // it, so restore write permission before replacing it.
    let clear = format!("chmod -R u+w {OPEN_NOTE_DIR} 2>/dev/null; rm -rf {OPEN_NOTE_DIR}");
    let _ = Command::new("openshell")
        .args(["sandbox", "exec", "-n", sandbox, "--"])
        .args(["sh", "-c", &clear])
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .status();

    // --no-git-ignore: upstream's own .gitignore would otherwise silently drop
    // files from the tree the agent is told is the engine.
    let upload = Command::new("openshell")
        .args(["sandbox", "upload", "--no-git-ignore", sandbox])
        .arg(&stage)
        .arg(parent)
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .status()
        .map_err(|e| format!("openshell upload: {e}"))?;
    if !upload.success() {
        return Err(format!("upload into {sandbox} failed: {upload}"));
    }

    // Read-only, so one round cannot leave edits for the next. Verified by the
    // ratings source: without it the round would silently be a plain design
    // round with a misleading prompt.
    let finish = format!(
        "chmod -R a-w {OPEN_NOTE_DIR} && test -f {OPEN_NOTE_DIR}/src/openrct2/ride/RideRatings.cpp"
    );
    let ok = Command::new("openshell")
        .args(["sandbox", "exec", "-n", sandbox, "--"])
        .args(["sh", "-c", &finish])
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .status()
        .map_err(|e| format!("verify checkout: {e}"))?;
    if !ok.success() {
        return Err(format!(
            "{OPEN_NOTE_DIR} in {sandbox} has no ratings source"
        ));
    }
    Ok(())
}

/// Tools a session may call without a prompt. MCP only by default; open note
/// adds the file tools, without which the staged source is unreadable and the
/// prompt describing it is a lie (observed: every Read denied, turns burnt).
fn allowed_tools(args: &Args) -> String {
    let mut allowed = vec!["mcp__coaster__*".to_string()];
    if args.open_note {
        allowed.extend(["Read", "Grep", "Glob", "Bash"].map(str::to_string));
    }
    if let Some(extra) = &args.extra_tools {
        for tool in extra.split(',').map(str::trim).filter(|t| !t.is_empty()) {
            if !allowed.iter().any(|a| a == tool) {
                allowed.push(tool.to_string());
            }
        }
    }
    allowed.join(",")
}

/// One agent session in the harness's sandbox. Success is judged by game
/// state, not the transcript; usage is captured best-effort either way.
/// Output streams to `session.log` / `session.err` in the round directory as
/// the session runs, so a round in progress can be tailed, and a round that
/// went wrong can be read afterwards. Files, not pipes: a session can outrun a
/// pipe buffer and deadlock while nobody is reading it.
#[allow(clippy::too_many_arguments)]
fn run_agent_session(
    args: &Args,
    contender: &Contender,
    port: u16,
    prompt: &str,
    round_dir: &Path,
    lease: &str,
    kill_on_timeout: bool,
    shutdown: &AtomicBool,
) -> SessionResult {
    let mut cmd = Command::new("openshell");
    let spend_before = match contender.harness {
        Harness::Opencode => openrouter_spend(),
        Harness::Codex if contender.codex_openrouter_model().is_some() => openrouter_spend(),
        Harness::ClaudeCode | Harness::Codex => None,
    };
    match contender.harness {
        Harness::ClaudeCode => {
            let mcp_config = json!({
                "mcpServers": {
                    "coaster": {"type": "http", "url": contender.mcp_url(port, lease, args.mode)}
                }
            });
            cmd.args(["sandbox", "exec", "-n", &args.sandbox, "--"])
                .args(["claude", "--model", &contender.model, "-p", prompt])
                .args(["--mcp-config", &mcp_config.to_string()])
                .args(["--allowedTools", &allowed_tools(args)])
                .args(["--max-turns", &args.max_turns.to_string()])
                // Event stream, not a summary: the round's trace is built from it.
                .args(["--output-format", "stream-json", "--verbose"]);
        }
        Harness::Opencode => {
            // opencode reads its MCP server list from a config file inside the
            // sandbox, so the per-contender endpoint has to be written there
            // before the session starts. Model comes fully qualified on the
            // command line (e.g. openrouter/poolside/...).
            if let Err(e) = write_opencode_config(args, contender, port, lease) {
                return SessionResult::failed(e);
            }
            cmd.args(["sandbox", "exec", "-n", &args.opencode_sandbox, "--"])
                .args(["opencode", "run", prompt, "-m", &contender.model])
                .args(["--format", "json"]);
        }
        Harness::Codex => {
            if let Err(e) = write_codex_config(args, contender, lease) {
                return SessionResult::failed(e);
            }
            let script = format!(
                "exec {} exec --json --skip-git-repo-check --cd \"$HOME\" \"$1\"",
                args.codex_bin
            );
            cmd.args(["sandbox", "exec", "-n", &args.codex_sandbox, "--"])
                .args(["sh", "-c", &script, "codex-session", prompt]);
        }
    }
    let log_path = round_dir.join("session.log");
    let err_path = round_dir.join("session.err");
    let (log, errlog) = match (File::create(&log_path), File::create(&err_path)) {
        (Ok(a), Ok(b)) => (a, b),
        _ => {
            return SessionResult::failed(format!(
                "cannot open session logs in {}",
                round_dir.display()
            ))
        }
    };
    cmd.stdin(Stdio::null())
        .stdout(Stdio::from(log))
        .stderr(Stdio::from(errlog));

    let mut child = match cmd.spawn() {
        Ok(c) => c,
        Err(e) => return SessionResult::failed(format!("openshell exec: {e}")),
    };
    let budget = Duration::from_secs(args.session_timeout);
    let started = Instant::now();
    let mut timed_out = false;
    let mut interrupted = false;
    let status = loop {
        match child.try_wait() {
            Ok(Some(status)) => break Some(status),
            Ok(None) if started.elapsed() >= budget || shutdown.load(Ordering::Relaxed) => {
                interrupted = shutdown.load(Ordering::Relaxed);
                timed_out = !interrupted;
                let _ = child.kill();
                let status = child.wait().ok();
                // The local client is gone; the agent inside the sandbox is
                // not. The in-sandbox sweep kills every agent process, so it
                // is only safe when no other session shares the sandbox
                // (concurrent parks rely on lease eviction instead: the stray
                // keeps thinking until its own limits, but the next round's
                // claim locks it out of the park).
                if kill_on_timeout {
                    kill_stray_agents(contender.sandbox(args));
                }
                break status;
            }
            Ok(None) => std::thread::sleep(Duration::from_secs(1)),
            Err(e) => return SessionResult::failed(format!("waiting on agent session: {e}")),
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
    if spend_before.is_some() && usage.get("cost_usd").and_then(Value::as_f64).unwrap_or(0.0) == 0.0
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
    let error = if interrupted {
        Some("run interrupted by a signal; scoring whatever it built".to_string())
    } else if timed_out {
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
    control: &mut mcp::McpClient,
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
        Ok(best) => (best, true),
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

    // Server-side writes from here, hence absolute paths: the game is a
    // separate process with its own working directory. Control plane, not the
    // agents' MCP endpoint, because these write this filesystem.
    let dir = std::fs::canonicalize(round_dir)
        .map_err(|e| format!("resolve {}: {e}", round_dir.display()))?;
    // Picture the scored coaster, cropped to the track: `best` when the score
    // came from an earlier (possibly demolished) build, otherwise the park as
    // it stands. The agent's own screenshot tool renders the whole map, which
    // is right for building and wrong for a thumbnail.
    let shot_path = dir.join("park.png");
    control
        .call(
            "capture_park",
            json!({"path": shot_path, "best": scored_from_best}),
        )
        .map_err(|e| format!("capture scored park: {e}"))?;

    // Save the same candidate as the report and picture, so the result can be
    // reopened and checked rather than taken on the report's word.
    let park_path = dir.join("park.park");
    control
        .call(
            "save_park",
            json!({"path": park_path, "best": scored_from_best}),
        )
        .map_err(|e| format!("save scored park: {e}"))?;
    // Saved before the replay, because capturing one ticks the simulation on.
    if args.replay_seconds > 0 {
        if let Err(e) = capture_replay(
            control,
            round_dir,
            replay_seconds(&report, args),
            scored_from_best,
        ) {
            eprintln!("  replay failed: {e}");
        }
    }
    Ok(report)
}

/// Records the running ride as a video: frames from the game, encoded by
/// ffmpeg, frames thrown away.
///
/// The camera is the track's bounding box and never moves, so consecutive
/// frames differ only where the train is. That is what makes this cheap to
/// keep: measured on a test circuit, 20 seconds came to 255 KB, and ten times
/// the frames cost under four times the bytes. The PNGs in between are large
/// (13 MB for those 400 frames) and are deleted once encoded.
/// How long to record: the lap time the game itself measured during the test
/// ("Ride time" in the ride window, the sum of the stations' segment times),
/// plus a little so the train is seen arriving rather than cut off mid-air.
/// Falls back to the cap when the ride never completed a test and so has no
/// measured time.
fn replay_seconds(report: &Value, args: &Args) -> u32 {
    const TAIL: i64 = 3;
    let lap = report
        .get("rides")
        .and_then(Value::as_array)
        .and_then(|rides| {
            rides
                .iter()
                .filter_map(|r| r.get("ride_time")?.as_i64())
                .max()
        })
        .unwrap_or(0);
    if lap <= 0 {
        return args.replay_seconds;
    }
    u32::try_from(lap + TAIL)
        .unwrap_or(args.replay_seconds)
        .min(args.replay_seconds)
}

fn capture_replay(
    control: &mut mcp::McpClient,
    round_dir: &Path,
    seconds: u32,
    best: bool,
) -> Result<(), String> {
    const FPS: u32 = 20;
    // 40 game ticks make a second, so every other tick is 20fps.
    let frames = seconds * FPS;
    let out = std::fs::canonicalize(round_dir)
        .map(|d| d.join("replay.mp4"))
        .map_err(|e| format!("resolve {}: {e}", round_dir.display()))?;
    // The game renders straight into ffmpeg, so there are no frames on disk to
    // clean up and nothing is PNG-encoded on the way.
    control.call(
        "capture_replay",
        json!({"out": out, "frames": frames, "every_ticks": 2, "zoom": 0, "best": best}),
    )?;
    Ok(())
}

fn write_json(path: &Path, value: &Value) -> Result<(), String> {
    let text = serde_json::to_string_pretty(value).map_err(|e| e.to_string())?;
    std::fs::write(path, text).map_err(|e| format!("{}: {e}", path.display()))
}

fn sha256_file(path: &Path) -> Result<String, String> {
    use std::io::Read;

    let mut file = File::open(path).map_err(|e| format!("open {}: {e}", path.display()))?;
    let mut hasher = Sha256::new();
    let mut buffer = [0u8; 64 * 1024];
    loop {
        let read = file
            .read(&mut buffer)
            .map_err(|e| format!("read {}: {e}", path.display()))?;
        if read == 0 {
            break;
        }
        hasher.update(&buffer[..read]);
    }
    Ok(format!("{:x}", hasher.finalize()))
}

fn sha256_bytes(bytes: &[u8]) -> String {
    let mut hasher = Sha256::new();
    hasher.update(bytes);
    format!("{:x}", hasher.finalize())
}

fn command_stdout(program: &str, args: &[&str], cwd: &Path) -> Option<String> {
    let output = Command::new(program)
        .args(args)
        .current_dir(cwd)
        .output()
        .ok()?;
    output
        .status
        .success()
        .then(|| String::from_utf8_lossy(&output.stdout).trim().to_string())
}

fn git_provenance(root: &Path) -> Value {
    let commit = command_stdout("git", &["rev-parse", "HEAD"], root);
    let diff = Command::new("git")
        .args([
            "diff",
            "--binary",
            "HEAD",
            "--",
            "rust/coaster-bench",
            "rust/orct2-agent",
            "src/openrct2/rustbridge",
            "src/openrct2/command_line/EvalCommands.cpp",
            "src/openrct2/interface/Screenshot.cpp",
        ])
        .current_dir(root)
        .output()
        .ok()
        .filter(|output| output.status.success())
        .map(|output| output.stdout)
        .unwrap_or_default();
    json!({
        "commit": commit,
        "dirty": !diff.is_empty(),
        "dirty_diff_sha256": (!diff.is_empty()).then(|| sha256_bytes(&diff)),
    })
}

fn image_id(image: &str, root: &Path) -> Option<String> {
    ["podman", "docker"].iter().find_map(|runtime| {
        command_stdout(
            runtime,
            &["image", "inspect", "--format", "{{.Id}}", image],
            root,
        )
        .filter(|id| !id.is_empty())
    })
}

fn bounded_output(mut command: Command, budget: Duration) -> Option<std::process::Output> {
    let mut child = command
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .ok()?;
    let deadline = Instant::now() + budget;
    loop {
        match child.try_wait() {
            Ok(Some(_)) => return child.wait_with_output().ok(),
            Ok(None) if Instant::now() >= deadline => {
                let _ = child.kill();
                let _ = child.wait();
                return None;
            }
            Ok(None) => std::thread::sleep(Duration::from_millis(100)),
            Err(_) => return None,
        }
    }
}

fn agent_version(args: &Args, contender: &Contender) -> Option<String> {
    // Read Node package metadata instead of invoking those agents: notably,
    // `opencode --version` performs network startup and can hang behind a
    // restricted policy.
    let (binary, version_args): (&str, Vec<&str>) = match contender.harness {
        Harness::ClaudeCode => (
            "/usr/local/bin/node",
            vec![
                "-p",
                "require('/usr/local/lib/node_modules/@anthropic-ai/claude-code/package.json').version",
            ],
        ),
        Harness::Opencode => (
            "/usr/local/bin/node",
            vec![
                "-p",
                "require('/usr/local/lib/node_modules/opencode-ai/package.json').version",
            ],
        ),
        Harness::Codex => (args.codex_bin.as_str(), vec!["--version"]),
    };
    let mut command = Command::new("openshell");
    command
        .args(["sandbox", "exec", "-n", contender.sandbox(args), "--"])
        .arg(binary)
        .args(version_args);
    let output = bounded_output(command, Duration::from_secs(15))?;
    if !output.status.success() {
        return None;
    }
    let stdout = String::from_utf8_lossy(&output.stdout).trim().to_string();
    let stderr = String::from_utf8_lossy(&output.stderr).trim().to_string();
    Some(if stdout.is_empty() { stderr } else { stdout }).filter(|v| !v.is_empty())
}

fn best_excitement(report: &Value) -> Option<f64> {
    // Interactive reports carry the scorer's authoritative value. Historical
    // and whole-program driver reports predate it, so retain the exact fallback
    // formula for those artifacts.
    if let Some(score) = report.get("score").and_then(Value::as_f64) {
        return Some(score);
    }
    let rides = report.get("rides")?.as_array()?;
    let raw = rides
        .iter()
        .filter_map(|r| r.get("excitement").and_then(Value::as_f64))
        .reduce(f64::max)?;
    // Same penalty math as driver.py (SIMILARITY_GRACE = 0.5).
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

/// One generated scenario park a multi-scenario run competes on.
struct ScenarioPark {
    /// Directory / display label, e.g. "seed_42".
    label: String,
    path: PathBuf,
    /// Prompt map line rendered from the generator's hints sidecar.
    map_line: String,
}

/// First N seeds of the committed list.
fn load_seeds(root: &Path, count: u32) -> Result<Vec<u64>, String> {
    let path = root.join("evals/scenarios/seeds.json");
    let text = std::fs::read_to_string(&path).map_err(|e| format!("{}: {e}", path.display()))?;
    let value: Value =
        serde_json::from_str(&text).map_err(|e| format!("{}: {e}", path.display()))?;
    let seeds: Vec<u64> = value
        .get("seeds")
        .and_then(Value::as_array)
        .map(|a| a.iter().filter_map(Value::as_u64).collect())
        .unwrap_or_default();
    if (count as usize) > seeds.len() {
        return Err(format!(
            "--scenarios {count} but {} only lists {} seeds",
            path.display(),
            seeds.len()
        ));
    }
    Ok(seeds[..count as usize].to_vec())
}

/// Generates (or reuses) the deterministic park for a seed and renders its
/// prompt map line from the hints sidecar. Same seed always gives the same
/// bytes, so the gitignored cache never goes stale; --make-park needs no RCT2
/// assets.
fn ensure_generated_park(root: &Path, seed: u64) -> Result<ScenarioPark, String> {
    let dir = root.join("evals/scenarios/generated");
    std::fs::create_dir_all(&dir).map_err(|e| e.to_string())?;
    let park = dir.join(format!("seed_{seed}.park"));
    let hints_path = dir.join(format!("seed_{seed}.hints.json"));
    if !park.is_file() || !hints_path.is_file() {
        let output = Command::new(root.join("build/openrct2-cli"))
            .args(["eval", "--make-park"])
            .arg(&park)
            .args(["--seed", &seed.to_string()])
            .output()
            .map_err(|e| format!("run --make-park: {e}"))?;
        if !park.is_file() || !hints_path.is_file() {
            return Err(format!(
                "park generation failed for seed {seed}: {}",
                String::from_utf8_lossy(&output.stderr).trim()
            ));
        }
    }
    let hints: Value =
        serde_json::from_str(&std::fs::read_to_string(&hints_path).map_err(|e| e.to_string())?)
            .map_err(|e| format!("{}: {e}", hints_path.display()))?;
    Ok(ScenarioPark {
        label: format!("seed_{seed}"),
        path: park,
        map_line: map_line_from_hints(&hints),
    })
}

/// Renders the generator's machine hints into the prompt map line. Keep the
/// shape in sync with map_line_from_hints in evals/driver.py.
fn map_line_from_hints(hints: &Value) -> String {
    let int = |ptr: &str| hints.pointer(ptr).and_then(Value::as_i64).unwrap_or(0);
    let size = int("/map_size");
    let mut parts = vec![format!(
        "a generated park, {size}x{size} tiles (playable {}-{} on both axes).",
        int("/playable/min"),
        int("/playable/max")
    )];
    let square = int("/start/open_square");
    let anchor = if square > 0 {
        format!(
            "a {square}x{square}-tile dry square centred on tile ({}, {})",
            int("/start/x"),
            int("/start/y")
        )
    } else {
        format!("around tile ({}, {})", int("/start/x"), int("/start/y"))
    };
    parts.push(format!(
        "The most open ground is {anchor} - anchor your station there and grow the layout outward, \
         using placement errors and valid_next_pieces to feel out the terrain."
    ));
    let hilliness = int("/hilliness");
    if hilliness == 0 {
        parts.push("The terrain is entirely flat.".to_string());
    } else {
        let roughness = match hilliness {
            1..=3 => "gently rolling",
            4..=6 => "hilly",
            _ => "mountainous",
        };
        parts.push(format!(
            "Terrain is {roughness}: surface heights span {}-{} z-units (one up_25 piece climbs 16 z-units).",
            int("/surface_z/min"),
            int("/surface_z/max")
        ));
    }
    let water = hints
        .get("water_fraction")
        .and_then(Value::as_f64)
        .unwrap_or(0.0);
    if water > 0.01 {
        parts.push(format!(
            "About {}% of the map is under water; you cannot build below the waterline.",
            (water * 100.0).round() as i64
        ));
    }
    parts.join(" ")
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

fn read_json(path: &Path) -> Result<Value, String> {
    let raw = std::fs::read_to_string(path).map_err(|e| format!("read {}: {e}", path.display()))?;
    serde_json::from_str(&raw).map_err(|e| format!("parse {}: {e}", path.display()))
}

fn archived_contender(round_dir: &Path) -> Result<Contender, String> {
    let usage = read_json(&round_dir.join("usage.json"))?;
    let model = usage
        .get("model")
        .and_then(Value::as_str)
        .ok_or("archived usage.json has no model")?;
    let harness = usage
        .get("harness")
        .and_then(Value::as_str)
        .ok_or("archived usage.json has no harness")?;
    let spec = match harness {
        "codex" => format!("codex:{model}"),
        "opencode" => format!("opencode:{model}"),
        "claude-code" => model.to_string(),
        other => return Err(format!("unsupported archived harness: {other}")),
    };
    Ok(Contender::parse(&spec))
}

fn archived_run_path(round_dir: &Path) -> Result<PathBuf, String> {
    let model_dir = round_dir
        .parent()
        .ok_or("round directory has no model parent")?;
    let run_dir = model_dir
        .parent()
        .ok_or("round directory has no run parent")?;
    let path = run_dir.join("run.json");
    path.is_file()
        .then_some(path)
        .ok_or_else(|| "archived run.json not found two levels above round".to_string())
}

fn use_archived_settings(args: &mut Args, run: &Value) {
    if let Some(scenario) = run.pointer("/scenario/path").and_then(Value::as_str) {
        args.scenario = scenario.to_string();
    }
    if let Some(ticks) = run.get("ticks").and_then(Value::as_u64) {
        args.ticks = ticks.min(u32::MAX as u64) as u32;
    }
    if let Some(seconds) = run
        .pointer("/budgets/replay_cap_seconds")
        .and_then(Value::as_u64)
    {
        args.replay_seconds = seconds.min(u32::MAX as u64) as u32;
    }
    if let Some(effort) = run.get("codex_reasoning_effort").and_then(Value::as_str) {
        args.codex_reasoning_effort = match effort {
            "low" => CodexReasoningEffort::Low,
            "high" => CodexReasoningEffort::High,
            "xhigh" => CodexReasoningEffort::Xhigh,
            "max" => CodexReasoningEffort::Max,
            "ultra" => CodexReasoningEffort::Ultra,
            _ => CodexReasoningEffort::Medium,
        };
    }
}

fn presentation_prompt(model: &str, source_report: &Value) -> String {
    let ride = source_report
        .get("rides")
        .and_then(Value::as_array)
        .and_then(|rides| rides.first())
        .cloned()
        .unwrap_or(Value::Null);
    format!(
        "This is a presentation-only epilogue for the roller coaster you already designed in a \
         completed CoasterBench run. You are {model}. The layout and its score are frozen. Only \
         three coaster tools are available: inspect best_result, inspect best_screenshot, then call \
         style_best_ride exactly once with a distinctive final name (32 characters maximum) and a \
         cohesive track, rail, and support colour scheme that suits what you built. Do not ask for \
         confirmation; make the creative decision yourself. End with one sentence explaining the \
         name and palette.\n\nOriginal ride measurements: {ride}"
    )
}

fn write_presentation_usage(
    path: &Path,
    contender: &Contender,
    session: &SessionResult,
) -> Result<(), String> {
    write_json(
        path,
        &json!({
            "harness": contender.harness.name(),
            "model": contender.model,
            "input_tokens": session.usage.get("input_tokens"),
            "output_tokens": session.usage.get("output_tokens"),
            "cache_read_tokens": session.usage.get("cache_read_tokens"),
            "cost_usd": session.usage.get("cost_usd"),
            "num_turns": session.usage.get("num_turns"),
        }),
    )
}

struct PresentationStage(PathBuf);

impl Drop for PresentationStage {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

/// Reconstructs an archived winner, proves it still has the recorded score,
/// then gives the original model a capability-limited naming/colour pass.
/// Artifacts are staged and replace the published set only after every capture
/// succeeds, so a failed model turn cannot damage the scored result.
fn present_archived_round(
    args: &mut Args,
    root: &Path,
    round_arg: &Path,
    shutdown: &AtomicBool,
) -> Result<(), String> {
    let round_dir = std::fs::canonicalize(round_arg)
        .map_err(|e| format!("resolve {}: {e}", round_arg.display()))?;
    let program = read_json(&round_dir.join("program.json"))?;
    let source_report_path = round_dir.join("report.json");
    let source_report = read_json(&source_report_path)?;
    let source_score =
        best_excitement(&source_report).ok_or("archived report has no scored ride")?;
    let source_report_sha256 = sha256_file(&source_report_path)?;
    let source_park_sha256 = sha256_file(&round_dir.join("park.park")).ok();
    let run = read_json(&archived_run_path(&round_dir)?)?;
    use_archived_settings(args, &run);
    if let Some(expected) = run.pointer("/scenario/sha256").and_then(Value::as_str) {
        let scenario = expand_home(&args.scenario);
        let actual = sha256_file(&scenario)?;
        if actual != expected {
            return Err(format!(
                "scenario hash mismatch for {}: archived {expected}, current {actual}",
                scenario.display()
            ));
        }
    }
    args.mode = BenchmarkMode::Presentation;
    args.open_note = false;
    args.extra_tools = None;

    let contender = archived_contender(&round_dir)?;
    let model = contender.display();
    println!(
        "presentation pass: {model}, archived score {source_score:.2}, {}",
        round_dir.display()
    );

    let _server = GameServer::spawn(
        args,
        root,
        &expand_home(&args.scenario),
        args.port,
        args.control_port,
    )?;
    let mut client = mcp::McpClient::new("127.0.0.1", args.port);
    wait_for_server(&mut client, Duration::from_secs(120))?;
    let mut control = mcp::McpClient::new("127.0.0.1", args.control_port);
    wait_for_server(&mut control, Duration::from_secs(120))?;

    let lease = format!("present-{model}-{}", epoch_secs());
    client.claim("127.0.0.1", args.port, &lease);
    let start = program
        .get("start")
        .ok_or("archived program has no start")?;
    client.call(
        "new_ride",
        json!({
            "ride_type": program.get("ride_type").and_then(Value::as_u64)
                .ok_or("archived program has no ride_type")?,
            "x": start.get("x").and_then(Value::as_i64).ok_or("program start has no x")?,
            "y": start.get("y").and_then(Value::as_i64).ok_or("program start has no y")?,
            "dir": start.get("dir").and_then(Value::as_i64).ok_or("program start has no dir")?,
        }),
    )?;
    let pieces = program
        .get("pieces")
        .and_then(Value::as_array)
        .ok_or("archived program has no piece array")?;
    let placed = client.call("place_pieces", json!({"pieces": pieces}))?;
    let placed_count = placed
        .get("placed_this_call")
        .and_then(Value::as_u64)
        .unwrap_or(0) as usize;
    if placed_count != pieces.len() {
        return Err(format!(
            "archived reconstruction placed {placed_count}/{} pieces: {placed}",
            pieces.len()
        ));
    }
    let rebuilt = client.call("finish_and_test", json!({"ticks": args.ticks}))?;
    let rebuilt_score =
        best_excitement(&rebuilt).ok_or("archived reconstruction did not produce a score")?;
    if (rebuilt_score - source_score).abs() > 0.005 {
        return Err(format!(
            "archived score mismatch: recorded {source_score:.2}, rebuilt {rebuilt_score:.2}; \
             refusing to restyle different ride physics"
        ));
    }
    println!("reconstruction verified at {rebuilt_score:.2}");

    let presentation_dir = round_dir.join("presentation");
    std::fs::create_dir_all(&presentation_dir)
        .map_err(|e| format!("create {}: {e}", presentation_dir.display()))?;
    reset_agent_state(contender.sandbox(args))?;
    let version = agent_version(args, &contender);
    let prompt = presentation_prompt(&model, &source_report);
    let session = run_agent_session(
        args,
        &contender,
        args.port,
        &prompt,
        &presentation_dir,
        &lease,
        true,
        shutdown,
    );
    write_presentation_usage(&presentation_dir.join("usage.json"), &contender, &session)?;

    let styled_report = client.call("best_result", json!({}))?;
    let presentation = styled_report.get("presentation").cloned().ok_or_else(|| {
        format!(
            "model did not style the ride{}",
            session
                .error
                .as_deref()
                .map(|e| format!(": {e}"))
                .unwrap_or_default()
        )
    })?;
    let styled_score = best_excitement(&styled_report).ok_or("styled report lost its score")?;
    if (styled_score - source_score).abs() > f64::EPSILON {
        return Err(format!(
            "presentation changed the score from {source_score} to {styled_score}"
        ));
    }
    if args.replay_seconds == 0 {
        return Err("presentation pass requires replay capture to refresh coaster colours".into());
    }

    let stage = round_dir.join(format!(
        ".presentation-stage-{}-{}",
        std::process::id(),
        epoch_secs()
    ));
    std::fs::create_dir(&stage).map_err(|e| format!("create {}: {e}", stage.display()))?;
    let _stage_cleanup = PresentationStage(stage.clone());
    write_json(&stage.join("report.json"), &styled_report)?;
    control.call(
        "capture_park",
        json!({"path": stage.join("park.png"), "best": true}),
    )?;
    control.call(
        "save_park",
        json!({"path": stage.join("park.park"), "best": true}),
    )?;
    capture_replay(
        &mut control,
        &stage,
        replay_seconds(&styled_report, args),
        true,
    )?;
    for name in [
        "report.json",
        "park.png",
        "park.park",
        "replay.mp4",
        "replay.png",
        "replay.json",
    ] {
        let staged = stage.join(name);
        if !staged.is_file() {
            return Err(format!(
                "presentation artifact missing: {}",
                staged.display()
            ));
        }
    }
    for name in [
        "report.json",
        "park.png",
        "park.park",
        "replay.mp4",
        "replay.png",
        "replay.json",
    ] {
        std::fs::rename(stage.join(name), round_dir.join(name))
            .map_err(|e| format!("install presentation artifact {name}: {e}"))?;
    }
    std::fs::remove_dir(&stage).map_err(|e| format!("remove {}: {e}", stage.display()))?;

    let provenance = json!({
        "kind": "posthoc-presentation",
        "model": contender.model,
        "harness": contender.harness.name(),
        "agent_version": version,
        "reasoning_effort": (contender.harness == Harness::Codex)
            .then(|| args.codex_reasoning_effort.as_str()),
        "score_before": source_score,
        "score_after": styled_score,
        "source_report_sha256": source_report_sha256,
        "source_park_sha256": source_park_sha256,
        "styled_report_sha256": sha256_file(&round_dir.join("report.json"))?,
        "styled_park_sha256": sha256_file(&round_dir.join("park.park"))?,
        "presentation": presentation,
        "session_error": session.error,
        "completed_at": rfc3339_now(),
    });
    write_json(&presentation_dir.join("pass.json"), &provenance)?;
    println!("model-authored presentation committed locally: {presentation}");
    Ok(())
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
    let mut args = Args::parse();
    if args.control_port == 0 {
        return Err(
            "--control-port must be non-zero: round reset and artifact collection require it"
                .into(),
        );
    }
    if args.control_port == args.port {
        return Err("agent and control ports must be different".into());
    }
    if args.open_note {
        if args.mode == BenchmarkMode::Library {
            return Err(
                "--open-note cannot be combined with --mode library; use one benchmark condition"
                    .into(),
            );
        }
        args.mode = BenchmarkMode::OpenNote;
    }
    args.open_note = args.mode == BenchmarkMode::OpenNote;
    let root = repo_root();
    let shutdown = install_shutdown_flag()?;
    if let Some(round_dir) = args.present_round.clone() {
        return present_archived_round(&mut args, &root, &round_dir, &shutdown);
    }

    // Debug aid: print the round-1 prompt verbatim and exit.
    if std::env::var_os("COASTER_BENCH_PRINT_PROMPT").is_some() {
        print!(
            "{}",
            prompt::round_prompt(&prompt::Round {
                ride_type: args.ride_type,
                round: 1,
                rounds: args.rounds,
                previous_feedback: None,
                modalities: Modalities::TEXT | Modalities::IMAGE,
                budget_secs: args.session_timeout,
                open_note_dir: args.open_note.then_some(OPEN_NOTE_DIR),
                single_turn: false,
                map_line: None,
            })
        );
        return Ok(());
    }
    if args.attach && args.scenarios > 0 {
        return Err("--attach reuses one running server; --scenarios needs one per park".into());
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

    let contenders: Vec<Contender> = args.models.iter().map(|s| Contender::parse(s)).collect();
    let mut _ephemeral: Vec<EphemeralSandbox> = Vec::new();
    let codex_providers = if contenders
        .iter()
        .any(|c| c.codex_openrouter_model().is_some())
    {
        format!("{},openrouter", args.codex_sandbox_provider)
    } else {
        args.codex_sandbox_provider.clone()
    };
    if args.fresh_sandbox {
        let lanes = [
            (
                Harness::ClaudeCode,
                SandboxRecipe {
                    image: &args.sandbox_image,
                    policy: &args.sandbox_policy,
                    provider: &args.sandbox_provider,
                },
                args.sandbox.clone(),
            ),
            (
                Harness::Opencode,
                SandboxRecipe {
                    image: &args.opencode_sandbox_image,
                    policy: &args.opencode_sandbox_policy,
                    provider: &args.opencode_sandbox_provider,
                },
                args.opencode_sandbox.clone(),
            ),
            (
                Harness::Codex,
                SandboxRecipe {
                    image: &args.codex_sandbox_image,
                    policy: &args.codex_sandbox_policy,
                    provider: &codex_providers,
                },
                args.codex_sandbox.clone(),
            ),
        ];
        let mut renamed: Vec<(Harness, String)> = Vec::new();
        for (harness, recipe, base) in &lanes {
            if !contenders.iter().any(|c| c.harness == *harness) {
                continue;
            }
            let name = ephemeral_sandbox_name(base, epoch_secs());
            println!("creating sandbox {name} from {}", recipe.image);
            _ephemeral.push(create_sandbox(recipe, args.port, &root, &name)?);
            renamed.push((*harness, name));
        }
        for (harness, name) in renamed {
            match harness {
                Harness::ClaudeCode => args.sandbox = name,
                Harness::Opencode => args.opencode_sandbox = name,
                Harness::Codex => args.codex_sandbox = name,
            }
        }
    }
    // Contenders share sandboxes, so per-sandbox setup runs once each.
    let sandboxes: BTreeSet<&str> = contenders.iter().map(|c| c.sandbox(&args)).collect();
    // A previous run killed from the outside can leave an agent alive in its
    // sandbox, still building in the park this run is about to use.
    for sandbox in &sandboxes {
        kill_stray_agents(sandbox);
    }
    for sandbox in contenders
        .iter()
        .map(|c| c.sandbox(&args))
        .collect::<std::collections::BTreeSet<_>>()
    {
        reset_agent_state(sandbox)?;
        println!("reset agent state in {sandbox}");
    }
    let open_note_sha = if args.open_note {
        let sha = upstream_merge_base(&root)?;
        for sandbox in &sandboxes {
            stage_open_note(&root, sandbox, &sha)?;
            println!(
                "open note: {OPEN_NOTE_DIR} in {sandbox} at upstream {}",
                &sha[..12]
            );
        }
        Some(sha)
    } else {
        None
    };
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
    // Multi-scenario runs generate their parks up front so run.json can
    // record what was promised before anything competes.
    let seeds = if args.scenarios > 0 {
        load_seeds(&root, args.scenarios)?
    } else {
        Vec::new()
    };
    if !seeds.is_empty() {
        println!("generating {} scenario park(s)...", seeds.len());
    }
    let scenario_parks: Vec<ScenarioPark> = seeds
        .iter()
        .map(|seed| ensure_generated_park(&root, *seed))
        .collect::<Result<_, _>>()?;

    let mut capabilities = Vec::new();
    if args.mode == BenchmarkMode::Library {
        capabilities.push("track_library");
    }
    if args.open_note {
        capabilities.extend(["engine_source", "file_tools", "python3", "shell"]);
    }
    let model_specs: Value = contenders
        .iter()
        .map(|contender| {
            (
                contender.display(),
                json!({
                    "id": contender.model,
                    "harness": contender.harness.name(),
                    "provider": contender.provider(&args),
                    "modalities": contender.modalities.as_query_value(),
                    "agent_version": agent_version(&args, contender),
                    "reasoning_effort": (contender.harness == Harness::Codex)
                        .then(|| args.codex_reasoning_effort.as_str()),
                }),
            )
        })
        .collect::<serde_json::Map<_, _>>()
        .into();
    // Single-park runs record the scenario file; multi-scenario runs record
    // the generated parks (regenerable per seed) instead of one file hash.
    let scenario_json = if scenario_parks.is_empty() {
        let scenario_path = expand_home(&args.scenario);
        json!({
            "path": args.scenario,
            "sha256": sha256_file(&scenario_path)?,
        })
    } else {
        json!({
            "generated": true,
            "scenarios": scenario_parks.iter().map(|p| p.label.clone()).collect::<Vec<_>>(),
            "seeds": seeds,
        })
    };
    let sandbox_images = json!({
        "claude_code": {
            "reference": args.sandbox_image,
            "image_id": image_id(&args.sandbox_image, &root),
        },
        "opencode": {
            "reference": args.opencode_sandbox_image,
            "image_id": image_id(&args.opencode_sandbox_image, &root),
        },
        "codex": {
            "reference": args.codex_sandbox_image,
            "image_id": image_id(&args.codex_sandbox_image, &root),
        },
    });
    let mut run_meta = json!({
            "schema_version": RUN_SCHEMA_VERSION,
            "condition": args.mode.condition_name(),
            "mode": args.mode.base_mode(),
            "orchestrator": "coaster-bench",
            "harness_version": env!("CARGO_PKG_VERSION"),
            "scorer": {
                "name": "adjusted_excitement",
                "version": SCORER_VERSION,
                "similarity_grace": 0.5,
            },
            "source": git_provenance(&root),
            "harness": run_harness,
            "harnesses": harnesses,
            "models": contenders.iter().map(Contender::display).collect::<Vec<_>>(),
            "model_specs": model_specs,
            "modalities": modalities,
            "rounds": args.rounds,
            "ticks": args.ticks,
            "ride_type": args.ride_type,
            "no_graphics": args.no_graphics,
            "similarity_grace": 0.5,
            "scenario": scenario_json,
            "budgets": {
                "rounds": args.rounds,
                "simulation_ticks": args.ticks,
                "session_timeout_seconds": args.session_timeout,
                "max_agent_turns": args.max_turns,
                "replay_cap_seconds": args.replay_seconds,
                "replay_fps": 20,
            },
            "environment": {
                "os": std::env::consts::OS,
                "arch": std::env::consts::ARCH,
                "rustc": command_stdout("rustc", &["--version"], &root),
                "sandbox_images": sandbox_images,
            },
            "open_note": args.open_note,
            // What the condition actually granted, so a run explains itself
            // without cross-referencing the harness version.
            "capabilities": capabilities,
            "agent_state": if args.fresh_sandbox { "fresh-sandbox" } else { "reset-per-run" },
            "round_reset": "baseline-park-snapshot",
            "artifact_integrity": "sha256",
            "sandbox": args.sandbox,
            "opencode_sandbox": args.opencode_sandbox,
            "codex_sandbox": args.codex_sandbox,
            "allowed_tools": allowed_tools(&args),
            "opencode_permission": opencode_permissions(&args),
            "codex_sandbox_mode": codex_sandbox_mode(&args),
            "codex_reasoning_effort": args.codex_reasoning_effort.as_str(),
            "open_note_source": open_note_sha,
    });
    if !scenario_parks.is_empty() {
        run_meta["scenarios"] = json!(scenario_parks
            .iter()
            .map(|p| p.label.clone())
            .collect::<Vec<_>>());
        run_meta["seeds"] = json!(seeds);
    }
    write_json(&run_dir.join("run.json"), &run_meta)?;

    if !scenario_parks.is_empty() {
        return run_multi_scenario(
            &args,
            &root,
            &contenders,
            &scenario_parks,
            &run_dir,
            &shutdown,
        );
    }

    let _server = if args.attach {
        None
    } else {
        Some(GameServer::spawn(
            &args,
            &root,
            &expand_home(&args.scenario),
            args.port,
            args.control_port,
        )?)
    };
    let mut client = mcp::McpClient::new("127.0.0.1", args.port);
    wait_for_server(&mut client, Duration::from_secs(120))?;
    let mut control = mcp::McpClient::new("127.0.0.1", args.control_port);
    wait_for_server(&mut control, Duration::from_secs(120))?;
    println!("game server ready on port {}", args.port);

    let mut standings: Vec<Value> = Vec::new();
    for contender in &contenders {
        if shutdown.load(Ordering::Relaxed) {
            eprintln!("interrupted: stopping before {}", contender.display());
            break;
        }
        standings.push(compete_on_park(
            &args,
            contender,
            &mut client,
            &mut control,
            args.port,
            &run_dir,
            None,
            true,
            &shutdown,
        ));
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
        &json!({"standings": standings}),
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

/// Fans contenders out across generated scenario parks in waves of
/// --concurrency: each wave slot is its own game server on port base+slot
/// (ports are reused across waves, so the sandbox policy only needs
/// `concurrency` consecutive ports). Contenders on the same park run
/// sequentially — they share its server and its single game thread.
fn run_multi_scenario(
    args: &Args,
    root: &Path,
    contenders: &[Contender],
    scenario_parks: &[ScenarioPark],
    run_dir: &Path,
    shutdown: &AtomicBool,
) -> Result<(), String> {
    let concurrency = args.concurrency.max(1);
    println!(
        "{} scenario park(s) in waves of {}; sandbox policy must allow ports {}-{}",
        scenario_parks.len(),
        concurrency,
        args.port,
        args.port + concurrency as u16 - 1
    );
    if concurrency > 1 {
        println!(
            "note: concurrent sessions share the sandboxes, so a timed-out session's \
             stray agent is locked out by lease eviction rather than killed"
        );
    }

    let mut outcomes: Vec<Value> = Vec::new();
    for wave in scenario_parks.chunks(concurrency) {
        let results: Vec<Result<Vec<Value>, String>> = std::thread::scope(|scope| {
            let handles: Vec<_> = wave
                .iter()
                .enumerate()
                .map(|(slot, park)| {
                    let port = args.port + slot as u16;
                    // Control ports pair with serve ports slot by slot, so the
                    // policy needs the same count of consecutive control ports.
                    let control_port = args.control_port + slot as u16;
                    scope.spawn(move || -> Result<Vec<Value>, String> {
                        let _server =
                            GameServer::spawn(args, root, &park.path, port, control_port)?;
                        let mut client = mcp::McpClient::new("127.0.0.1", port);
                        wait_for_server(&mut client, Duration::from_secs(120))?;
                        let mut control = mcp::McpClient::new("127.0.0.1", control_port);
                        wait_for_server(&mut control, Duration::from_secs(120))?;
                        println!("[{}] game server ready on port {port}", park.label);
                        Ok(contenders
                            .iter()
                            .map(|contender| {
                                compete_on_park(
                                    args,
                                    contender,
                                    &mut client,
                                    &mut control,
                                    port,
                                    run_dir,
                                    Some(park),
                                    concurrency == 1,
                                    shutdown,
                                )
                            })
                            .collect())
                    })
                })
                .collect();
            handles
                .into_iter()
                .map(|handle| {
                    handle
                        .join()
                        .unwrap_or_else(|_| Err("scenario thread panicked".into()))
                })
                .collect()
        });
        for result in results {
            match result {
                Ok(mut entries) => outcomes.append(&mut entries),
                // A dead park scores nothing for anyone; the aggregate treats
                // its missing entries as zero, same as a park nobody solved.
                Err(e) => eprintln!("scenario park failed: {e}"),
            }
        }
    }

    let mut standings: Vec<Value> = contenders
        .iter()
        .map(|contender| {
            let model = contender.display();
            let per_scenario: Vec<Value> = scenario_parks
                .iter()
                .map(|park| {
                    outcomes
                        .iter()
                        .find(|o| {
                            o["model"].as_str() == Some(model.as_str())
                                && o["scenario"].as_str() == Some(park.label.as_str())
                        })
                        .cloned()
                        .unwrap_or_else(|| {
                            json!({
                                "model": model,
                                "scenario": park.label,
                                "best_excitement": null,
                                "best_round": null,
                            })
                        })
                })
                .collect();
            // A park with no rated coaster scores zero: the aggregate measures
            // reliability across parks, not just the parks that went well.
            let mut bests: Vec<f64> = per_scenario
                .iter()
                .map(|p| p["best_excitement"].as_f64().unwrap_or(0.0))
                .collect();
            let mean = bests.iter().sum::<f64>() / bests.len().max(1) as f64;
            bests.sort_by(f64::total_cmp);
            let median = match bests.len() {
                0 => 0.0,
                n if n % 2 == 1 => bests[n / 2],
                n => (bests[n / 2 - 1] + bests[n / 2]) / 2.0,
            };
            json!({
                "model": model,
                "harness": contender.harness.name(),
                "aggregate": {
                    "mean_best_excitement": mean,
                    "median_best_excitement": median,
                    "scenarios_scored": per_scenario
                        .iter()
                        .filter(|p| p["best_excitement"].as_f64().unwrap_or(0.0) > 0.0)
                        .count(),
                    "scenarios_total": scenario_parks.len(),
                },
                "per_scenario": per_scenario,
            })
        })
        .collect();
    standings.sort_by(|a, b| {
        let mean = |v: &Value| {
            v.pointer("/aggregate/mean_best_excitement")
                .and_then(Value::as_f64)
                .unwrap_or(-1.0)
        };
        mean(b)
            .partial_cmp(&mean(a))
            .unwrap_or(std::cmp::Ordering::Equal)
    });
    write_json(
        &run_dir.join("standings.json"),
        &json!({
            "scenarios": scenario_parks.iter().map(|p| p.label.clone()).collect::<Vec<_>>(),
            "standings": standings,
        }),
    )?;

    println!("\n=== FINAL STANDINGS (aggregate over scenarios) ===");
    for (place, entry) in standings.iter().enumerate() {
        let model = entry["model"].as_str().unwrap_or("?");
        let agg = &entry["aggregate"];
        println!(
            "{}. {model}: mean best excitement {:.2} (median {:.2}, scored {}/{} parks)",
            place + 1,
            agg["mean_best_excitement"].as_f64().unwrap_or(0.0),
            agg["median_best_excitement"].as_f64().unwrap_or(0.0),
            agg["scenarios_scored"].as_u64().unwrap_or(0),
            agg["scenarios_total"].as_u64().unwrap_or(0),
        );
    }
    Ok(())
}

/// One contender's full set of rounds on one park. Returns the standings
/// entry: {model, harness, scenario?, best_excitement, best_round}.
///
/// `kill_on_timeout` must be false whenever another session might share the
/// contender's sandbox (concurrent scenario parks): the timeout sweep kills
/// every agent process in the sandbox, not just this session's.
#[allow(clippy::too_many_arguments)]
fn compete_on_park(
    args: &Args,
    contender: &Contender,
    client: &mut mcp::McpClient,
    control: &mut mcp::McpClient,
    port: u16,
    run_dir: &Path,
    scenario: Option<&ScenarioPark>,
    kill_on_timeout: bool,
    shutdown: &AtomicBool,
) -> Value {
    let model = contender.display();
    let tag = match scenario {
        Some(s) => format!("{model} @ {}", s.label),
        None => model.clone(),
    };
    let mut best: Option<(u32, f64)> = None;
    let mut feedback: Option<String> = None;
    for round in 1..=args.rounds {
        if shutdown.load(Ordering::Relaxed) {
            eprintln!("interrupted: stopping before {tag} round {round}");
            break;
        }
        // Restore the exact pre-round-one scenario before taking the new
        // lease, so clock, RNG, guests, and weather do not depend on
        // contender order.
        if let Err(e) = control.call("reset_park", json!({})) {
            eprintln!("[{tag}] round {round}: reset park failed: {e}");
            break;
        }
        // One lease per round: claiming it evicts any agent still alive
        // from an earlier round, which would otherwise build in this park.
        let lease = format!("{tag}-r{round}-{}", epoch_secs()).replace([' ', '@'], "_");
        client.claim("127.0.0.1", port, &lease);
        let prompt = prompt::round_prompt(&prompt::Round {
            ride_type: args.ride_type,
            round,
            rounds: args.rounds,
            previous_feedback: feedback.as_deref(),
            modalities: contender.modalities,
            budget_secs: args.session_timeout,
            open_note_dir: args.open_note.then_some(OPEN_NOTE_DIR),
            single_turn: contender.harness.single_turn(),
            map_line: scenario.map(|s| s.map_line.as_str()),
        });
        // Created up front: the session streams its logs in here while it
        // runs, so `tail -f` works on a round in progress.
        let mut round_dir = run_dir.join(&model);
        if let Some(s) = scenario {
            round_dir = round_dir.join(&s.label);
        }
        round_dir = round_dir.join(format!("round_{round}"));
        if let Err(e) = std::fs::create_dir_all(&round_dir) {
            eprintln!("[{tag}] round {round}: create {}: {e}", round_dir.display());
            continue;
        }
        println!(
            "[{tag}] round {round}: agent session starting (log: {})",
            round_dir.join("session.log").display()
        );
        let session = run_agent_session(
            args,
            contender,
            port,
            &prompt,
            &round_dir,
            &lease,
            kill_on_timeout,
            shutdown,
        );
        if let Some(e) = &session.error {
            eprintln!("[{tag}] round {round}: {e}");
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
                eprintln!("[{tag}] round {round}: usage write failed: {e}");
            }
        }
        match collect_round(client, control, args, &round_dir) {
            Ok(report) => {
                let score = best_excitement(&report);
                println!(
                    "[{tag}] round {round}: excitement {}",
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
                eprintln!("[{tag}] round {round}: collect failed: {e}");
                feedback = Some(format!(
                    "{{\"error\": \"round produced no rated ride: {e}\"}}"
                ));
            }
        }
    }
    let mut entry = json!({
        "model": model,
        "harness": contender.harness.name(),
        "best_excitement": best.map(|(_, s)| s),
        "best_round": best.map(|(r, _)| r),
    });
    if let Some(s) = scenario {
        entry["scenario"] = json!(s.label);
    }
    entry
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
            .mcp_url(8791, "lease-1", BenchmarkMode::Design)
            .ends_with("/mcp?modalities=text&condition=design&lease=lease-1"));
        assert!(multimodal
            .mcp_url(8791, "lease-1", BenchmarkMode::Library)
            .ends_with("/mcp?modalities=text,image&condition=library&lease=lease-1"));
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
    fn replay_length_follows_the_measured_lap() {
        let args = Args::parse_from(["coaster-bench"]);
        // The game measured the lap, so record that plus a tail rather than a
        // guess: a fixed 20s cut the test oval's 56s circuit off two thirds in.
        let report = json!({"rides": [{"ride_time": 56}]});
        assert_eq!(replay_seconds(&report, &args), 59);

        // Several rides in the park: the longest lap is the one that has to fit.
        let two = json!({"rides": [{"ride_time": 12}, {"ride_time": 40}]});
        assert_eq!(replay_seconds(&two, &args), 43);

        // A ride that never completed a test has no measured time; fall back to
        // the cap rather than recording nothing.
        let untested = json!({"rides": [{"ride_time": 0}]});
        assert_eq!(replay_seconds(&untested, &args), args.replay_seconds);
        assert_eq!(
            replay_seconds(&json!({"rides": []}), &args),
            args.replay_seconds
        );

        // A pathological lap is bounded by the cap.
        let epic = json!({"rides": [{"ride_time": 9999}]});
        assert_eq!(replay_seconds(&epic, &args), args.replay_seconds);
    }

    #[test]
    fn codex_specs_pick_their_backend_from_the_model_id() {
        let oauth = Contender::parse("codex:gpt-5.6-sol");
        assert_eq!(oauth.harness, Harness::Codex);
        assert_eq!(oauth.model, "gpt-5.6-sol");
        assert_eq!(
            oauth.codex_openrouter_model(),
            None,
            "the subscription backend uses its own login"
        );

        let via_openrouter = Contender::parse("codex:openrouter/openai/gpt-5.1");
        assert_eq!(via_openrouter.harness, Harness::Codex);
        assert_eq!(
            via_openrouter.codex_openrouter_model(),
            Some("openai/gpt-5.1"),
            "codex is pointed at OpenRouter with the bare model id"
        );

        assert_eq!(
            Contender::parse("claude-sonnet-5").codex_openrouter_model(),
            None,
            "other harnesses are never the codex backend"
        );
    }

    #[test]
    fn codex_contenders_run_in_the_codex_sandbox() {
        let args = Args::parse_from(["coaster-bench"]);
        assert_eq!(
            Contender::parse("codex:gpt-5.6-sol").sandbox(&args),
            "codex-arena"
        );
    }

    #[test]
    fn archived_presentation_is_a_distinct_capability_condition() {
        let args = Args::try_parse_from([
            "coaster-bench",
            "--present-round",
            "evals/runs/example/model/round_6",
        ])
        .expect("presentation args");
        assert_eq!(
            args.present_round.as_deref(),
            Some(Path::new("evals/runs/example/model/round_6"))
        );
        assert_eq!(BenchmarkMode::Presentation.mcp_condition(), "presentation");
    }

    #[test]
    fn presentation_prompt_freezes_the_layout_and_requires_one_style() {
        let prompt = presentation_prompt(
            "gpt-5.6-sol",
            &json!({"rides": [{"excitement": 7.81, "num_inversions": 5}]}),
        );
        assert!(prompt.contains("layout and its score are frozen"));
        assert!(prompt.contains("style_best_ride exactly once"));
        assert!(prompt.contains("\"excitement\":7.81"));
        assert!(prompt.contains("\"num_inversions\":5"));
    }

    #[test]
    fn codex_config_grants_a_shell_only_when_the_run_does() {
        let mut args = Args::parse_from(["coaster-bench"]);
        assert!(!allowed_tools(&args).contains("Bash"));
        args.open_note = true;
        assert!(allowed_tools(&args).contains("Bash"));

        args.open_note = false;
        args.extra_tools = Some("Bash".into());
        assert!(allowed_tools(&args).contains("Bash"));
    }

    #[test]
    fn codex_config_keeps_top_level_keys_out_of_the_tables() {
        let mut args = Args::parse_from(["coaster-bench"]);
        args.open_note = true;
        let config = codex_config(
            &args,
            &Contender::parse("codex:openrouter/openai/gpt-5.1"),
            "l1",
        );

        let first_table = config.find('[').expect("config has tables");
        for key in [
            "model =",
            "model_provider =",
            "approval_policy =",
            "sandbox_mode =",
            "model_reasoning_effort =",
        ] {
            let at = config.find(key).unwrap_or_else(|| panic!("{key} missing"));
            assert!(at < first_table, "{key} fell inside a table:\n{config}");
        }
        assert!(config.contains("sandbox_mode = \"danger-full-access\""));
        assert!(config.contains("wire_api = \"responses\""), "chat is gone");
        assert!(config.contains("[mcp_servers.coaster]"));
        assert!(config.contains("lease=l1"), "per-round lease reaches codex");
        assert!(
            config.contains("default_tools_approval_mode = \"approve\""),
            "headless codex must not cancel coaster calls for lack of a user-input handler"
        );
    }

    #[test]
    fn codex_subscription_config_has_no_provider_block() {
        let args = Args::parse_from(["coaster-bench"]);
        let config = codex_config(&args, &Contender::parse("codex:gpt-5.6-sol"), "l1");
        assert!(config.starts_with("model = \"gpt-5.6-sol\""));
        assert!(
            config.contains("model_reasoning_effort = \"medium\""),
            "published runs pin the declared effort instead of inheriting low"
        );
        assert!(
            !config.contains("model_provider"),
            "own login, not OpenRouter"
        );
        assert!(
            config.contains("sandbox_mode = \"read-only\""),
            "a blind round gets the tighter mode"
        );
    }

    #[test]
    fn codex_auth_timestamp_is_the_shape_codex_writes() {
        let stamped = rfc3339_now();
        assert_eq!(stamped.len(), 24, "{stamped}");
        assert!(stamped.ends_with(".000Z"), "{stamped}");
        assert_eq!(stamped.as_bytes()[10], b'T', "{stamped}");
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
    fn hints_render_the_anchor_terrain_and_water() {
        let hints = json!({
            "map_size": 100,
            "playable": {"min": 1, "max": 98},
            "hilliness": 5,
            "water_fraction": 0.23,
            "surface_z": {"min": 112, "max": 240},
            "start": {"x": 58, "y": 17, "surface_z": 128, "open_square": 15},
        });
        let line = map_line_from_hints(&hints);
        assert!(line.contains("100x100 tiles (playable 1-98"));
        assert!(line.contains("15x15-tile dry square centred on tile (58, 17)"));
        assert!(line.contains("hilly"));
        assert!(line.contains("112-240 z-units"));
        assert!(line.contains("23% of the map is under water"));
    }

    #[test]
    fn flat_dry_hints_stay_quiet_about_terrain_and_water() {
        let hints = json!({
            "map_size": 100,
            "playable": {"min": 1, "max": 98},
            "hilliness": 0,
            "water_fraction": 0.0,
            "surface_z": {"min": 112, "max": 112},
            "start": {"x": 50, "y": 50, "surface_z": 112, "open_square": 31},
        });
        let line = map_line_from_hints(&hints);
        assert!(line.contains("entirely flat"));
        assert!(!line.contains("under water"));
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
        let authoritative = json!({
            "score": 3.5,
            "rides": [{"excitement": 8.0}],
            "similarity": {"similarity": 0.0}
        });
        assert_eq!(best_excitement(&authoritative), Some(3.5));
    }

    #[test]
    fn unrated_ride_scores_none() {
        let report = json!({"rides": [{"excitement": null}]});
        assert!(best_excitement(&report).is_none());
    }

    #[test]
    fn every_shutdown_signal_raises_the_flag() {
        let flag = install_shutdown_flag().expect("register");
        assert!(!flag.load(Ordering::Relaxed));
        for signal in [
            signal_hook::consts::SIGINT,
            signal_hook::consts::SIGTERM,
            signal_hook::consts::SIGHUP,
        ] {
            flag.store(false, Ordering::Relaxed);
            signal_hook::low_level::raise(signal).expect("raise");
            // Delivery is asynchronous, but to this same process, so it lands
            // almost immediately; poll briefly rather than sleeping blind.
            let mut seen = false;
            for _ in 0..100 {
                if flag.load(Ordering::Relaxed) {
                    seen = true;
                    break;
                }
                std::thread::sleep(Duration::from_millis(10));
            }
            assert!(seen, "signal {signal} did not set the shutdown flag");
        }
    }

    #[test]
    fn open_note_grants_the_file_tools_and_plain_runs_do_not() {
        let mut args = Args::parse_from(["coaster-bench"]);
        assert_eq!(allowed_tools(&args), "mcp__coaster__*");
        args.open_note = true;
        let allowed = allowed_tools(&args);
        for tool in ["Read", "Grep", "Glob", "Bash", "mcp__coaster__*"] {
            assert!(
                allowed.contains(tool),
                "{tool} must be allowed under open note"
            );
        }
    }

    #[test]
    fn policy_port_is_rewritten_to_the_running_port() {
        let policy = "network_policies:\n  coaster_game:\n    endpoints:\n      - host: host.containers.internal\n        port: 8791\n        protocol: rest\n";
        let out = policy_with_port(policy, 8899).expect("valid policy");
        assert!(out.contains("port: 8899"), "port rewritten in place: {out}");
        assert!(
            out.contains("host: host.containers.internal"),
            "rest untouched"
        );
        assert!(!out.contains("8791"));
    }

    #[test]
    fn policy_rewrite_does_not_touch_inference_ports() {
        let policy = "network_policies:\n  coaster_game:\n    endpoints:\n      - host: host.containers.internal\n        port: 8791\n  inference:\n    endpoints:\n      - host: api.openai.com\n        port: 443\n";
        let out = policy_with_port(policy, 1234).expect("valid policy");
        assert!(out.contains("port: 1234"), "{out}");
        assert!(out.contains("port: 443"), "{out}");
        assert!(out.contains("api.openai.com"), "{out}");
    }

    #[test]
    fn policy_rewrite_requires_the_named_game_endpoint() {
        let policy = "process:\n  run_as_user: sandbox\n";
        assert!(policy_with_port(policy, 1234).is_err());
    }

    #[test]
    fn ephemeral_names_fit_the_gateway_limit() {
        let name = ephemeral_sandbox_name("coaster-sub", 1_784_947_247);
        assert!(
            name.len() <= MAX_SANDBOX_NAME,
            "{name} is {} chars",
            name.len()
        );
        assert!(name.starts_with("coaster-sub-"));

        let long = ephemeral_sandbox_name("a-very-long-sandbox-name-indeed", 1_784_947_247);
        assert!(
            long.len() <= MAX_SANDBOX_NAME,
            "{long} is {} chars",
            long.len()
        );
        assert!(
            long.ends_with(name.rsplit('-').next().unwrap()),
            "id survives the trim"
        );
    }

    #[test]
    fn ephemeral_names_differ_between_runs() {
        let a = ephemeral_sandbox_name("coaster-sub", 1_784_947_247);
        let b = ephemeral_sandbox_name("coaster-sub", 1_784_947_248);
        assert_ne!(a, b);
    }

    #[test]
    fn extra_tools_are_added_without_open_note_and_never_duplicated() {
        let mut args = Args::parse_from(["coaster-bench"]);
        args.extra_tools = Some("Bash".into());
        assert_eq!(allowed_tools(&args), "mcp__coaster__*,Bash");

        args.open_note = true;
        let allowed = allowed_tools(&args);
        assert_eq!(
            allowed.matches("Bash").count(),
            1,
            "no duplicate: {allowed}"
        );
    }

    #[test]
    fn opencode_blind_runs_lose_the_file_tools_and_open_note_gets_them() {
        let mut args = Args::parse_from(["coaster-bench"]);
        let blind = opencode_permissions(&args);
        for tool in ["bash", "read", "grep", "glob", "edit", "external_directory"] {
            assert_eq!(blind[tool], "deny", "{tool} must be denied on a blind run");
        }

        args.open_note = true;
        let open = opencode_permissions(&args);
        for tool in ["bash", "read", "grep", "glob", "external_directory"] {
            assert_eq!(open[tool], "allow", "{tool} needed under open note");
        }
        assert_eq!(
            open["webfetch"], "deny",
            "network tools stay denied either way"
        );
    }
}
