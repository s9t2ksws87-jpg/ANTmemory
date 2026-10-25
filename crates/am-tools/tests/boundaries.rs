//! Codexレビューの境界（R1〜R3・R5・R6、全体目標の下ろし方）を、toolを呼んで確かめる。

use am_domain::{ApplyCtx, Command, NewMemory};
use am_store::Store;
use am_tools::ctx::{Session, Workspace};
use am_tools::render::estimate_tokens;
use am_tools::tools::call;
use serde_json::json;

fn ws() -> (tempfile::TempDir, Workspace) {
    let d = tempfile::tempdir().unwrap();
    let mut store = Store::create_with_model(&d.path().join("v4.db"), "t", Some("m")).unwrap();
    let cx = ApplyCtx { actor: "seed".into(), build: "t".into(), ..Default::default() };
    let rows = [
        ("aaaaaaaaaa", "接地振動の閾値", "こんにゃくの接地では振動の閾値を0.3にした。".repeat(8)),
        ("bbbbbbbbbb", "接地判定の方式", "接地は足の速度と高さで判定する。振動とは別に扱う。".repeat(8)),
        ("cccccccccc", "大会の改善点", "第一回大会では受付の手順を改善した。".repeat(8)),
    ];
    for (id, topic, content) in rows {
        let mem = NewMemory {
            id: id.into(),
            topic: topic.into(),
            content,
            tags: vec![],
            importance: "medium".into(),
            expires_at: None,
            source_ref: None,
            anchors: vec![],
            env: None,
        };
        let _a = store.apply(&cx, Command::Write { mem, reason: "seed".into(), lineage: None, goal: None, uses: vec![] }).unwrap();
    }
    (d, Workspace::new("t", None, store, None, Some("test".into()), "test").unwrap())
}

fn conv_session(session: &str) -> Session {
    let mut s = Session::new("c1", "claude-code", "t");
    s.conversation = Some(("claude-code".into(), session.into()));
    s
}

fn text(ws: &mut Workspace, s: &mut Session, tool: &str, args: serde_json::Value) -> String {
    let r = call(ws, s, tool, &args);
    assert!(!r.is_error, "{tool} {args}: {}", r.text);
    r.text
}

fn uses(ws: &Workspace, kind: &str) -> i64 {
    ws.conn().query_row("SELECT count(*) FROM uses WHERE kind=?1", [kind], |r| r.get(0)).unwrap()
}

fn context_revision(s: &str) -> String {
    s.split("context_revision=").nth(1).expect("complete context stamp").split_whitespace().next().unwrap().to_string()
}

#[test]
fn shared_context_cache_tracks_states_and_reference_lifecycle() {
    let (_d, mut w) = ws();
    let mut s = Session::new("a", "codex", "test");
    text(&mut w, &mut s, "set_state", json!({"key":"goal.current","value":{"final_goal":"ゲーム完成","purpose_memory_id":"aaaaaaaaaa"}}));
    text(&mut w, &mut s, "set_state", json!({"key":"work.current","value":{"step":1,"blocking_unknown_ids":["bbbbbbbbbb"]}}));
    let q = json!({"query":"", "context_key":"work.current", "known_revisions":{}});
    let full = text(&mut w, &mut s, "search_context", q.clone());
    assert!(full.contains("blocking_unknown_ids") && full.contains("接地は足の速度"));
    let rev = context_revision(&full);
    let mut retained = q.clone();
    retained["known_revisions"] = json!({"@context":rev});
    let cached = text(&mut w, &mut s, "search_context", retained.clone());
    assert!(cached.contains("CONTEXT_UNCHANGED") && !cached.contains("ゲーム完成"), "{cached}");
    text(&mut w, &mut s, "retract_context", json!({"id":"bbbbbbbbbb","reason":"再検討"}));
    let changed = text(&mut w, &mut s, "search_context", retained.clone());
    assert!(changed.contains("REFERENCE_STALE") && changed.contains("retracted") && context_revision(&changed) != rev);
    text(&mut w, &mut s, "set_state", json!({"key":"work.current","value":{"step":2}}));
    let changed2 = text(&mut w, &mut s, "search_context", retained);
    assert_ne!(context_revision(&changed), context_revision(&changed2));
    // 長い参照Rawは省略を明示し、保持したことにするstampを出さない。
    text(&mut w, &mut s, "set_state", json!({"key":"work.current","value":{"large":"巨大な本文".repeat(1000)}}));
    let compact = text(&mut w, &mut s, "search_context", q);
    assert!(compact.contains("CONTEXT_INCOMPLETE") && !compact.contains("context_revision="));
}

