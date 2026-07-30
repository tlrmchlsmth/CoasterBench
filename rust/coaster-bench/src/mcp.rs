//! Minimal JSON-RPC client for the game's MCP server (plain HTTP, JSON
//! response mode, one request per POST — see rust/orct2-agent/src/mcp.rs).

use serde_json::{json, Value};

pub struct McpClient {
    url: String,
    next_id: u64,
}

impl McpClient {
    pub fn new(host: &str, port: u16) -> McpClient {
        McpClient {
            url: format!("http://{host}:{port}/mcp"),
            next_id: 1,
        }
    }

    /// Takes ownership of the park for `lease`, locking out any agent still
    /// alive from an earlier round. Every later request carries the lease.
    pub fn claim(&mut self, host: &str, port: u16, lease: &str) {
        self.url = format!("http://{host}:{port}/mcp?lease={lease}&claim=1");
        let claimed = self.call("get_state", json!({})).is_ok();
        // Ownership is taken by the claim=1 request itself; drop the flag so
        // the rest of the round runs as an ordinary lease holder.
        self.url = format!("http://{host}:{port}/mcp?lease={lease}");
        let _ = claimed;
    }

    fn rpc(&mut self, method: &str, params: Value) -> Result<Value, String> {
        let id = self.next_id;
        self.next_id += 1;
        let body = json!({"jsonrpc": "2.0", "id": id, "method": method, "params": params});
        let response: Value = ureq::post(&self.url)
            .timeout(std::time::Duration::from_secs(600))
            .send_json(body)
            .map_err(|e| format!("{method}: {e}"))?
            .into_json()
            .map_err(|e| format!("{method}: bad json: {e}"))?;
        if let Some(err) = response.get("error") {
            return Err(format!("{method}: rpc error: {err}"));
        }
        Ok(response.get("result").cloned().unwrap_or(Value::Null))
    }

    pub fn initialize(&mut self) -> Result<(), String> {
        self.rpc(
            "initialize",
            json!({"protocolVersion": "2024-11-05", "capabilities": {}, "clientInfo": {"name": "coaster-bench", "version": env!("CARGO_PKG_VERSION")}}),
        )
        .map(|_| ())
    }

    /// Calls a tool and returns the parsed JSON of its first text content
    /// block (the game server always returns exactly one).
    pub fn call(&mut self, tool: &str, arguments: Value) -> Result<Value, String> {
        let result = self.rpc("tools/call", json!({"name": tool, "arguments": arguments}))?;
        if result
            .get("isError")
            .and_then(Value::as_bool)
            .unwrap_or(false)
        {
            return Err(format!(
                "{tool}: {}",
                first_text(&result).unwrap_or_default()
            ));
        }
        let text = first_text(&result).ok_or_else(|| format!("{tool}: no text content"))?;
        serde_json::from_str(&text).or(Ok(Value::String(text)))
    }
}

fn first_text(result: &Value) -> Option<String> {
    result
        .get("content")?
        .as_array()?
        .iter()
        .find(|c| c.get("type").and_then(Value::as_str) == Some("text"))?
        .get("text")?
        .as_str()
        .map(str::to_string)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn first_text_reads_content_blocks() {
        let v = serde_json::json!({"content": [{"type": "image", "data": "x"}, {"type": "text", "text": "{\"a\":1}"}]});
        assert_eq!(first_text(&v).as_deref(), Some("{\"a\":1}"));
    }
}
