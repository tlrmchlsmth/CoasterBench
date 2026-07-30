//! Normalised session traces.
//!
//! Each harness streams its own event format to `session.log`: opencode emits
//! `--format json` events, Claude Code emits `--output-format stream-json`.
//! Neither is worth teaching the site about, so both are folded into one
//! `trace.jsonl` per round:
//!
//!   {"seq":0,"kind":"text","text":"I'll start with the station"}
//!   {"seq":1,"kind":"tool","name":"place_pieces","input":{...},"output":"...",
//!    "status":"completed","dur_ms":41}
//!   {"seq":2,"kind":"step","tokens":{...},"cost_usd":0.0137}
//!
//! Token and cost totals fall out of the same pass, which beats the previous
//! OpenRouter spend-delta estimate: it is per session rather than per key, so
//! a second session running anywhere else no longer inflates the number.

use serde_json::{json, Map, Value};

use crate::Harness;

/// One normalised trace plus the usage totals implied by it.
pub struct Trace {
    pub events: Vec<Value>,
    pub usage: Value,
}

impl Trace {
    /// Trace as JSON Lines, ready to write next to the round's artifacts.
    pub fn to_jsonl(&self) -> String {
        let mut out = String::new();
        for event in &self.events {
            out.push_str(&event.to_string());
            out.push('\n');
        }
        out
    }

    pub fn tool_calls(&self) -> usize {
        self.events
            .iter()
            .filter(|e| e.get("kind").and_then(Value::as_str) == Some("tool"))
            .count()
    }
}

pub fn parse(harness: Harness, raw: &str) -> Trace {
    match harness {
        Harness::Opencode => parse_opencode(raw),
        Harness::ClaudeCode => parse_claude(raw),
        Harness::Codex => parse_codex(raw),
    }
}

fn lines_as_json(raw: &str) -> impl Iterator<Item = Value> + '_ {
    raw.lines()
        .map(str::trim)
        .filter(|l| l.starts_with('{'))
        .filter_map(|l| serde_json::from_str::<Value>(l).ok())
}

fn push_event(events: &mut Vec<Value>, mut event: Map<String, Value>) {
    event.insert("seq".into(), json!(events.len()));
    events.push(Value::Object(event));
}

/// opencode `--format json`: one event per line, each wrapping a `part`.
fn parse_opencode(raw: &str) -> Trace {
    let mut events = Vec::new();
    let (mut input, mut output, mut reasoning, mut cache_read) = (0i64, 0i64, 0i64, 0i64);
    let mut cost = 0.0;
    let mut first_ts: Option<i64> = None;

    for event in lines_as_json(raw) {
        let kind = event.get("type").and_then(Value::as_str).unwrap_or("");
        let part = event.get("part").cloned().unwrap_or(Value::Null);
        let ts = event.get("timestamp").and_then(Value::as_i64);
        if first_ts.is_none() {
            first_ts = ts;
        }
        let ms = match (ts, first_ts) {
            (Some(t), Some(first)) => json!(t - first),
            _ => Value::Null,
        };

        match kind {
            "text" => {
                let text = part
                    .get("text")
                    .and_then(Value::as_str)
                    .unwrap_or("")
                    .trim();
                if text.is_empty() {
                    continue;
                }
                let mut e = Map::new();
                e.insert("kind".into(), json!("text"));
                e.insert("ms".into(), ms);
                e.insert("text".into(), json!(text));
                push_event(&mut events, e);
            }
            "tool_use" => {
                let state = part.get("state").cloned().unwrap_or(Value::Null);
                let mut e = Map::new();
                e.insert("kind".into(), json!("tool"));
                e.insert("ms".into(), ms);
                e.insert(
                    "name".into(),
                    json!(strip_server_prefix(
                        part.get("tool").and_then(Value::as_str).unwrap_or("")
                    )),
                );
                e.insert(
                    "input".into(),
                    state.get("input").cloned().unwrap_or(json!({})),
                );
                e.insert(
                    "output".into(),
                    state.get("output").cloned().unwrap_or(Value::Null),
                );
                e.insert(
                    "status".into(),
                    state.get("status").cloned().unwrap_or(Value::Null),
                );
                if let (Some(start), Some(end)) = (
                    state.pointer("/time/start").and_then(Value::as_i64),
                    state.pointer("/time/end").and_then(Value::as_i64),
                ) {
                    e.insert("dur_ms".into(), json!(end - start));
                }
                push_event(&mut events, e);
            }
            "step_finish" => {
                let step_cost = part.get("cost").and_then(Value::as_f64).unwrap_or(0.0);
                cost += step_cost;
                input += part
                    .pointer("/tokens/input")
                    .and_then(Value::as_i64)
                    .unwrap_or(0);
                output += part
                    .pointer("/tokens/output")
                    .and_then(Value::as_i64)
                    .unwrap_or(0);
                reasoning += part
                    .pointer("/tokens/reasoning")
                    .and_then(Value::as_i64)
                    .unwrap_or(0);
                cache_read += part
                    .pointer("/tokens/cache/read")
                    .and_then(Value::as_i64)
                    .unwrap_or(0);
                let mut e = Map::new();
                e.insert("kind".into(), json!("step"));
                e.insert("ms".into(), ms);
                e.insert(
                    "tokens".into(),
                    part.get("tokens").cloned().unwrap_or(json!({})),
                );
                e.insert("cost_usd".into(), json!(step_cost));
                push_event(&mut events, e);
            }
            _ => {}
        }
    }

    let steps = events
        .iter()
        .filter(|e| e.get("kind").and_then(Value::as_str) == Some("step"))
        .count();
    Trace {
        usage: json!({
            "input_tokens": input,
            "output_tokens": output + reasoning,
            "cache_read_tokens": cache_read,
            "cost_usd": cost,
            "num_turns": steps,
        }),
        events,
    }
}

