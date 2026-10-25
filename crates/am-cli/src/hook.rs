//! `antmemory hook`：Claude Code のhookから呼ばれ、観測をdaemonへ渡す（§5.2 C2）。
//!
//! clientの動作を妨げないよう、何があっても終了コード0で、stdoutには何も書かない
//! （SessionStartのstdoutは会話へ差し込まれるため）。失敗はログに残す。

use std::io::Read;

use serde_json::{json, Value};

use crate::ipc;

pub fn run() -> i32 {
    let mut input = String::new();
    std::io::stdin().read_to_string(&mut input).ok();
    let r = (|| -> anyhow::Result<String> {
        let layout = ipc::layout()?;
        let v: Value = serde_json::from_str(&input)?;
        let s = |k: &str| v.get(k).and_then(|x| x.as_str()).map(String::from);
        let session_id = s("session_id").ok_or_else(|| anyhow::anyhow!("session_id が無い"))?;
        let cwd = s("cwd").unwrap_or_else(|| std::env::current_dir().map(|p| p.display().to_string()).unwrap_or_default());
        let client = std::env::var("ANT_MEMORY_CLIENT").unwrap_or_else(|_| "claude-code".into());
        let resolved = am_tools::workspace::resolve(&layout, std::path::Path::new(&cwd), &|k| std::env::var(k).ok()).map_err(anyhow::Error::msg)?;
        let ev = am_tools::hooks::HookEvent {
            client,
            event: s("hook_event_name").unwrap_or_default(),
            session_id,
            cwd: Some(cwd),
            source: s("source"),
            tool_name: s("tool_name"),
            tool_input: v.get("tool_input").cloned().unwrap_or(Value::Null),
        };
        let reply = ipc::request(&json!({ "am": "hook", "workspace": resolved, "event": ev }), &layout, true)?;
        Ok(reply.to_string())
    })();
    if let Err(e) = r {
        if let Ok(layout) = ipc::layout() {
            let p = ipc::runtime_dir(&layout).join("hook.log");
            crate::maintain::rotate(&p);
            let ts = chrono::Utc::now().to_rfc3339();
            std::fs::create_dir_all(ipc::runtime_dir(&layout)).ok();
            // hookのログにも書けなければ、clientを止めないよう諦める。
            std::fs::OpenOptions::new()
                .create(true)
                .append(true)
                .open(p)
                .and_then(|mut f| std::io::Write::write_all(&mut f, format!("{ts} {e}\n").as_bytes()))
                .ok();
        }
    }
    0
}