#[test]
fn c0_read_reference_survives_reconnection_without_mixing_searches() {
    let (_d, mut w) = ws();
    let mut a = Session::new("a", "antigravity", "t");
    let mut other = Session::new("other", "antigravity", "t");
    let found = text(&mut w, &mut a, "search_context", json!({"query":"接地 alphaqz7091","search_mode":"bm25","min_score":0}));
    let reference = found.split("**read:** `").nth(1).unwrap().split('`').next().unwrap().to_string();
    let (mid, sid) = reference.split_once('@').unwrap();
    text(&mut w, &mut other, "search_context", json!({"query":"接地 betaqz8163","search_mode":"bm25","min_score":0}));
    let mut b = Session::new("b", "antigravity", "t");
    // 単なるIDは別の会話の最新検索へ勝手に結ばない。
    text(&mut w, &mut b, "read_context", json!({"id":mid}));
    assert_eq!(uses(&w, "recalled"), 0);
    let raw = text(&mut w, &mut b, "read_context", json!({"id":reference}));
    assert!(raw.contains(mid));
    let hit = text(&mut w, &mut b, "search_context", json!({"query":"alphaqz7091","search_mode":"bm25"}));
    assert!(hit.contains(mid), "{hit}");
    let terms: Vec<String> =
        w.conn().prepare("SELECT term FROM learned_evidence").unwrap().query_map([], |r| r.get(0)).unwrap().collect::<Result<_, _>>().unwrap();
    assert!(terms.contains(&"alphaqz7091".into()) && !terms.contains(&"betaqz8163".into()));
    let mut c = Session::new("c", "antigravity", "t");
    text(&mut w, &mut c, "read_context", json!({"id":reference}));
    let scopes: i64 = w.conn().query_row("SELECT count(DISTINCT scope_key) FROM learned_evidence", [], |r| r.get(0)).unwrap();
    assert_eq!(scopes, 1, "再接続のたびに一会話分を加算しない");
    // 別の結果IDや存在しない検索への付替えは学習されない。
    let n = uses(&w, "recalled");
    text(&mut w, &mut c, "read_context", json!({"id":format!("cccccccccc@{sid}")}));
    text(&mut w, &mut c, "read_context", json!({"id":format!("{mid}@sINVALID")}));
    assert_eq!(uses(&w, "recalled"), n);
    let useful_count = uses(&w, "cited");
    assert_eq!(useful_count, 0);
}

#[test]
fn failed_search_or_use_record_does_not_commit_unseen_goals() {
    let (d, mut w) = ws();
    let mut a = Session::new("a", "codex", "t");
    text(&mut w, &mut a, "post_goal", json!({"title":"未提示の札"}));
    for fail_uses in [false, true] {
        let mut b = Session::new("b", "codex", "t");
        if fail_uses {
            w.conn().execute_batch("CREATE TRIGGER fail_use BEFORE INSERT ON uses BEGIN SELECT RAISE(ABORT,'test-use-failure'); END;").unwrap();
        } else {
            std::fs::write(d.path().join(am_store::V4_FROZEN), "test").unwrap();
        }
        let failed = call(&mut w, &mut b, "search_context", &json!({"query":"接地", "not_relevant":["aaaaaaaaaa"]}));
        assert!(failed.is_error, "{}", failed.text);
        assert!(b.held.is_none(), "エラーで未提示の版を保持してはいけない");
        if fail_uses {
            w.conn().execute_batch("DROP TRIGGER fail_use").unwrap();
        } else {
            std::fs::remove_file(d.path().join(am_store::V4_FROZEN)).unwrap();
        }
        let recovered = text(&mut w, &mut b, "search_context", json!({"query":"接地"}));
        assert!(recovered.contains("未提示の札"), "{recovered}");
    }
}