/// Claude Code `--output-format stream-json`: assistant messages carry text and
/// tool_use blocks, the following user message carries the tool results, and a
/// final result event carries session usage.
fn parse_claude(raw: &str) -> Trace {
    let mut events: Vec<Value> = Vec::new();
    let mut usage = Value::Null;
    // tool_use id -> index in events, so the result can be attached when the
    // following user message arrives.
    let mut pending: Vec<(String, usize)> = Vec::new();

    for event in lines_as_json(raw) {
        match event.get("type").and_then(Value::as_str).unwrap_or("") {
            "assistant" => {
                let blocks = event
                    .pointer("/message/content")
                    .and_then(Value::as_array)
                    .cloned()
                    .unwrap_or_default();
                for block in blocks {
                    match block.get("type").and_then(Value::as_str).unwrap_or("") {
                        // Reasoning is where the interesting failures live: a
                        // model arguing with itself about a rejected piece.
                        "thinking" => {
                            let text = block
                                .get("thinking")
                                .and_then(Value::as_str)
                                .unwrap_or("")
                                .trim();
                            if text.is_empty() {
                                continue;
                            }
                            let mut e = Map::new();
                            e.insert("kind".into(), json!("thinking"));
                            e.insert("text".into(), json!(text));
                            push_event(&mut events, e);
                        }
                        "text" => {
                            let text = block
                                .get("text")
                                .and_then(Value::as_str)
                                .unwrap_or("")
                                .trim();
                            if text.is_empty() {
                                continue;
                            }
                            let mut e = Map::new();
                            e.insert("kind".into(), json!("text"));
                            e.insert("text".into(), json!(text));
                            push_event(&mut events, e);
                        }
                        "tool_use" => {
                            let mut e = Map::new();
                            e.insert("kind".into(), json!("tool"));
                            e.insert(
                                "name".into(),
                                json!(strip_server_prefix(
                                    block.get("name").and_then(Value::as_str).unwrap_or("")
                                )),
                            );
                            e.insert(
                                "input".into(),
                                block.get("input").cloned().unwrap_or(json!({})),
                            );
                            if let Some(id) = block.get("id").and_then(Value::as_str) {
                                pending.push((id.to_string(), events.len()));
                            }
                            push_event(&mut events, e);
                        }
                        _ => {}
                    }
                }
            }
            "user" => {
                let blocks = event
                    .pointer("/message/content")
                    .and_then(Value::as_array)
                    .cloned()
                    .unwrap_or_default();
                for block in blocks {
                    if block.get("type").and_then(Value::as_str) != Some("tool_result") {
                        continue;
                    }
                    let Some(id) = block.get("tool_use_id").and_then(Value::as_str) else {
                        continue;
                    };
                    let Some(pos) = pending.iter().position(|(pid, _)| pid == id) else {
                        continue;
                    };
                    let (_, index) = pending.remove(pos);
                    let is_error = block
                        .get("is_error")
                        .and_then(Value::as_bool)
                        .unwrap_or(false);
                    if let Some(target) = events.get_mut(index).and_then(Value::as_object_mut) {
                        target.insert(
                            "output".into(),
                            json!(flatten_content(block.get("content"))),
                        );
                        target.insert(
                            "status".into(),
                            json!(if is_error { "error" } else { "completed" }),
                        );
                    }
                }
            }
            "result" => {
                usage = json!({
                    "input_tokens": event.pointer("/usage/input_tokens"),
                    "output_tokens": event.pointer("/usage/output_tokens"),
                    "cache_read_tokens": event.pointer("/usage/cache_read_input_tokens"),
                    "cost_usd": event.get("total_cost_usd"),
                    "num_turns": event.get("num_turns"),
                });
            }
            _ => {}
        }
    }
    Trace { events, usage }
}

