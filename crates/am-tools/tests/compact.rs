//! 3者の意見箱に挙がった一連の操作を、隔離storeで検証する。
use am_domain::{ApplyCtx, Command};
use am_store::{query, Store};
use am_tools::{
    ctx::{Session, Workspace},
    render::{estimate_tokens, Utf16Text},
    tools::call,
};
use serde_json::{json, Value};

fn fixture() -> (tempfile::TempDir, Workspace, Session) {
    let dir = tempfile::tempdir().unwrap();
    let store = Store::create_with_model(&dir.path().join("v4.db"), "t", Some("m")).unwrap();
    let w = Workspace::new("t", Some(dir.path().into()), store, None, Some("test".into()), "t").unwrap();
    (dir, w, Session::new("a", "codex", "t"))
}
fn run(w: &mut Workspace, s: &mut Session, name: &str, args: Value) -> String {
    let r = call(w, s, name, &args);
    assert!(!r.is_error, "{name}: {}", r.text);
    r.text
}
fn save(w: &mut Workspace, s: &mut Session, content: &str) -> String {
    run(w, s, "write_context", json!({"content":content})).split('`').nth(1).unwrap().into()
}
fn focus(w: &Workspace, mid: &str) -> Option<String> {
    use rusqlite::OptionalExtension;
    w.conn().query_row("SELECT goal_id FROM goal_links WHERE memory_id=?1", [mid], |r| r.get(0)).optional().unwrap()
}

#[test]
fn inspect_other_work_then_save_and_continue_own_work() {
    let (_d, mut w, mut s) = fixture();
    run(&mut w, &mut s, "post_goal", json!({"title":"別作業B"}));
    run(&mut w, &mut s, "close_goal", json!({"id":"g1"}));
    run(&mut w, &mut s, "post_goal", json!({"title":"現在作業A"}));
    run(&mut w, &mut s, "show_goals", json!({"id":"g1","trace":true}));
    run(&mut w, &mut s, "show_goals", json!({}));
    let id = save(&mut w, &mut s, "現在作業Aの結果");
    assert_eq!(focus(&w, &id).as_deref(), Some("g2"));
    run(&mut w, &mut s, "post_goal", json!({"title":"Aの次の局所作業"}));
    assert_eq!(query::goals(w.conn()).unwrap()[2].parent.as_deref(), Some("g2"));
    let mut other = Session::new("b", "antigravity", "t");
    run(&mut w, &mut other, "close_goal", json!({"id":"g3"}));
    let id = save(&mut w, &mut s, "対象が他接続で完了した後の独立メモ");
    assert_eq!(focus(&w, &id), None);
    // ストアの境界でも完了札への暗黙リンクを防ぐ。
    s.focus = Some("g1".into());
    let id = save(&mut w, &mut s, "古い焦点を持った接続");
    assert_eq!(focus(&w, &id), None);
}

#[test]
fn search_spends_budget_on_content_even_with_many_goals() {
    let (_d, mut w, mut s) = fixture();
    for i in 0..51 {
        run(&mut w, &mut s, "post_goal", json!({"title":format!("作業{i}"),"parent":"none","note":"長い根拠".repeat(100)}));
    }
    let id = save(&mut w, &mut s, "検索監査。検索予算は本文から割り当てると決めた。ただし実Usageは未確認。");
    for budget in [50, 100, 300, 600, 1000] {
        let text = run(&mut w, &mut s, "search_context", json!({"query":"検索監査","search_mode":"bm25","top_k":1,"token_budget":budget}));
        assert!(estimate_tokens(&text) <= budget, "{budget}: {text}");
        if budget >= 300 {
            assert!(text.contains("本文から割り当てる") && text.contains(&id), "{text}");
        }
    }
}

#[test]
fn all_goal_detail_has_a_budget_and_lossless_continuation() {
    let (_d, mut w, mut s) = fixture();
    let note = "根拠🧠の記録。".repeat(200);
    run(&mut w, &mut s, "post_goal", json!({"title":"親","note":note}));
    for i in 0..30 {
        save(&mut w, &mut s, &format!("報告{i}。結果。"));
    }
    let full = run(&mut w, &mut s, "show_goals", json!({"id":"g1","trace":true,"verbose":true,"token_budget":100000}));
    for budget in [100, 600, 1200] {
        let mut offset = 0;
        let mut joined = String::new();
        for _ in 0..1000 {
            let page = run(&mut w, &mut s, "show_goals", json!({"id":"g1","trace":true,"verbose":true,"token_budget":budget,"offset":offset}));
            assert!(estimate_tokens(&page) <= budget, "{budget}: {page}");
            assert!(!page.contains('�'));
            if let Some((body, tail)) = page.rsplit_once("\n続き: ") {
                joined.push_str(body);
                let next = tail.split("offset=").nth(1).unwrap().split(',').next().unwrap().parse::<usize>().unwrap();
                assert!(next > offset);
                offset = next;
            } else {
                joined.push_str(&page);
                break;
            }
        }
        assert_eq!(joined, full);
    }
    let brief = run(&mut w, &mut s, "show_goals", json!({"id":"g1"}));
    assert!(estimate_tokens(&brief) < 250 && !brief.contains(&note));
}

