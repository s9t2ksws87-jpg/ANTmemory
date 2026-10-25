//! v3のsnapshotとv4 DBを突き合わせる。両方とも読取りだけ。
//!
//! 一致を強制するためではなく、違いを一覧にするための照合。

use std::collections::HashMap;

use rusqlite::Connection;
use serde_json::json;

use crate::{v3read, Result};

#[derive(Debug, Default)]
pub struct VerifyReport {
    pub checks: Vec<Check>,
    pub mismatches: Vec<String>,
    /// 一致しなかった項目の総数（`mismatches` は先頭だけ持つ）。
    pub mismatch_total: usize,
}

#[derive(Debug)]
pub struct Check {
    pub name: String,
    pub v3: i64,
    pub v4: i64,
    pub ok: bool,
}

const MISMATCH_LIST_LIMIT: usize = 50;

impl VerifyReport {
    fn count(&mut self, name: &str, v3: i64, v4: i64) {
        self.checks.push(Check { name: name.to_string(), v3, v4, ok: v3 == v4 });
        if v3 != v4 {
            self.miss(format!("{name}: v3={v3} v4={v4}"));
        }
    }

    fn miss(&mut self, msg: String) {
        self.mismatch_total += 1;
        if self.mismatches.len() < MISMATCH_LIST_LIMIT {
            self.mismatches.push(msg);
        }
    }

    pub fn ok(&self) -> bool {
        self.mismatch_total == 0
    }

    pub fn to_json(&self) -> serde_json::Value {
        json!({
            "ok": self.ok(),
            "mismatch_total": self.mismatch_total,
            "checks": self.checks.iter().map(|c| json!({"name": c.name, "v3": c.v3, "v4": c.v4, "ok": c.ok})).collect::<Vec<_>>(),
            "mismatches": self.mismatches,
        })
    }
}

fn one(conn: &Connection, sql: &str) -> Result<i64> {
    Ok(conn.query_row(sql, [], |r| r.get(0))?)
}

