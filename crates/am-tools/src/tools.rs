//! 19のtool。v3の16 toolは名前・引数・1行目の形を保ち（§11）、札の3つを足す。
//!
//! DBを変えるのは `Workspace::apply`（= `Store::apply`）だけ。ここでは引数を確かめ、
//! 命令を作り、結果を文字にする。

use std::collections::HashSet;
use std::time::Instant;

use am_domain::{Basis, Command, GoalClose, Lineage, NewGoal, NewMemory, Proposal, ReadRecord, SearchRecord, UseRecord};
use am_store::query::{self, Filter, MemoryView};
use am_store::StoreError;
use serde_json::{json, Value};

use crate::anchorview;
use crate::ctx::{new_id, Level, Session, Workspace};
use crate::goals::{self, Tree};
use crate::render::{dots, estimate_tokens, query_excerpts, short, Utf16Text};
use crate::search::{self, SearchArgs};
use crate::useful;
use crate::utter;

pub struct ToolResult {
    pub text: String,
    pub is_error: bool,
}

type R = Result<String, String>;

fn err(e: StoreError) -> String {
    e.to_string()
}

const IMPORTANCE: &[&str] = &["low", "medium", "high", "critical"];
const VISIBILITY: &[&str] = &["hot", "warm", "cold", "archive"];

/// 引数の読取り。型が違えば理由を返す（黙って既定値にしない）。
struct A<'a>(&'a Value);

impl A<'_> {
    fn raw(&self, k: &str) -> Option<&Value> {
        self.0.get(k).filter(|v| !v.is_null())
    }
    fn has(&self, k: &str) -> bool {
        self.0.get(k).is_some()
    }
    fn s(&self, k: &str) -> Result<Option<String>, String> {
        match self.raw(k) {
            None => Ok(None),
            Some(Value::String(s)) => Ok(Some(s.clone())),
            Some(_) => Err(format!("{k} は文字列で渡してください")),
        }
    }
    fn req(&self, k: &str) -> Result<String, String> {
        self.s(k)?.ok_or_else(|| format!("{k} は必須です"))
    }
    fn b(&self, k: &str) -> Result<Option<bool>, String> {
        match self.raw(k) {
            None => Ok(None),
            Some(Value::Bool(b)) => Ok(Some(*b)),
            Some(_) => Err(format!("{k} は true / false で渡してください")),
        }
    }
    fn n(&self, k: &str) -> Result<Option<f64>, String> {
        match self.raw(k) {
            None => Ok(None),
            Some(Value::Number(n)) => Ok(n.as_f64()),
            Some(_) => Err(format!("{k} は数で渡してください")),
        }
    }
    fn int(&self, k: &str, min: i64, max: i64) -> Result<Option<i64>, String> {
        match self.n(k)? {
            None => Ok(None),
            Some(f) if f.fract() == 0.0 && f >= min as f64 && f <= max as f64 => Ok(Some(f as i64)),
            Some(f) => Err(format!("{k} は {min}〜{max} の整数で渡してください（{f}）")),
        }
    }
    fn strs(&self, k: &str) -> Result<Option<Vec<String>>, String> {
        match self.raw(k) {
            None => Ok(None),
            Some(Value::Array(a)) => a
                .iter()
                .map(|v| v.as_str().map(String::from).ok_or_else(|| format!("{k} は文字列の配列で渡してください")))
                .collect::<Result<Vec<_>, _>>()
                .map(Some),
            Some(Value::String(s)) => Ok(Some(vec![s.clone()])),
            Some(_) => Err(format!("{k} は文字列の配列で渡してください")),
        }
    }
    fn one_of(&self, k: &str, allowed: &[&str]) -> Result<Option<String>, String> {
        match self.s(k)? {
            Some(v) if !allowed.contains(&v.as_str()) => Err(format!("{k} は {} のどれか（{v}）", allowed.join(" / "))),
            v => Ok(v),
        }
    }
    fn ttl(&self) -> Result<Option<String>, String> {
        match self.n("ttl_days")? {
            None => Ok(None),
            // 上限は100年。時刻の加算は溢れを確かめる（過大な値で接続処理をpanicさせない）。
            Some(d) if d > 0.0 && d <= TTL_MAX_DAYS => chrono::Duration::try_milliseconds((d * 86_400_000.0) as i64)
                .and_then(|span| chrono::Utc::now().checked_add_signed(span))
                .map(|t| Some(t.to_rfc3339_opts(chrono::SecondsFormat::Millis, true)))
                .ok_or_else(|| format!("ttl_days が大きすぎる（{d}）")),
            Some(d) if d > TTL_MAX_DAYS => Err(format!("ttl_days は {TTL_MAX_DAYS} 日（100年）までで渡してください（{d}）")),
            Some(_) => Err("ttl_days は正の数で渡してください".into()),
        }
    }
}

const TTL_MAX_DAYS: f64 = 36_500.0;

pub fn call(ws: &mut Workspace, s: &mut Session, name: &str, args: &Value) -> ToolResult {
    let Some(notice) = s.workspace_notice.clone() else {
        return call_inner(ws, s, name, args);
    };
    // 不明な接続先は最初に一度だけ知らせる。その説明で検索本文の予算を超えない。
    let default = match name {
        "search_context" => Some(if args["full_content"] == true { 4000 } else { 1000 }),
        "list_context" | "show_goals" => Some(1200),
        "list_unknowns" | "list_state" => Some(1000),
        _ => None,
    };
    let budget = match args.get("token_budget") {
        None | Some(Value::Null) => default,
        Some(v) => v.as_u64().filter(|v| *v <= 1_000_000).and_then(|v| usize::try_from(v).ok()),
    };
    let mut adjusted = if args.is_null() { json!({}) } else { args.clone() };
    if let Some(budget) = budget {
        let reserved = estimate_tokens(&notice) + 2;
        let minimum = if name == "search_context" { 50 } else { 100 };
        if budget < reserved + minimum {
            return ToolResult { text: notice, is_error: true };
        }
        if let Some(obj) = adjusted.as_object_mut() {
            obj.insert("token_budget".into(), json!(budget - reserved));
        }
    }
    let mut r = call_inner(ws, s, name, &adjusted);
    r.text = format!("{notice}\n\n{}", r.text);
    if !r.is_error {
        s.workspace_notice = None;
    }
    r
}

fn call_inner(ws: &mut Workspace, s: &mut Session, name: &str, args: &Value) -> ToolResult {
    // 長く続く接続でも期限切れを見落とさない（v3は毎回 expireDueChunks を呼んでいた）。
    if let Err(e) = expire_due(ws, s) {
        return ToolResult { text: format!("エラー: {e}"), is_error: true };
    }
    s.sync_generation(ws);
    // 他の接続が下ろした札へ、新しい作業を暗黙に結び付けない。
    if s.focus
        .as_ref()
        .is_some_and(|id| ws.conn().query_row("SELECT state FROM goals WHERE id=?1", [id], |r| r.get::<_, String>(0)).ok().as_deref() != Some("posted"))
    {
        s.focus = None;
    }
    let a = A(args);
    let r = match name {
        "search_context" => search_context(ws, s, &a),
        "write_context" if a.has("kind") => observe_context(ws, s, &a),
        "write_context" => write_context(ws, s, &a),
        "read_context" => read_context(ws, s, &a),
        "list_context" => list_context(ws, &a),
        "correct_context" => correct_context(ws, s, &a),
        "supersede_context" => supersede_context(ws, s, &a),
        "retract_context" => simple(ws, s, &a, "retract"),
        "restore_context" => simple(ws, s, &a, "restore"),
        "change_importance" => change_importance(ws, s, &a),
        "set_visibility" => set_visibility(ws, s, &a),
        "get_context_history" => history(ws, &a),
        "get_state" => get_state(ws, &a),
        "set_state" => set_state(ws, s, &a),
        "list_state" => list_state(ws, &a),
        "observe_context" => observe_context(ws, s, &a),
        "list_unknowns" => list_unknowns(ws, &a),
        "post_goal" => post_goal(ws, s, &a),
        "close_goal" => close_goal(ws, s, &a),
        "show_goals" => show_goals(ws, s, &a),
        other => Err(format!("知らないtool: {other}")),
    };
    match r {
        Ok(mut text) => {
            let schema = list();
            if let Some(def) = schema["tools"].as_array().and_then(|ts| ts.iter().find(|t| t["name"] == name)) {
                if let Some(obj) = args.as_object() {
                    let extra: Vec<_> = obj
                        .keys()
                        .filter(|k| {
                            !def["inputSchema"]["properties"].as_object().is_some_and(|ps| ps.contains_key(*k))
                                && !(name == "search_context" && k.as_str() == "limit")
                        })
                        .collect();
                    if !extra.is_empty() {
                        text.push_str(&format!("\n未対応の引数: {}", extra.into_iter().map(|k| short(k, 30)).take(5).collect::<Vec<_>>().join(", ")));
                    }
                }
            }
            ToolResult { text, is_error: false }
        }
        Err(e) if e.starts_with("CAS_CONFLICT") => ToolResult { text: e, is_error: true },
        Err(e) => ToolResult { text: format!("エラー: {e}"), is_error: true },
    }
}

fn expire_due(ws: &mut Workspace, s: &Session) -> Result<(), String> {
    let due: bool = ws
        .conn()
        .query_row("SELECT EXISTS(SELECT 1 FROM memory_state WHERE status='active' AND expires_at IS NOT NULL AND expires_at <= ?1)", [am_store::now()], |r| {
            r.get(0)
        })
        .map_err(|e| e.to_string())?;
    if due {
        let mut cx = s.apply_ctx();
        cx.actor = "system".into();
        ws.apply(&cx, Command::ExpireDue).map_err(err)?.detail.get("expired").map(|_| ()).unwrap_or(());
    }
    Ok(())
}

// ─── 検索 ───────────────────────────────────────────────────────────────────

const PREFIX: &str = "保存記録の抜粋（全文: read_context）。";

struct Entry {
    id: String,
    heading: String,
    block: String,
    status_line: String,
    revision: Option<String>,
}

fn revision(query: &str, full: bool, limit: usize, v: &MemoryView, score: f64, place: Option<&str>) -> String {
    use sha2::Digest;
    let key = json!({"view": 6, "query": query, "full": full, "limit": limit, "id": v.id, "hash": v.content_hash,
        "status": v.status, "visibility": v.visibility, "current": v.current, "score": score, "place": place,
        "tags": v.tags, "updated": v.updated, "source": v.source_ref});
    hex::encode(sha2::Sha256::digest(key.to_string().as_bytes()))[..16].to_string()
}

fn meta_line(v: &MemoryView) -> String {
    format!(
        "**id:** `{}` | **status:** {} | **visibility:** {} | **current:** {} | **created:** {}",
        v.id,
        v.status,
        v.visibility,
        if v.current { "yes" } else { "no" },
        v.created
    )
}