#[test]
fn inventory_pages_do_not_hide_rows_or_raw() {
    let (_d, mut w, mut s) = fixture();
    for i in 0..35 {
        save(&mut w, &mut s, &format!("棚卸し{i}🧠\n{}", "本文の説明".repeat(40)));
    }
    let mut offset = 0;
    let mut seen = std::collections::HashSet::new();
    for _ in 0..40 {
        let text = run(&mut w, &mut s, "list_context", json!({"offset":offset,"token_budget":600}));
        assert!(estimate_tokens(&text) <= 600, "{text}");
        assert!(text.contains("35件中"));
        for line in text.lines().filter(|l| l.starts_with("- `")) {
            assert!(seen.insert(line.split('`').nth(1).unwrap().to_string()));
        }
        if text.contains("末尾") {
            break;
        }
        let next = text.split("offset=").nth(1).unwrap().split(',').next().unwrap().parse::<i64>().unwrap();
        assert!(next > offset);
        offset = next;
    }
    assert_eq!(seen.len(), 35);
    let min = run(&mut w, &mut s, "list_context", json!({"token_budget":100}));
    assert!(estimate_tokens(&min) <= 100 && !min.contains("offset=0,"), "{min}");
}

#[test]
fn partial_read_is_exact_and_verbose_does_not_change_raw() {
    let (d, mut w, mut s) = fixture();
    std::fs::write(d.path().join("test.txt"), "現物🧠本文").unwrap();
    let body = format!("{}結論。{}", "長い本文🧠".repeat(50), "末尾🦉".repeat(50));
    let saved = run(&mut w, &mut s, "write_context", json!({"content":body,"anchors":["test.txt:L1"]}));
    let id = saved.split('`').nth(1).unwrap();
    let compact = run(&mut w, &mut s, "read_context", json!({"id":id}));
    let verbose = run(&mut w, &mut s, "read_context", json!({"id":id,"verbose":true}));
    assert!(compact.ends_with(&body) && verbose.ends_with(&body));
    assert!(!compact.contains("今の該当箇所") && !compact.contains("読取り:"));
    assert!(verbose.contains("**記録時:**"));
    let raw = Utf16Text::new(&body);
    let range = run(&mut w, &mut s, "read_context", json!({"id":id,"start":33,"length":130}));
    let from = raw.boundary(33);
    let to = raw.boundary(from + 130);
    assert!(range.contains(&raw.slice(from, to)) && !range.contains('�'));
    for budget in [100, 200, 600] {
        let part = run(&mut w, &mut s, "read_context", json!({"id":id,"token_budget":budget}));
        assert!(estimate_tokens(&part) <= budget, "{budget}: {part}");
        assert!((part.contains("続き:") || part.ends_with(&body)) && !part.contains('�'), "{part}");
        let limited = run(&mut w, &mut s, "read_context", json!({"id":id,"length":200,"token_budget":budget}));
        assert!(estimate_tokens(&limited) <= budget, "{budget}: {limited}");
    }
    assert_eq!(query::view(w.conn(), id).unwrap().unwrap().content, body);
}

#[test]
fn unknown_defaults_are_current_and_resolution_is_visible() {
    let (_d, mut w, mut s) = fixture();
    let made = run(&mut w, &mut s, "write_context", json!({"kind":"unknown","content":"原理は未確認"}));
    let id = made.split('`').nth(1).unwrap();
    let hyp = run(&mut w, &mut s, "write_context", json!({"kind":"hypothesis","parent_id":id,"content":"原理は値の比較かもしれない"}));
    let h = hyp.split('`').nth(1).unwrap();
    let found = run(&mut w, &mut s, "search_context", json!({"query":"原理","search_mode":"bm25"}));
    assert!(found.contains("未解決") && found.contains("仮説"), "{found}");
    run(&mut w, &mut s, "observe_context", json!({"kind":"observation","parent_id":id,"resolves_unknown":true,"content":"原理を現物で確認した"}));
    let listing = run(&mut w, &mut s, "list_unknowns", json!({}));
    assert!(!listing.contains(id));
    let found = run(&mut w, &mut s, "search_context", json!({"query":"原理","search_mode":"bm25"}));
    assert!(found.contains("解決報告:"));
    assert!(query::view(w.conn(), h).unwrap().unwrap().current); // 仮説を推測で撤回しない。
    run(&mut w, &mut s, "retract_context", json!({"id":id,"reason":"撤回"}));
    let all = run(&mut w, &mut s, "list_unknowns", json!({"include_resolved":true}));
    assert!(!all.contains(id));
    let all = run(&mut w, &mut s, "list_unknowns", json!({"include_resolved":true,"include_history":true}));
    assert!(all.contains(id) && all.contains("retracted"));
}