fn parse_codex(raw: &str) -> Trace {
    let mut events = Vec::new();
    let (mut input, mut output, mut cached) = (0i64, 0i64, 0i64);
    let mut turns = 0i64;

    for event in lines_as_json(raw) {
        let kind = event.get("type").and_then(Value::as_str).unwrap_or("");
        match kind {
            "item.completed" | "item.updated" => {
                let item = event.get("item").cloned().unwrap_or(Value::Null);
                let item_type = item.get("type").and_then(Value::as_str).unwrap_or("");
                if kind == "item.updated" && item_type != "error" {
                    continue;
                }
                match item_type {
                    "agent_message" | "assistant_message" => {
                        push_text(
                            &mut events,
                            "text",
                            first_str(&item, &["/text", "/message"]),
                        );
                    }
                    "reasoning" => {
                        push_text(
                            &mut events,
                            "thinking",
                            first_str(&item, &["/text", "/summary", "/content"]),
                        );
                    }
                    "error" => {
                        push_text(
                            &mut events,
                            "error",
                            first_str(&item, &["/message", "/text"]),
                        );
                    }
                    "mcp_tool_call" | "command_execution" | "file_change" | "web_search" => {
                        let mut e = Map::new();
                        e.insert("kind".into(), json!("tool"));
                        let name = match item_type {
                            "mcp_tool_call" => first_str(&item, &["/tool", "/tool_name", "/name"]),
                            "command_execution" => "shell".to_string(),
                            other => other.to_string(),
                        };
                        e.insert("name".into(), json!(strip_server_prefix(&name)));
                        e.insert(
                            "input".into(),
                            first_value(&item, &["/arguments", "/input", "/command"]),
                        );
                        let output = first_value(
                            &item,
                            &["/result", "/output", "/aggregated_output", "/error"],
                        );
                        e.insert(
                            "output".into(),
                            match output.get("content") {
                                Some(content) => json!(flatten_content(Some(content))),
                                None => match output.get("message") {
                                    Some(message) => message.clone(),
                                    None => output,
                                },
                            },
                        );
                        e.insert(
                            "status".into(),
                            item.get("status").cloned().unwrap_or(json!("completed")),
                        );
                        push_event(&mut events, e);
                    }
                    _ => {}
                }
            }
            "turn.completed" => {
                turns += 1;
                let usage = event.get("usage").cloned().unwrap_or(Value::Null);
                input += usage
                    .get("input_tokens")
                    .and_then(Value::as_i64)
                    .unwrap_or(0);
                output += usage
                    .get("output_tokens")
                    .and_then(Value::as_i64)
                    .unwrap_or(0);
                cached += usage
                    .get("cached_input_tokens")
                    .and_then(Value::as_i64)
                    .unwrap_or(0);
                let mut e = Map::new();
                e.insert("kind".into(), json!("step"));
                e.insert("tokens".into(), usage);
                push_event(&mut events, e);
            }
            "turn.failed" | "error" => {
                let message = first_str(&event, &["/error/message", "/message"]);
                push_text(&mut events, "error", message);
            }
            _ => {}
        }
    }

    Trace {
        usage: json!({
            "input_tokens": input,
            "output_tokens": output,
            "cache_read_tokens": cached,
            "cost_usd": 0.0,
            "num_turns": turns,
        }),
        events,
    }
}

