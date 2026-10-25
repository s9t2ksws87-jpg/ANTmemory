//! 記憶・state・札・embeddingの命令。失敗した命令は何も残さない。

use am_domain::{ApplyCtx, Basis, Command, GoalClose, NewGoal, NewMemory, UseRecord, Utterance};
use am_store::{query, Store, StoreError};
use serde_json::json;

fn cx() -> ApplyCtx {
    ApplyCtx { actor: "t".into(), build: "t".into(), conn_id: Some("c1".into()), ..Default::default() }
}

fn store() -> (tempfile::TempDir, Store) {
    let d = tempfile::tempdir().unwrap();
    let s = Store::create_with_model(&d.path().join("v4.db"), "t", Some("m1")).unwrap();
    (d, s)
}

fn mem(id: &str, topic: &str) -> NewMemory {
    NewMemory {
        id: id.into(),
        topic: topic.into(),
        content: format!("{topic}の本文"),
        tags: vec!["x".into()],
        importance: "medium".into(),
        expires_at: None,
        source_ref: Some("doc/a.md".into()),
        anchors: vec![],
        env: None,
    }
}

fn write(s: &mut Store, id: &str) {
    let _a = s.apply(&cx(), Command::Write { mem: mem(id, id), reason: "write_context".into(), lineage: None, goal: None, uses: vec![] }).unwrap();
}

fn events(s: &Store) -> i64 {
    s.reader().query_row("SELECT count(*) FROM events", [], |r| r.get(0)).unwrap()
}

#[test]
fn model_switch_is_audited_and_respects_freeze() {
    let (d, mut s) = store();
    write(&mut s, "aaaaaaaaaa");
    let before = events(&s);
    std::fs::write(d.path().join(am_store::V4_FROZEN), "test").unwrap();
    assert!(s.apply(&cx(), Command::ActivateModel { model_id: "ruri".into() }).is_err());
    assert_eq!(query::meta(s.reader(), "active_model_id").unwrap().as_deref(), Some("m1"));
    assert_eq!(events(&s), before);
    std::fs::remove_file(d.path().join(am_store::V4_FROZEN)).unwrap();
    let applied = s.apply(&cx(), Command::ActivateModel { model_id: "ruri".into() }).unwrap();
    assert_eq!(applied.event_seqs.len(), 1);
    let event: (String, String, String) = s.reader().query_row("SELECT kind,actor,payload_json FROM events ORDER BY seq DESC LIMIT 1", [], |r| Ok((r.get(0)?,r.get(1)?,r.get(2)?))).unwrap();
    assert_eq!((event.0.as_str(), event.1.as_str()), ("model_activated", "t"));
    assert_eq!(serde_json::from_str::<serde_json::Value>(&event.2).unwrap(), json!({"old":"m1","new":"ruri"}));
    let _applied = s.apply(&cx(), Command::ActivateModel { model_id: "ruri".into() }).unwrap();
    assert_eq!(events(&s), before + 1);
    assert_eq!(query::view(s.reader(), "aaaaaaaaaa").unwrap().unwrap().content, "aaaaaaaaaaの本文");
}