#[test]
fn automatic_reuse_requires_observable_generation_and_only_returned_bodies() {
    let (_d, mut w, mut s) = fixture();
    save(&mut w, &mut s, "検索監査の本文。境界を確認した。");
    let q = json!({"query":"検索監査","search_mode":"bm25"});
    run(&mut w, &mut s, "search_context", q.clone());
    assert!(!run(&mut w, &mut s, "search_context", q.clone()).contains("UNCHANGED"));
    s.conversation = Some(("claude-code".into(), "S".into()));
    let bump = |w: &mut Workspace, bump| {
        let _applied = w
            .apply(
                &ApplyCtx::default(),
                Command::BumpConversation {
                    conversation_id: "claude-code:S".into(),
                    client: "claude-code".into(),
                    session: "S".into(),
                    source: "hook".into(),
                    bump,
                },
            )
            .unwrap();
    };
    bump(&mut w, false);
    assert!(!run(&mut w, &mut s, "search_context", q.clone()).contains("UNCHANGED"));
    assert!(run(&mut w, &mut s, "search_context", q.clone()).contains("UNCHANGED"));
    bump(&mut w, true);
    assert!(!run(&mut w, &mut s, "search_context", q.clone()).contains("UNCHANGED"));
    let fresh = json!({"query":"検索監査","search_mode":"bm25","fresh":true});
    assert!(!run(&mut w, &mut s, "search_context", fresh).contains("UNCHANGED"));
    let mut other = Session::new("b", "claude-code", "t");
    other.conversation = s.conversation.clone();
    assert!(!run(&mut w, &mut other, "search_context", q).contains("UNCHANGED"));
}

#[test]
fn unsupported_controls_are_not_silent_and_limit_alias_works() {
    let (_d, mut w, mut s) = fixture();
    for i in 0..5 {
        save(&mut w, &mut s, &format!("検索監査{i}。確認。"));
    }
    let out = run(&mut w, &mut s, "search_context", json!({"query":"検索監査","search_mode":"bm25","limit":1}));
    assert_eq!(out.matches("**read:**").count(), 1);
    let out = run(&mut w, &mut s, "search_context", json!({"query":"検索監査","search_mode":"bm25","bogus":true}));
    assert!(out.contains("未対応の引数: bogus"));
}

#[test]
fn goal_continuation_detects_intervening_edits() {
    let (_d, mut w, mut s) = fixture();
    run(&mut w, &mut s, "post_goal", json!({"title":"大きな札","note":"原文の根拠".repeat(300)}));
    let first = run(&mut w, &mut s, "show_goals", json!({"id":"g1","verbose":true,"token_budget":200}));
    let tail = first.rsplit_once("\n続き: ").unwrap().1;
    let revision = tail.split("revision=\"").nth(1).unwrap().split('"').next().unwrap();
    let offset: usize = tail.split("offset=").nth(1).unwrap().split(',').next().unwrap().parse().unwrap();
    run(&mut w, &mut s, "post_goal", json!({"id":"g1","note":"変更後の根拠"}));
    let next = run(&mut w, &mut s, "show_goals", json!({"id":"g1","verbose":true,"offset":offset,"revision":revision}));
    assert!(next.contains("更新された") && next.contains("offset=0"));
}

#[test]
fn large_state_listing_is_bounded_but_get_state_is_lossless() {
    let (_d, mut w, mut s) = fixture();
    let raw = "状態🧠".repeat(1000);
    run(&mut w, &mut s, "set_state", json!({"key":"work.current","value":raw}));
    let listed = run(&mut w, &mut s, "list_state", json!({}));
    assert!(estimate_tokens(&listed) <= 1000 && listed.contains("work.current") && !listed.contains(&raw));
    assert_eq!(run(&mut w, &mut s, "get_state", json!({"key":"work.current"})), raw);
}