// 通常の状態は省き、判断を変える状態・来歴は短く残す。
fn flags(ws: &Workspace, v: &MemoryView) -> String {
    let mut out = vec![];
    if v.status != "active" {
        out.push(v.status.clone());
    }
    if !v.current {
        out.push("履歴".into());
    }
    if matches!(v.visibility.as_str(), "cold" | "archive") {
        out.push(v.visibility.clone());
    }
    if v.tags.iter().any(|t| t == "tk:hypothesis") {
        out.push("仮説".into());
    }
    if v.tags.iter().any(|t| t == "tk:unknown") {
        let solved: Option<String> =
            ws.conn().query_row("SELECT src FROM relations WHERE dst=?1 AND kind='resolves' ORDER BY created_at DESC LIMIT 1", [&v.id], |r| r.get(0)).ok();
        out.push(solved.map(|id| format!("解決報告:{id}")).unwrap_or_else(|| "未解決".into()));
    }
    if let Ok(id) =
        ws.conn().query_row("SELECT src FROM relations WHERE dst=?1 AND kind='supersedes' ORDER BY created_at DESC LIMIT 1", [&v.id], |r| r.get::<_, String>(0))
    {
        out.push(format!("後継:{id}"));
    }
    if out.is_empty() {
        String::new()
    } else {
        format!(" 〔{}〕", out.join("・"))
    }
}

fn date(v: &MemoryView) -> &str {
    v.created.get(..10).unwrap_or(&v.created)
}

fn source_line(ws: &Workspace, v: &MemoryView, anchors: &[am_store::query::StoredAnchor]) -> Option<String> {
    let src = v.source_ref.as_ref()?;
    // source_ref 由来の同じ取手を表示している時だけ省く。別出典は残す。
    if anchors.iter().take(2).any(|a| a.raw == *src && matches!(a.anchor, am_domain::Anchor::File { .. })) {
        return None;
    }
    let root = ws.root.as_ref().map(|r| r.to_string_lossy().replace('\\', "/"));
    let norm = src.replace('\\', "/");
    let norm = norm.strip_prefix("file:///").unwrap_or(&norm);
    let shown = root.as_ref().and_then(|r| norm.strip_prefix(&format!("{r}/"))).unwrap_or(norm);
    Some(format!("source: {shown}"))
}

fn learned_line(l: &[(String, f64)]) -> Option<String> {
    if l.is_empty() {
        return None;
    }
    let tags = l.iter().take(4).map(|(t, s)| format!("#{t}{}", dots(*s))).collect::<Vec<_>>().join(" ");
    Some(format!("**学習tags:** {tags}（使われた実績。事実の保証ではない）"))
}

fn set_focus(ws: &Workspace, s: &mut Session, goal: Option<String>) -> Result<bool, String> {
    let Some(g) = goal else { return Ok(false) };
    if g == "none" || g.is_empty() {
        let changed = s.focus.is_some();
        s.focus = None;
        return Ok(changed);
    }
    let t = Tree::load(ws).map_err(err)?;
    if t.get(&g).is_none() {
        return Err(format!("札が無い: {g}（show_goals で一覧）"));
    }
    if t.get(&g).is_some_and(|g| g.state != "posted") {
        return Err(format!("札 {g} は下ろされている。閲覧は show_goals(id=\"{g}\")"));
    }
    let changed = s.focus.as_deref() != Some(g.as_str());
    s.focus = Some(g);
    Ok(changed)
}

fn search_context(ws: &mut Workspace, s: &mut Session, a: &A) -> R {
    let query_s = a.s("query")?.unwrap_or_default();
    let top_k = a.int("top_k", 1, 20)?.or(a.int("limit", 1, 20)?).unwrap_or(4) as usize;
    let min_score = a.n("min_score")?.unwrap_or(0.1);
    let mode = a.one_of("search_mode", &["hybrid", "bm25", "semantic"])?.unwrap_or_else(|| "hybrid".into());
    let full = a.b("full_content")?.unwrap_or(false);
    let budget = a.int("token_budget", 50, 1_000_000)?.unwrap_or(if full { 4000 } else { 1000 }) as usize;
    let known: Option<serde_json::Map<String, Value>> = match a.raw("known_revisions") {
        None => None,
        Some(Value::Object(m)) => Some(m.clone()),
        Some(_) => return Err("known_revisions は {id: revision} の形で渡してください".into()),
    };
    let not_relevant = a.strs("not_relevant")?.unwrap_or_default();
    let level = s.level(ws);
    let focus_changed = set_focus(ws, s, a.s("goal")?)?;
    let fresh = a.b("fresh")?.unwrap_or(false) || focus_changed;
    if fresh {
        s.memory_held.clear();
    }
    let auto_known = level == Level::C2 && known.is_none() && !fresh;
    // 普段は本文中心の短い表示。札の全体・連携段階・search_id・取手の追跡の詳細は verbose のときだけ。
    let verbose = a.b("verbose")?.unwrap_or(false);

    // 直前に読んだが辿らず使わず、すぐ言い換えた検索（C1/C2）。この検索を記録する前に見る。
    let reform = useful::reformulated(ws, s, level, &crate::terms::query_terms(&query_s));

    let args = SearchArgs {
        query: query_s.clone(),
        filter: Filter {
            agent: a.s("agent")?,
            tags: a.strs("tags")?.unwrap_or_default(),
            include_history: a.b("include_history")?.unwrap_or(false),
            include_hidden: a.b("include_hidden")?.unwrap_or(false),
        },
        top_k,
        min_score,
        mode,
    };
    let ranked = search::search(ws, &args).map_err(err)?;
    let ids: Vec<String> = ranked.hits.iter().map(|h| h.id.clone()).collect();
    let views = query::views(ws.conn(), &ids).map_err(err)?;

    let context_held = s.context_held.clone().filter(|(_, at)| !fresh && (level == Level::C2 || at.elapsed() < goals::REFRESH_AFTER));
    let context_key = a.s("context_key")?;
    let legacy = if context_key.is_some() || verbose {
        crate::shared_context::render(ws, context_key.as_deref(), known.as_ref(), context_held.as_ref().map(|(r, _)| r.as_str()), budget.min(600))?
    } else {
        None
    };
    let search_id = format!("s{}", new_id(&query_s));
    let mut entries: Vec<Entry> = vec![];
    for (i, h) in ranked.hits.iter().enumerate() {
        let Some(v) = views.get(&h.id) else { continue };
        let limit = if i == 0 { 300 } else { 220 };
        // 現物は保持版の判定より先に見直す。移動・消失は表示の版に入れ、保持版があっても隠さない（E3）。
        let anchors = query::anchors_of(ws.conn(), &h.id).map_err(err)?;
        let place = if verbose {
            anchorview::line(ws, &anchors, 3).map(|l| format!("**取手:** {l}"))
        } else {
            anchorview::short_line(ws, &anchors, 2).map(|l| format!("**現物:** {l}"))
        };
        let state_flags = flags(ws, v);
        let rev = revision(&query_s, full, limit, v, h.score, Some(&format!("{verbose}:{}{}", place.as_deref().unwrap_or(""), state_flags)));
        let heading = if verbose {
            format!("### [relative_score: {}] {}{state_flags}", fmt_score(h.score), short(&v.topic, 80))
        } else {
            format!("### {}{state_flags}", short(&v.topic, 80))
        };
        let mut meta = if verbose { meta_line(v) } else { date(v).to_string() };
        // idは正本のまま。readの参照は、選んだ検索を接続の寿命に依存せず運ぶ。
        let read_ref = if query_s.trim().is_empty() { h.id.clone() } else { format!("{}@{search_id}", h.id) };
        meta.push_str(&format!(" | **read:** `{read_ref}`"));
        if let Some(src) = source_line(ws, v, &anchors) {
            meta.push_str(&format!(" | {src}"));
        }
        // 予算で本文を省いた参照に使う行。完全な表示の版は載せない（E1：未提示の本文を保持済みにさせない）。
        let status_line = meta.clone();
        if known.is_some() {
            meta.push_str(&format!(" | **revision:** `{rev}`"));
        }
        if (!fresh && known.as_ref().and_then(|k| k.get(&h.id)).and_then(|x| x.as_str()) == Some(rev.as_str()))
            || (auto_known && s.memory_held.get(&h.id) == Some(&rev))
        {
            entries.push(Entry {
                id: h.id.clone(),
                heading: heading.clone(),
                block: format!("UNCHANGED id=`{}` read=`{read_ref}`", h.id),
                status_line,
                revision: Some(rev),
            });
            continue;
        }
        let mut lines = vec![heading.clone(), meta];
        if let Some(l) = place {
            lines.push(l);
        }
        if verbose {
            if let Some(l) = learned_line(&h.learned) {
                lines.push(l);
            }
        }
        if h.score == 0.0 && h.reasons.iter().any(|r| r.starts_with("learned_tag:") || r.starts_with("recalled_tag:")) {
            let words: Vec<&str> = h.recall.iter().chain(h.learned.iter()).map(|(t, _)| t.as_str()).take(3).collect();
            lines.push(format!("**理由:** 前にこの語（{}）で探して選ばれた記憶（本文には無い語）", words.join("・")));
        }
        let body = if full {
            v.content.clone()
        } else {
            let e = query_excerpts(&v.content, &query_s, limit);
            if e.complete {
                v.content.clone()
            } else {
                let t = Utf16Text::new(&v.content);
                format!("抜粋 {}", e.ranges.iter().map(|(x, y)| format!("[{x}:{y}] {}", t.slice(*x, *y))).collect::<Vec<_>>().join("\n"))
            }
        };
        lines.push(body);
        entries.push(Entry { id: h.id.clone(), heading, block: lines.join("\n"), status_line, revision: Some(rev) });
    }

    // 末尾（必ず付ける）と別枠（入れば付ける）を先に作り、応答全体で予算を判定する（R1）。
    let sem = if ranked.semantic.used {
        if ranked.semantic.pending > 0 {
            format!("semantic=ok（vector未計算 semantic_pending={}）", ranked.semantic.pending)
        } else {
            "semantic=ok".into()
        }
    } else if args.mode == "bm25" {
        "semantic=off".into()
    } else {
        format!("semantic=unavailable（{}）BM25だけで検索", ranked.semantic.note.clone().unwrap_or_default())
    };
    let focus_note = s.focus.clone().map(|f| format!(" 焦点:{f}")).unwrap_or_default();
    let status = if verbose {
        format!("\n\n(search_id={search_id} 連携:{}{focus_note} {sem})", level.as_str())
    } else if !ranked.semantic.used && args.mode != "bm25" {
        format!("\n\n(意味の近さでは探していない：{})", ranked.semantic.note.clone().unwrap_or_default())
    } else {
        String::new()
    };
    // 別枠（§6.6）：以前この問いで開かれた記憶。開かれた数と使われた数を分けて出す。
    let mut past_block = String::new();
    if std::env::var("ANT_MEMORY_PAST_READS").as_deref() != Ok("off") && !query_s.trim().is_empty() {
        let past = query::past_reads_for_terms(ws.conn(), &ranked.terms, 10).map_err(err)?;
        // 通常枠と同じ条件（可視性・履歴・tags・agent）を通ったものだけ（E4）。
        let allowed: HashSet<String> =
            query::eligible_ids(ws.conn(), &args.filter, past.iter().map(|p| p.0.clone()).collect()).map_err(err)?.into_iter().collect();
        let past: Vec<_> = past.into_iter().filter(|p| allowed.contains(&p.0)).collect();
        let ranked_ids: HashSet<&String> = ids.iter().collect();
        let head = "\n\n以前この問いで開かれた記憶（利用の証明ではない）:\n";
        let cap = (budget as f64 * 0.15) as usize;
        let mut lines: Vec<String> = vec![];
        for (mid, q, opened, used) in past.into_iter().filter(|p| !ranked_ids.contains(&p.0)).take(3) {
            let Some(v) = query::view(ws.conn(), &mid).map_err(err)? else {
                continue;
            };
            let l = if verbose {
                format!("- {mid} {} 開かれた:{opened} / 使われた:{used}（{q}） status={}", short(&v.topic, 60), v.status)
            } else {
                format!("- `{mid}` {}{}", short(&v.topic, 60), flags(ws, &v))
            };
            let trial = format!("{head}{}", [lines.clone(), vec![l.clone()]].concat().join("\n"));
            if estimate_tokens(&trial) > cap {
                break;
            }
            lines.push(l);
        }
        if !lines.is_empty() {
            past_block = format!("{head}{}", lines.join("\n"));
        }
    }

    let heading = if full { "保存記録（Raw）".to_string() } else { PREFIX.to_string() };
    let slogans = goals::summary(ws, s).map_err(err)?;
    let slogans = if budget >= 300 && !slogans.is_empty() { format!("\n\n{slogans}") } else { String::new() };
    let join = |sel: &[&Entry], partial: usize, past: &str| {
        let body = if entries.is_empty() {
            "No matching chunks found.".to_string()
        } else {
            sel.iter().map(|e| e.block.clone()).collect::<Vec<_>>().join("\n\n---\n\n")
        };
        let mut notes = vec![];
        if sel.len() < entries.len() || partial > 0 {
            notes.push(format!("token_budgetにより本文/後続を省略（{}件）。Rawはread_context", entries.len() - sel.len() + partial));
        }
        let foot = if notes.is_empty() { String::new() } else { format!("\n\n({})", notes.join("。")) };
        format!("{heading}\n\n{body}{foot}{past}{status}{slogans}")
    };
    let all: Vec<&Entry> = entries.iter().collect();
    // 予算の中で、通常の結果を先に取る。別枠は、通常の結果を1件以上残せるときだけ付ける。
    let fit = |past: &str| -> Option<(String, Vec<String>)> {
        let b = budget;
        let mut kept: Vec<&Entry> = vec![];
        for e in &entries {
            let mut trial = kept.clone();
            trial.push(e);
            if estimate_tokens(&join(&trial, 0, past)) <= b {
                kept = trial;
            } else {
                break;
            }
        }
        if kept.is_empty() && !entries.is_empty() {
            let e = &entries[0];
            let reference = Entry {
                id: e.id.clone(),
                heading: e.heading.clone(),
                block: format!("{}\n{}\nmetadata/本文省略。read_contextでRawを取得", e.heading, e.status_line),
                status_line: String::new(),
                revision: None,
            };
            let t = join(&[&reference], 1, past);
            return (estimate_tokens(&t) <= b).then(|| (t, vec![e.id.clone()]));
        }
        let t = join(&kept, 0, past);
        (estimate_tokens(&t) <= b).then(|| (t, kept.iter().map(|e| e.id.clone()).collect()))
    };
    let (mut text, returned): (String, Vec<String>) = if estimate_tokens(&join(&all, 0, &past_block)) > budget {
        match fit("") {
            Some((mut t, r)) => {
                if estimate_tokens(&format!("{t}{past_block}")) <= budget {
                    t.push_str(&past_block);
                }
                (t, r)
            }
            None => {
                // 見出しと末尾だけでも入らない。札の節は返していないので、保持版も確定しない。
                let first = entries.first().map(|e| e.id.clone());
                let t = match &first {
                    Some(id) => format!("BUDGET_TOO_SMALL: read_context(id=\"{id}@{search_id}\")。札は show_goals"),
                    None => format!("BUDGET_TOO_SMALL: 札は show_goals (search_id={search_id})"),
                };
                (t, first.into_iter().collect())
            }
        }
    } else {
        (join(&all, 0, &past_block), entries.iter().map(|e| e.id.clone()).collect())
    };

    // 検索を記録する（返したものだけ）。この後の読取り・使用はこのidに結び付く。
    let results: Vec<(String, f64, Vec<String>)> =
        ranked.hits.iter().filter(|h| returned.contains(&h.id)).map(|h| (h.id.clone(), h.score, h.reasons.clone())).collect();
    let cx = s.apply_ctx();
    let gen = s.gen_seen;
    if !query_s.trim().is_empty() {
        let _recorded = ws
            .apply(
                &cx,
                Command::RecordSearch(SearchRecord {
                    search_id: search_id.clone(),
                    query_raw: query_s.clone(),
                    query_terms: ranked.terms.clone(),
                    gen,
                    focus_goal: s.focus.clone(),
                    results,
                }),
            )
            .map_err(err)?;
    }
    let mut uses: Vec<UseRecord> = reform;
    for id in &not_relevant {
        uses.push(UseRecord {
            memory_id: id.clone(),
            kind: "rejected".into(),
            weight: useful::W_REJECTED,
            search_id: Some(search_id.clone()),
            scope_key: s.scope_key(),
            terms: ranked.terms.clone(),
        });
    }
    if !uses.is_empty() {
        let _u = ws.apply(&cx, Command::RecordUses(uses)).map_err(err)?;
    }
    // 検索本文が先。補助表示は残り枠に入り、実際に返すときだけ保持する。
    let cap = budget;
    let room = cap.saturating_sub(estimate_tokens(&text) + 2);
    if room >= 50 {
        let section = goals::section(ws, s, level, fresh, room.min(250).saturating_sub(50), !verbose).map_err(err)?;
        if !section.text.is_empty() && !section.text.contains("変化なし") && estimate_tokens(&format!("{text}\n\n{}", section.text)) <= cap {
            text.push_str(&format!("\n\n{}", section.text));
            let _shown = section.commit(s);
        }
    }
    if let Some(l) = legacy {
        if estimate_tokens(&format!("{text}\n\n{}", l.text)) <= cap {
            text.push_str(&format!("\n\n{}", l.text));
            s.context_held = l.full.map(|r| match context_held {
                Some((old, at)) if old == r => (r, at),
                _ => (r, Instant::now()),
            });
        } else {
            let hint = "\nCONTEXT_INCOMPLETE: get_state で取得";
            if estimate_tokens(&format!("{text}{hint}")) <= cap {
                text.push_str(hint);
            }
        }
    }
    if level == Level::C2 {
        if s.memory_held.len() > 256 {
            s.memory_held.clear();
        }
        for e in &entries {
            // 参照だけ・予算落ち・失敗では既読にしない。
            if returned.contains(&e.id) && text.contains(&e.block) {
                if let Some(rev) = &e.revision {
                    s.memory_held.insert(e.id.clone(), rev.clone());
                }
            }
        }
    }
    Ok(text)
}

