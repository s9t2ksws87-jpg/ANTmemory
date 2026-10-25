//! v3のstoreを読む。読取り専用で、v3のファイルには書かない。

use std::path::Path;
use std::time::Duration;

use am_domain::legacy::{LegacyRow, V3Chunk, V3Event, V3Recall, V3Relation, V3State};
use am_domain::V3Batch;
use rusqlite::backup::Backup;
use rusqlite::types::ValueRef;
use rusqlite::{Connection, OpenFlags};
use sha2::{Digest, Sha256};

use crate::Result;

/// embeddingのモデルと計算の手順（server/lib/native-embeddings.mjs と同じ：平均・L2正規化・最大512 token）。
/// v3（transformers.js）とv4（tract）で同じ値になることは、移行時に標本で確かめて記録する。
pub const MODEL_ID: &str = "Xenova/all-MiniLM-L6-v2:mean:l2:max512";

/// v4で専用の表へ移すv3の表。これ以外は `legacy_rows` へそのまま移す。
const MAPPED: &[&str] = &[
    "chunks",
    "memory_lifecycle",
    "memory_visibility",
    "memory_events",
    "memory_relations",
    "state",
    "memory_recall_log",
];

pub fn open_read_only(path: &Path) -> Result<Connection> {
    Ok(Connection::open_with_flags(
        path,
        OpenFlags::SQLITE_OPEN_READ_ONLY | OpenFlags::SQLITE_OPEN_NO_MUTEX,
    )?)
}

/// SQLiteのbackup APIで一貫したsnapshotを取る。v3が動いていても、その時点の版になる。
/// 戻り値はsnapshotファイルのsha256。
pub fn snapshot(src: &Path, dst: &Path) -> Result<String> {
    let from = open_read_only(src)?;
    let mut to = Connection::open(dst)?;
    {
        let backup = Backup::new(&from, &mut to)?;
        backup.run_to_completion(256, Duration::ZERO, None)?;
    }
    drop(to);
    let bytes = std::fs::read(dst).map_err(|e| crate::StoreError::Precondition(format!("snapshotを読めない: {e}")))?;
    Ok(hex::encode(Sha256::digest(&bytes)))
}

fn tables(conn: &Connection) -> Result<Vec<String>> {
    let mut st = conn.prepare("SELECT name FROM sqlite_master WHERE type='table' ORDER BY name")?;
    let names = st.query_map([], |r| r.get::<_, String>(0))?.collect::<std::result::Result<Vec<_>, _>>()?;
    Ok(names)
}

fn is_internal(name: &str) -> bool {
    // FTSの索引はv4で作り直す派生物。sqlite_* はSQLite自身の表。
    name.starts_with("sqlite_") || name == "chunks_fts" || name.starts_with("chunks_fts_")
}