fn first_str(value: &Value, paths: &[&str]) -> String {
    paths
        .iter()
        .filter_map(|p| value.pointer(p))
        .find_map(|v| match v {
            Value::String(s) if !s.trim().is_empty() => Some(s.trim().to_string()),
            _ => None,
        })
        .unwrap_or_default()
}

fn first_value(value: &Value, paths: &[&str]) -> Value {
    paths
        .iter()
        .filter_map(|p| value.pointer(p))
        .find(|v| !v.is_null())
        .cloned()
        .unwrap_or(Value::Null)
}

fn push_text(events: &mut Vec<Value>, kind: &str, text: String) {
    if text.is_empty() {
        return;
    }
    let mut e = Map::new();
    e.insert("kind".into(), json!(kind));
    e.insert("text".into(), json!(text));
    push_event(events, e);
}

/// Tool results arrive either as a string or as a list of content blocks;
/// images are noted rather than inlined, since a trace should stay readable.
fn flatten_content(content: Option<&Value>) -> String {
    match content {
        Some(Value::String(s)) => s.clone(),
        Some(Value::Array(blocks)) => blocks
            .iter()
            .map(|b| match b.get("type").and_then(Value::as_str) {
                Some("text") => b
                    .get("text")
                    .and_then(Value::as_str)
                    .unwrap_or("")
                    .to_string(),
                Some("image") => "[image]".to_string(),
                _ => String::new(),
            })
            .collect::<Vec<_>>()
            .join("\n"),
        _ => String::new(),
    }
}

/// Both harnesses namespace MCP tools by server ("coaster_place_piece",
/// "mcp__coaster__place_piece"); the trace shows the bare tool name.
fn strip_server_prefix(name: &str) -> &str {
    name.strip_prefix("mcp__coaster__")
        .or_else(|| name.strip_prefix("coaster__"))
        .or_else(|| name.strip_prefix("coaster."))
        .or_else(|| name.strip_prefix("coaster_"))
        .unwrap_or(name)
}

#[cfg(test)]
mod tests {
    use super::*;

    const OPENCODE: &str = r#"
{"type":"step_start","timestamp":1000,"part":{"type":"step-start"}}
{"type":"text","timestamp":1200,"part":{"type":"text","text":" Building the station"}}
{"type":"tool_use","timestamp":1500,"part":{"type":"tool","tool":"coaster_place_pieces","state":{"status":"completed","input":{"pieces":["begin_station"]},"output":"{\"pieces_placed\":1}","time":{"start":1500,"end":1541}}}}
{"type":"step_finish","timestamp":1600,"part":{"type":"step-finish","tokens":{"input":64,"output":5,"reasoning":40,"cache":{"read":7232,"write":0}},"cost":0.00137835}}
"#;

    const CLAUDE: &str = r#"
{"type":"system","subtype":"init","model":"claude-sonnet-5"}
{"type":"assistant","message":{"content":[{"type":"text","text":"Placing the station"},{"type":"tool_use","id":"tu_1","name":"mcp__coaster__place_piece","input":{"piece":"begin_station"}}]}}
{"type":"user","message":{"content":[{"type":"tool_result","tool_use_id":"tu_1","content":[{"type":"text","text":"ok"}]}]}}
{"type":"assistant","message":{"content":[{"type":"tool_use","id":"tu_2","name":"mcp__coaster__place_piece","input":{"piece":"loop"}}]}}
{"type":"user","message":{"content":[{"type":"tool_result","tool_use_id":"tu_2","is_error":true,"content":"piece rejected"}]}}
{"type":"result","num_turns":4,"total_cost_usd":0.42,"usage":{"input_tokens":10,"output_tokens":20,"cache_read_input_tokens":30}}
"#;

    const CODEX: &str = r#"
{"type":"thread.started","thread_id":"019f9a75-92fe-7110-86c9-50f1dc603cea"}
{"type":"turn.started"}
{"type":"item.completed","item":{"id":"item_0","type":"reasoning","text":"Station first, then the lift."}}
{"type":"item.completed","item":{"id":"item_1","type":"agent_message","text":"Placing the station."}}
{"type":"item.completed","item":{"id":"item_2","type":"mcp_tool_call","tool":"coaster__place_pieces","status":"completed","arguments":{"pieces":["begin_station"]},"result":"{\"pieces_placed\":1}"}}
{"type":"item.completed","item":{"id":"item_3","type":"command_execution","command":"python3 -c 'print(1)'","status":"completed","aggregated_output":"1\n"}}
{"type":"turn.completed","usage":{"input_tokens":1200,"cached_input_tokens":900,"output_tokens":340}}
"#;