fn fmt_score(x: f64) -> String {
    if x.fract() == 0.0 {
        format!("{}", x as i64)
    } else {
        format!("{x}")
    }
}

// ─── 読取り ─────────────────────────────────────────────────────────────────

fn read_context(ws: &mut Workspace, s: &mut Session, a: &A) -> R {
    let supplied = a.req("id")?;
    let (id, embedded_via) = supplied.split_once('@').map(|(id, sid)| (id.to_string(), Some(sid.to_string()))).unwrap_or_else(|| (supplied.clone(), None));
    let via_arg = a.s("via")?;
    if embedded_via.is_some() && via_arg.is_some() && embedded_via != via_arg {
        return Err("read参照とviaの検索が違います。検索結果のread参照だけをidに渡してください".into());
    }
    let via = embedded_via.or(via_arg);
    let verdict = a.one_of("verdict", &["useful", "not_relevant"])?;
    let verbose = a.b("verbose")?.unwrap_or(false);
    let start = a.int("start", 0, i64::MAX)?.map(|x| x as usize);
    let around = a.int("around", 0, i64::MAX)?.map(|x| x as usize);
    let length = a.int("length", 1, 1_000_000)?.map(|x| x as usize);
    if start.is_some() && around.is_some() {
        return Err("start と around はどちらか一つ".into());
    }
    let read_budget = a.int("token_budget", 100, 1_000_000)?.map(|x| x as usize);
    let Some(v) = query::view(ws.conn(), &id).map_err(err)? else {
        return Ok(format!("No chunk found with id `{id}`."));
    };
    let (search_id, link) = useful::link_read(ws, s, &id, via.as_deref());
    let cx = s.apply_ctx();
    let gen = s.gen_seen;
    let _r = ws.apply(&cx, Command::RecordRead(ReadRecord { memory_id: id.clone(), search_id: search_id.clone(), link: link.into(), gen })).map_err(err)?;
    // 検索から選んで読んだ：その検索の語を「思い出す語」にする（どの検索か曖昧なら付けない）。
    // 読んで「この問いに無関係」と言ったものは、選んだことにしない（その語で呼び戻さない）。
    let rejected = verdict.as_deref() == Some("not_relevant");
    if let (false, Some(sid), "explicit" | "inferred" | "inferred_conn") = (rejected, &search_id, link) {
        let u = useful::make_use(ws, s, &id, useful::KIND_RECALLED, useful::W_RECALLED, Some(sid.clone()));
        if !u.terms.is_empty() {
            let _u = ws.apply(&cx, Command::RecordUses(vec![u])).map_err(err)?;
        }
    }
    if let Some(vd) = &verdict {
        let (kind, w) = if vd == "useful" { ("cited", useful::W_CITED) } else { ("rejected", useful::W_REJECTED) };
        let u = useful::make_use(ws, s, &id, kind, w, search_id.clone());
        let _u = ws.apply(&cx, Command::RecordUses(vec![u])).map_err(err)?;
    }
    let mut header = vec![
        format!("## {}{}", short(&v.topic, 100), flags(ws, &v)),
        format!("**id:** `{}` | **status:** {} | **visibility:** {} | **current:** {}", v.id, v.status, v.visibility, if v.current { "yes" } else { "no" }),
        format!(
            "**agent:** {} | **tags:** {} | **importance:** {} | **created:** {} | **updated:** {}",
            v.author,
            v.tags.join(", "),
            v.importance,
            v.created,
            v.updated
        ),
    ];
    if !verbose {
        header.truncate(1);
        header.push(format!("{} · {} · `{}`", date(&v), v.author, id));
    }
    let anchors = query::anchors_of(ws.conn(), &id).map_err(err)?;
    if let Some(src) = source_line(ws, &v, &anchors) {
        header.push(src);
    }
    if let Some(env) = query::recorded_env(ws.conn(), &id).map_err(err)?.filter(|_| verbose) {
        let g = &env["git"];
        let at = match (g["head"].as_str(), g["branch"].as_str()) {
            (Some(h), Some(b)) => format!("HEAD {}（{b}）", &h[..h.len().min(7)]),
            (Some(h), None) => format!("HEAD {}", &h[..h.len().min(7)]),
            _ => "gitの外".to_string(),
        };
        let dirty = if g["dirty"].as_bool() == Some(true) { format!("・未commitの変更 {}件", g["changed"]) } else { String::new() };
        header.push(format!("**記録時:** {} {at}{dirty}", env["workspace"].as_str().unwrap_or("")));
    }
    if let Ok(succ) =
        ws.conn().query_row("SELECT src FROM relations WHERE dst=?1 AND kind='supersedes' ORDER BY created_at DESC LIMIT 1", [&id], |r| r.get::<_, String>(0))
    {
        header.push(format!("**後継:** `{succ}`（この本文は履歴。現在の指示として適用しない）"));
    }
    if !anchors.is_empty() {
        let mut lines = vec!["**取手（今の現物を再観測）:**".to_string()];
        let max_anchors = if verbose { 12 } else { 3 };
        for an in anchors.iter().take(max_anchors) {
            let seen = anchorview::observe(ws, an);
            let tag = match an.origin.as_str() {
                "touched" => "（この記録の前に編集）",
                "inherited" => "（旧版から継承）",
                _ => "",
            };
            lines.push(format!("- {} {}{tag}", seen.label, an.segment));
            if let Some((p, loc)) = &seen.excerpt_from.as_ref().filter(|_| verbose && seen.label.contains("同一")) {
                if let Some(ex) = anchorview::excerpt(ws, p, loc.as_deref(), 240) {
                    lines.push(format!("  今の該当箇所 {ex}"));
                }
            }
        }
        if anchors.len() > max_anchors {
            lines.push(format!("- ほか{}件（verbose=true）", anchors.len() - max_anchors));
        }
        header.push(lines.join("\n"));
    }
    if verbose {
        let ev = query::learned_tags_for_memory(ws.conn(), &id).map_err(err)?;
        let learned = useful::strengths(&ev);
        if let Some(l) = learned.get(&id).and_then(|l| learned_line(l)) {
            header.push(l);
        }
        if let Some(r) = useful::recall_strengths(&ev).get(&id).filter(|r| !r.is_empty()) {
            let words = r.iter().take(6).map(|(t, _)| format!("#{t}")).collect::<Vec<_>>().join(" ");
            header.push(format!("**思い出す語:** {words}（この語で探して選ばれた実績。tagsとは別に自動で付く）"));
        }
        let link_note = match (&search_id, link) {
            (Some(sid), l) => format!("**読取り:** 検索 {sid} から（{l}）"),
            (None, _) => "**読取り:** 検索を経ない直接の読取り".into(),
        };
        header.push(link_note);
    }
    let raw = Utf16Text::new(&v.content);
    let ranged = start.is_some() || around.is_some() || length.is_some() || read_budget.is_some();
    if !ranged {
        return Ok(format!("{}\n\n{}", header.join("\n"), v.content));
    }
    let from = raw.boundary(start.unwrap_or_else(|| around.unwrap_or(0).saturating_sub(length.unwrap_or(600) / 2)));
    let to = if read_budget.is_some() && length.is_none() && around.is_none() { raw.len() } else { raw.boundary(from.saturating_add(length.unwrap_or(600))) };
    // 範囲本文に予算を優先配分。詳細ヘッダでRawの枠を消費しない。
    let head = format!("## {}{}\n`{id}` {} 部分Raw start={from} / UTF-16全{}", short(&v.topic, 24), flags(ws, &v), date(&v), raw.len());
    let body = raw.slice(from, to);
    let next = |at: usize| format!("read_context(id=\"{id}\", start={})", from + at);
    let tail = if to < raw.len() { format!("\n続き: read_context(id=\"{id}\", start={to})") } else { String::new() };
    let (body, complete) = if let Some(b) = read_budget {
        crate::render::page(&body, 0, b.saturating_sub(estimate_tokens(&format!("{head}{tail}")) + 2), next)
    } else {
        (body, true)
    };
    let tail = if complete { tail } else { String::new() };
    Ok(format!("{head}\n\n{body}{tail}"))
}

