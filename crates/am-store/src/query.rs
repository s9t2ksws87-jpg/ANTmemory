//! 表示と検索のための読取り。ここではDBを変えない。
//!
//! 読取りの結果は、表示側で使う列の組（tuple）で返すものが多い。
#![allow(clippy::type_complexity)]

use std::collections::HashMap;

use rusqlite::{params, params_from_iter, Connection, OptionalExtension};
use serde_json::Value;

use crate::Result;

#[derive(Debug, Clone)]
pub struct MemoryView {
    pub id: String,
    pub kind: String,
    pub topic: String,
    pub content: String,
    pub tags: Vec<String>,
    pub tags_json: String,
    pub author: String,
    pub content_hash: String,
    pub created: String,
    pub updated: String,
    /// 期限を過ぎたactiveは expired として見せる（v3と同じ）。
    pub status: String,
    pub visibility: String,
    pub importance: String,
    pub expires_at: Option<String>,
    pub current: bool,
    /// 最新の source_observed（v3と同じ取り方）。
    pub source_ref: Option<String>,
}

const VIEW_SQL: &str = "SELECT m.id, m.kind, m.topic, m.content, m.tags_json, m.author, m.content_hash, m.created_at,
    s.updated_at, s.status, s.visibility, s.importance, s.expires_at,
    EXISTS(SELECT 1 FROM relations r WHERE r.dst=m.id AND r.kind='supersedes'),
    (SELECT json_extract(e.payload_json, '$.new_value') FROM events e
       WHERE e.subject=m.id AND e.kind IN ('source_observed','v3.source_observed') ORDER BY e.created_at DESC, e.seq DESC LIMIT 1)
  FROM memories m JOIN memory_state s ON s.id=m.id";

fn view_row(r: &rusqlite::Row, now: &str) -> rusqlite::Result<MemoryView> {
    let tags_json: String = r.get(4)?;
    let mut status: String = r.get(9)?;
    let expires: Option<String> = r.get(12)?;
    if status == "active" && expires.as_deref().is_some_and(|e| e <= now) {
        status = "expired".into();
    }
    let has_successor: bool = r.get(13)?;
    Ok(MemoryView {
        id: r.get(0)?,
        kind: r.get(1)?,
        topic: r.get(2)?,
        content: r.get(3)?,
        tags: serde_json::from_str(&tags_json).unwrap_or_default(),
        tags_json,
        author: r.get(5)?,
        content_hash: r.get(6)?,
        created: r.get(7)?,
        updated: r.get(8)?,
        current: status == "active" && !has_successor,
        status,
        visibility: r.get(10)?,
        importance: r.get(11)?,
        expires_at: expires,
        source_ref: r.get(14)?,
    })
}

pub fn view(conn: &Connection, id: &str) -> Result<Option<MemoryView>> {
    let now = crate::now();
    Ok(conn.query_row(&format!("{VIEW_SQL} WHERE m.id=?1"), [id], |r| view_row(r, &now)).optional()?)
}

pub fn views(conn: &Connection, ids: &[String]) -> Result<HashMap<String, MemoryView>> {
    let now = crate::now();
    let mut out = HashMap::new();
    let mut st = conn.prepare_cached(&format!("{VIEW_SQL} WHERE m.id=?1"))?;
    for id in ids {
        if let Some(v) = st.query_row([id], |r| view_row(r, &now)).optional()? {
            out.insert(id.clone(), v);
        }
    }
    Ok(out)
}

pub fn list(conn: &Connection, agent: Option<&str>, tags: &[String], limit: i64, offset: i64) -> Result<Vec<MemoryView>> {
    let now = crate::now();
    let mut sql = format!("{VIEW_SQL} WHERE 1=1");
    let mut args: Vec<Value> = vec![];
    if let Some(a) = agent {
        sql.push_str(&format!(" AND m.author=?{}", args.len() + 1));
        args.push(Value::from(a));
    }
    if !tags.is_empty() {
        sql.push_str(&format!(" AND EXISTS (SELECT 1 FROM json_each(m.tags_json) st WHERE st.value IN (SELECT value FROM json_each(?{})))", args.len() + 1));
        args.push(Value::from(serde_json::to_string(tags).expect("tags")));
    }
    sql.push_str(&format!(" ORDER BY s.updated_at DESC, m.id LIMIT ?{} OFFSET ?{}", args.len() + 1, args.len() + 2));
    args.push(Value::from(limit));
    args.push(Value::from(offset));
    let mut st = conn.prepare(&sql)?;
    let rows = st.query_map(params_from_iter(args.iter().map(json_to_sql)), |r| view_row(r, &now))?;
    Ok(rows.collect::<std::result::Result<_, _>>()?)
}

