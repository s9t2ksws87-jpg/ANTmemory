//! 小さなv3 DBを作り、取込みと照合を通す。

use am_domain::{Anchor, AnchorObservation, AnchorRow, AnchorStatus, ApplyCtx, Command};
use am_store::{v3read, verify, Store, StoreError};
use rusqlite::Connection;

const V3_SCHEMA: &str = r#"
CREATE TABLE chunks (id TEXT PRIMARY KEY, topic TEXT NOT NULL, agent TEXT NOT NULL DEFAULT 'global',
  tags TEXT NOT NULL DEFAULT '[]', importance TEXT NOT NULL DEFAULT 'medium', content TEXT NOT NULL,
  embedding BLOB, created_at TEXT NOT NULL, updated_at TEXT NOT NULL, expires_at TEXT);
CREATE VIRTUAL TABLE chunks_fts USING fts5(id UNINDEXED, topic, tags, agent, content, content='chunks', content_rowid=rowid, tokenize='trigram');
CREATE TABLE state (key TEXT PRIMARY KEY, value TEXT NOT NULL, updated_at TEXT NOT NULL);
CREATE TABLE memory_events (event_id TEXT PRIMARY KEY, chunk_id TEXT NOT NULL, event_type TEXT NOT NULL, actor TEXT NOT NULL,
  model TEXT, provider TEXT, reason TEXT, old_value TEXT, new_value TEXT, created_at TEXT NOT NULL);
CREATE TABLE memory_lifecycle (chunk_id TEXT PRIMARY KEY, status TEXT NOT NULL DEFAULT 'active', updated_at TEXT NOT NULL);
CREATE TABLE memory_relations (source_id TEXT NOT NULL, target_id TEXT NOT NULL, relation_type TEXT NOT NULL, actor TEXT NOT NULL,
  reason TEXT, created_at TEXT NOT NULL, PRIMARY KEY (source_id, target_id, relation_type));
CREATE TABLE memory_recall_log (recall_id TEXT PRIMARY KEY, chunk_id TEXT NOT NULL, session_id TEXT, query TEXT, reason TEXT NOT NULL,
  score REAL, rank INTEGER, created_at TEXT NOT NULL);
CREATE TABLE memory_usage (chunk_id TEXT PRIMARY KEY, access_count INTEGER NOT NULL DEFAULT 0, last_accessed_at TEXT, last_session_id TEXT);
"#;

fn v3_fixture(dir: &std::path::Path) -> std::path::PathBuf {
    let p = dir.join("store.db");
    let c = Connection::open(&p).unwrap();
    c.execute_batch(V3_SCHEMA).unwrap();
    c.execute_batch(
        r#"
        INSERT INTO chunks VALUES ('aaaaaaaaaa','計画','codex','["tk:observation","x"]','high','本文A',x'00000000','2026-08-01T00:00:00.000Z','2026-08-01T00:00:00.000Z',NULL);
        INSERT INTO chunks VALUES ('bbbbbbbbbb','訂正後','claude-code','[]','medium','本文B',NULL,'2026-08-02T00:00:00.000Z','2026-08-02T00:00:00.000Z','2026-12-01T00:00:00.000Z');
        INSERT INTO memory_lifecycle VALUES ('aaaaaaaaaa','superseded','2026-08-02T00:00:00.000Z');
        INSERT INTO memory_relations VALUES ('bbbbbbbbbb','aaaaaaaaaa','supersedes','claude-code','訂正','2026-08-02T00:00:00.000Z');
        INSERT INTO memory_events VALUES ('e1','aaaaaaaaaa','created','codex',NULL,NULL,'write_context',NULL,NULL,'2026-08-01T00:00:00.000Z');
        INSERT INTO memory_events VALUES ('e2','aaaaaaaaaa','source_observed','codex',NULL,NULL,'src',NULL,'doc/計画.md','2026-08-01T00:00:01.000Z');
        INSERT INTO state VALUES ('goal.current','{"purpose_memory_id":"aaaaaaaaaa"}','2026-08-03T00:00:00.000Z');
        INSERT INTO memory_recall_log VALUES ('r1','aaaaaaaaaa',NULL,'計画','["relevance_match"]',0.9,1,'2026-08-04T00:00:00.000Z');
        INSERT INTO memory_recall_log VALUES ('r2','bbbbbbbbbb',NULL,'計画','["relevance_match"]',0.5,2,'2026-08-04T00:00:00.000Z');
        INSERT INTO memory_usage VALUES ('aaaaaaaaaa',3,'2026-08-04T00:00:00.000Z',NULL);
        INSERT INTO memory_lifecycle VALUES ('0000000000','active','2026-09-13T15:54:01.877Z');
        "#,
    )
    .unwrap();
    p
}

fn cx() -> ApplyCtx {
    ApplyCtx { actor: "test".into(), build: "test".into(), ..Default::default() }
}