// 一覧は行数と総量の両方で切り、実際に返した行の続きへ進む。
fn inventory(lines: &[(String, String)], total: i64, offset: i64, budget: usize, tool: &str) -> String {
    let foot = |n: usize| {
        let next = offset.saturating_add(n as i64);
        if next < total {
            format!("{total}件中 {}–{next}件。続き: {tool}(offset={next}, 同じ絞込条件)", if n == 0 { offset } else { offset + 1 })
        } else {
            format!("{total}件中 {n}件表示（offset={offset}）。末尾")
        }
    };
    let mut kept = vec![];
    for (line, fallback) in lines {
        let mut trial = kept.clone();
        trial.push(line.clone());
        if estimate_tokens(&format!("{}\n{}", trial.join("\n"), foot(trial.len()))) <= budget {
            kept = trial;
        } else if kept.is_empty() {
            let small = format!("{}\n{}", fallback, foot(1));
            if estimate_tokens(&small) <= budget {
                kept.push(fallback.clone());
            }
            break;
        } else {
            break;
        }
    }
    if kept.is_empty() && !lines.is_empty() {
        return "1件も収まらないため、token_budgetを増やしてください。".into();
    }
    format!("{}\n{}", kept.join("\n"), foot(kept.len()))
}

fn list_context(ws: &mut Workspace, a: &A) -> R {
    let agent = a.s("agent")?;
    let tags = a.strs("tags")?.unwrap_or_default();
    let limit = a.int("limit", 1, 500)?.unwrap_or(20);
    let offset = a.int("offset", 0, i64::MAX)?.unwrap_or(0);
    let budget = a.int("token_budget", 100, 1_000_000)?.unwrap_or(1200) as usize;
    let verbose = a.b("verbose")?.or(a.b("detail")?).unwrap_or(false);
    let rows = query::list(ws.conn(), agent.as_deref(), &tags, limit, offset).map_err(err)?;
    let total = query::list_count(ws.conn(), agent.as_deref(), &tags).map_err(err)?;
    let lines = rows
        .iter()
        .map(|r| {
            let basic = format!("- `{}` {} {}{}", r.id, date(r), short(&r.topic, 60), flags(ws, r));
            let detail = if verbose {
                format!(
                    " | {} | {} | {} | {}{}",
                    r.author,
                    r.tags.join(","),
                    r.importance,
                    r.updated,
                    r.source_ref.as_ref().map(|s| format!(" | source:{s}")).unwrap_or_default()
                )
            } else {
                String::new()
            };
            (format!("{basic}{detail}\n  抜粋: {}", short(&r.content, 80)), format!("- `{}`{}（read_context）", r.id, flags(ws, r)))
        })
        .collect::<Vec<_>>();
    Ok(inventory(&lines, total, offset, budget, "list_context"))
}

// ─── 書込 ───────────────────────────────────────────────────────────────────

/// topicを省いたら、本文の最初の行（見出しの記号は除く）を40字までで使う。本文はそのまま保存する。
fn topic_or_first_line(topic: Option<String>, content: &str) -> String {
    if let Some(t) = topic.filter(|t| !t.trim().is_empty()) {
        return t;
    }
    let line = content.lines().map(|l| l.trim().trim_start_matches(['#', '-', '*', '>']).trim()).find(|l| !l.is_empty()).unwrap_or("");
    let n = line.chars().count();
    if n > 40 {
        format!("{}…", line.chars().take(39).collect::<String>())
    } else {
        line.to_string()
    }
}

struct Prepared {
    anchors: Vec<am_domain::NewAnchor>,
    uses: Vec<UseRecord>,
    used_notes: Vec<String>,
    env: Option<Value>,
}

/// 取手（anchors・source_ref・touched）と、used、書いたときの環境。
/// 環境（workspace・HEAD・branch・未commitの変更）は、取手が無い記録にも自動で付ける。gitは1回だけ呼ぶ。
fn prepare(ws: &mut Workspace, s: &Session, a: &A, source_ref: Option<&str>, inherited: Vec<String>) -> Result<Prepared, String> {
    let genv = ws.root.as_deref().and_then(am_anchor::capture::git_env);
    let head = genv.as_ref().and_then(|g| g.head.clone());
    let head = head.as_deref();
    let mut anchors = anchorview::prepare(ws, &a.strs("anchors")?.unwrap_or_default(), "agent", head);
    if let Some(src) = source_ref {
        anchors.extend(anchorview::prepare(ws, &[src.to_string()], "source_ref", head));
    }
    anchors.extend(anchorview::prepare(ws, &inherited, "inherited", head));
    let touched_paths = s.conversation_id().and_then(|c| ws.touched.get(&c).cloned()).unwrap_or_default();
    let known: HashSet<String> = anchorview::file_paths(&anchors).into_iter().collect();
    let touched: Vec<String> = touched_paths.into_iter().filter(|p| !known.contains(p)).collect();
    anchors.extend(anchorview::prepare(ws, &touched, "touched", head));
    let used = a.strs("used")?.unwrap_or_default();
    let out = useful::uses_for_write(ws, s, &used, &anchorview::file_paths(&anchors));
    let env = json!({
        "workspace": ws.slug,
        "root": ws.root.as_ref().map(|r| r.display().to_string()),
        "client": s.client,
        "git": genv,
    });
    Ok(Prepared { anchors, uses: out.uses, used_notes: out.notes, env: Some(env) })
}

/// 書込の後。返す行は、Agentが渡した used への返事だけ（促しや小言は出さない）。
fn after_write(ws: &mut Workspace, s: &Session, p: &Prepared, id: &str) -> Vec<String> {
    let mut lines = vec![];
    if let Some(c) = s.conversation_id() {
        ws.touched.remove(&c);
    }
    if !p.used_notes.is_empty() {
        lines.push(format!("used: {}", p.used_notes.join(", ")));
    }
    if let Some(n) = &ws.notify_write {
        n(&ws.slug, id);
    }
    ws.invalidate_resolver();
    lines
}

fn apply_write(ws: &mut Workspace, s: &Session, build: impl Fn(String) -> Command) -> Result<String, String> {
    let cx = s.apply_ctx();
    // idは時刻と連番から作る。衝突したら一度だけ作り直す。
    for attempt in 0..2 {
        let id = new_id(&format!("{}:{attempt}", s.client));
        match ws.apply(&cx, build(id.clone())) {
            Ok(_) => return Ok(id),
            Err(StoreError::Precondition(m)) if m.starts_with("idが衝突") && attempt == 0 => continue,
            Err(e) => return Err(err(e)),
        }
    }
    Err("idを作れない".into())
}

fn write_context(ws: &mut Workspace, s: &mut Session, a: &A) -> R {
    let content = a.req("content")?;
    let topic = topic_or_first_line(a.s("topic")?, &content);
    let tags = a.strs("tags")?.unwrap_or_default();
    let importance = a.one_of("importance", IMPORTANCE)?.unwrap_or_else(|| "medium".into());
    let expires_at = a.ttl()?;
    let source_ref = a.s("source_ref")?.map(|x| x.trim().to_string()).filter(|x| !x.is_empty());
    let p = prepare(ws, s, a, source_ref.as_deref(), vec![])?;
    let goal = s.focus.clone();
    let id = apply_write(ws, s, |id| Command::Write {
        mem: NewMemory {
            id,
            topic: topic.clone(),
            content: content.clone(),
            tags: tags.clone(),
            importance: importance.clone(),
            expires_at: expires_at.clone(),
            source_ref: source_ref.clone(),
            anchors: p.anchors.clone(),
            env: p.env.clone(),
        },
        reason: "write_context".into(),
        lineage: None,
        goal: goal.clone(),
        uses: p.uses.clone(),
    })?;
    let src = source_ref.as_ref().map(|x| format!(" | source={x}")).unwrap_or_default();
    let mut out = vec![format!("Chunk saved: id=`{id}` | topic=\"{topic}\" | tags=[{}] | importance={importance}{src}", tags.join(", "))];
    out.extend(after_write(ws, s, &p, &id));
    Ok(out.join("\n"))
}