pub fn verify(v3: &Connection, v4: &Connection) -> Result<VerifyReport> {
    let mut rep = VerifyReport::default();
    let v3_counts: HashMap<String, i64> = v3read::table_counts(v3)?.into_iter().collect();
    let c3 = |t: &str| v3_counts.get(t).copied().unwrap_or(0);

    rep.count("chunks → memories", c3("chunks"), one(v4, "SELECT count(*) FROM memories")?);
    rep.count("memories → memory_state", one(v4, "SELECT count(*) FROM memories")?, one(v4, "SELECT count(*) FROM memory_state")?);
    rep.count("memories → memories_fts", one(v4, "SELECT count(*) FROM memories")?, one(v4, "SELECT count(*) FROM memories_fts")?);
    // 記憶の無い状態行は legacy_rows の `(orphan)` へ移しているので、合わせて数える。
    let orphans = |t: &str| -> Result<i64> {
        Ok(v4.query_row("SELECT count(*) FROM legacy_rows WHERE table_name = ?1", [v3read::orphan_label(t)], |r| r.get(0))?)
    };
    rep.count(
        "memory_lifecycle → status(v3_row) + orphan",
        c3("memory_lifecycle"),
        one(v4, "SELECT count(*) FROM memory_state WHERE status_origin='v3_row'")? + orphans("memory_lifecycle")?,
    );
    rep.count(
        "memory_visibility → visibility(v3_row) + orphan",
        c3("memory_visibility"),
        one(v4, "SELECT count(*) FROM memory_state WHERE visibility_origin='v3_row'")? + orphans("memory_visibility")?,
    );
    rep.count("memory_events → events(v3.*)", c3("memory_events"), one(v4, "SELECT count(*) FROM events WHERE kind LIKE 'v3.%'")?);
    rep.count("memory_relations → relations", c3("memory_relations"), one(v4, "SELECT count(*) FROM relations")?);
    rep.count("state → state", c3("state"), one(v4, "SELECT count(*) FROM state")?);
    rep.count("memory_recall_log → search_results", c3("memory_recall_log"), one(v4, "SELECT count(*) FROM search_results")?);
    rep.count(
        "chunks.embedding → embeddings",
        if v3_counts.contains_key("chunks") { one(v3, "SELECT count(*) FROM chunks WHERE embedding IS NOT NULL")? } else { 0 },
        one(v4, "SELECT count(*) FROM embeddings WHERE model_id=(SELECT value FROM meta WHERE key='v3_embedding_model')")?,
    );

    // legacy_rowsへ移した表は、表ごとに行数を照合する。
    let mut legacy_v4: HashMap<String, i64> = HashMap::new();
    {
        let mut st = v4.prepare("SELECT table_name, count(*) FROM legacy_rows GROUP BY table_name")?;
        for row in st.query_map([], |r| Ok((r.get::<_, String>(0)?, r.get::<_, i64>(1)?)))? {
            let (t, n) = row?;
            legacy_v4.insert(t, n);
        }
    }
    let mapped = ["chunks", "memory_lifecycle", "memory_visibility", "memory_events", "memory_relations", "state", "memory_recall_log"];
    let mut legacy_names: Vec<&String> = v3_counts.keys().filter(|t| !mapped.contains(&t.as_str())).collect();
    legacy_names.sort();
    for t in legacy_names {
        rep.count(&format!("{t} → legacy_rows"), c3(t), legacy_v4.get(t).copied().unwrap_or(0));
    }

    // 記憶ごとに、本文と属性がbyte単位で同じか、hashが作り直しても同じかを確かめる。
    if v3_counts.contains_key("chunks") {
        let mut st3 = v3.prepare("SELECT id, topic, agent, tags, importance, content, created_at, expires_at, embedding FROM chunks")?;
        let mut st4 = v4.prepare(
            "SELECT m.topic, m.author, m.tags_json, s.importance, m.content, m.created_at, s.expires_at, m.content_hash,
                    (SELECT vector FROM embeddings e WHERE e.id = m.id AND e.model_id=(SELECT value FROM meta WHERE key='v3_embedding_model'))
             FROM memories m JOIN memory_state s ON s.id = m.id WHERE m.id = ?1",
        )?;
        let rows = st3.query_map([], |r| {
            Ok((
                r.get::<_, String>(0)?, r.get::<_, String>(1)?, r.get::<_, String>(2)?, r.get::<_, String>(3)?,
                r.get::<_, String>(4)?, r.get::<_, String>(5)?, r.get::<_, String>(6)?, r.get::<_, Option<String>>(7)?,
                r.get::<_, Option<Vec<u8>>>(8)?,
            ))
        })?;
        let mut checked = 0i64;
        let mut equal = 0i64;
        for row in rows {
            let (id, topic, agent, tags, importance, content, created, expires, emb) = row?;
            checked += 1;
            let got = st4.query_row([&id], |r| {
                Ok((
                    r.get::<_, String>(0)?, r.get::<_, String>(1)?, r.get::<_, String>(2)?, r.get::<_, String>(3)?,
                    r.get::<_, String>(4)?, r.get::<_, String>(5)?, r.get::<_, Option<String>>(6)?, r.get::<_, String>(7)?,
                    r.get::<_, Option<Vec<u8>>>(8)?,
                ))
            });
            match got {
                Err(rusqlite::Error::QueryReturnedNoRows) => rep.miss(format!("memory {id}: v4に無い")),
                Err(e) => return Err(e.into()),
                Ok((t4, a4, g4, i4, c4, cr4, ex4, h4, e4)) => {
                    let mut diffs = Vec::new();
                    if t4 != topic { diffs.push("topic") }
                    if a4 != agent { diffs.push("agent") }
                    if g4 != tags { diffs.push("tags") }
                    if i4 != importance { diffs.push("importance") }
                    if c4 != content { diffs.push("content") }
                    if cr4 != created { diffs.push("created_at") }
                    if ex4 != expires { diffs.push("expires_at") }
                    if h4 != am_domain::content_hash(&topic, &tags, &content) { diffs.push("content_hash") }
                    if e4 != emb { diffs.push("embedding") }
                    if diffs.is_empty() {
                        equal += 1;
                    } else {
                        rep.miss(format!("memory {id}: {}", diffs.join(",")));
                    }
                }
            }
        }
        rep.checks.push(Check { name: "memories byte-equal".into(), v3: checked, v4: equal, ok: checked == equal });
    }

    // stateの値も、文字列として同じか確かめる。
    if v3_counts.contains_key("state") {
        let mut st = v3.prepare("SELECT key, value FROM state")?;
        let rows = st.query_map([], |r| Ok((r.get::<_, String>(0)?, r.get::<_, String>(1)?)))?;
        for row in rows {
            let (k, v) = row?;
            let got: Option<String> = v4
                .query_row("SELECT value_json FROM state WHERE key = ?1", [&k], |r| r.get(0))
                .map(Some)
                .or_else(|e| if matches!(e, rusqlite::Error::QueryReturnedNoRows) { Ok(None) } else { Err(e) })?;
            if got.as_deref() != Some(v.as_str()) {
                rep.miss(format!("state {k}: 値が違う"));
            }
        }
    }

    Ok(rep)
}