fn json_to_sql(v: &Value) -> rusqlite::types::Value {
    match v {
        Value::Null => rusqlite::types::Value::Null,
        Value::Number(n) => n.as_i64().map(rusqlite::types::Value::Integer).unwrap_or_else(|| rusqlite::types::Value::Real(n.as_f64().unwrap_or(0.0))),
        Value::String(s) => rusqlite::types::Value::Text(s.clone()),
        other => rusqlite::types::Value::Text(other.to_string()),
    }
}

pub fn list_count(conn: &Connection, agent: Option<&str>, tags: &[String]) -> Result<i64> {
    Ok(conn.query_row(
        "SELECT count(*) FROM memories m JOIN memory_state s ON s.id=m.id
        WHERE (?1 IS NULL OR m.author=?1) AND (?2='[]' OR EXISTS
        (SELECT 1 FROM json_each(m.tags_json) t WHERE t.value IN (SELECT value FROM json_each(?2))))",
        params![agent, serde_json::to_string(tags).expect("tags")],
        |r| r.get(0),
    )?)
}

/// 棚卸しのUNKNOWN。履歴と解決済みは別々に選ぶ。
pub fn unknown_page(conn: &Connection, resolved: bool, history: bool, limit: i64, offset: i64) -> Result<(Vec<UnknownRow>, i64)> {
    let condition = "FROM memories m JOIN memory_state s ON s.id=m.id
        WHERE EXISTS(SELECT 1 FROM json_each(m.tags_json) WHERE value='tk:unknown')
        AND (?1 OR NOT EXISTS(SELECT 1 FROM relations r WHERE r.dst=m.id AND r.kind='resolves'))
        AND (?2 OR (s.status='active' AND (s.expires_at IS NULL OR s.expires_at > ?3)
             AND NOT EXISTS(SELECT 1 FROM relations r WHERE r.dst=m.id AND r.kind='supersedes')))";
    let now = crate::now();
    let total = conn.query_row(&format!("SELECT count(*) {condition}"), params![resolved, history, now], |r| r.get(0))?;
    let mut st = conn.prepare(&format!("SELECT m.id {condition} ORDER BY s.updated_at DESC, m.id LIMIT ?4 OFFSET ?5"))?;
    let ids: Vec<String> = st.query_map(params![resolved, history, now, limit, offset], |r| r.get(0))?.collect::<std::result::Result<_, _>>()?;
    let mut rows = vec![];
    for id in ids {
        if let Some(view) = view(conn, &id)? {
            let resolved_by = conn
                .query_row("SELECT src FROM relations WHERE dst=?1 AND kind='resolves' ORDER BY created_at DESC LIMIT 1", [&id], |r| r.get(0))
                .optional()?;
            let derived = conn.query_row("SELECT count(*) FROM relations WHERE dst=?1 AND kind='derived_from'", [&id], |r| r.get(0))?;
            rows.push(UnknownRow { view, resolved_by, derived });
        }
    }
    Ok((rows, total))
}

/// 検索の母集団を絞る条件。v3の eligibilityClause と同じ意味。
#[derive(Debug, Clone, Default)]
pub struct Filter {
    pub agent: Option<String>,
    pub tags: Vec<String>,
    pub include_history: bool,
    pub include_hidden: bool,
}

impl Filter {
    fn sql(&self, args: &mut Vec<Value>, now: &str) -> String {
        let mut s = String::new();
        if let Some(a) = &self.agent {
            args.push(Value::from(a.clone()));
            s.push_str(&format!(" AND m.author=?{}", args.len()));
        }
        if !self.tags.is_empty() {
            args.push(Value::from(serde_json::to_string(&self.tags).expect("tags")));
            s.push_str(&format!(" AND EXISTS (SELECT 1 FROM json_each(m.tags_json) st WHERE st.value IN (SELECT value FROM json_each(?{})))", args.len()));
        }
        if !self.include_history {
            args.push(Value::from(now.to_string()));
            s.push_str(&format!(" AND s.status='active' AND (s.expires_at IS NULL OR s.expires_at > ?{})", args.len()));
        }
        if !self.include_hidden {
            s.push_str(" AND s.visibility IN ('hot','warm')");
        }
        s
    }
}

pub struct FtsRow {
    pub id: String,
    pub topic: String,
    pub body_folded: String,
    pub score: f64,
}