fn observe_context(ws: &mut Workspace, s: &mut Session, a: &A) -> R {
    let kind = a.one_of("kind", &["observation", "unknown", "hypothesis"])?.ok_or("kind は必須です")?;
    let content = a.req("content")?;
    let topic = topic_or_first_line(a.s("topic")?, &content);
    let parent = a.s("parent_id")?;
    let resolves = a.b("resolves_unknown")?.unwrap_or(false);
    if resolves && kind != "observation" {
        return Err("resolves_unknown=true は observation でのみ指定できます".into());
    }
    if resolves && parent.is_none() {
        return Err("resolves_unknown=true には、解決した UNKNOWN の parent_id を指定してください".into());
    }
    let mut tags = vec![format!("tk:{kind}")];
    for t in a.strs("tags")?.unwrap_or_default() {
        if !tags.contains(&t) && !t.is_empty() {
            tags.push(t);
        }
    }
    let importance = a.one_of("importance", IMPORTANCE)?.unwrap_or_else(|| if kind == "unknown" { "high".into() } else { "medium".into() });
    let expires_at = a.ttl()?;
    let source_ref = a.s("source_ref")?.map(|x| x.trim().to_string()).filter(|x| !x.is_empty());
    let reason = a.s("reason")?;
    let p = prepare(ws, s, a, source_ref.as_deref(), vec![])?;
    let goal = s.focus.clone();
    let lineage = parent.clone().map(|pid| Lineage { parent_id: pid, resolves, reason: reason.clone() });
    let id = apply_write(ws, s, |id| Command::Write {
        mem: NewMemory {
            id,
            topic: topic.clone(),
            content: content.clone(),
            tags: tags.clone(),
            importance: importance.clone(),
            expires_at: expires_at.clone(),
            source_ref: source_ref.clone(),
            anchors: p.anchors.clone(),
            env: p.env.clone(),
        },
        reason: format!("observe_context:{kind}"),
        lineage: lineage.clone(),
        goal: goal.clone(),
        uses: p.uses.clone(),
    })?;
    let mut out = vec![format!("観測記録 id=`{id}` kind={kind}"), format!("tags={}", tags.join(", "))];
    if let Some(pid) = &parent {
        out.push(format!("parent=`{pid}`"));
    }
    if let Some(src) = &source_ref {
        out.push(format!("source_ref={src}"));
    }
    if resolves {
        out.push(format!("UNKNOWN `{}` はこの再観測でresolved。旧UNKNOWN本文は保持されています。", parent.unwrap_or_default()));
    }
    out.extend(after_write(ws, s, &p, &id));
    Ok(out.join("\n"))
}

fn correct_context(ws: &mut Workspace, s: &mut Session, a: &A) -> R {
    let id = a.req("id")?;
    let reason = a.req("reason")?;
    let clear_ttl = a.b("clear_ttl")?.unwrap_or(false);
    if a.raw("ttl_days").is_some() && clear_ttl {
        return Err("ttl_days と clear_ttl は同時指定できません".into());
    }
    let Some(old) = query::view(ws.conn(), &id).map_err(err)? else {
        return Err(format!("訂正対象が見つかりません: {id}"));
    };
    let fields = ["topic", "content", "tags", "importance", "ttl_days", "source_ref", "anchors"];
    if fields.iter().all(|f| a.raw(f).is_none()) && !clear_ttl {
        return Err("訂正内容を1つ以上指定してください".into());
    }
    let topic = a.s("topic")?.unwrap_or(old.topic.clone());
    let content = a.s("content")?.unwrap_or(old.content.clone());
    let tags = a.strs("tags")?.unwrap_or(old.tags.clone());
    let importance = a.one_of("importance", IMPORTANCE)?.unwrap_or(old.importance.clone());
    let expires_at = if clear_ttl { None } else { a.ttl()?.or(old.expires_at.clone()) };
    let source_ref = a.s("source_ref")?.or(old.source_ref.clone()).map(|x| x.trim().to_string()).filter(|x| !x.is_empty());
    // 取手は、明示が無ければ旧版のものを引き継ぐ（指紋は今の現物で取り直す）。
    let inherited: Vec<String> = if a.raw("anchors").is_none() {
        query::anchors_of(ws.conn(), &id).map_err(err)?.into_iter().filter(|x| x.origin == "agent" || x.origin == "inherited").map(|x| x.segment).collect()
    } else {
        vec![]
    };
    let p = prepare(ws, s, a, source_ref.as_deref(), inherited)?;
    let goal = s.focus.clone();
    let new_id_ = apply_write(ws, s, |nid| Command::Supersede {
        old_id: id.clone(),
        mem: NewMemory {
            id: nid,
            topic: topic.clone(),
            content: content.clone(),
            tags: tags.clone(),
            importance: importance.clone(),
            expires_at: expires_at.clone(),
            source_ref: source_ref.clone(),
            anchors: p.anchors.clone(),
            env: p.env.clone(),
        },
        reason: reason.clone(),
        goal: goal.clone(),
        uses: p.uses.clone(),
    })?;
    let mut out = vec![format!("訂正完了。旧記録 `{id}` は superseded のままRaw保持。\n新記録 id=`{new_id_}`。省略したmetadataは旧記録から継承しました。")];
    out.extend(after_write(ws, s, &p, &new_id_));
    Ok(out.join("\n"))
}

fn supersede_context(ws: &mut Workspace, s: &mut Session, a: &A) -> R {
    let old_id = a.req("old_id")?;
    let content = a.req("content")?;
    let topic = topic_or_first_line(a.s("topic")?, &content);
    let reason = a.req("reason")?;
    let Some(old) = query::view(ws.conn(), &old_id).map_err(err)? else {
        return Err(format!("置換対象が見つかりません: {old_id}"));
    };
    let tags = a.strs("tags")?.unwrap_or(old.tags.clone());
    let importance = a.one_of("importance", IMPORTANCE)?.unwrap_or(old.importance.clone());
    let expires_at = match a.ttl()? {
        Some(e) => Some(e),
        None => old.expires_at.clone(),
    };
    let source_ref = a.s("source_ref")?.map(|x| x.trim().to_string()).filter(|x| !x.is_empty());
    let p = prepare(ws, s, a, source_ref.as_deref(), vec![])?;
    let goal = s.focus.clone();
    let nid = apply_write(ws, s, |nid| Command::Supersede {
        old_id: old_id.clone(),
        mem: NewMemory {
            id: nid,
            topic: topic.clone(),
            content: content.clone(),
            tags: tags.clone(),
            importance: importance.clone(),
            expires_at: expires_at.clone(),
            source_ref: source_ref.clone(),
            anchors: p.anchors.clone(),
            env: p.env.clone(),
        },
        reason: reason.clone(),
        goal: goal.clone(),
        uses: p.uses.clone(),
    })?;
    let mut out = vec![format!("置換完了。\n旧記録 `{old_id}` は superseded（本文は残っています）。\n新記録 id=`{nid}`")];
    out.extend(after_write(ws, s, &p, &nid));
    Ok(out.join("\n"))
}

fn simple(ws: &mut Workspace, s: &mut Session, a: &A, op: &str) -> R {
    let id = a.req("id")?;
    let reason = a.s("reason")?.unwrap_or_default();
    let cx = s.apply_ctx();
    match op {
        "retract" => {
            let r = ws.apply(&cx, Command::Retract { id: id.clone(), reason }).map_err(err)?;
            if r.detail["unchanged"] == true {
                Ok(format!("記録 `{id}` は既に retracted です（変更なし）。"))
            } else {
                Ok(format!("記録 `{id}` を retracted にしました。本文は消えていません。"))
            }
        }
        _ => {
            let r = ws.apply(&cx, Command::Restore { id: id.clone(), reason }).map_err(err)?;
            if r.detail["unchanged"] == true {
                Ok(format!("記録 `{id}` は既に active です（変更なし）。"))
            } else {
                Ok(format!("記録 `{id}` を active に戻しました。"))
            }
        }
    }
}

fn change_importance(ws: &mut Workspace, s: &mut Session, a: &A) -> R {
    let id = a.req("id")?;
    let importance = a.one_of("importance", IMPORTANCE)?.ok_or("importance は必須です")?;
    let reason = a.s("reason")?.unwrap_or_default();
    let r = ws.apply(&s.apply_ctx(), Command::ChangeImportance { id: id.clone(), importance, reason }).map_err(err)?;
    Ok(format!("重要度を {} → {} に変更しました (`{id}`)。", r.detail["from"].as_str().unwrap_or("?"), r.detail["to"].as_str().unwrap_or("?")))
}

fn set_visibility(ws: &mut Workspace, s: &mut Session, a: &A) -> R {
    let id = a.req("id")?;
    let visibility = a.one_of("visibility", VISIBILITY)?.ok_or("visibility は必須です")?;
    let reason = a.s("reason")?.unwrap_or_default();
    let r = ws.apply(&s.apply_ctx(), Command::SetVisibility { id: id.clone(), visibility: visibility.clone(), reason }).map_err(err)?;
    if r.detail.get("unchanged").is_some() {
        return Ok(format!("記録 `{id}` は既に {visibility} です。"));
    }
    Ok(format!("visibility を {} → {} に変更しました (`{id}`)。", r.detail["from"].as_str().unwrap_or("?"), r.detail["to"].as_str().unwrap_or("?")))
}

fn history(ws: &mut Workspace, a: &A) -> R {
    let id = a.req("id")?;
    let status = query::status_of(ws.conn(), &id).map_err(err)?.unwrap_or_else(|| "active".into());
    let (events, rels) = query::history(ws.conn(), &id).map_err(err)?;
    if events.is_empty() && rels.is_empty() {
        return Ok(format!("履歴イベントはまだありません。現在状態: {status}"));
    }
    let mut lines = vec![format!("状態: {status}"), String::new(), "イベント:".to_string()];
    for e in events {
        let mut l = format!("- {} [{}] by {}", e.created_at, e.kind, e.actor);
        if let Some(m) = e.model {
            l.push_str(&format!(" ({m})"));
        }
        if let Some(r) = e.reason {
            l.push_str(&format!(" — 理由: {r}"));
        }
        if e.old_value.is_some() || e.new_value.is_some() {
            l.push_str(&format!(" ({} → {})", e.old_value.as_deref().unwrap_or("-"), e.new_value.as_deref().unwrap_or("-")));
        }
        lines.push(l);
    }
    if !rels.is_empty() {
        lines.push(String::new());
        lines.push("関係:".into());
        for r in rels {
            lines.push(format!(
                "- {} {}: `{}`{}",
                if r.out { "この記録が" } else { "この記録を" },
                r.kind,
                r.other,
                r.reason.map(|x| format!(" — 理由: {x}")).unwrap_or_default()
            ));
        }
    }
    Ok(lines.join("\n"))
}

// ─── state ──────────────────────────────────────────────────────────────────