#[test]
fn r1_whole_response_fits_budget() {
    let (_d, mut w) = ws();
    let mut s = Session::new("c1", "codex", "t");
    // 別枠に出る「以前この問いで開かれた記憶」を作る：検索から開いた記憶が、次の検索の上位に入らない。
    let _ = text(&mut w, &mut s, "search_context", json!({ "query": "接地 振動" }));
    let _ = text(&mut w, &mut s, "read_context", json!({ "id": "bbbbbbbbbb" }));
    let q = json!({ "query": "接地 振動 閾値", "top_k": 1 });
    let free = text(&mut w, &mut s, "search_context", q.clone());
    assert!(free.contains("以前この問いで開かれた記憶") && free.contains("bbbbbbbbbb"), "別枠が出る前提: {free}");
    for budget in [60, 120, 200, 300, 450, 700, 1000] {
        let mut a = q.clone();
        a["token_budget"] = json!(budget);
        let t = text(&mut w, &mut s, "search_context", a);
        assert!(estimate_tokens(&t) <= budget, "budget={budget} tokens={} text={t}", estimate_tokens(&t));
    }
}

#[test]
fn r2_tree_not_marked_seen_when_dropped() {
    let (_d, mut w) = ws();
    let mut s = Session::new("c1", "codex", "t");
    let _ = text(&mut w, &mut s, "post_goal", json!({ "title": "物理演算", "note": "テスト" }));
    let _ = text(&mut w, &mut s, "post_goal", json!({ "title": "接地振動", "parent": "g1" }));
    let _ = text(&mut w, &mut s, "post_goal", json!({ "title": "兄弟の枝", "parent": "g1" }));
    // 新しい接続：予算が小さく札の節が落ちる → 次の検索で全体が出る（「変化なし」にならない）。
    let mut s2 = Session::new("c2", "codex", "t");
    let tiny = text(&mut w, &mut s2, "search_context", json!({ "query": "接地", "token_budget": 50 }));
    assert!(tiny.starts_with("BUDGET_TOO_SMALL"), "{tiny}");
    let next = text(&mut w, &mut s2, "search_context", json!({ "query": "接地" }));
    assert!(next.contains("g1 物理演算") && !next.contains("変化なし"), "{next}");
    // 子だけを見せた後は、兄弟の枝を表示済みにしない。
    let mut s3 = Session::new("c3", "codex", "t");
    let _ = text(&mut w, &mut s3, "show_goals", json!({ "goal": "g2" }));
    let after = text(&mut w, &mut s3, "search_context", json!({ "query": "接地" }));
    assert!(after.contains("g3 兄弟の枝"), "{after}");
}

#[test]
fn r3_posted_children_stay_visible_under_closed_parent() {
    let (_d, mut w) = ws();
    let mut s = Session::new("c1", "codex", "t");
    let _ = text(&mut w, &mut s, "post_goal", json!({ "title": "親", "note": "テスト" }));
    let _ = text(&mut w, &mut s, "post_goal", json!({ "title": "未完了の子", "parent": "g1" }));
    let closed = text(&mut w, &mut s, "close_goal", json!({ "id": "g1", "state": "dropped" }));
    assert!(closed.contains("配下に掲示中の札が残っている") && closed.contains("g2"), "{closed}");
    let mut s2 = Session::new("c2", "codex", "t");
    let all = text(&mut w, &mut s2, "show_goals", json!({}));
    assert!(all.contains("g2 未完了の子") && all.contains("g1 親") && all.contains("✗不要"), "{all}");
}