/// FTS5（trigram）での候補。3字以上の語はMATCH、2字の語は部分一致（v3の searchFTS と同じ）。
pub fn fts_candidates(conn: &Connection, long: &[String], short: &[String], f: &Filter) -> Result<Vec<FtsRow>> {
    let now = crate::now();
    let mut out: HashMap<String, FtsRow> = HashMap::new();
    let fold = |topic: &str, content: &str, tags: &str| format!("{topic} {content} {tags}").to_lowercase();
    if !long.is_empty() {
        let phrase = long.iter().map(|t| format!("\"{}\"", t.replace('"', ""))).collect::<Vec<_>>().join(" OR ");
        let mut args = vec![Value::from(phrase)];
        let filter = f.sql(&mut args, &now);
        let sql = format!(
            "SELECT m.id, m.topic, m.content, m.tags_json, -bm25(memories_fts, 0, 3, 1, 1, 0.5) AS score
               FROM memories_fts JOIN memories m ON m.id = memories_fts.id JOIN memory_state s ON s.id=m.id
              WHERE memories_fts MATCH ?1{filter}"
        );
        let mut st = conn.prepare(&sql)?;
        let rows = st.query_map(params_from_iter(args.iter().map(json_to_sql)), |r| {
            let (topic, content, tags): (String, String, String) = (r.get(1)?, r.get(2)?, r.get(3)?);
            Ok(FtsRow { id: r.get(0)?, body_folded: fold(&topic, &content, &tags), topic, score: r.get(4)? })
        })?;
        for row in rows {
            let row = row?;
            out.insert(row.id.clone(), row);
        }
    }
    if !short.is_empty() {
        let mut args: Vec<Value> = short.iter().map(|t| Value::from(t.clone())).collect();
        let ors =
            (1..=short.len()).map(|i| format!("instr(lower(m.topic || ' ' || m.content || ' ' || m.tags_json), ?{i}) > 0")).collect::<Vec<_>>().join(" OR ");
        let filter = f.sql(&mut args, &now);
        let sql = format!("SELECT m.id, m.topic, m.content, m.tags_json FROM memories m JOIN memory_state s ON s.id=m.id WHERE ({ors}){filter}");
        let mut st = conn.prepare(&sql)?;
        let rows = st.query_map(params_from_iter(args.iter().map(json_to_sql)), |r| {
            let (topic, content, tags): (String, String, String) = (r.get(1)?, r.get(2)?, r.get(3)?);
            Ok(FtsRow { id: r.get(0)?, body_folded: fold(&topic, &content, &tags), topic, score: 0.0 })
        })?;
        for row in rows {
            let row = row?;
            out.entry(row.id.clone()).or_insert(row);
        }
    }
    Ok(out.into_values().collect())
}

pub fn population(conn: &Connection) -> Result<i64> {
    Ok(conn.query_row("SELECT count(*) FROM memories", [], |r| r.get(0))?)
}

/// 今のモデルのvectorを、母集団の条件付きで全て読む。
pub fn vectors(conn: &Connection, model_id: &str, f: &Filter) -> Result<Vec<(String, Vec<u8>)>> {
    let now = crate::now();
    let mut args = vec![Value::from(model_id.to_string())];
    let filter = f.sql(&mut args, &now);
    let sql = format!(
        "SELECT m.id, e.vector FROM embeddings e JOIN memories m ON m.id=e.id JOIN memory_state s ON s.id=m.id
          WHERE e.model_id=?1 AND e.content_hash=m.content_hash{filter}"
    );
    let mut st = conn.prepare(&sql)?;
    let rows = st.query_map(params_from_iter(args.iter().map(json_to_sql)), |r| Ok((r.get(0)?, r.get(1)?)))?;
    Ok(rows.collect::<std::result::Result<_, _>>()?)
}

/// 今のモデルのvectorが無い記憶（workerが計算する）。
pub fn missing_embeddings(conn: &Connection, model_id: &str, limit: i64) -> Result<Vec<(String, String, String, String, String)>> {
    let mut st = conn.prepare(
        "SELECT m.id, m.topic, m.tags_json, m.content, m.content_hash FROM memories m
          WHERE NOT EXISTS (SELECT 1 FROM embeddings e WHERE e.id=m.id AND e.model_id=?1 AND e.content_hash=m.content_hash)
          ORDER BY m.created_at DESC LIMIT ?2",
    )?;
    let rows = st.query_map(params![model_id, limit], |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?, r.get(4)?)))?;
    Ok(rows.collect::<std::result::Result<_, _>>()?)
}

pub fn count_missing_embeddings(conn: &Connection, model_id: &str) -> Result<i64> {
    Ok(conn.query_row(
        "SELECT count(*) FROM memories m
          WHERE NOT EXISTS (SELECT 1 FROM embeddings e WHERE e.id=m.id AND e.model_id=?1 AND e.content_hash=m.content_hash)",
        [model_id],
        |r| r.get(0),
    )?)
}