fn get_state(ws: &mut Workspace, a: &A) -> R {
    let key = a.req("key")?;
    if key == "goal.current" {
        // 互換の読取り：正式な全体目標の札から作る。無ければv3のstateをそのまま返す。
        let t = Tree::load(ws).map_err(err)?;
        let overall: Vec<_> = t.goals.iter().filter(|g| g.kind == "overall" && g.state == "posted").collect();
        if !overall.is_empty() {
            let v = json!({
                "final_goal": overall.iter().map(|g| g.title.clone()).collect::<Vec<_>>().join(" / "),
                "goal_ids": overall.iter().map(|g| g.id.clone()).collect::<Vec<_>>(),
                "source": "v4の札（照合済みのユーザー発言が根拠）。詳細は show_goals",
            });
            return Ok(serde_json::to_string_pretty(&v).expect("json"));
        }
    }
    match query::state_get(ws.conn(), &key).map_err(err)? {
        None => Ok(format!("State key \"{key}\" not found.")),
        Some((v, _)) => {
            let val: Value = serde_json::from_str(&v).unwrap_or(Value::String(v.clone()));
            let body = match &val {
                Value::String(s) => s.clone(),
                other => serde_json::to_string_pretty(other).expect("json"),
            };
            if key == "goal.current" {
                Ok(format!("{body}\n（v3のstate。v4の正式な全体目標ではない。札は show_goals）"))
            } else {
                Ok(body)
            }
        }
    }
}

fn set_state(ws: &mut Workspace, s: &mut Session, a: &A) -> R {
    let key = a.req("key")?;
    if !a.has("value") {
        return Err("value は必須です".into());
    }
    let value = a.0.get("value").cloned().unwrap_or(Value::Null);
    let expected = if a.has("expected_value") { Some(a.0.get("expected_value").cloned().unwrap_or(Value::Null)) } else { None };
    let cas = expected.is_some();
    match ws.apply(&s.apply_ctx(), Command::SetState { key: key.clone(), value, expected }) {
        Ok(r) => {
            let at = r.detail["updated_at"].as_str().unwrap_or("").to_string();
            let mut out = if cas { format!("State \"{key}\" atomically updated at {at}.") } else { format!("State \"{key}\" written at {at}.") };
            if key == "goal.current" {
                // v3の慣習のまま書けるようにする。ただし照合した全体目標（札）にはしない。
                out.push_str(" （共有state）");
            }
            Ok(out)
        }
        Err(e @ StoreError::CasConflict { .. }) => Err(e.to_string()),
        Err(e) => Err(err(e)),
    }
}

fn list_state(ws: &mut Workspace, a: &A) -> R {
    let q = a.s("query")?;
    let offset = a.int("offset", 0, i64::MAX)?.unwrap_or(0);
    let rows = query::state_list(ws.conn(), q.as_deref(), a.int("limit", 1, 500)?.unwrap_or(20), offset).map_err(err)?;
    let total = ws
        .conn()
        .query_row("SELECT count(*) FROM state WHERE ?1 IS NULL OR instr(key,?1)>0", [q.as_deref()], |r| r.get::<_, i64>(0))
        .map_err(|e| e.to_string())?;
    let lines = rows
        .iter()
        .map(|(k, v, at)| {
            let line = format!("- {k} · {} · {}", at.get(..10).unwrap_or(at), short(v, 120));
            (line, format!("- {k}（値: get_state）"))
        })
        .collect::<Vec<_>>();
    Ok(inventory(&lines, total, offset, a.int("token_budget", 100, 1_000_000)?.unwrap_or(1000) as usize, "list_state"))
}

fn list_unknowns(ws: &mut Workspace, a: &A) -> R {
    let resolved = a.b("include_resolved")?.unwrap_or(false);
    let history = a.b("include_history")?.unwrap_or(false);
    let offset = a.int("offset", 0, i64::MAX)?.unwrap_or(0);
    let budget = a.int("token_budget", 100, 1_000_000)?.unwrap_or(1000) as usize;
    let (rows, total) = query::unknown_page(ws.conn(), resolved, history, a.int("limit", 1, 500)?.unwrap_or(10), offset).map_err(err)?;
    let lines = rows
        .iter()
        .map(|r| {
            let basic = format!("- `{}` {} {}{}", r.view.id, date(&r.view), short(&r.view.topic, 60), flags(ws, &r.view));
            (format!("{basic}\n  {}", short(&r.view.content, 80)), format!("- `{}`{}", r.view.id, flags(ws, &r.view)))
        })
        .collect::<Vec<_>>();
    Ok(inventory(&lines, total, offset, budget, "list_unknowns"))
}

// ─── 札 ─────────────────────────────────────────────────────────────────────

fn verify_quote(ws: &Workspace, s: &Session, quote: &str) -> utter::Outcome {
    let session = s.conversation.as_ref().map(|(_, sess)| sess.as_str());
    utter::verify(&utter::Target { client: &s.client, session, workspace_root: ws.root_path(), cwd: s.cwd.as_deref() }, quote)
}

/// 札の名前は24字まで。超えたら23字＋…に丸め、元の名前は根拠の注として残す（突き返さない）。
fn fit_title(t: &str) -> (String, Option<String>) {
    let t = t.trim();
    if t.chars().count() <= 24 {
        return (t.to_string(), None);
    }
    (format!("{}…", t.chars().take(23).collect::<String>()), Some(t.to_string()))
}

fn post_goal(ws: &mut Workspace, s: &mut Session, a: &A) -> R {
    let cx = s.apply_ctx();
    if let Some(id) = a.s("id")? {
        let title = a.s("title")?.map(|t| fit_title(&t).0);
        let note = a.s("note")?;
        if title.is_none() && note.is_none() {
            return Err("編集では title か note を渡してください".into());
        }
        let _r = ws.apply(&cx, Command::EditGoal { id: id.clone(), title, basis_note: note }).map_err(err)?;
        return Ok(format!("札 {id} を編集した。"));
    }
    let kind = a.one_of("kind", &["overall", "work"])?.unwrap_or_else(|| "work".into());
    let (title, long_title) = fit_title(&a.req("title")?);
    let quote = a.s("quote")?.filter(|q| !q.trim().is_empty());
    let note = a.s("note")?.filter(|q| !q.trim().is_empty());
    let used = a.strs("used")?.unwrap_or_default();
    let parent = match a.s("parent")? {
        Some(p) if p == "none" => None,
        Some(p) => Some(p),
        None if kind == "work" => s.focus.clone(),
        None => None,
    };

    let t = Tree::load(ws).map_err(err)?;
    let gid = format!("g{}", query::next_goal_number(ws.conn()).map_err(err)?);
    let mut basis = vec![];
    let mut utterance = None;
    let mut verify_note = String::new();
    if let Some(q) = &quote {
        match verify_quote(ws, s, q) {
            utter::Outcome::Found(u) => {
                basis.push(Basis::Utterance { utterance_id: u.id.clone(), quote: u.quoted_span.clone() });
                verify_note = format!(
                    "引用を会話ログで照合した（{} {}:{}）。照合したのは発言の存在と出所まで。この札を掲げることが今のユーザーの指示に沿うかは、呼んだ側が確かめる",
                    u.client, u.file, u.line_no
                );
                utterance = Some(u);
            }
            utter::Outcome::NotFound { why, searched } => {
                if kind == "overall" {
                    // 照合できない全体目標は、候補に留める（正式な札にしない）。
                    let pid = format!("p{}", query::next_proposal_number(ws.conn()).map_err(err)?);
                    let _r = ws
                        .apply(
                            &cx,
                            Command::ProposeGoal(Proposal {
                                id: pid.clone(),
                                action: "post".into(),
                                goal_id: None,
                                parent: None,
                                title: Some(title.clone()),
                                quote: q.clone(),
                                reason: Some(why.clone()),
                            }),
                        )
                        .map_err(err)?;
                    return Ok(format!(
                        "全体目標は掲げていない。候補（未確認）{pid} として記録した: {title}\n理由: {why}（会話ログ{}件を探した）\n全体目標が未設定でも、作業目標で進められる。後で照合できたら、同じ quote で post_goal(kind=\"overall\") を呼び直す",
                        searched.len()
                    ));
                }
                basis.push(Basis::UnverifiedQuote { quote: q.clone() });
                verify_note = format!("引用は会話ログで照合できなかった（{why}）。未確認の引用として持つ");
            }
        }
    }
    if let Some(n) = &note {
        basis.push(Basis::Initiative { note: n.clone(), from: used.first().cloned() });
    }
    if let Some(lt) = &long_title {
        basis.push(Basis::Initiative { note: format!("札の名前は24字に丸めた。元の名前: {lt}"), from: None });
    }
    if kind == "overall" && quote.is_none() {
        // 引用の無い全体目標は、エラーにせず候補に留める（正式な札にはしない）。
        let pid = format!("p{}", query::next_proposal_number(ws.conn()).map_err(err)?);
        let _r = ws
            .apply(
                &cx,
                Command::ProposeGoal(Proposal {
                    id: pid.clone(),
                    action: "post".into(),
                    goal_id: None,
                    parent: None,
                    title: Some(title.clone()),
                    quote: String::new(),
                    reason: Some(note.clone().unwrap_or_else(|| "ユーザー発言の引用が無い".into())),
                }),
            )
            .map_err(err)?;
        return Ok(format!(
            "全体目標の候補（未確認）{pid} として記録した: {title}
全体目標はユーザーが言ったときだけ正式にする。言った発言を quote に入れて呼び直せば掲げられる。作業目標（kind を省く）ならこのまま掲げられる"
        ));
    }
    if kind == "work" && parent.is_none() && quote.is_none() && note.is_none() {
        // 理由を書かなくても掲げられる。誰がいつ掲げたかは残る。
        basis.push(Basis::Initiative { note: "Agentの判断（理由の記載なし）".into(), from: used.first().cloned() });
    }
    if let Some(p) = &parent {
        if t.get(p).is_none() {
            return Err(format!("親の札が無い: {p}"));
        }
    }
    let resolves = query::open_proposals(ws.conn())
        .map_err(err)?
        .into_iter()
        .find(|p| p.action == "post" && quote.as_deref().is_some_and(|q| utter::norm(q) == utter::norm(&p.quote)))
        .map(|p| p.id);
    let _r = ws
        .apply(
            &cx,
            Command::PostGoal(NewGoal {
                id: gid.clone(),
                kind: kind.clone(),
                parent: parent.clone(),
                title: title.clone(),
                basis,
                utterance,
                resolves_proposal: if kind == "overall" { resolves } else { None },
            }),
        )
        .map_err(err)?;
    if !used.is_empty() {
        let out = useful::uses_for_write(ws, s, &used, &[]);
        if !out.uses.is_empty() {
            let _u = ws.apply(&cx, Command::RecordUses(out.uses)).map_err(err)?;
        }
    }
    s.focus = Some(gid.clone());

    let mut out = vec![format!("札を掲げた: {gid} {title}{}", if kind == "overall" { "（全体目標）" } else { "" })];
    if long_title.is_some() {
        out.push("名前が長いので24字に丸めた（元の名前は根拠に残した）".into());
    }
    if let Some(p) = &parent {
        out.push(format!("親: {p}"));
    }
    if !verify_note.is_empty() {
        out.push(verify_note);
    }
    out.push(format!("作業対象: {gid}"));
    Ok(out.join("\n"))
}