pub fn read(conn: &Connection, source_path: &str, snapshot_sha256: &str) -> Result<V3Batch> {
    let names = tables(conn)?;
    let has = |t: &str| names.iter().any(|n| n == t);

    let status_sql = if has("memory_lifecycle") {
        "(SELECT l.status FROM memory_lifecycle l WHERE l.chunk_id = c.id)"
    } else {
        "NULL"
    };
    let vis_sql = if has("memory_visibility") {
        "(SELECT v.visibility FROM memory_visibility v WHERE v.chunk_id = c.id)"
    } else {
        "NULL"
    };
    let src_sql = if has("memory_events") {
        "(SELECT e.new_value FROM memory_events e WHERE e.chunk_id = c.id AND e.event_type = 'source_observed' ORDER BY e.created_at DESC LIMIT 1)"
    } else {
        "NULL"
    };
    let sql = format!(
        "SELECT c.id, c.topic, c.agent, c.tags, c.importance, c.content, c.embedding,
                c.created_at, c.updated_at, c.expires_at, {src_sql}, {status_sql}, {vis_sql}
         FROM chunks c ORDER BY c.created_at, c.id"
    );
    let chunks = conn
        .prepare(&sql)?
        .query_map([], |r| {
            Ok(V3Chunk {
                id: r.get(0)?,
                topic: r.get(1)?,
                agent: r.get(2)?,
                tags_json: r.get(3)?,
                importance: r.get(4)?,
                content: r.get(5)?,
                embedding: r.get(6)?,
                created_at: r.get(7)?,
                updated_at: r.get(8)?,
                expires_at: r.get(9)?,
                source_ref: r.get(10)?,
                status: r.get(11)?,
                visibility: r.get(12)?,
            })
        })?
        .collect::<std::result::Result<Vec<_>, _>>()?;

    let events = if has("memory_events") {
        conn.prepare(
            "SELECT event_id, chunk_id, event_type, actor, model, provider, reason, old_value, new_value, created_at
             FROM memory_events",
        )?
        .query_map([], |r| {
            Ok(V3Event {
                event_id: r.get(0)?,
                chunk_id: r.get(1)?,
                event_type: r.get(2)?,
                actor: r.get(3)?,
                model: r.get(4)?,
                provider: r.get(5)?,
                reason: r.get(6)?,
                old_value: r.get(7)?,
                new_value: r.get(8)?,
                created_at: r.get(9)?,
            })
        })?
        .collect::<std::result::Result<Vec<_>, _>>()?
    } else {
        Vec::new()
    };

    let relations = if has("memory_relations") {
        conn.prepare("SELECT source_id, target_id, relation_type, actor, reason, created_at FROM memory_relations")?
            .query_map([], |r| {
                Ok(V3Relation {
                    source_id: r.get(0)?,
                    target_id: r.get(1)?,
                    relation_type: r.get(2)?,
                    actor: r.get(3)?,
                    reason: r.get(4)?,
                    created_at: r.get(5)?,
                })
            })?
            .collect::<std::result::Result<Vec<_>, _>>()?
    } else {
        Vec::new()
    };

    let state = if has("state") {
        conn.prepare("SELECT key, value, updated_at FROM state ORDER BY key")?
            .query_map([], |r| Ok(V3State { key: r.get(0)?, value: r.get(1)?, updated_at: r.get(2)? }))?
            .collect::<std::result::Result<Vec<_>, _>>()?
    } else {
        Vec::new()
    };

    let recalls = if has("memory_recall_log") {
        conn.prepare(
            "SELECT recall_id, chunk_id, session_id, query, reason, score, rank, created_at
             FROM memory_recall_log ORDER BY created_at, rank, recall_id",
        )?
        .query_map([], |r| {
            Ok(V3Recall {
                recall_id: r.get(0)?,
                chunk_id: r.get(1)?,
                session_id: r.get(2)?,
                query: r.get(3)?,
                reason: r.get(4)?,
                score: r.get(5)?,
                rank: r.get(6)?,
                created_at: r.get(7)?,
            })
        })?
        .collect::<std::result::Result<Vec<_>, _>>()?
    } else {
        Vec::new()
    };

    let mut legacy = Vec::new();
    for t in names.iter().filter(|n| !MAPPED.contains(&n.as_str()) && !is_internal(n)) {
        legacy.extend(read_legacy_table(conn, t)?);
    }
    // 記憶が無いのに状態だけある行（v3で、存在しないidへのrestore等が作った）。
    // memory_state には入れられないので、落とさずに legacy_rows へ移す。
    for t in ["memory_lifecycle", "memory_visibility"].into_iter().filter(|t| has(t)) {
        let sql = format!("SELECT * FROM {t} x WHERE NOT EXISTS (SELECT 1 FROM chunks c WHERE c.id = x.chunk_id)");
        legacy.extend(read_rows(conn, &sql, &orphan_label(t))?);
    }

    Ok(V3Batch {
        source_path: source_path.to_string(),
        snapshot_sha256: snapshot_sha256.to_string(),
        embedding_model: MODEL_ID.to_string(),
        chunks,
        events,
        relations,
        state,
        recalls,
        legacy,
        anchors: Vec::new(),
    })
}

pub fn orphan_label(table: &str) -> String {
    format!("{table}(orphan)")
}

/// 表の行を、列名をkeyにしたJSONへそのまま写す。blobはhexにする。
pub fn read_legacy_table(conn: &Connection, table: &str) -> Result<Vec<LegacyRow>> {
    read_rows(conn, &format!("SELECT * FROM \"{}\"", table.replace('"', "\"\"")), table)
}

fn read_rows(conn: &Connection, sql: &str, table: &str) -> Result<Vec<LegacyRow>> {
    let mut st = conn.prepare(sql)?;
    let cols: Vec<String> = st.column_names().iter().map(|s| s.to_string()).collect();
    let mut rows = st.query([])?;
    let mut out = Vec::new();
    while let Some(r) = rows.next()? {
        let mut obj = serde_json::Map::new();
        for (i, c) in cols.iter().enumerate() {
            let v = match r.get_ref(i)? {
                ValueRef::Null => serde_json::Value::Null,
                ValueRef::Integer(n) => serde_json::json!(n),
                ValueRef::Real(f) => serde_json::json!(f),
                // 不正なUTF-8を置換文字で書き換えないよう、その場合はhexで持つ。
                ValueRef::Text(t) => match std::str::from_utf8(t) {
                    Ok(s) => serde_json::Value::String(s.to_string()),
                    Err(_) => serde_json::json!({ "$text_hex": hex::encode(t) }),
                },
                ValueRef::Blob(b) => serde_json::json!({ "$blob_hex": hex::encode(b) }),
            };
            obj.insert(c.clone(), v);
        }
        out.push(LegacyRow { table: table.to_string(), row_json: serde_json::Value::Object(obj).to_string() });
    }
    Ok(out)
}

/// 照合用：v3の表ごとの行数。
pub fn table_counts(conn: &Connection) -> Result<Vec<(String, i64)>> {
    let mut out = Vec::new();
    for t in tables(conn)?.into_iter().filter(|n| !is_internal(n)) {
        let n: i64 = conn.query_row(&format!("SELECT count(*) FROM \"{}\"", t.replace('"', "\"\"")), [], |r| r.get(0))?;
        out.push((t, n));
    }
    Ok(out)
}