pub fn meta(conn: &Connection, key: &str) -> Result<Option<String>> {
    Ok(conn.query_row("SELECT value FROM meta WHERE key=?1", [key], |r| r.get(0)).optional()?)
}

pub struct HistoryEvent {
    pub kind: String,
    pub actor: String,
    pub model: Option<String>,
    pub reason: Option<String>,
    pub old_value: Option<String>,
    pub new_value: Option<String>,
    pub created_at: String,
}

pub struct HistoryRelation {
    pub out: bool,
    pub kind: String,
    pub other: String,
    pub reason: Option<String>,
}

/// v3のイベント（`v3.*`）とv4のイベントを、同じ形で時刻順に返す。
pub fn history(conn: &Connection, id: &str) -> Result<(Vec<HistoryEvent>, Vec<HistoryRelation>)> {
    let mut st = conn.prepare("SELECT kind, actor, model, payload_json, created_at FROM events WHERE subject=?1 ORDER BY created_at, seq")?;
    let events = st
        .query_map([id], |r| {
            let kind: String = r.get(0)?;
            let payload: String = r.get(3)?;
            let p: Value = serde_json::from_str(&payload).unwrap_or(Value::Null);
            let s = |k: &str| p.get(k).and_then(|v| v.as_str()).map(String::from);
            Ok(HistoryEvent {
                kind: kind.strip_prefix("v3.").unwrap_or(&kind).to_string(),
                actor: r.get(1)?,
                model: r.get(2)?,
                reason: s("reason"),
                old_value: s("old_value"),
                new_value: s("new_value"),
                created_at: r.get(4)?,
            })
        })?
        .collect::<std::result::Result<Vec<_>, _>>()?;
    let mut rels = vec![];
    for (sql, out) in [
        ("SELECT kind, dst, reason FROM relations WHERE src=?1 ORDER BY created_at", true),
        ("SELECT kind, src, reason FROM relations WHERE dst=?1 ORDER BY created_at", false),
    ] {
        let mut st = conn.prepare(sql)?;
        for r in st.query_map([id], |r| Ok(HistoryRelation { out, kind: r.get(0)?, other: r.get(1)?, reason: r.get(2)? }))? {
            rels.push(r?);
        }
    }
    Ok((events, rels))
}

pub fn status_of(conn: &Connection, id: &str) -> Result<Option<String>> {
    Ok(conn.query_row("SELECT status FROM memory_state WHERE id=?1", [id], |r| r.get(0)).optional()?)
}

pub struct UnknownRow {
    pub view: MemoryView,
    pub resolved_by: Option<String>,
    pub derived: i64,
}

pub fn unknowns(conn: &Connection, include_resolved: bool, limit: i64) -> Result<Vec<UnknownRow>> {
    let mut st = conn.prepare(
        "SELECT m.id,
                (SELECT r.src FROM relations r WHERE r.dst=m.id AND r.kind='resolves' ORDER BY r.created_at DESC LIMIT 1),
                (SELECT count(*) FROM relations r WHERE r.dst=m.id AND r.kind='derived_from')
           FROM memories m JOIN memory_state s ON s.id=m.id
          WHERE m.tags_json LIKE '%\"tk:unknown\"%'
            AND (?1 = 1 OR NOT EXISTS (SELECT 1 FROM relations rr WHERE rr.dst=m.id AND rr.kind='resolves'))
          ORDER BY s.updated_at DESC LIMIT ?2",
    )?;
    let rows: Vec<(String, Option<String>, i64)> = st
        .query_map(params![include_resolved as i64, limit.clamp(1, 500)], |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)))?
        .collect::<std::result::Result<_, _>>()?;
    let mut out = vec![];
    for (id, resolved_by, derived) in rows {
        if let Some(view) = view(conn, &id)? {
            out.push(UnknownRow { view, resolved_by, derived });
        }
    }
    Ok(out)
}

pub fn state_get(conn: &Connection, key: &str) -> Result<Option<(String, String)>> {
    Ok(conn.query_row("SELECT value_json, updated_at FROM state WHERE key=?1", [key], |r| Ok((r.get(0)?, r.get(1)?))).optional()?)
}

pub fn state_list(conn: &Connection, query: Option<&str>, limit: i64, offset: i64) -> Result<Vec<(String, String, String)>> {
    let mut st = conn.prepare(
        "SELECT key, value_json, updated_at FROM state WHERE (?1 IS NULL OR instr(key, ?1) > 0)
          ORDER BY updated_at DESC, key LIMIT ?2 OFFSET ?3",
    )?;
    let rows = st.query_map(params![query, limit, offset], |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)))?;
    Ok(rows.collect::<std::result::Result<_, _>>()?)
}

