//! 実チャットで見つけた、接続先の見落としと検索由来タグの否定を再現する。
use am_store::{query, Store};
use am_tools::{
    ctx::{Session, Workspace},
    render::estimate_tokens,
    tools::call,
    workspace::FALLBACK_NOTICE,
};
use serde_json::{json, Value};

fn fixture() -> (tempfile::TempDir, Workspace, Session) {
    let d = tempfile::tempdir().unwrap();
    let store = Store::create(&d.path().join("v4.db"), "t").unwrap();
    let ws = Workspace::new("t", Some(d.path().into()), store, None, None, "t").unwrap();
    (d, ws, Session::new("original", "codex", "t"))
}
fn run(w: &mut Workspace, s: &mut Session, name: &str, args: Value) -> String {
    let r = call(w, s, name, &args);
    assert!(!r.is_error, "{name}: {}", r.text);
    r.text
}
fn reference(text: &str) -> String {
    text.split("**read:** `").nth(1).unwrap().split('`').next().unwrap().into()
}

#[test]
fn reading_then_rejecting_across_connections_stops_recall_without_touching_raw_or_other_terms() {
    let (_d, mut w, mut original) = fixture();
    let raw = "実験記録。本文をそのまま保つ。";
    let saved = run(&mut w, &mut original, "write_context", json!({"content":raw}));
    let mid = saved.split('`').nth(1).unwrap().to_string();
    let found = run(&mut w, &mut original, "search_context", json!({"query":"実験記録 xyzzyaudit","search_mode":"bm25"}));
    let via = reference(&found);
    let mut reader = Session::new("reader", "codex", "t");
    run(&mut w, &mut reader, "read_context", json!({"id":via}));
    assert!(run(&mut w, &mut original, "search_context", json!({"query":"xyzzyaudit","search_mode":"bm25"})).contains(&mid));
    let other = run(&mut w, &mut original, "search_context", json!({"query":"実験記録 keepword","search_mode":"bm25"}));
    run(&mut w, &mut reader, "read_context", json!({"id":reference(&other)}));
    let mut reject = Session::new("reject", "antigravity", "t");
    run(&mut w, &mut reject, "read_context", json!({"id":via,"verdict":"not_relevant"}));
    let strength = |w: &Workspace| -> f64 {
        w.conn().query_row("SELECT raw FROM learned_tags WHERE memory_id=?1 AND term='xyzzyaudit' AND class='recall'", [&mid], |r| r.get(0)).unwrap()
    };
    let negative = strength(&w);
    assert!(negative <= 0.0);
    let mut repeated = Session::new("repeat", "codex", "t");
    run(&mut w, &mut repeated, "read_context", json!({"id":via,"verdict":"not_relevant"}));
    assert_eq!(strength(&w), negative); // 同じ検索元scopeの明示否定は重複計上しない。
    assert!(!run(&mut w, &mut original, "search_context", json!({"query":"xyzzyaudit","search_mode":"bm25"})).contains(&mid));
    assert!(run(&mut w, &mut original, "search_context", json!({"query":"keepword","search_mode":"bm25"})).contains(&mid));
    assert_eq!(query::view(w.conn(), &mid).unwrap().unwrap().content, raw);
    let retained: i64 = w.conn().query_row("SELECT count(*) FROM uses WHERE memory_id=?1", [&mid], |r| r.get(0)).unwrap();
    assert!(retained >= 4); // 観測と否定の履歴は消さない。
}

#[test]
fn search_rejection_also_cancels_the_learned_only_candidate() {
    let (_d, mut w, mut s) = fixture();
    let saved = run(&mut w, &mut s, "write_context", json!({"content":"実験記録"}));
    let mid = saved.split('`').nth(1).unwrap().to_string();
    let first = run(&mut w, &mut s, "search_context", json!({"query":"実験記録 xyzzyaudit","search_mode":"bm25"}));
    run(&mut w, &mut s, "read_context", json!({"id":reference(&first)}));
    run(&mut w, &mut s, "search_context", json!({"query":"xyzzyaudit","search_mode":"bm25","not_relevant":[mid]}));
    // フィードバックを受けた次の検索から反映する（この呼出し自身の順位は学習前に計算される）。
    assert!(!run(&mut w, &mut s, "search_context", json!({"query":"xyzzyaudit","search_mode":"bm25"})).contains(&mid));
}

#[test]
fn fallback_notice_is_once_and_shares_the_budget_without_truncating_full_raw() {
    let (_d, mut w, mut s) = fixture();
    let saved = run(&mut w, &mut s, "write_context", json!({"content":"実験記録。".repeat(300)}));
    let mid = saved.split('`').nth(1).unwrap().to_string();
    s.workspace_notice = Some(FALLBACK_NOTICE.into());
    let invalid = call(&mut w, &mut s, "search_context", &json!({"query":"実験記録","token_budget":1000001}));
    assert!(invalid.is_error && s.workspace_notice.is_some());
    let found = run(&mut w, &mut s, "search_context", json!({"query":"実験記録","search_mode":"bm25","token_budget":500}));
    assert!(found.starts_with(FALLBACK_NOTICE) && estimate_tokens(&found) <= 500);
    assert!(s.workspace_notice.is_none());
    assert!(!run(&mut w, &mut s, "show_goals", json!({})).contains("_shared"));
    s.workspace_notice = Some(FALLBACK_NOTICE.into());
    let read = run(&mut w, &mut s, "read_context", json!({"id":mid}));
    assert!(read.starts_with(FALLBACK_NOTICE) && read.ends_with(&"実験記録。".repeat(300)));
}
