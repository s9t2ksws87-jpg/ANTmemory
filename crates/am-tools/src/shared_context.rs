//! V3 の共有 state と明示参照。完全に提示した版だけを @context で省略する。
use am_store::query;
use serde_json::{json, Map, Value};
use sha2::{Digest, Sha256};

use crate::{ctx::Workspace, render::estimate_tokens};

fn hash(value: &Value) -> String {
    hex::encode(Sha256::digest(value.to_string().as_bytes()))[..16].to_string()
}

/// 返した共有文脈。`full` は、全体を返した（または前回の全体から変わっていない）ときの版。
pub struct Shown {
    pub text: String,
    pub full: Option<String>,
}

/// `held` は、この接続が前に全体を見た版。同じなら1行だけにする（札の節と同じ扱い。毎回の検索で同じ本文を繰り返さない）。
pub fn render(ws: &Workspace, key: Option<&str>, known: Option<&Map<String, Value>>, held: Option<&str>, budget: usize) -> Result<Option<Shown>, String> {
    Ok(render_text(ws, key, known, held, budget)?.map(|(text, full)| Shown { text, full }))
}

fn render_text(
    ws: &Workspace,
    key: Option<&str>,
    known: Option<&Map<String, Value>>,
    held: Option<&str>,
    budget: usize,
) -> Result<Option<(String, Option<String>)>, String> {
    let get = |key: &str| -> Result<Value, String> {
        let row = query::state_get(ws.conn(), key).map_err(|e| e.to_string())?;
        let (value, updated) = match row {
            Some((v, at)) => (serde_json::from_str::<Value>(&v).map_err(|e| e.to_string())?, Some(at)),
            None => (Value::Null, None),
        };
        Ok(json!({"key": key, "value": value, "updated": updated}))
    };
    let goal = get("goal.current")?;
    let work = key.filter(|k| *k != "goal.current").map(get).transpose()?;
    if goal["value"].is_null() && work.is_none() {
        return Ok(None);
    }
    let mut flags: Vec<&str> = vec![];
    let valid_goal = ["final_goal", "purpose_memory_id"].iter().any(|k| goal["value"][k].as_str().is_some_and(|s| !s.trim().is_empty()));
    if goal["value"].is_null() {
        flags.push("GOAL_UNSET");
    } else if !valid_goal {
        flags.push("GOAL_INVALID");
    }
    if work.as_ref().is_some_and(|w| w["value"].is_null()) {
        flags.push("WORK_UNSET");
    }
    let mut refs = vec![];
    let mut blocks: Vec<(String, String)> = vec![];
    for owner in std::iter::once(&goal).chain(work.iter()) {
        blocks.push((
            format!("state {} updated={}\n{}", owner["key"], owner["updated"], owner["value"]),
            format!("state {}: 本文省略。get_stateで取得", owner["key"]),
        ));
        let value = &owner["value"];
        let mut ids: Vec<(&str, &Value)> = ["purpose_memory_id", "task_memory_id"].iter().filter_map(|f| value.get(f).map(|id| (*f, id))).collect();
        if let Some(unknowns) = value.get("blocking_unknown_ids") {
            if let Some(ids_) = unknowns.as_array() {
                ids.extend(ids_.iter().map(|id| ("blocking_unknown_ids", id)));
            } else {
                flags.push("REFERENCE_INVALID");
            }
        }
        if ids.len() > 6 {
            flags.push("REFERENCE_LIMIT");
        }
        for (field, id) in ids.into_iter().take(6) {
            let mut reference = json!({"owner": owner["key"], "field": field, "id": id});
            let pointer = format!("ref {}.{field}={id}", owner["key"]);
            let m = match id.as_str().filter(|s| !s.trim().is_empty()) {
                Some(id) => query::view(ws.conn(), id).map_err(|e| e.to_string())?,
                None => {
                    flags.push("REFERENCE_INVALID");
                    None
                }
            };
            if let Some(m) = m {
                let related = |kind: &str| -> Result<Vec<String>, String> {
                    let mut st = ws
                        .conn()
                        .prepare("SELECT src FROM relations WHERE dst=?1 AND kind=?2 ORDER BY created_at DESC, src LIMIT 4")
                        .map_err(|e| e.to_string())?;
                    let rows = st.query_map(rusqlite::params![m.id, kind], |r| r.get(0)).map_err(|e| e.to_string())?;
                    rows.collect::<Result<Vec<String>, _>>().map_err(|e| e.to_string())
                };
                let successors = related("supersedes")?;
                let resolved = if m.tags.iter().any(|t| t == "tk:unknown") { related("resolves")?.first().cloned() } else { None };
                if !m.current {
                    flags.push("REFERENCE_STALE");
                }
                if successors.len() > 1 {
                    flags.push("REFERENCE_BRANCH");
                }
                if resolved.is_some() {
                    flags.push("UNKNOWN_RESOLUTION_RECORDED");
                }
                reference["view"] = json!({"hash": m.content_hash, "status": m.status, "visibility": m.visibility,
                    "importance": m.importance, "current": m.current, "updated": m.updated, "source": m.source_ref,
                    "successors": successors, "resolved": resolved});
                let meta = format!(
                    "{pointer} status={} visibility={} current={}{}{}{}",
                    m.status,
                    m.visibility,
                    if m.current { "yes" } else { "no" },
                    m.source_ref.as_ref().map(|x| format!(" source={x}")).unwrap_or_default(),
                    if successors.is_empty() { String::new() } else { format!(" successors={}", successors.join(",")) },
                    resolved.map(|x| format!(" resolved_by={x}")).unwrap_or_default()
                );
                blocks.push((
                    format!("{meta}\n{}{}", if m.current { "" } else { "履歴本文（現在の指示として適用しない）\n" }, m.content),
                    format!("{meta}\n本文省略。read_contextでRawを取得"),
                ));
            } else {
                flags.push("REFERENCE_MISSING");
                let line = format!("{pointer} MISSING/INVALID");
                blocks.push((line.clone(), line));
            }
            refs.push(reference);
        }
    }
    let goal_refs: Vec<_> = refs.iter().filter(|r| r["owner"] == "goal.current").collect();
    let goal_revision = hash(&json!({"goal": goal, "references": goal_refs}));
    if let Some(w) = &work {
        if valid_goal && w["value"].is_object() {
            let v = &w["value"];
            if v.get("goal_key").is_some_and(|k| k != "goal.current") {
                flags.push("GOAL_KEY_MISMATCH");
            }
            if let Some(rev) = v.get("goal_revision") {
                if rev.as_str() != Some(&goal_revision) {
                    flags.push("GOAL_CHANGED");
                }
            } else if let Some(seen) = v.get("goal_seen") {
                flags.push(if seen == &goal["value"]["purpose_memory_id"] { "GOAL_REVIEW_POINTER_ONLY" } else { "GOAL_CHANGED" });
            } else {
                flags.push("GOAL_NOT_REVIEWED");
            }
        }
    }
    flags.sort_unstable();
    flags.dedup();
    let revision = hash(&json!({"view": 1, "goal": goal, "work": work, "refs": refs, "flags": flags}));
    let head = format!("## SHARED_CONTEXT（v3のstate。照合済みの全体目標ではない）\ngoal_revision={goal_revision}\n{}", flags.join(" "));
    if known.and_then(|k| k.get("@context")).and_then(Value::as_str) == Some(&revision) {
        let text = format!("{head}\nCONTEXT_UNCHANGED revision={revision}");
        if estimate_tokens(&text) <= budget {
            return Ok(Some((text, Some(revision))));
        }
    }
    if known.is_none() && held == Some(revision.as_str()) {
        let flags = if flags.is_empty() { String::new() } else { format!(" {}", flags.join(" ")) };
        let text = format!("## SHARED_CONTEXT（v3のstate）: 前回の表示から変化なし{flags}。全文は get_state(\"goal.current\")");
        return Ok(Some((text, Some(revision))));
    }
    let stamp = if known.is_some() { format!("\ncontext_revision={revision} (known_revisions[\"@context\"])") } else { String::new() };
    let full = format!("{head}\n{}{}", blocks.iter().map(|b| b.0.as_str()).collect::<Vec<_>>().join("\n\n"), stamp);
    if !flags.contains(&"REFERENCE_LIMIT") && estimate_tokens(&full) <= budget {
        return Ok(Some((full, Some(revision))));
    }
    // 部分提示に完全版のstampを付けない。未提示のRawを次の検索で省略させない。
    let mut parts: Vec<String> = blocks.iter().map(|b| b.1.clone()).collect();
    let join = |parts: &[String]| format!("{head}\nCONTEXT_INCOMPLETE\n{}", parts.join("\n"));
    if estimate_tokens(&join(&parts)) <= budget {
        for (i, block) in blocks.iter().enumerate() {
            let old = std::mem::replace(&mut parts[i], block.0.clone());
            if estimate_tokens(&join(&parts)) > budget {
                parts[i] = old;
            }
        }
        return Ok(Some((join(&parts), None)));
    }
    Ok(Some((
        format!(
            "CONTEXT_INCOMPLETE: get_state(\"goal.current\"){} → read_context(参照id)",
            key.map(|k| format!("; get_state({})", json!(k))).unwrap_or_default()
        ),
        None,
    )))
}