#[derive(Debug, Clone)]
pub struct StoredAnchor {
    pub ord: i64,
    pub raw: String,
    pub segment: String,
    pub anchor: am_domain::Anchor,
    pub capture: Option<am_domain::Capture>,
    pub origin: String,
    pub recorded_at: String,
}

pub fn anchors_of(conn: &Connection, id: &str) -> Result<Vec<StoredAnchor>> {
    let mut st =
        conn.prepare_cached("SELECT ord, raw, segment, anchor_json, capture_json, origin, recorded_at FROM anchors WHERE memory_id=?1 ORDER BY ord")?;
    let rows = st.query_map([id], |r| {
        let a: String = r.get(3)?;
        let c: Option<String> = r.get(4)?;
        Ok(StoredAnchor {
            ord: r.get(0)?,
            raw: r.get(1)?,
            segment: r.get(2)?,
            anchor: serde_json::from_str(&a).unwrap_or(am_domain::Anchor::Opaque { raw: a }),
            capture: c.and_then(|c| serde_json::from_str(&c).ok()),
            origin: r.get(5)?,
            recorded_at: r.get(6)?,
        })
    })?;
    Ok(rows.collect::<std::result::Result<_, _>>()?)
}

/// 同じ現物（workspace相対パス）を取手に持つ記憶。
pub fn memories_with_anchor_path(conn: &Connection, path: &str) -> Result<Vec<String>> {
    let mut st = conn
        .prepare_cached("SELECT DISTINCT memory_id FROM anchors WHERE json_extract(anchor_json, '$.kind')='file' AND json_extract(anchor_json, '$.path')=?1")?;
    let rows = st.query_map([path], |r| r.get(0))?;
    Ok(rows.collect::<std::result::Result<_, _>>()?)
}

#[derive(Debug, Clone)]
pub struct GoalRow {
    pub id: String,
    pub kind: String,
    pub parent: Option<String>,
    pub title: String,
    pub basis: Vec<am_domain::Basis>,
    pub state: String,
    pub merged_into: Option<String>,
    pub posted_by: String,
    pub posted_at: String,
    pub closed_at: Option<String>,
    pub closed_by: Option<String>,
    pub close_reason: Option<String>,
    pub updated_seq: i64,
}

pub fn goals(conn: &Connection) -> Result<Vec<GoalRow>> {
    let mut st = conn.prepare(
        "SELECT id, kind, parent, title, basis_json, state, merged_into, posted_by, posted_at, closed_at, closed_by, close_reason, updated_seq
           FROM goals ORDER BY posted_at, id",
    )?;
    let rows = st.query_map([], |r| {
        let b: String = r.get(4)?;
        Ok(GoalRow {
            id: r.get(0)?,
            kind: r.get(1)?,
            parent: r.get(2)?,
            title: r.get(3)?,
            basis: serde_json::from_str(&b).unwrap_or_default(),
            state: r.get(5)?,
            merged_into: r.get(6)?,
            posted_by: r.get(7)?,
            posted_at: r.get(8)?,
            closed_at: r.get(9)?,
            closed_by: r.get(10)?,
            close_reason: r.get(11)?,
            updated_seq: r.get(12)?,
        })
    })?;
    Ok(rows.collect::<std::result::Result<_, _>>()?)
}

pub fn next_goal_number(conn: &Connection) -> Result<i64> {
    let n: i64 = conn.query_row("SELECT COALESCE(MAX(CAST(substr(id, 2) AS INTEGER)), 0) FROM goals WHERE id GLOB 'g[0-9]*'", [], |r| r.get(0))?;
    Ok(n + 1)
}

pub struct ProposalRow {
    pub id: String,
    pub action: String,
    pub goal_id: Option<String>,
    pub title: Option<String>,
    pub quote: String,
    pub proposed_by: String,
    pub created_at: String,
}

pub fn open_proposals(conn: &Connection) -> Result<Vec<ProposalRow>> {
    let mut st =
        conn.prepare("SELECT id, action, goal_id, title, quote, proposed_by, created_at FROM goal_proposals WHERE resolved_seq IS NULL ORDER BY created_at")?;
    let rows = st.query_map([], |r| {
        Ok(ProposalRow {
            id: r.get(0)?,
            action: r.get(1)?,
            goal_id: r.get(2)?,
            title: r.get(3)?,
            quote: r.get(4)?,
            proposed_by: r.get(5)?,
            created_at: r.get(6)?,
        })
    })?;
    Ok(rows.collect::<std::result::Result<_, _>>()?)
}