#[test]
fn r5_unrelated_next_search_is_not_reformulation() {
    let (_d, mut w) = ws();
    let mut s = conv_session("S1"); // C1
    let _ = text(&mut w, &mut s, "search_context", json!({ "query": "接地 振動 閾値" }));
    let _ = text(&mut w, &mut s, "read_context", json!({ "id": "aaaaaaaaaa" }));
    let _ = text(&mut w, &mut s, "search_context", json!({ "query": "大会 改善" }));
    assert_eq!(uses(&w, "reformulated"), 0, "別件の検索で前の記憶を否定しない");
    let _ = text(&mut w, &mut s, "search_context", json!({ "query": "接地 振動 揺れ" }));
    assert_eq!(uses(&w, "reformulated"), 1, "言い換えた検索なら弱い否定");
}

#[test]
fn r6_new_generation_does_not_link_old_searches() {
    let (_d, mut w) = ws();
    let mut s = conv_session("S2");
    let _ = text(&mut w, &mut s, "search_context", json!({ "query": "接地 振動" }));
    let r = text(&mut w, &mut s, "read_context", json!({ "id": "aaaaaaaaaa", "verbose": true }));
    assert!(r.contains("（inferred）"), "{r}");
    // 同じ会話で圧縮（hook）→ 世代が進む。
    let ev = am_tools::hooks::HookEvent {
        client: "claude-code".into(),
        event: "SessionStart".into(),
        session_id: "S2".into(),
        cwd: None,
        source: Some("compact".into()),
        tool_name: None,
        tool_input: serde_json::Value::Null,
    };
    let _ = am_tools::hooks::apply(&mut w, &ev, "t").unwrap();
    let r = text(&mut w, &mut s, "read_context", json!({ "id": "bbbbbbbbbb", "verbose": true }));
    assert!(r.contains("検索を経ない直接の読取り"), "旧世代の検索に結ばない: {r}");
    let wrote = text(&mut w, &mut s, "write_context", json!({ "topic": "x", "content": "y", "used": ["aaaaaaaaaa"] }));
    assert!(wrote.contains("aaaaaaaaaa（この会話で読んでいないため数えない）"), "旧世代の読取りはcitedにしない: {wrote}");
}

#[test]
fn overall_goal_cannot_be_closed_with_its_posting_quote() {
    let home = tempfile::tempdir().unwrap();
    let proj = home.path().join(".claude/projects/ws");
    std::fs::create_dir_all(&proj).unwrap();
    let line = |t: &str| json!({ "type": "user", "sessionId": "S3", "cwd": "ws", "message": { "role": "user", "content": t } }).to_string();
    std::fs::write(proj.join("S3.jsonl"), [line("徒競走のゲームを作りたい"), line("徒競走のゲームはもう終わりにしよう")].join("\n")).unwrap();
    // このtestだけがホームを差し替える（他のtestは会話ログを読まない）。
    std::env::set_var("USERPROFILE", home.path());
    std::env::set_var("HOME", home.path());
    let (_d, mut w) = ws();
    let mut s = conv_session("S3");
    s.cwd = Some("ws".into());
    let p = text(&mut w, &mut s, "post_goal", json!({ "kind": "overall", "title": "徒競走ゲーム", "quote": "徒競走のゲームを作りたい" }));
    assert!(p.contains("札を掲げた: g1") && p.contains("発言の存在と出所まで"), "{p}");
    // 掲げたときと同じ発言や、引用なしでは下ろさない。ただしエラーで止めず「下ろす候補」にする。
    let r = text(&mut w, &mut s, "close_goal", json!({ "id": "g1", "quote": "徒競走のゲームを作りたい" }));
    assert!(r.contains("下ろす候補") && r.contains("掲げたときの発言と同じ"), "{r}");
    let r = text(&mut w, &mut s, "close_goal", json!({ "id": "g1" }));
    assert!(r.contains("下ろす候補"), "{r}");
    let board = text(&mut w, &mut s, "show_goals", json!({}));
    assert!(board.contains("g1 徒競走ゲーム") && board.contains("下ろす候補（未確認）"), "{board}");
    let ok = text(&mut w, &mut s, "close_goal", json!({ "id": "g1", "quote": "もう終わりにしよう" }));
    assert!(ok.contains("札を下ろした: g1"), "{ok}");
}