fn close_goal(ws: &mut Workspace, s: &mut Session, a: &A) -> R {
    let id = a.req("id")?;
    let how = match a.one_of("state", &["done", "dropped", "merged"])?.as_deref().unwrap_or("done") {
        "dropped" => GoalClose::Dropped,
        "merged" => GoalClose::Merged { into: a.s("into")?.ok_or("state=merged には into（統合先の札）が必須")? },
        _ => GoalClose::Done,
    };
    let reason = a.s("reason")?;
    let t = Tree::load(ws).map_err(err)?;
    let Some(g) = t.get(&id).cloned() else {
        return Err(format!("札が無い: {id}"));
    };
    let cx = s.apply_ctx();
    let mut utterance = None;
    if g.kind == "overall" {
        // 全体目標はユーザーが決めたもの。下ろすにも、ユーザーが終わりを言った発言を照合する（Q2）。
        // 照合できないときはエラーで止めず、「下ろす候補」として記録して先へ進めるようにする。
        let quote = a.s("quote")?.filter(|q| !q.trim().is_empty());
        let outcome: Result<am_domain::Utterance, String> = match &quote {
            None => Err("終わりを言ったユーザー発言の引用（quote）が無い".into()),
            Some(q) => match verify_quote(ws, s, q) {
                utter::Outcome::Found(u) => {
                    let posted_with: Vec<String> = g
                        .basis
                        .iter()
                        .filter_map(|b| match b {
                            Basis::Utterance { utterance_id, .. } => {
                                query::utterance(ws.conn(), utterance_id).ok().flatten().map(|x| format!("{}:{}", x.2, x.3))
                            }
                            _ => None,
                        })
                        .collect();
                    if posted_with.contains(&format!("{}:{}", u.file, u.line_no)) {
                        Err("引用した発言は、この全体目標を掲げたときの発言と同じ".into())
                    } else {
                        Ok(u)
                    }
                }
                utter::Outcome::NotFound { why, .. } => Err(why),
            },
        };
        match outcome {
            Ok(u) => utterance = Some(u),
            Err(why) => {
                let pid = format!("p{}", query::next_proposal_number(ws.conn()).map_err(err)?);
                let _r = ws
                    .apply(
                        &cx,
                        Command::ProposeGoal(Proposal {
                            id: pid.clone(),
                            action: "close".into(),
                            goal_id: Some(id.clone()),
                            parent: None,
                            title: None,
                            quote: quote.clone().unwrap_or_default(),
                            reason: Some(reason.clone().map(|r| format!("{r}（{why}）")).unwrap_or(why.clone())),
                        }),
                    )
                    .map_err(err)?;
                return Ok(format!(
                    "全体目標 {id} 「{}」を「下ろす候補」{pid} として記録した（{why}）。
全体目標はユーザーが決めたものなので、下ろすのはユーザーが終わりを言ったとき：その発言を quote にして close_goal を呼び直せば下ろせる。
配下の作業目標は今まで通り下ろせる。この札を待たずに次の作業へ進んでよい",
                    g.title
                ));
            }
        }
    }
    let done = matches!(how, GoalClose::Done);
    let _r = ws.apply(&cx, Command::CloseGoal { id: id.clone(), how, reason, utterance }).map_err(err)?;
    // goal_done：その札（と配下）を焦点にした検索から「役に立った」記憶へ +0.2。
    if done {
        let scope = t.subtree_ids(&id);
        let rows: Vec<(String, Option<String>, String)> = ws
            .conn()
            .prepare(
                "SELECT u.memory_id, u.search_id, s.focus_goal FROM uses u JOIN searches s ON s.search_id=u.search_id
                  WHERE u.weight > 0 AND u.kind NOT IN ('goal_done', 'recalled') AND s.focus_goal IS NOT NULL",
            )
            .and_then(|mut st| st.query_map([], |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)))?.collect())
            .map_err(|e: rusqlite::Error| e.to_string())?;
        let mut seen = HashSet::new();
        let uses: Vec<UseRecord> = rows
            .into_iter()
            .filter(|(m, _, f)| scope.contains(f) && seen.insert(m.clone()))
            .map(|(m, sid, _)| useful::make_use(ws, s, &m, "goal_done", useful::W_GOAL_DONE, sid))
            .collect();
        if !uses.is_empty() {
            let _u = ws.apply(&cx, Command::RecordUses(uses)).map_err(err)?;
        }
    }
    // 焦点が下ろした札そのものなら親へ。配下の掲示中の札にあるなら、そのまま続ける。
    if s.focus.as_deref() == Some(id.as_str()) {
        s.focus = g.parent.clone();
    }

    let t2 = Tree::load(ws).map_err(err)?;
    let mark = goals::state_mark(t2.get(&id).expect("closed goal"));
    let left = t2.posted_descendants(&id);
    let warn = if left.is_empty() {
        String::new()
    } else {
        format!("\n配下に掲示中の札が残っている: {}（{}件。show_goals(id=\"{id}\")）", left.iter().take(3).cloned().collect::<Vec<_>>().join(", "), left.len())
    };
    Ok(format!("札を下ろした: {id} {} {mark}{warn}", g.title))
}

fn show_goals(ws: &mut Workspace, s: &mut Session, a: &A) -> R {
    let id = a.s("id")?;
    let goal = a.s("goal")?;
    if matches!((&id, &goal), (Some(i), Some(g)) if i != g) {
        return Err("id と goal は同じ札を指定してください".into());
    }
    let target = id.or(goal);
    let enter = a.b("enter")?.unwrap_or(false);
    let trace = a.b("trace")?.unwrap_or(false);
    let verbose = a.b("verbose")?.unwrap_or(false);
    let budget = a.int("token_budget", 100, 1_000_000)?.unwrap_or(1200) as usize;
    let offset = a.int("offset", 0, i64::MAX)?.unwrap_or(0) as usize;
    let target = target.filter(|g| g != "none" && !g.is_empty());
    if trace && target.is_none() {
        return Err("trace には id（札のID）を指定してください".into());
    }
    let t = Tree::load(ws).map_err(err)?;
    if let Some(id) = &target {
        if t.get(id).is_none() {
            return Err(format!("札が無い: {id}"));
        }
    }
    if enter {
        set_focus(ws, s, Some(target.clone().unwrap_or_else(|| "none".into())))?;
    } else if a.s("id")?.as_deref() == Some("none") || a.s("goal")?.as_deref() == Some("none") {
        set_focus(ws, s, Some("none".into()))?;
    }
    // 先に配下を示す。根拠・追跡も含めた最終表示全体をページに分ける。
    let tree = goals::render_full(&t, target.as_deref(), s.focus.as_deref(), &Default::default(), usize::MAX / 2);
    let mut out = vec![if tree.text.is_empty() { "札なし。全体目標は未設定".into() } else { tree.text }];
    if let Some(gid) = &target {
        let g = t.get(gid).expect("validated");
        out.push(format!(
            "担当: {} · {}{}",
            g.posted_by,
            g.posted_at.get(..10).unwrap_or(&g.posted_at),
            if g.kind == "overall" { " · ユーザー根拠照合済み" } else { "" }
        ));
        if trace {
            out.extend(trace_lines(ws, &t, gid)?);
        }
        let basis = goals::basis_lines(ws, g).join("\n");
        if verbose {
            out.push(basis);
        } else if !basis.is_empty() {
            out.push(format!("{}{}", short(&basis, 120), if basis.chars().count() > 120 { "（全文: verbose=true）" } else { "" }));
        }
        if let Some(reason) = &g.close_reason {
            out.push(format!("終了理由: {}", if verbose { reason.clone() } else { short(reason, 120) }));
        }
    } else {
        out.extend(goals::proposals(ws));
        if verbose {
            for p in query::open_proposals(ws.conn()).map_err(err)? {
                out.push(format!("候補 {} 原文: {}", p.id, p.quote));
            }
        }
    }
    let body = out.join("\n");
    use sha2::Digest;
    let rev = hex::encode(sha2::Sha256::digest(body.as_bytes()))[..16].to_string();
    if a.s("revision")?.is_some_and(|r| r != rev) {
        return Ok("札の表示が更新されたため、offset=0から再取得してください。".into());
    }
    let args = target.as_ref().map(|id| format!("id=\"{id}\", ")).unwrap_or_default();
    let (text, complete) =
        crate::render::page(&body, offset, budget, |at| format!("show_goals({args}offset={at}, trace={trace}, verbose={verbose}, revision=\"{rev}\")"));
    // 閲覧先を作業先にしない。未提示の枝も既読にしない。
    if offset == 0 && complete && target.as_ref().is_none_or(|g| t.root_of(g) == *g) {
        s.held = Some(crate::ctx::Held { seq: query::last_seq(ws.conn()).map_err(err)?, full_at: Instant::now(), complete: true, root: target });
    }
    Ok(text)
}

/// 札の追跡（§7）。報告・根拠参照・観測を混ぜずに出す。
fn trace_lines(ws: &mut Workspace, t: &Tree, gid: &str) -> Result<Vec<String>, String> {
    let scope = t.subtree_ids(gid);
    let mut links = vec![];
    let mut conns = vec![];
    for g in &scope {
        for (mid, at) in query::goal_links(ws.conn(), g).map_err(err)? {
            links.push((at, mid, g.clone()));
        }
        conns.extend(query::conns_linked_to_goal(ws.conn(), g).map_err(err)?);
    }
    links.sort();
    links.reverse();
    conns.sort();
    conns.dedup();
    let mut out = vec![];
    let since = t.get(gid).map(|g| g.posted_at.as_str()).unwrap_or("");
    let ops = query::observed_ops(ws.conn(), &conns, since, 1).map_err(err)?;
    if let Some((at, kind, what)) = ops.last() {
        out.push(format!("最後のMCP観測: {at} {kind} {what}（MCP外は未観測）"));
    }
    if !links.is_empty() {
        out.push(format!("報告 {}件（実行の確認とは別。read_contextで取手を再観測）", links.len()));
    }
    for (at, mid, g) in &links {
        if let Some(v) = query::view(ws.conn(), mid).map_err(err)? {
            out.push(format!("- {at} `{mid}` {} [{}・{}]{g}", short(&v.topic, 80), v.author, v.status));
        }
    }
    Ok(out)
}

// ─── tool一覧 ───────────────────────────────────────────────────────────────

fn schema(props: Value, required: &[&str]) -> Value {
    json!({ "type": "object", "properties": props, "required": required, "additionalProperties": true })
}