pub fn next_proposal_number(conn: &Connection) -> Result<i64> {
    let n: i64 = conn.query_row("SELECT COALESCE(MAX(CAST(substr(id, 2) AS INTEGER)), 0) FROM goal_proposals WHERE id GLOB 'p[0-9]*'", [], |r| r.get(0))?;
    Ok(n + 1)
}

pub fn goal_events_since(conn: &Connection, seq: i64) -> Result<Vec<(i64, String, String)>> {
    let mut st = conn.prepare("SELECT seq, kind, subject FROM events WHERE seq > ?1 AND kind IN ('goal_posted','goal_edited','goal_closed') ORDER BY seq")?;
    let rows = st.query_map([seq], |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)))?;
    Ok(rows.collect::<std::result::Result<_, _>>()?)
}

pub fn last_seq(conn: &Connection) -> Result<i64> {
    Ok(conn.query_row("SELECT COALESCE(MAX(seq), 0) FROM events", [], |r| r.get(0))?)
}

pub fn goal_links(conn: &Connection, goal: &str) -> Result<Vec<(String, String)>> {
    let mut st = conn.prepare("SELECT memory_id, created_at FROM goal_links WHERE goal_id=?1 ORDER BY created_at")?;
    let rows = st.query_map([goal], |r| Ok((r.get(0)?, r.get(1)?)))?;
    Ok(rows.collect::<std::result::Result<_, _>>()?)
}

pub fn utterance(conn: &Connection, id: &str) -> Result<Option<(String, String, String, i64, String)>> {
    Ok(conn
        .query_row("SELECT client, session, file, line_no, quoted_span FROM utterances WHERE id=?1", [id], |r| {
            Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?, r.get(4)?))
        })
        .optional()?)
}

/// 観測（daemonが見た操作）。札の焦点の間に、その接続・会話で行われた検索・読取り・書込。
pub fn observed_ops(conn: &Connection, conn_ids: &[String], since: &str, limit: i64) -> Result<Vec<(String, String, String)>> {
    if conn_ids.is_empty() {
        return Ok(vec![]);
    }
    let marks = (1..=conn_ids.len()).map(|i| format!("?{i}")).collect::<Vec<_>>().join(",");
    let n = conn_ids.len();
    let sql = format!(
        "SELECT created_at, 'search', COALESCE(query_raw,'') FROM searches WHERE conn_id IN ({marks}) AND created_at >= ?{a}
         UNION ALL SELECT created_at, 'read', memory_id FROM reads WHERE conn_id IN ({marks}) AND created_at >= ?{a}
         UNION ALL SELECT created_at, 'write', subject FROM events WHERE kind='created' AND conn_id IN ({marks}) AND created_at >= ?{a}
         ORDER BY 1 DESC LIMIT ?{b}",
        a = n + 1,
        b = n + 2
    );
    let mut args: Vec<Value> = conn_ids.iter().map(|c| Value::from(c.clone())).collect();
    args.push(Value::from(since.to_string()));
    args.push(Value::from(limit));
    let mut st = conn.prepare(&sql)?;
    let rows = st.query_map(params_from_iter(args.iter().map(json_to_sql)), |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)))?;
    Ok(rows.collect::<std::result::Result<_, _>>()?)
}

pub fn conns_linked_to_goal(conn: &Connection, goal: &str) -> Result<Vec<String>> {
    let mut st = conn.prepare("SELECT DISTINCT conn_id FROM goal_links WHERE goal_id=?1 AND conn_id IS NOT NULL")?;
    let rows = st.query_map([goal], |r| r.get(0))?;
    Ok(rows.collect::<std::result::Result<_, _>>()?)
}

/// 同じ接続（または会話）での、最近の検索とその結果。readの結び付けに使う。
/// `gen` があれば、その会話の同じ世代（圧縮・clearの後は新しい世代）だけを見る（R6）。
pub fn recent_searches(
    conn: &Connection,
    conn_id: Option<&str>,
    conversation_id: Option<&str>,
    gen: Option<i64>,
    since: &str,
) -> Result<Vec<(String, String, Vec<String>, Vec<String>)>> {
    let mut st = conn.prepare(
        "SELECT search_id, created_at, COALESCE(query_terms_json,'[]') FROM searches
          WHERE origin='v4' AND created_at >= ?3 AND ((?2 IS NOT NULL AND conversation_id=?2) OR (?2 IS NULL AND conn_id=?1))
            AND (?4 IS NULL OR gen=?4)
          ORDER BY created_at DESC LIMIT 20",
    )?;
    let rows: Vec<(String, String, String)> =
        st.query_map(params![conn_id, conversation_id, since, gen], |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)))?.collect::<std::result::Result<_, _>>()?;
    let mut out = vec![];
    let mut res = conn.prepare_cached("SELECT memory_id FROM search_results WHERE search_id=?1 ORDER BY rank")?;
    for (sid, at, terms) in rows {
        let ids: Vec<String> = res.query_map([&sid], |r| r.get(0))?.collect::<std::result::Result<_, _>>()?;
        out.push((sid, at, serde_json::from_str(&terms).unwrap_or_default(), ids));
    }
    Ok(out)
}

