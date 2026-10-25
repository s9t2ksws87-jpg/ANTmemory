//! MCP（JSON-RPC 2.0、1行1メッセージ）。接続ごとに1つの `Conn` を持つ。

use std::sync::{Arc, Mutex};

use serde_json::{json, Value};

use crate::ctx::{Session, Workspace};
use crate::tools;

pub const SUPPORTED: &[&str] = &["2025-06-18", "2025-03-26", "2024-11-05"];
pub const SERVER_VERSION: &str = env!("CARGO_PKG_VERSION");

pub struct Conn {
    pub ws: Arc<Mutex<Workspace>>,
    pub session: Session,
}

fn reply(id: &Value, result: Value) -> String {
    json!({ "jsonrpc": "2.0", "id": id, "result": result }).to_string()
}

fn error(id: &Value, code: i64, msg: &str) -> String {
    json!({ "jsonrpc": "2.0", "id": id, "error": { "code": code, "message": msg } }).to_string()
}

impl Conn {
    /// 1行を処理し、返す行があれば返す（通知には返さない）。
    pub fn handle_line(&mut self, line: &str) -> Option<String> {
        let msg: Value = match serde_json::from_str(line) {
            Ok(v) => v,
            Err(e) => return Some(error(&Value::Null, -32700, &format!("parse error: {e}"))),
        };
        if let Value::Array(batch) = &msg {
            let out: Vec<Value> = batch
                .iter()
                .filter_map(|m| self.handle(m))
                .filter_map(|s| serde_json::from_str(&s).ok())
                .collect();
            return (!out.is_empty()).then(|| Value::Array(out).to_string());
        }
        self.handle(&msg)
    }

    fn handle(&mut self, msg: &Value) -> Option<String> {
        let id = msg.get("id").cloned();
        let method = msg.get("method").and_then(|m| m.as_str()).unwrap_or("");
        let params = msg.get("params").cloned().unwrap_or(Value::Null);
        let Some(id) = id else {
            // 通知（initialized、cancelled など）には応答しない。
            return None;
        };
        if method.is_empty() {
            // 応答（server→clientの要求は出さないので、来ても無視する）。
            return None;
        }
        Some(match method {
            "initialize" => {
                let asked = params.get("protocolVersion").and_then(|v| v.as_str()).unwrap_or("");
                let version = if SUPPORTED.contains(&asked) { asked } else { SUPPORTED[0] };
                reply(
                    &id,
                    json!({
                        "protocolVersion": version,
                        "capabilities": { "tools": { "listChanged": false } },
                        "serverInfo": { "name": "ant-memory", "version": SERVER_VERSION },
                        "instructions": tools::INSTRUCTIONS,
                    }),
                )
            }
            "ping" => reply(&id, json!({})),
            "tools/list" => reply(&id, tools::list()),
            "tools/call" => {
                let name = params.get("name").and_then(|n| n.as_str()).unwrap_or("");
                let args = params.get("arguments").cloned().unwrap_or_else(|| json!({}));
                let r = {
                    let mut ws = match self.ws.lock() {
                        Ok(g) => g,
                        Err(p) => p.into_inner(),
                    };
                    // 1つの呼出しの異常で接続ごと落とさない。書込は Store::apply のtransactionなので、途中のpanicは戻る。
                    std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| tools::call(&mut ws, &mut self.session, name, &args)))
                        .unwrap_or_else(|p| {
                            let what = p.downcast_ref::<String>().cloned().or_else(|| p.downcast_ref::<&str>().map(|s| s.to_string()));
                            tools::ToolResult {
                                text: format!("エラー: {name} の処理中に内部の異常（{}）。この呼出しの書込は反映していない", what.unwrap_or_default()),
                                is_error: true,
                            }
                        })
                };
                reply(&id, json!({ "content": [{ "type": "text", "text": r.text }], "isError": r.is_error }))
            }
            "resources/list" => reply(&id, json!({ "resources": [] })),
            "prompts/list" => reply(&id, json!({ "prompts": [] })),
            other => error(&id, -32601, &format!("method not found: {other}")),
        })
    }
}