    const CODEX_QUOTA_FAILURE: &str = r#"
{"type":"thread.started","thread_id":"019f9a76-7873-7282-aba6-f0438ce79b49"}
{"type":"turn.started"}
{"type":"error","message":"You've hit your usage limit."}
{"type":"turn.failed","error":{"message":"You've hit your usage limit."}}
"#;

    #[test]
    fn codex_items_become_tools_text_and_totals() {
        let trace = parse(Harness::Codex, CODEX);
        assert_eq!(trace.tool_calls(), 2, "mcp call and shell command");

        let mcp = trace
            .events
            .iter()
            .find(|e| e["name"] == "place_pieces")
            .expect("mcp tool call, server prefix stripped");
        assert_eq!(mcp["input"]["pieces"][0], "begin_station");
        assert_eq!(mcp["output"], "{\"pieces_placed\":1}");
        assert_eq!(mcp["status"], "completed");

        let shell = trace
            .events
            .iter()
            .find(|e| e["name"] == "shell")
            .expect("open note lets codex run a shell");
        assert_eq!(shell["input"], "python3 -c 'print(1)'");
        assert_eq!(shell["output"], "1\n");

        assert!(trace.events.iter().any(|e| e["kind"] == "thinking"));
        assert!(trace.events.iter().any(|e| e["kind"] == "text"));
        assert_eq!(trace.usage["input_tokens"], 1200);
        assert_eq!(trace.usage["output_tokens"], 340);
        assert_eq!(trace.usage["cache_read_tokens"], 900);
        assert_eq!(trace.usage["num_turns"], 1);
    }

    #[test]
    fn codex_records_why_a_round_produced_nothing() {
        let trace = parse(Harness::Codex, CODEX_QUOTA_FAILURE);
        let errors: Vec<&str> = trace
            .events
            .iter()
            .filter(|e| e["kind"] == "error")
            .filter_map(|e| e["text"].as_str())
            .collect();
        assert_eq!(
            errors.len(),
            2,
            "both the error and the failed turn are kept: {errors:?}"
        );
        assert!(errors[0].contains("usage limit"));
        assert_eq!(trace.usage["num_turns"], 0, "no turn ever completed");
    }

    #[test]
    fn codex_tolerates_renamed_item_fields() {
        let renamed = r#"
{"type":"item.completed","item":{"type":"assistant_message","message":"done"}}
{"type":"item.completed","item":{"type":"mcp_tool_call","tool_name":"coaster.get_state","input":{"a":1},"output":"ok"}}
"#;
        let trace = parse(Harness::Codex, renamed);
        assert_eq!(trace.tool_calls(), 1);
        let tool = trace.events.iter().find(|e| e["kind"] == "tool").unwrap();
        assert_eq!(tool["name"], "get_state");
        assert_eq!(tool["output"], "ok");
        assert_eq!(
            tool["status"], "completed",
            "an item that reports no status still completed"
        );
        assert!(trace.events.iter().any(|e| e["text"] == "done"));
    }

    #[test]
    fn opencode_events_normalise_with_totals() {
        let trace = parse(Harness::Opencode, OPENCODE);
        assert_eq!(trace.events.len(), 3, "text, tool, step");
        assert_eq!(trace.tool_calls(), 1);

        let text = &trace.events[0];
        assert_eq!(text["kind"], "text");
        assert_eq!(text["text"], "Building the station", "trimmed");
        assert_eq!(text["ms"], 200, "relative to the first event");

        let tool = &trace.events[1];
        assert_eq!(tool["name"], "place_pieces", "server prefix stripped");
        assert_eq!(tool["input"]["pieces"][0], "begin_station");
        assert_eq!(tool["status"], "completed");
        assert_eq!(tool["dur_ms"], 41);

        // reasoning tokens count as output; cost sums across steps.
        assert_eq!(trace.usage["input_tokens"], 64);
        assert_eq!(trace.usage["output_tokens"], 45);
        assert_eq!(trace.usage["cache_read_tokens"], 7232);
        assert_eq!(trace.usage["num_turns"], 1);
        assert!((trace.usage["cost_usd"].as_f64().unwrap() - 0.00137835).abs() < 1e-9);
    }