/// その接続・会話で読んだ記憶と、結び付いた検索。
pub fn reads_in_scope(
    conn: &Connection,
    conn_id: Option<&str>,
    conversation_id: Option<&str>,
    gen: Option<i64>,
    since: &str,
) -> Result<Vec<(String, Option<String>, String)>> {
    let mut st = conn.prepare(
        "SELECT memory_id, search_id, created_at FROM reads
          WHERE created_at >= ?3 AND ((?2 IS NOT NULL AND conversation_id=?2) OR (?2 IS NULL AND conn_id=?1))
            AND (?4 IS NULL OR gen=?4)
          ORDER BY created_at DESC",
    )?;
    let rows = st.query_map(params![conn_id, conversation_id, since, gen], |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)))?;
    Ok(rows.collect::<std::result::Result<_, _>>()?)
}

pub fn search_terms(conn: &Connection, search_id: &str) -> Result<Vec<String>> {
    let t: Option<String> = conn.query_row("SELECT query_terms_json FROM searches WHERE search_id=?1", [search_id], |r| r.get(0)).optional()?.flatten();
    Ok(t.and_then(|t| serde_json::from_str(&t).ok()).unwrap_or_default())
}

/// 学習tagsの証拠。(memory_id, term, scope_key, weight, created_at, kind)
pub type Evidence = (String, String, String, f64, String, String);

pub fn learned_evidence(conn: &Connection, terms: &[String]) -> Result<Vec<Evidence>> {
    if terms.is_empty() {
        return Ok(vec![]);
    }
    let marks = (1..=terms.len()).map(|i| format!("?{i}")).collect::<Vec<_>>().join(",");
    let sql = format!(
        "SELECT le.memory_id, le.term, le.scope_key, le.weight, le.created_at, COALESCE(u.kind, '') FROM learned_evidence le
           JOIN memory_state s ON s.id=le.memory_id LEFT JOIN uses u ON u.use_id=le.use_id
          WHERE le.term IN ({marks}) AND s.status != 'retracted'"
    );
    let mut st = conn.prepare(&sql)?;
    let rows = st.query_map(params_from_iter(terms.iter()), |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?, r.get(4)?, r.get(5)?)))?;
    Ok(rows.collect::<std::result::Result<_, _>>()?)
}

pub fn learned_for_memory(conn: &Connection, id: &str) -> Result<Vec<Evidence>> {
    let mut st = conn.prepare(
        "SELECT le.memory_id, le.term, le.scope_key, le.weight, le.created_at, COALESCE(u.kind, '') FROM learned_evidence le
           LEFT JOIN uses u ON u.use_id=le.use_id WHERE le.memory_id=?1",
    )?;
    let rows = st.query_map([id], |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?, r.get(4)?, r.get(5)?)))?;
    Ok(rows.collect::<std::result::Result<_, _>>()?)
}