#[test]
fn v2_fts_upgrade_preserves_raw_and_events_and_respects_freeze() {
    let (d, mut s) = store();
    let cx = ApplyCtx { actor: "authorprobe".into(), ..cx() };
    let _applied = s.apply(&cx, Command::Write { mem: mem("aaaaaaaaaa", "記録"), reason: "test".into(), lineage: None, goal: None, uses: vec![] }).unwrap();
    let before = events(&s);
    let body = query::view(s.reader(), "aaaaaaaaaa").unwrap().unwrap().content;
    // 旧試用DBの索引構造を作る。記憶やイベントは実際のstore経由で保存したもの。
    s.reader().execute_batch("DROP TABLE memories_fts;
        CREATE VIRTUAL TABLE memories_fts USING fts5(id UNINDEXED, topic, tags, content, tokenize='trigram');
        INSERT INTO memories_fts SELECT id,topic,'x',content FROM memories;
        UPDATE meta SET value='2' WHERE key='schema_version';").unwrap();
    drop(s);
    let db = d.path().join("v4.db");
    assert!(Store::open(&db, true).is_err());
    std::fs::write(d.path().join(am_store::V4_FROZEN), "test").unwrap();
    assert!(Store::open(&db, false).is_err());
    let raw = rusqlite::Connection::open(&db).unwrap();
    assert_eq!(query::meta(&raw, "schema_version").unwrap().as_deref(), Some("2"));
    drop(raw);
    std::fs::remove_file(d.path().join(am_store::V4_FROZEN)).unwrap();
    let upgraded = Store::open(&db, false).unwrap();
    assert_eq!(query::meta(upgraded.reader(), "schema_version").unwrap().as_deref(), Some(am_store::SCHEMA_VERSION.to_string().as_str()));
    assert_eq!(events(&upgraded), before);
    assert_eq!(query::view(upgraded.reader(), "aaaaaaaaaa").unwrap().unwrap().content, body);
    let hits = query::fts_candidates(upgraded.reader(), &["authorprobe".into()], &[], &query::Filter::default()).unwrap();
    assert_eq!(hits.iter().map(|r| r.id.as_str()).collect::<Vec<_>>(), vec!["aaaaaaaaaa"]);
    drop(upgraded);
    assert!(Store::open(&db, true).is_ok());
    assert!(Store::open(&db, false).is_ok());
}

#[test]
fn write_supersede_retract_restore() {
    let (_d, mut s) = store();
    write(&mut s, "aaaaaaaaaa");
    let v = query::view(s.reader(), "aaaaaaaaaa").unwrap().unwrap();
    assert_eq!((v.status.as_str(), v.current, v.source_ref.as_deref()), ("active", true, Some("doc/a.md")));

    let uses = vec![UseRecord { memory_id: "aaaaaaaaaa".into(), kind: "cited".into(), weight: 1.0, search_id: None, scope_key: "k".into(), terms: vec!["本文".into()] }];
    let _a = s.apply(&cx(), Command::RecordUses(uses)).unwrap();
    let _a = s
        .apply(&cx(), Command::Supersede { old_id: "aaaaaaaaaa".into(), mem: mem("bbbbbbbbbb", "新"), reason: "直す".into(), goal: None, uses: vec![] })
        .unwrap();
    let old = query::view(s.reader(), "aaaaaaaaaa").unwrap().unwrap();
    assert_eq!((old.status.as_str(), old.current), ("superseded", false));
    // 学習tagsの強度は半分が後継へ移る。
    let w: f64 = s.reader().query_row("SELECT weight FROM learned_evidence WHERE memory_id='bbbbbbbbbb'", [], |r| r.get(0)).unwrap();
    assert_eq!(w, 0.5);

    // 置換済みは二度置換できず、イベントも残らない。
    let before = events(&s);
    let e = s.apply(&cx(), Command::Supersede { old_id: "aaaaaaaaaa".into(), mem: mem("cccccccccc", "c"), reason: "r".into(), goal: None, uses: vec![] });
    assert!(matches!(e, Err(StoreError::Precondition(_))));
    assert_eq!(events(&s), before);
    assert!(query::view(s.reader(), "cccccccccc").unwrap().is_none());

    // 後継のある旧版は戻せない。後継の無い撤回は戻せる。
    assert!(s.apply(&cx(), Command::Restore { id: "aaaaaaaaaa".into(), reason: "r".into() }).is_err());
    let _a = s.apply(&cx(), Command::Retract { id: "bbbbbbbbbb".into(), reason: "誤り".into() }).unwrap();
    assert!(s.apply(&cx(), Command::Retract { id: "bbbbbbbbbb".into(), reason: " ".into() }).is_err());
    let _a = s.apply(&cx(), Command::Restore { id: "bbbbbbbbbb".into(), reason: "戻す".into() }).unwrap();
    assert_eq!(query::view(s.reader(), "bbbbbbbbbb").unwrap().unwrap().status, "active");
    let (h, rels) = query::history(s.reader(), "bbbbbbbbbb").unwrap();
    let kinds: Vec<&str> = h.iter().map(|e| e.kind.as_str()).collect();
    assert_eq!(kinds, ["created", "source_observed", "retracted", "restored"]);
    assert_eq!(rels.len(), 1);
}

#[test]
fn expiry_and_restore_clears_ttl() {
    let (_d, mut s) = store();
    let mut m = mem("aaaaaaaaaa", "期限");
    m.expires_at = Some("2000-01-01T00:00:00.000Z".into());
    let _a = s.apply(&cx(), Command::Write { mem: m, reason: "w".into(), lineage: None, goal: None, uses: vec![] }).unwrap();
    assert_eq!(query::view(s.reader(), "aaaaaaaaaa").unwrap().unwrap().status, "expired");
    let _a = s.apply(&cx(), Command::Restore { id: "aaaaaaaaaa".into(), reason: "要る".into() }).unwrap();
    let v = query::view(s.reader(), "aaaaaaaaaa").unwrap().unwrap();
    assert_eq!((v.status.as_str(), v.expires_at), ("active", None));
}

#[test]
fn repeated_retract_and_restore_are_noops_without_new_history() {
    let (_d, mut s) = store();
    write(&mut s, "aaaaaaaaaa");
    for retract in [false, true, false] {
        if retract {
            let _a = s.apply(&cx(), Command::Retract { id: "aaaaaaaaaa".into(), reason: "撤回".into() }).unwrap();
        }
        let before = events(&s);
        let previous = query::view(s.reader(), "aaaaaaaaaa").unwrap().unwrap();
        let cmd = if retract {
            Command::Retract { id: "aaaaaaaaaa".into(), reason: "再送".into() }
        } else {
            Command::Restore { id: "aaaaaaaaaa".into(), reason: "再送".into() }
        };
        let a = s.apply(&cx(), cmd).unwrap();
        assert_eq!(a.detail["unchanged"], true);
        assert!(a.event_seqs.is_empty());
        assert_eq!(events(&s), before);
        let after = query::view(s.reader(), "aaaaaaaaaa").unwrap().unwrap();
        assert_eq!((after.status, after.updated, after.content), (previous.status, previous.updated, previous.content));
        let _a = s.apply(&cx(), Command::Restore { id: "aaaaaaaaaa".into(), reason: "復帰".into() }).unwrap();
    }
    assert!(s.apply(&cx(), Command::Restore { id: "aaaaaaaaaa".into(), reason: " ".into() }).is_err());
}

#[test]
fn state_cas() {
    let (_d, mut s) = store();
    let _a = s.apply(&cx(), Command::SetState { key: "k".into(), value: json!({"a": 1}), expected: None }).unwrap();
    let e = s.apply(&cx(), Command::SetState { key: "k".into(), value: json!(2), expected: Some(json!({"a": 2})) });
    assert!(matches!(e, Err(StoreError::CasConflict { .. })));
    let _a = s.apply(&cx(), Command::SetState { key: "k".into(), value: json!(2), expected: Some(json!({"a": 1})) }).unwrap();
    // 無いキーは null と比べる（v3と同じ）。
    let _a = s.apply(&cx(), Command::SetState { key: "new".into(), value: json!(1), expected: Some(json!(null)) }).unwrap();
    assert_eq!(query::state_get(s.reader(), "k").unwrap().unwrap().0, "2");
}

#[test]
fn state_cas_treats_json_text_as_the_same_object() {
    // 意見箱 2026-10-25：文字列で書いたobjectを、objectの expected_value で更新する（逆も）。
    let (_d, mut s) = store();
    let as_text = json!("{\"count\":100,\"step\":1}");
    let _a = s.apply(&cx(), Command::SetState { key: "t".into(), value: as_text.clone(), expected: None }).unwrap();
    // 保存した値は渡したまま（文字列）。
    assert_eq!(query::state_get(s.reader(), "t").unwrap().unwrap().0, as_text.to_string());
    let _a = s
        .apply(&cx(), Command::SetState { key: "t".into(), value: json!({"step": 2, "count": 101}), expected: Some(json!({"step": 1, "count": 100})) })
        .unwrap();
    let _a = s
        .apply(&cx(), Command::SetState { key: "t".into(), value: json!(3), expected: Some(json!("{\"count\":101,\"step\":2}")) })
        .unwrap();
    // 中身が違えば、形が揃っても拒否する。ただの文字列は文字列のまま比べる。
    let e = s.apply(&cx(), Command::SetState { key: "t".into(), value: json!(4), expected: Some(json!("3")) });
    assert!(matches!(e, Err(StoreError::CasConflict { .. })));
    let e = s.apply(&cx(), Command::SetState { key: "t".into(), value: json!(4), expected: Some(json!("{\"count\":0}")) });
    assert!(matches!(e, Err(StoreError::CasConflict { .. })));
}

fn goal(id: &str, kind: &str, parent: Option<&str>, title: &str, basis: Vec<Basis>, u: Option<Utterance>) -> Command {
    Command::PostGoal(NewGoal {
        id: id.into(),
        kind: kind.into(),
        parent: parent.map(String::from),
        title: title.into(),
        basis,
        utterance: u,
        resolves_proposal: None,
    })
}

#[test]
fn goal_rules() {
    let (_d, mut s) = store();
    let note = || vec![Basis::Initiative { note: "理由".into(), from: None }];
    // 24字を超える名前は切り詰めずに拒否する。
    assert!(s.apply(&cx(), goal("g1", "work", None, &"あ".repeat(25), note(), None)).is_err());
    // 最上位の作業目標には根拠が要る。全体目標には照合した発言が要る。
    assert!(s.apply(&cx(), goal("g1", "work", None, "作業", vec![], None)).is_err());
    assert!(s.apply(&cx(), goal("g1", "overall", None, "全体", vec![Basis::UnverifiedQuote { quote: "q".into() }], None)).is_err());
    assert_eq!(events(&s), 0, "拒否した札はイベントを残さない");

    let u = Utterance {
        id: "u1".into(),
        client: "claude-code".into(),
        session: "S".into(),
        file: "f".into(),
        line_no: 1,
        line_sha256: "h".into(),
        quoted_span: "徒競走のゲーム".into(),
        said_at: None,
    };
    let _a = s
        .apply(&cx(), goal("g1", "overall", None, "徒競走ゲームを作る", vec![Basis::Utterance { utterance_id: "u1".into(), quote: "徒競走のゲーム".into() }], Some(u)))
        .unwrap();
    let _a = s.apply(&cx(), goal("g2", "work", Some("g1"), "物理演算", vec![], None)).unwrap();
    let _a = s.apply(&cx(), goal("g3", "work", Some("g2"), "接地振動", vec![], None)).unwrap();
    // 全体目標は、照合した発言なしでは下ろせない。
    assert!(s.apply(&cx(), Command::CloseGoal { id: "g1".into(), how: GoalClose::Done, reason: None, utterance: None }).is_err());
    assert!(s.apply(&cx(), Command::CloseGoal { id: "g3".into(), how: GoalClose::Merged { into: "g3".into() }, reason: None, utterance: None }).is_err());
    let _a = s.apply(&cx(), Command::CloseGoal { id: "g3".into(), how: GoalClose::Done, reason: None, utterance: None }).unwrap();
    assert!(s.apply(&cx(), Command::CloseGoal { id: "g3".into(), how: GoalClose::Dropped, reason: None, utterance: None }).is_err());
    // 下ろした札の子には掲げられない。
    assert!(s.apply(&cx(), goal("g4", "work", Some("g3"), "子", vec![], None)).is_err());
    let gs = query::goals(s.reader()).unwrap();
    assert_eq!(gs.iter().map(|g| g.state.as_str()).collect::<Vec<_>>(), ["posted", "posted", "done"]);
    // 子の根拠には親が入る。
    assert!(matches!(gs[1].basis[0], Basis::Parent { .. }));
}

#[test]
fn embedding_publish_checks_model_and_hash() {
    let (_d, mut s) = store();
    write(&mut s, "aaaaaaaaaa");
    let hash = query::view(s.reader(), "aaaaaaaaaa").unwrap().unwrap().content_hash;
    let pubv = |model: &str, hash: &str| Command::PublishEmbedding {
        id: "aaaaaaaaaa".into(),
        model_id: model.into(),
        content_hash: hash.into(),
        vector: vec![0u8; 8],
        computed_by: "t".into(),
    };
    let r = s.apply(&cx(), pubv("other", &hash)).unwrap();
    assert!(r.detail.get("discarded").is_some());
    let r = s.apply(&cx(), pubv("m1", "wrong")).unwrap();
    assert!(r.detail.get("discarded").is_some());
    assert_eq!(query::count_missing_embeddings(s.reader(), "m1").unwrap(), 1);
    let _a = s.apply(&cx(), pubv("m1", &hash)).unwrap();
    assert_eq!(query::count_missing_embeddings(s.reader(), "m1").unwrap(), 0);
}

#[test]
fn learned_summary_counts_each_conversation_once() {
    let (_d, mut s) = store();
    write(&mut s, "aaaaaaaaaa");
    let u = |scope: &str, kind: &str, w: f64| UseRecord {
        memory_id: "aaaaaaaaaa".into(),
        kind: kind.into(),
        weight: w,
        search_id: None,
        scope_key: scope.into(),
        terms: vec!["接地".into()],
    };
    // 同じ会話で何度使っても最大の1回。別の会話は足す。否定は引く。recalled は別の種類。
    let _a = s
        .apply(&cx(), Command::RecordUses(vec![u("a", "cited", 1.0), u("a", "cited", 1.0), u("b", "followed", 0.4), u("b", "reformulated", -0.2), u("a", "recalled", 0.3), u("a", "recalled", 0.3)]))
        .unwrap();
    let rows = query::learned_tags_for_memory(s.reader(), "aaaaaaaaaa").unwrap();
    let raw = |class: &str| rows.iter().find(|r| r.2 == class).map(|r| r.3).unwrap();
    assert!((raw("useful") - 1.2).abs() < 1e-9, "{rows:?}");
    assert!((raw("recall") - 0.3).abs() < 1e-9, "{rows:?}");
}

#[test]
fn old_rejection_rebuilds_only_derived_recall_and_respects_freeze() {
    let (d, mut s) = store();
    write(&mut s, "aaaaaaaaaa");
    let usage = |kind: &str, weight: f64| UseRecord {
        memory_id: "aaaaaaaaaa".into(),
        kind: kind.into(),
        weight,
        search_id: None,
        scope_key: "conn:source".into(),
        terms: vec!["needle".into()],
    };
    let _a = s.apply(&cx(), Command::RecordUses(vec![usage("recalled", 0.3), usage("rejected", -1.0)])).unwrap();
    let body = query::view(s.reader(), "aaaaaaaaaa").unwrap().unwrap().content;
    let count = events(&s);
    let evidence: i64 = s.reader().query_row("SELECT count(*) FROM learned_evidence", [], |r| r.get(0)).unwrap();
    // 旧版ではusefulだけに否定が入り、recallには+0.3が残っていた。
    s.reader()
        .execute_batch(
            "UPDATE learned_scope SET neg=0 WHERE class='recall';
        UPDATE learned_tags SET raw=0.3 WHERE class='recall';
        DELETE FROM meta WHERE key='learning_rules_version';",
        )
        .unwrap();
    drop(s);
    let db = d.path().join("v4.db");
    let readonly = Store::open(&db, true).unwrap();
    assert!(query::meta(readonly.reader(), "learning_rules_version").unwrap().is_none());
    drop(readonly);
    std::fs::write(d.path().join(am_store::V4_FROZEN), "test").unwrap();
    assert!(Store::open(&db, false).is_err());
    std::fs::remove_file(d.path().join(am_store::V4_FROZEN)).unwrap();
    let s = Store::open(&db, false).unwrap();
    let strength: f64 = s.reader().query_row("SELECT raw FROM learned_tags WHERE class='recall' AND term='needle'", [], |r| r.get(0)).unwrap();
    assert!(strength <= 0.0);
    assert_eq!(query::meta(s.reader(), "learning_rules_version").unwrap().as_deref(), Some("2"));
    assert_eq!(events(&s), count);
    assert_eq!(s.reader().query_row("SELECT count(*) FROM learned_evidence", [], |r| r.get::<_, i64>(0)).unwrap(), evidence);
    assert_eq!(query::view(s.reader(), "aaaaaaaaaa").unwrap().unwrap().content, body);
    drop(s);
    let s = Store::open(&db, false).unwrap();
    assert_eq!(s.reader().query_row("SELECT raw FROM learned_tags WHERE class='recall' AND term='needle'", [], |r| r.get::<_, f64>(0)).unwrap(), strength);
}