#[test]
fn import_then_verify_matches() {
    let d = tempfile::tempdir().unwrap();
    let v3 = v3_fixture(d.path());
    let snap = d.path().join("snap.db");
    let sha = v3read::snapshot(&v3, &snap).unwrap();
    let snap_conn = v3read::open_read_only(&snap).unwrap();
    let mut batch = v3read::read(&snap_conn, "fixture", &sha).unwrap();
    batch.anchors.push(AnchorRow {
        memory_id: "aaaaaaaaaa".into(),
        ord: 0,
        raw: "doc/計画.md".into(),
        segment: "doc/計画.md".into(),
        anchor: Anchor::File { path: "doc/計画.md".into(), external: false, locator: None, commit: None },
        recorded_at: "2026-08-01T00:00:01.000Z".into(),
    });

    let mut store = Store::create(&d.path().join("v4.db"), "test").unwrap();
    let applied = store.apply(&cx(), Command::ImportV3(Box::new(batch))).unwrap();
    let count = |k: &str| applied.counts.iter().find(|(n, _)| *n == k).unwrap().1;
    assert_eq!(count("memories"), 2);
    assert_eq!(count("status_defaulted"), 1); // bbbbbbbbbb にはlifecycle行が無い
    assert_eq!(count("searches"), 1); // 同じ(query, session, 時刻)は1回の検索
    assert_eq!(count("legacy_rows"), 2); // memory_usage と、記憶の無いlifecycle行

    let report = verify::verify(&snap_conn, store.reader()).unwrap();
    assert!(report.ok(), "{:#?}", report.to_json());

    // ruriへ切り替えて派生vectorが共存しても、v3取込みの照合はv3のvectorだけを見る。
    let _a = store.apply(&cx(), Command::ActivateModel { model_id: "cl-nagoya/ruri-test".into() }).unwrap();
    let hash = store.reader().query_row("SELECT content_hash FROM memories WHERE id='aaaaaaaaaa'", [], |r| r.get(0)).unwrap();
    let _a = store.apply(&cx(), Command::PublishEmbedding {
        id: "aaaaaaaaaa".into(), model_id: "cl-nagoya/ruri-test".into(), content_hash: hash,
        vector: vec![1,2,3,4], computed_by: "v4-ort".into(),
    }).unwrap();
    assert!(verify::verify(&snap_conn, store.reader()).unwrap().ok());

    let kind: String = store.reader().query_row("SELECT kind FROM memories WHERE id='aaaaaaaaaa'", [], |r| r.get(0)).unwrap();
    assert_eq!(kind, "observation");
    // FTSのtrigramで日本語が引ける。
    let hit: String = store.reader().query_row("SELECT id FROM memories_fts WHERE memories_fts MATCH '本文A'", [], |r| r.get(0)).unwrap();
    assert_eq!(hit, "aaaaaaaaaa");
}

#[test]
fn import_twice_is_rejected_and_rolls_back() {
    let d = tempfile::tempdir().unwrap();
    let v3 = v3_fixture(d.path());
    let conn = v3read::open_read_only(&v3).unwrap();
    let mut store = Store::create(&d.path().join("v4.db"), "test").unwrap();
    let _first = store.apply(&cx(), Command::ImportV3(Box::new(v3read::read(&conn, "f", "x").unwrap()))).unwrap();
    let events_before: i64 = store.reader().query_row("SELECT count(*) FROM events", [], |r| r.get(0)).unwrap();
    let err = store.apply(&cx(), Command::ImportV3(Box::new(v3read::read(&conn, "f", "x").unwrap()))).unwrap_err();
    assert!(matches!(err, StoreError::Precondition(_)));
    let events_after: i64 = store.reader().query_row("SELECT count(*) FROM events", [], |r| r.get(0)).unwrap();
    assert_eq!(events_before, events_after);
}

#[test]
fn observation_for_unknown_anchor_fails_without_partial_write() {
    let d = tempfile::tempdir().unwrap();
    let mut store = Store::create(&d.path().join("v4.db"), "test").unwrap();
    let err = store.apply(
        &cx(),
        Command::RecordAnchorObservations(vec![AnchorObservation {
            memory_id: "nope".into(),
            ord: 0,
            observed_at: "t".into(),
            status: AnchorStatus::Missing { reason: "x".into() },
        }]),
    );
    assert!(err.is_err());
    let events: i64 = store.reader().query_row("SELECT count(*) FROM events", [], |r| r.get(0)).unwrap();
    assert_eq!(events, 0, "失敗した命令のイベントが残ってはいけない");
}

#[test]
fn create_refuses_to_overwrite() {
    let d = tempfile::tempdir().unwrap();
    let p = d.path().join("v4.db");
    let _s = Store::create(&p, "test").unwrap();
    assert!(matches!(Store::create(&p, "test"), Err(StoreError::AlreadyExists(_))));
}