/// 別枠（§6.6）：近い過去の問いで開かれた記憶と、開かれた会話数・使われた数。
pub fn past_reads_for_terms(conn: &Connection, terms: &[String], limit: i64) -> Result<Vec<(String, String, i64, i64)>> {
    if terms.is_empty() {
        return Ok(vec![]);
    }
    // 否定された検索語から「過去に読んだ」という別枠で再登場させない。
    let rejected: std::collections::HashSet<String> = conn
        .prepare_cached("SELECT memory_id FROM learned_tags WHERE class='recall' AND raw<=0 AND term IN (SELECT value FROM json_each(?1))")?
        .query_map([serde_json::to_string(terms).expect("terms")], |r| r.get(0))?
        .collect::<std::result::Result<_, _>>()?;
    // 検索語の重なりで近さを見る。θ：語の半分以上（少なくとも1語）が重なる過去の検索。
    let mut st = conn.prepare(
        "SELECT r.memory_id, s.query_raw, s.query_terms_json, COALESCE(r.conversation_id, r.conn_id)
           FROM reads r JOIN searches s ON s.search_id=r.search_id
          WHERE s.origin='v4' ORDER BY r.read_id DESC LIMIT 2000",
    )?;
    let rows: Vec<(String, Option<String>, Option<String>, Option<String>)> =
        st.query_map([], |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?)))?.collect::<std::result::Result<_, _>>()?;
    let want: std::collections::HashSet<&str> = terms.iter().map(|s| s.as_str()).collect();
    let need = (want.len().div_ceil(2)).max(1);
    let mut agg: HashMap<String, (String, std::collections::HashSet<String>)> = HashMap::new();
    for (mid, q, t, scope) in rows {
        if rejected.contains(&mid) {
            continue;
        }
        let ts: Vec<String> = t.and_then(|t| serde_json::from_str(&t).ok()).unwrap_or_default();
        let overlap = ts.iter().filter(|x| want.contains(x.as_str())).count();
        if overlap >= need {
            let e = agg.entry(mid).or_insert_with(|| (q.unwrap_or_default(), Default::default()));
            e.1.insert(scope.unwrap_or_default());
        }
    }
    let mut out = vec![];
    for (mid, (q, scopes)) in agg {
        let used: i64 = conn.query_row("SELECT count(*) FROM uses WHERE memory_id=?1 AND weight > 0 AND kind != 'recalled'", [&mid], |r| r.get(0))?;
        out.push((mid, q, scopes.len() as i64, used));
    }
    out.sort_by(|a, b| b.2.cmp(&a.2).then(a.0.cmp(&b.0)));
    out.truncate(limit as usize);
    Ok(out)
}

pub fn conversation_gen(conn: &Connection, id: &str) -> Result<Option<i64>> {
    Ok(conn.query_row("SELECT gen FROM conversations WHERE conversation_id=?1", [id], |r| r.get(0)).optional()?)
}

pub fn exists(conn: &Connection, id: &str) -> Result<bool> {
    Ok(conn.query_row("SELECT 1 FROM memories WHERE id=?1", [id], |_| Ok(())).optional()?.is_some())
}

/// 渡したidのうち、母集団の条件を満たすもの。
pub fn eligible_ids(conn: &Connection, f: &Filter, ids: Vec<String>) -> Result<Vec<String>> {
    if ids.is_empty() {
        return Ok(vec![]);
    }
    let now = crate::now();
    let mut args = vec![Value::from(serde_json::to_string(&ids).expect("ids"))];
    let filter = f.sql(&mut args, &now);
    let sql = format!("SELECT m.id FROM memories m JOIN memory_state s ON s.id=m.id WHERE m.id IN (SELECT value FROM json_each(?1)){filter}");
    let mut st = conn.prepare(&sql)?;
    let rows = st.query_map(params_from_iter(args.iter().map(json_to_sql)), |r| r.get(0))?;
    Ok(rows.collect::<std::result::Result<_, _>>()?)
}

/// 書いたときの環境（`recorded_env` イベント）。v3から移した記憶には無い。
pub fn recorded_env(conn: &Connection, id: &str) -> Result<Option<Value>> {
    let s: Option<String> =
        conn.query_row("SELECT payload_json FROM events WHERE subject=?1 AND kind='recorded_env' ORDER BY seq DESC LIMIT 1", [id], |r| r.get(0)).optional()?;
    Ok(s.and_then(|s| serde_json::from_str(&s).ok()))
}

/// 学習tagsの集計（schema 4）。(memory_id, term, class, raw, last_pos_at)。撤回した記憶は除く。
pub type TagRow = (String, String, String, f64, Option<String>);

pub fn learned_tags(conn: &Connection, terms: &[String]) -> Result<Vec<TagRow>> {
    if terms.is_empty() {
        return Ok(vec![]);
    }
    let mut st = conn.prepare_cached(
        "SELECT t.memory_id, t.term, t.class, t.raw, t.last_pos_at FROM learned_tags t JOIN memory_state s ON s.id=t.memory_id
          WHERE t.term IN (SELECT value FROM json_each(?1)) AND s.status != 'retracted' AND t.raw > 0",
    )?;
    let rows = st.query_map([serde_json::to_string(terms).expect("terms")], |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?, r.get(4)?)))?;
    Ok(rows.collect::<std::result::Result<_, _>>()?)
}

pub fn learned_tags_for_memory(conn: &Connection, id: &str) -> Result<Vec<TagRow>> {
    let mut st = conn.prepare_cached("SELECT memory_id, term, class, raw, last_pos_at FROM learned_tags WHERE memory_id=?1 AND raw > 0")?;
    let rows = st.query_map([id], |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?, r.get(4)?)))?;
    Ok(rows.collect::<std::result::Result<_, _>>()?)
}
