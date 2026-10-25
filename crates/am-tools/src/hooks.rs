//! clientのhook（Claude Code）から届く観測。会話の識別子・圧縮/clear・開いた/編集したファイル。
//!
//! hookが届いた会話はC2になる（§5.2）。hookはDBへ直接書かず、`apply` を通す。

use am_domain::Command;
use serde::{Deserialize, Serialize};
use serde_json::Value;

use crate::ctx::Workspace;
use crate::useful;

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct HookEvent {
    pub client: String,
    /// SessionStart / UserPromptSubmit / PostToolUse
    pub event: String,
    pub session_id: String,
    pub cwd: Option<String>,
    /// SessionStart の startup / resume / clear / compact
    pub source: Option<String>,
    pub tool_name: Option<String>,
    pub tool_input: Value,
}

/// workspace rootからの相対パス（'/' 区切り）。rootの外なら None。
pub fn relative(root: &std::path::Path, path: &str) -> Option<String> {
    let r = root.display().to_string().replace('\\', "/").trim_end_matches('/').to_lowercase();
    let p = path.replace('\\', "/");
    let lp = p.to_lowercase();
    if lp.starts_with(&format!("{r}/")) {
        Some(p[r.len() + 1..].to_string())
    } else if !std::path::Path::new(path).is_absolute() {
        Some(p.trim_start_matches("./").to_string())
    } else {
        None
    }
}

pub fn apply(ws: &mut Workspace, ev: &HookEvent, build: &str) -> Result<String, String> {
    let conv = format!("{}:{}", ev.client, ev.session_id);
    let cx = am_domain::ApplyCtx {
        actor: ev.client.clone(),
        build: build.into(),
        conn_id: None,
        conversation_id: Some(conv.clone()),
        model: None,
        provider: None,
    };
    let bump = ev.event == "SessionStart" && matches!(ev.source.as_deref(), Some("clear") | Some("compact"));
    let r = ws
        .apply(
            &cx,
            Command::BumpConversation {
                conversation_id: conv.clone(),
                client: ev.client.clone(),
                session: ev.session_id.clone(),
                source: "hook".into(),
                bump,
            },
        )
        .map_err(|e| e.to_string())?;
    let mut note = format!("{} gen={}", ev.event, r.detail["gen"]);
    if ev.event != "PostToolUse" {
        return Ok(note);
    }
    let Some(root) = ws.root.clone() else { return Ok(note) };
    let path = ev
        .tool_input
        .get("file_path")
        .or_else(|| ev.tool_input.get("notebook_path"))
        .or_else(|| ev.tool_input.get("path"))
        .and_then(|p| p.as_str())
        .unwrap_or("");
    let Some(rel) = (!path.is_empty()).then(|| relative(&root, path)).flatten() else { return Ok(note) };
    match ev.tool_name.as_deref() {
        // 編集したファイル：次の書込に touched として添える。読んだだけのファイルは添えない。
        Some("Edit") | Some("Write") | Some("MultiEdit") | Some("NotebookEdit") => {
            let v = ws.touched.entry(conv).or_default();
            if !v.contains(&rel) {
                v.push(rel.clone());
            }
            note.push_str(&format!(" touched={rel}"));
        }
        // 記憶を読んだ後に、その取手の先を開いた（followed）。
        Some("Read") => {
            // 今の世代の読取りだけに結ぶ（圧縮・clearの前の読取りを、新しい文脈の行動へ結ばない。R6）。
            let s = crate::ctx::Session {
                conversation: Some((ev.client.clone(), ev.session_id.clone())),
                gen_seen: am_store::query::conversation_gen(ws.conn(), &conv).ok().flatten(),
                ..crate::ctx::Session::new("hook", &ev.client, build)
            };
            let read = useful::reads_in_scope(ws, &s);
            let with_anchor = am_store::query::memories_with_anchor_path(ws.conn(), &rel).map_err(|e| e.to_string())?;
            let mut uses = vec![];
            for mid in with_anchor.iter().filter(|m| read.contains_key(*m)) {
                let already: i64 = ws
                    .conn()
                    .query_row(
                        "SELECT count(*) FROM uses WHERE memory_id=?1 AND scope_key=?2 AND kind='followed'",
                        rusqlite::params![mid, s.scope_key()],
                        |r| r.get(0),
                    )
                    .unwrap_or(1);
                if already == 0 {
                    uses.push(useful::make_use(ws, &s, mid, "followed", useful::W_FOLLOWED, read[mid].clone()));
                }
            }
            if !uses.is_empty() {
                note.push_str(&format!(" followed={}", uses.len()));
                let _u = ws.apply(&cx, Command::RecordUses(uses)).map_err(|e| e.to_string())?;
            }
        }
        _ => {}
    }
    Ok(note)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn relative_paths() {
        let root = std::path::Path::new(r"f:\ANTIGRABTY\デュエマしようぜ！");
        assert_eq!(relative(root, r"F:\ANTIGRABTY\デュエマしようぜ！\CLAUDE.md").as_deref(), Some("CLAUDE.md"));
        assert_eq!(relative(root, "doc/a.md").as_deref(), Some("doc/a.md"));
        assert_eq!(relative(root, r"C:\other\x.md"), None);
    }
}