    #[test]
    fn claude_tool_results_attach_to_their_calls() {
        let trace = parse(Harness::ClaudeCode, CLAUDE);
        assert_eq!(trace.tool_calls(), 2);

        let ok = trace
            .events
            .iter()
            .find(|e| e["name"] == "place_piece")
            .unwrap();
        assert_eq!(ok["output"], "ok");
        assert_eq!(ok["status"], "completed");

        let failed = trace
            .events
            .iter()
            .find(|e| e["input"]["piece"] == "loop")
            .unwrap();
        assert_eq!(
            failed["status"], "error",
            "is_error becomes a failed status"
        );
        assert_eq!(failed["output"], "piece rejected", "string content is kept");

        assert_eq!(trace.usage["cost_usd"], 0.42);
        assert_eq!(trace.usage["num_turns"], 4);
    }

    #[test]
    fn seq_numbers_are_dense_and_ordered() {
        let trace = parse(Harness::ClaudeCode, CLAUDE);
        for (i, event) in trace.events.iter().enumerate() {
            assert_eq!(event["seq"], i);
        }
    }

    // Captured from live sessions, so a harness changing its event shape shows
    // up here rather than as an empty trace after a paid run.
    #[test]
    fn real_opencode_session_parses() {
        let raw = include_str!("../tests/fixtures/opencode-session.jsonl");
        let trace = parse(Harness::Opencode, raw);
        assert_eq!(trace.tool_calls(), 2);
        let names: Vec<&str> = trace
            .events
            .iter()
            .filter_map(|e| e.get("name").and_then(Value::as_str))
            .collect();
        assert_eq!(names, vec!["get_state", "valid_next_pieces"]);
        assert!(trace.events.iter().any(|e| e["kind"] == "text"));
        assert!(trace.usage["cost_usd"].as_f64().unwrap() > 0.0);
        assert!(trace.usage["input_tokens"].as_i64().unwrap() > 0);
        assert_eq!(trace.usage["num_turns"], 3);
    }

    #[test]
    fn real_claude_session_parses() {
        let raw = include_str!("../tests/fixtures/claude-session.jsonl");
        let trace = parse(Harness::ClaudeCode, raw);
        assert_eq!(trace.tool_calls(), 1);
        let tool = trace.events.iter().find(|e| e["kind"] == "tool").unwrap();
        assert_eq!(tool["name"], "Bash");
        assert_eq!(tool["status"], "completed", "result attached to the call");
        assert!(tool["output"].as_str().unwrap().contains("hello"));
        assert!(
            trace.events.iter().any(|e| e["kind"] == "thinking"),
            "reasoning blocks are kept"
        );
        assert!(trace.usage["cost_usd"].as_f64().unwrap() > 0.0);
    }

    #[test]
    fn real_codex_session_parses() {
        let raw = include_str!("../tests/fixtures/codex-session.jsonl");
        let trace = parse(Harness::Codex, raw);

        assert_eq!(trace.tool_calls(), 3, "two mcp calls and one shell command");

        let listed = trace
            .events
            .iter()
            .find(|e| e["name"] == "list_mcp_resources")
            .expect("completed mcp call");
        assert_eq!(
            listed["output"], "{\"resources\":[]}",
            "content blocks flattened to text"
        );

        let failed = trace
            .events
            .iter()
            .find(|e| e["status"] == "failed")
            .expect("failed mcp call");
        assert!(
            failed["output"]
                .as_str()
                .unwrap_or_default()
                .contains("method not found"),
            "the error message is what explains the failure: {failed}"
        );

        assert!(trace.events.iter().any(|e| e["kind"] == "thinking"));
        assert!(trace.events.iter().any(|e| e["kind"] == "text"));
        assert_eq!(trace.usage["input_tokens"], 134_224);
        assert_eq!(trace.usage["cache_read_tokens"], 117_760);
        assert_eq!(trace.usage["output_tokens"], 3836);
        assert_eq!(trace.usage["num_turns"], 1);
    }

    #[test]
    fn garbage_lines_are_skipped_rather_than_fatal() {
        let trace = parse(Harness::Opencode, "not json\n\n{\"type\":\"nope\"}\n");
        assert!(trace.events.is_empty());
        assert_eq!(trace.usage["num_turns"], 0);
    }
}