#[test]
fn easy_write_and_recall_by_search_words() {
    let (_d, mut w) = ws();
    let mut s = Session::new("c1", "codex", "t");
    // content だけで書ける。返事は1行（促しや小言を出さない）。
    let r = text(
        &mut w,
        &mut s,
        "write_context",
        json!({ "content": "## 段差の当たり判定
足が段差に引っかかる。接地の判定を緩めると直った。" }),
    );
    assert!(
        r.starts_with("Chunk saved:")
            && r.contains("topic=\"段差の当たり判定\"")
            && !r.contains(
                "
"
            ),
        "{r}"
    );
    let id = r.split('`').nth(1).unwrap().to_string();
    let read = text(&mut w, &mut s, "read_context", json!({ "id": id, "verbose": true }));
    assert!(read.contains("**記録時:**"), "記録時の環境が自動で付く: {read}");
    // 「こんにゃく」は本文に無い。接地で当たったものを選んで読むと、その語でも思い出せる。
    let first = text(&mut w, &mut s, "search_context", json!({ "query": "こんにゃく 段差 接地" }));
    assert!(first.contains(&id), "{first}");
    let _ = text(&mut w, &mut s, "read_context", json!({ "id": id }));
    let again = text(&mut w, &mut s, "search_context", json!({ "query": "こんにゃく" }));
    assert!(again.contains(&id) && again.contains("前にこの語"), "検索由来タグで思い出す: {again}");
    let read = text(&mut w, &mut s, "read_context", json!({ "id": id, "verbose": true }));
    assert!(read.contains("**思い出す語:**") && read.contains("#こんにゃく"), "{read}");
    // 何度読んでも、同じ会話では強さが増え続けない。
    for _ in 0..5 {
        let _ = text(&mut w, &mut s, "search_context", json!({ "query": "こんにゃく 段差 接地" }));
        let _ = text(&mut w, &mut s, "read_context", json!({ "id": id }));
    }
    let ev = am_store::query::learned_tags_for_memory(w.conn(), &id).unwrap();
    let r = am_tools::useful::recall_strengths(&ev);
    assert!(r[&id].iter().all(|(_, v)| *v <= am_tools::useful::W_RECALLED + 1e-9), "{:?}", r[&id]);
    // 普段の検索は短い：連携段階やsearch_idは出さない。
    assert!(!again.contains("search_id=") && !again.contains("連携:"), "{again}");
    let verbose = text(&mut w, &mut s, "search_context", json!({ "query": "段差", "verbose": true }));
    assert!(verbose.contains("search_id=") && verbose.contains("連携:"), "{verbose}");
}

#[test]
fn goals_are_forgiving() {
    let (_d, mut w) = ws();
    let mut s = Session::new("c1", "codex", "t");
    // 理由を書かなくても、長い名前でも掲げられる。
    let r = text(&mut w, &mut s, "post_goal", json!({ "title": "こんにゃくの物理演算で接地したときの振動を計測して閾値を決める" }));
    assert!(r.contains("札を掲げた: g1") && r.contains("24字に丸めた"), "{r}");
    let g = am_store::query::goals(w.conn()).unwrap();
    assert_eq!(g[0].title.chars().count(), 24);
    // 引用の無い全体目標はエラーにせず候補に留める。
    let r = text(&mut w, &mut s, "post_goal", json!({ "kind": "overall", "title": "全体" }));
    assert!(r.contains("候補（未確認）"), "{r}");
    // v3の慣習の set_state("goal.current") も書ける（正式な全体目標にはしない）。
    let r = text(&mut w, &mut s, "set_state", json!({ "key": "goal.current", "value": { "final_goal": "x" } }));
    assert!(r.contains("written") && r.contains("共有state"), "{r}");
}

#[test]
fn oversized_ttl_is_an_ordinary_error() {
    // Codex実利用 E2：過大なTTLで接続処理をpanicさせない。
    let (_d, mut w) = ws();
    let mut s = Session::new("c1", "codex", "t");
    for d in [1e100, 36_501.0, f64::MAX] {
        let r = call(&mut w, &mut s, "write_context", &json!({ "content": "過大TTL", "ttl_days": d }));
        assert!(r.is_error && r.text.contains("ttl_days"), "{d}: {}", r.text);
    }
    let r = call(&mut w, &mut s, "write_context", &json!({ "content": "百年", "ttl_days": 36_500 }));
    assert!(!r.is_error, "{}", r.text);
}

#[test]
fn budget_reference_carries_no_full_revision() {
    // Codex実利用 E1：本文を省いた参照に完全な表示の版を載せない。広げた検索で UNCHANGED にならない。
    let (_d, mut w) = ws();
    let mut s = Session::new("c1", "codex", "t");
    let small = text(&mut w, &mut s, "search_context", json!({ "query": "接地", "full_content": true, "token_budget": 200, "known_revisions": {} }));
    assert!(small.contains("本文省略") && !small.contains("revision:"), "{small}");
    let wide = text(&mut w, &mut s, "search_context", json!({ "query": "接地", "full_content": true, "token_budget": 20000, "known_revisions": {} }));
    let rev = wide.split("**revision:** `").nth(1).unwrap().split('`').next().unwrap().to_string();
    let id = wide.split("**read:** `").nth(1).unwrap().split('@').next().unwrap().to_string();
    let again = text(&mut w, &mut s, "search_context", json!({ "query": "接地", "full_content": true, "token_budget": 20000, "known_revisions": { id.clone(): rev } }));
    assert!(again.contains(&format!("UNCHANGED id=`{id}`")), "{again}");
}

#[test]
fn past_reads_respect_the_same_filter() {
    // Codex実利用 E4：別枠も可視性・履歴・tagsの条件を通す。
    let (_d, mut w) = ws();
    let mut s = conv_session("past");
    let r = text(&mut w, &mut s, "search_context", json!({ "query": "大会 受付" }));
    let read = r.split("**read:** `").nth(1).unwrap().split('`').next().unwrap().to_string();
    let id = read.split('@').next().unwrap().to_string();
    let _ = text(&mut w, &mut s, "read_context", json!({ "id": read }));
    let _ = text(&mut w, &mut s, "set_visibility", json!({ "id": id, "visibility": "archive", "reason": "試験" }));
    let mut s2 = conv_session("past2");
    let r = text(&mut w, &mut s2, "search_context", json!({ "query": "大会 受付" }));
    assert!(!r.contains(&id), "archiveの記憶が別枠に出た: {r}");
    let r = text(&mut w, &mut s2, "search_context", json!({ "query": "大会 受付", "include_hidden": true }));
    assert!(r.contains(&id), "{r}");
    let r = text(&mut w, &mut s2, "search_context", json!({ "query": "大会 受付", "include_hidden": true, "tags": ["別の棚"] }));
    assert!(!r.contains(&id), "違うtagsの記憶が別枠に出た: {r}");
}

#[test]
fn empty_goal_board_adds_no_heading() {
    let (_d, mut w) = ws();
    let mut s = Session::new("c1", "codex", "t");
    let _ = text(&mut w, &mut s, "post_goal", json!({ "title": "済む作業", "note": "試験" }));
    let _ = text(&mut w, &mut s, "close_goal", json!({ "id": "g1" }));
    let mut s2 = Session::new("c2", "codex", "t");
    let r = text(&mut w, &mut s2, "search_context", json!({ "query": "接地" }));
    assert!(!r.contains("## 札"), "{r}");
}

#[test]
fn not_relevant_read_does_not_teach_the_query() {
    // Codex実利用の再確認：無関係と言って読んだ記憶を、その語の「思い出す語」にしない。
    let (_d, mut w) = ws();
    let mut s = conv_session("nr");
    let r = text(&mut w, &mut s, "search_context", json!({ "query": "大会 受付" }));
    let read = r.split("**read:** `").nth(1).unwrap().split('`').next().unwrap().to_string();
    let _ = text(&mut w, &mut s, "read_context", json!({ "id": read, "verdict": "not_relevant" }));
    assert_eq!(uses(&w, "recalled"), 0);
    assert_eq!(uses(&w, "rejected"), 1);
}

#[test]
fn shared_context_is_shown_once_per_connection_while_unchanged() {
    let (_d, mut w) = ws();
    let mut s = Session::new("c1", "codex", "t");
    let _ = text(&mut w, &mut s, "set_state", json!({"key":"goal.current","value":{"final_goal":"ゲーム完成","purpose_memory_id":"aaaaaaaaaa"}}));
    let first = text(&mut w, &mut s, "search_context", json!({ "query": "接地", "context_key":"goal.current", "token_budget":2000 }));
    assert!(first.contains("ゲーム完成"), "{first}");
    let second = text(&mut w, &mut s, "search_context", json!({ "query": "大会", "context_key":"goal.current", "token_budget":2000 }));
    assert!(second.contains("前回の表示から変化なし") && !second.contains("ゲーム完成"), "{second}");
    // 変われば、また全体。
    let _ = text(&mut w, &mut s, "set_state", json!({"key":"goal.current","value":{"final_goal":"ゲーム公開","purpose_memory_id":"aaaaaaaaaa"}}));
    let third = text(&mut w, &mut s, "search_context", json!({ "query": "大会", "context_key":"goal.current", "token_budget":2000 }));
    assert!(third.contains("ゲーム公開"), "{third}");
    // 別の接続は初めから全体を見る。
    let mut s2 = Session::new("c2", "codex", "t");
    let other = text(&mut w, &mut s2, "search_context", json!({ "query": "大会", "context_key":"goal.current", "token_budget":2000 }));
    assert!(other.contains("ゲーム公開"), "{other}");
}

#[test]
fn goal_id_alias_enters_shows_children_and_can_be_edited_without_search() {
    let (_d, mut w) = ws();
    let mut s = Session::new("c1", "codex", "t");
    text(&mut w, &mut s, "post_goal", json!({"title":"親の作業","parent":"none"}));
    text(&mut w, &mut s, "post_goal", json!({"title":"子の作業","parent":"g1"}));
    let by_id = text(&mut w, &mut s, "show_goals", json!({"id":"g1", "enter":true}));
    assert!(by_id.contains("子の作業") && by_id.contains("親の作業"), "{by_id}");
    assert_eq!(s.focus.as_deref(), Some("g1"));
    let before: i64 = w.conn().query_row("SELECT count(*) FROM goals", [], |r| r.get(0)).unwrap();
    text(&mut w, &mut s, "post_goal", json!({"id":"g1","title":"親の新しい名前","note":"方針を絞った"}));
    let edited = text(&mut w, &mut s, "show_goals", json!({"goal":"g1","id":"g1"}));
    assert!(edited.contains("親の新しい名前") && edited.contains("方針を絞った"), "{edited}");
    assert_eq!(w.conn().query_row("SELECT count(*) FROM goals", [], |r| r.get::<_, i64>(0)).unwrap(), before);
    let conflict = call(&mut w, &mut s, "show_goals", &json!({"id":"g2","goal":"g1"}));
    assert!(conflict.is_error && conflict.text.contains("同じ札"));
    assert_eq!(s.focus.as_deref(), Some("g1"));
    let missing = call(&mut w, &mut s, "show_goals", &json!({"id":"g999"}));
    assert!(missing.is_error);
    assert_eq!(s.focus.as_deref(), Some("g1"));
    text(&mut w, &mut s, "show_goals", json!({"id":"none"}));
    assert!(s.focus.is_none());
    let tools = am_tools::tools::list();
    assert_eq!(tools["tools"].as_array().unwrap().len(), 19);
    let show = tools["tools"].as_array().unwrap().iter().find(|t| t["name"] == "show_goals").unwrap();
    assert_eq!(show["inputSchema"]["properties"]["id"]["type"], "string");
}

#[test]
fn inventory_preview_is_short_unicode_safe_and_does_not_replace_raw() {
    let (_d, mut w) = ws();
    let mut s = Session::new("c1", "codex", "t");
    let body = format!("一行目🧠\n{} RAW_TAIL", "説明🦉".repeat(40));
    let saved = text(&mut w, &mut s, "write_context", json!({"topic":"棚卸し","content":body,"tags":["preview"]}));
    let id = saved.split("id=`").nth(1).unwrap().split('`').next().unwrap();
    let listing = text(&mut w, &mut s, "list_context", json!({"tags":["preview"]}));
    let preview = listing.split("抜粋: ").nth(1).unwrap().lines().next().unwrap();
    assert_eq!(preview.chars().count(), 81);
    assert!(preview.starts_with("一行目🧠 ") && preview.ends_with('…') && !preview.contains("RAW_TAIL"), "{listing}");
    let raw = text(&mut w, &mut s, "read_context", json!({"id":id}));
    assert!(raw.contains(&body));
}

#[test]
fn missing_write_targets_are_rejected_before_preparing_anchors() {
    let (_d, mut w) = ws();
    let mut s = Session::new("c1", "codex", "t");
    let before = query_count(&w, "events");
    let r = call(&mut w, &mut s, "supersede_context", &json!({
        "old_id":"missing", "content":"新しい本文", "reason":"訂正", "anchors":[3]
    }));
    assert!(r.is_error && r.text.contains("置換対象が見つかりません: missing"), "{}", r.text);
    let r = call(&mut w, &mut s, "observe_context", &json!({"kind":"observation","content":"再観測","resolves_unknown":true}));
    assert!(r.is_error && r.text.contains("parent_id を指定"), "{}", r.text);
    assert_eq!(query_count(&w, "events"), before);
}

fn query_count(w: &Workspace, table: &str) -> i64 {
    w.conn().query_row(&format!("SELECT count(*) FROM {table}"), [], |r| r.get(0)).unwrap()
}

#[test]
fn old_split_anchor_is_reobserved_without_rewriting_history_or_inventing_a_fingerprint() {
    let (d, mut w) = ws();
    w.root = Some(d.path().to_path_buf());
    std::fs::create_dir_all(d.path().join("doc")).unwrap();
    std::fs::write(d.path().join("doc/既存 記録.md"), "過去の指紋は無い。現在ここにある。").unwrap();
    let raw = "doc/既存 記録.md";
    let anchor = am_domain::Anchor::File { path: "doc/既存".into(), external: false, locator: Some("記録.md".into()), commit: None };
    let new_anchor = am_domain::NewAnchor { raw: raw.into(), segment: raw.into(), anchor: anchor.clone(), capture: None, origin: "agent".into() };
    let mem = NewMemory {
        id: "dddddddddd".into(), topic: "既存の取手".into(), content: "既存の取手のRaw".into(), tags: vec!["old-anchor".into()],
        importance: "medium".into(), expires_at: None, source_ref: None, anchors: vec![new_anchor], env: None,
    };
    let _a = w.apply(&ApplyCtx::default(), Command::Write { mem, reason: "旧版を再現".into(), lineage: None, goal: None, uses: vec![] }).unwrap();
    let mut s = Session::new("c1", "codex", "t");
    let shown = text(&mut w, &mut s, "read_context", json!({"id":"dddddddddd"}));
    assert!(shown.contains("[現物あり]") && !shown.contains("[同一]"), "{shown}");
    let found = text(&mut w, &mut s, "search_context", json!({"query":"既存","tags":["old-anchor"],"search_mode":"bm25"}));
    assert!(found.contains("**現物:** doc/既存 記録.md") && !found.contains("今は無い"), "{found}");
    let stored = am_store::query::anchors_of(w.conn(), "dddddddddd").unwrap();
    assert_eq!(stored[0].anchor, anchor);
    assert_eq!(stored[0].segment, raw);
    assert!(stored[0].capture.is_none());
}