pub fn list() -> Value {
    let imp = json!({ "type": "string", "enum": IMPORTANCE });
    let strs = json!({ "type": "array", "items": { "type": "string" } });
    let anchors = json!({ "type": "array", "items": { "type": "string" },
        "description": "任意の現物パス（doc/a.md#見出し、src/x.rs:L10-L30）。指紋は自動" });
    let used = json!({ "type": "array", "items": { "type": "string" },
        "description": "任意。読んで役立った記憶ID、最大5件" });
    let tools = vec![
        json!({ "name": "search_context",
            "description": "Search at task start or on unfamiliar changes. Default: 4 excerpts, ~1000-token response. Open read references as-is. goal explicitly selects work; fresh resends context.",
            "inputSchema": schema(json!({
                "query": { "type": "string", "description": "Search query." },
                "tags": strs.clone(), "agent": { "type": "string" },
                "top_k": { "type": "integer", "minimum": 1, "maximum": 20, "description": "Maximum results (default 4)." },
                "min_score": { "type": "number", "minimum": 0 },
                "search_mode": { "type": "string", "enum": ["hybrid", "bm25", "semantic"] },
                "include_history": { "type": "boolean", "description": "履歴(superseded/retracted/expired)も結果に含める" },
                "include_hidden": { "type": "boolean", "description": "cold/archiveの低可視性記憶も結果に含める" },
                "token_budget": { "type": "integer", "minimum": 50, "description": "応答全体の概算token上限" },
                "full_content": { "type": "boolean", "description": "Raw全文を要求（既定は問いに応じた原文抜粋）" },
                "known_revisions": { "type": "object", "additionalProperties": { "type": "string" }, "description": "任意の表示版 {id:revision}。{}で取得。圧縮後は空に" },
                "context_key": { "type": "string", "description": "v3の作業state（互換）を同梱する" },
                "goal": { "type": "string", "description": "札に入る（焦点にする）。\"none\"で焦点を外す" },
                "fresh": { "type": "boolean", "description": "保持版を捨てて再表示（圧縮後にも使用可）" },
                "not_relevant": { "type": "array", "items": { "type": "string" }, "description": "この問いに無関係だった記憶のid（この検索語に対してだけ効く）" },
                "verbose": { "type": "boolean", "description": "詳細メタ・札・共有stateを追加" }
            }), &["query"]) }),
        json!({ "name": "write_context",
            "description": "Save a decision, failure or progress. Only content is needed; topic and git provenance are automatic. Optional kind records unknown/hypothesis/observation.",
            "inputSchema": schema(json!({
                "topic": { "type": "string" }, "content": { "type": "string" }, "tags": strs.clone(), "importance": imp.clone(),
                "kind": { "type": "string", "enum": ["unknown", "hypothesis", "observation"] }, "parent_id": { "type": "string" }, "resolves_unknown": { "type": "boolean" }, "reason": { "type": "string" },
                "ttl_days": { "type": "number", "exclusiveMinimum": 0 },
                "source_ref": { "type": "string", "description": "Optional handle back to Raw/evidence such as file/commit/experiment/conversation/URL." },
                "anchors": anchors.clone(), "used": used.clone()
            }), &["content"]) }),
        json!({ "name": "read_context",
            "description": "Read stored Raw, with compact provenance. start/length or around select UTF-16 ranges; omitted reads all. verbose adds environment, anchors and learning details.",
            "inputSchema": schema(json!({
                "id": { "type": "string", "description": "read参照（id@検索ID）をそのまま。通常IDも可" },
                "verbose": { "type": "boolean" },
                "start": { "type": "integer", "minimum": 0 }, "length": { "type": "integer", "minimum": 1 },
                "around": { "type": "integer", "minimum": 0 }, "token_budget": { "type": "integer", "minimum": 100 },
                "via": { "type": "string", "description": "開いた元の search_id（省略時は同じ会話・接続の最近の検索から推定）" },
                "verdict": { "type": "string", "enum": ["useful", "not_relevant"], "description": "任意。この問いに役立った / 無関係だった" }
            }), &["id"]) }),
        json!({ "name": "list_context",
            "description": "Inventory with short Raw previews, total count and continuation. Default: 20 rows within ~1200 tokens. Full body: read_context.",
            "inputSchema": schema(json!({ "agent": { "type": "string" }, "tags": strs.clone(),
                "limit": { "type": "integer", "minimum": 1, "maximum": 500 }, "offset": { "type": "integer", "minimum": 0 }, "token_budget": { "type": "integer", "minimum": 100 }, "verbose": { "type": "boolean" }, "detail": { "type": "boolean" } }), &[]) }),
        json!({ "name": "correct_context",
            "description": "Correct an active memory. Omitted fields are inherited. Original Raw and history remain. reason required.",
            "inputSchema": schema(json!({
                "id": { "type": "string", "description": "訂正対象の現行記録ID" }, "topic": { "type": "string" }, "content": { "type": "string" },
                "tags": strs.clone(), "importance": imp.clone(), "ttl_days": { "type": "number", "exclusiveMinimum": 0 },
                "clear_ttl": { "type": "boolean" }, "source_ref": { "type": "string" }, "anchors": anchors.clone(), "used": used.clone(),
                "reason": { "type": "string", "description": "訂正理由（必須）" }
            }), &["id", "reason"]) }),
        json!({ "name": "supersede_context",
            "description": "Replace an old memory, linking and retaining its Raw. reason required.",
            "inputSchema": schema(json!({
                "old_id": { "type": "string" }, "topic": { "type": "string" }, "content": { "type": "string" }, "tags": strs.clone(),
                "importance": imp.clone(), "ttl_days": { "type": "number", "exclusiveMinimum": 0 }, "source_ref": { "type": "string" },
                "anchors": anchors.clone(), "used": used.clone(), "reason": { "type": "string", "description": "なぜ置き換えるのか（必須）" }
            }), &["old_id", "content", "reason"]) }),
        json!({ "name": "retract_context", "description": "Retract without deletion; idempotent. reason required.",
            "inputSchema": schema(json!({ "id": { "type": "string" }, "reason": { "type": "string" } }), &["id", "reason"]) }),
        json!({ "name": "restore_context",
            "description": "Restore a retracted/expired memory with no successor; idempotent. reason required.",
            "inputSchema": schema(json!({ "id": { "type": "string" }, "reason": { "type": "string" } }), &["id", "reason"]) }),
        json!({ "name": "change_importance", "description": "Change importance with a recorded reason.",
            "inputSchema": schema(json!({ "id": { "type": "string" }, "importance": imp.clone(), "reason": { "type": "string" } }), &["id", "importance", "reason"]) }),
        json!({ "name": "set_visibility", "description": "Change hot/warm/cold/archive independently of importance. reason required.",
            "inputSchema": schema(json!({ "id": { "type": "string" }, "visibility": { "type": "string", "enum": VISIBILITY }, "reason": { "type": "string" } }), &["id", "visibility", "reason"]) }),
        json!({ "name": "get_context_history", "description": "Memory event history and replacement links.",
            "inputSchema": schema(json!({ "id": { "type": "string" } }), &["id"]) }),
        json!({ "name": "get_state", "description": "Read exact shared state for CAS. goal.current prefers verified overall goals.",
            "inputSchema": schema(json!({ "key": { "type": "string" } }), &["key"]) }),
        json!({ "name": "set_state",
            "description": "Write shared state. expected_value enables CAS. goal.current remains compatibility state, not a verified goal.",
            "inputSchema": schema(json!({ "key": { "type": "string" }, "value": {}, "expected_value": {} }), &["key", "value"]) }),
        json!({ "name": "list_state", "description": "List/filter state keys with short values and continuation; get_state reads exact values.",
            "inputSchema": schema(json!({ "query": { "type": "string" }, "limit": { "type": "integer", "minimum": 1, "maximum": 500 }, "offset": { "type": "integer", "minimum": 0 }, "token_budget": { "type": "integer", "minimum": 100 } }), &[]) }),
        json!({ "name": "observe_context",
            "description": "Observation cycle; also available through write_context(kind). hypothesis needs a parent UNKNOWN. resolves_unknown requires an observation and its parent.",
            "inputSchema": schema(json!({
                "kind": { "type": "string", "enum": ["observation", "unknown", "hypothesis"] }, "topic": { "type": "string" }, "content": { "type": "string" },
                "parent_id": { "type": "string" }, "source_ref": { "type": "string" }, "resolves_unknown": { "type": "boolean" },
                "tags": strs.clone(), "importance": imp.clone(), "ttl_days": { "type": "number", "exclusiveMinimum": 0 }, "reason": { "type": "string" },
                "anchors": anchors.clone(), "used": used.clone()
            }), &["kind", "content"]) }),
        json!({ "name": "list_unknowns", "description": "Unresolved current UNKNOWNs, default 10 with continuation. include_history and include_resolved are independent.",
            "inputSchema": schema(json!({ "include_resolved": { "type": "boolean" }, "include_history": { "type": "boolean" }, "offset": { "type": "integer", "minimum": 0 }, "token_budget": { "type": "integer", "minimum": 100 }, "limit": { "type": "integer", "minimum": 1, "maximum": 500 } }), &[]) }),
        json!({ "name": "post_goal",
            "description": "Create work card (title), or edit (id). New work selects it; parent defaults to current work (none for root). overall requires a user quote verified in the conversation; otherwise a proposal. Titles over 24 chars are shortened; original kept.",
            "inputSchema": schema(json!({
                "kind": { "type": "string", "enum": ["work", "overall"] },
                "title": { "type": "string", "description": "短い札の名前（24字まで。長ければ丸め、元の名前は根拠に残す）" },
                "parent": { "type": "string", "description": "親の札。省略時は焦点。\"none\"で最上位" },
                "quote": { "type": "string", "description": "根拠になるユーザー発言の引用（原文のまま、4字以上）" },
                "note": { "type": "string", "description": "Agentの判断の根拠（作業目標）" },
                "id": { "type": "string", "description": "編集する札ID" }, "used": used.clone()
            }), &[]) }),
        json!({ "name": "close_goal",
            "description": "Close when finished: done/dropped/merged(into). Posted cards stay until closed. Overall goal needs verified user quote.",
            "inputSchema": schema(json!({
                "id": { "type": "string" }, "state": { "type": "string", "enum": ["done", "dropped", "merged"] },
                "into": { "type": "string" }, "reason": { "type": "string" }, "quote": { "type": "string" }
            }), &["id"]) }),
        json!({ "name": "show_goals",
            "description": "View board or subtree without changing work. enter=true explicitly selects work. trace adds reports; verbose adds full basis. Default ~1200 tokens with UTF-16 offset continuation.",
            "inputSchema": schema(json!({
                "id": { "type": "string", "description": "見る札。省略時は全体。noneは作業対象を解除" },
                "goal": { "type": "string", "description": "idの互換名。同時指定する場合は同じ値" },
                "trace": { "type": "boolean" }, "enter": { "type": "boolean" }, "verbose": { "type": "boolean" }, "revision": { "type": "string", "description": "続きに付く表示版。変更時は先頭へ戻す" },
                "offset": { "type": "integer", "minimum": 0, "description": "表示本文のUTF-16位置。続きの値をそのまま使用" }, "token_budget": { "type": "integer", "minimum": 100 }
            }), &[]) }),
    ];
    json!({ "tools": tools })
}

pub const INSTRUCTIONS: &str = "ANTmemory: この作業空間の共有メモリ。始めに search_context(query)。見覚えのない変更や他エージェントの作業に気づいたときも、関連語やパスで検索すると判断・目的・停止点を辿れる。記録の報告は現物と照合する。残すときは write_context(content) だけでよい（topic・tags・取手は任意。HEADなどの記録時の情報は自動で付く）。詳しく見たい記憶は read_context(id=検索結果のread参照)。参照をそのまま渡すだけで検索語を覚える。通常の記憶IDでも読める。今の現物の場所や変更も、そこで確かめられる。作業目標を掲げるなら post_goal(title, note)、終わったら close_goal(id)。";
