//! 「役に立ったか」→ 学習tagsの強度（設計案v2 §6）。
//!
//! 読まれたことは数えない。数えるのは cited / followed / anchored_outcome / goal_done と、
//! 否定の rejected / reformulated だけ。初期値（重み・α・半減期）はU11で確かめる仮の値。

use std::collections::HashMap;

use am_domain::UseRecord;
use am_store::query::{self, TagRow};

use crate::ctx::{Level, Session, Workspace};

pub const W_CITED: f64 = 1.0;
pub const W_FOLLOWED: f64 = 0.4;
pub const W_ANCHORED: f64 = 0.6;
pub const W_GOAL_DONE: f64 = 0.2;
pub const W_REJECTED: f64 = -1.0;
pub const W_REFORMULATED: f64 = -0.2;
/// 検索由来タグ：検索から選んで本文を読んだ（思い出す語の実績。有用性とは別に数える）。
pub const W_RECALLED: f64 = 0.3;
pub const RECALL_CAP: f64 = 1.0;
pub const RECALL_ALPHA: f64 = 0.1;
pub const KIND_RECALLED: &str = "recalled";
pub const STRENGTH_CAP: f64 = 3.0;
pub const HALF_LIFE_DAYS: f64 = 90.0;
/// 順位への寄与：α × 一致した語の最大の強度。複合語（盤面・盤面勝率・盤面勝率君）が重なって数が増えても、
/// 寄与が膨らまないよう、和ではなく最大を使う。上限は基礎スコアの上位1件（1.0）より十分小さくする。
pub const ALPHA: f64 = 0.1;
pub const BONUS_CAP: f64 = 0.3;
/// 学習tagsだけで当たった記憶が、通常の表に入れる数。
pub const LEARNED_ONLY_SLOTS: usize = 2;
/// 1回の書込で `used` に数える数。
pub const USED_LIMIT: usize = 5;
/// 読んでから「使った」とみなせる時間の範囲。
pub const READ_WINDOW_HOURS: i64 = 24;

fn days_since(ts: &str) -> f64 {
    chrono::DateTime::parse_from_rfc3339(ts)
        .map(|t| (chrono::Utc::now() - t.with_timezone(&chrono::Utc)).num_seconds() as f64 / 86400.0)
        .unwrap_or(0.0)
        .max(0.0)
}

/// 有用性の強度。会話ごとの数え方はstoreの集計（learned_tags）が持ち、ここでは上限と減衰を掛ける。
pub fn strengths(rows: &[TagRow]) -> HashMap<String, Vec<(String, f64)>> {
    decay(rows, "useful", STRENGTH_CAP)
}

/// 検索由来タグの強度（recalled）。会話ごとに1回、上限は RECALL_CAP。
pub fn recall_strengths(rows: &[TagRow]) -> HashMap<String, Vec<(String, f64)>> {
    decay(rows, "recall", RECALL_CAP)
}

fn decay(rows: &[TagRow], class: &str, cap: f64) -> HashMap<String, Vec<(String, f64)>> {
    let mut out: HashMap<String, Vec<(String, f64)>> = HashMap::new();
    for (mid, term, _, raw, last) in rows.iter().filter(|r| r.2 == class) {
        let Some(last) = last else { continue };
        let v = raw.min(cap) * 0.5f64.powf(days_since(last) / HALF_LIFE_DAYS);
        if v > 0.0 {
            out.entry(mid.clone()).or_default().push((term.clone(), v));
        }
    }
    for v in out.values_mut() {
        v.sort_by(|a, b| b.1.partial_cmp(&a.1).unwrap_or(std::cmp::Ordering::Equal));
    }
    out
}

pub fn recall_bonus(strengths: &[(String, f64)]) -> f64 {
    RECALL_ALPHA * strengths.iter().map(|(_, s)| *s).fold(0.0, f64::max)
}

pub fn bonus(strengths: &[(String, f64)]) -> f64 {
    (ALPHA * strengths.iter().map(|(_, s)| *s).fold(0.0, f64::max)).min(BONUS_CAP)
}

pub fn since(hours: i64) -> String {
    (chrono::Utc::now() - chrono::Duration::hours(hours)).to_rfc3339_opts(chrono::SecondsFormat::Millis, true)
}

/// その会話（無ければ接続）で読んだ記憶 → 読んだときの検索。最近のものを優先する。
pub fn reads_in_scope(ws: &Workspace, s: &Session) -> HashMap<String, Option<String>> {
    let rows = query::reads_in_scope(ws.conn(), Some(&s.conn_id), s.conversation_id().as_deref(), s.gen_seen, &since(READ_WINDOW_HOURS)).unwrap_or_default();
    let mut out = HashMap::new();
    for (mid, sid, _) in rows {
        out.entry(mid).or_insert(sid);
    }
    out
}

/// read_context を、どの検索から開いたかへ結ぶ（§6.2 reads.link）。
pub fn link_read(ws: &Workspace, s: &Session, id: &str, via: Option<&str>) -> (Option<String>, &'static str) {
    if let Some(v) = via {
        // 選択された結果であることをDB側で確認する。別の記憶への語の付替えはしない。
        let valid: bool = ws
            .conn()
            .query_row(
                "SELECT EXISTS(SELECT 1 FROM search_results r JOIN searches s ON s.search_id=r.search_id
              WHERE r.search_id=?1 AND r.memory_id=?2 AND s.origin='v4')",
                rusqlite::params![v, id],
                |r| r.get(0),
            )
            .unwrap_or(false);
        return if valid { (Some(v.to_string()), "explicit") } else { (None, "direct") };
    }
    let recent = query::recent_searches(ws.conn(), Some(&s.conn_id), s.conversation_id().as_deref(), s.gen_seen, &since(1)).unwrap_or_default();
    let hits: Vec<&String> = recent.iter().filter(|(_, _, _, ids)| ids.iter().any(|x| x == id)).map(|(sid, ..)| sid).collect();
    let weak = s.conversation.is_none();
    match hits.as_slice() {
        [] => (None, "direct"),
        [one] => (Some((*one).clone()), if weak { "inferred_conn" } else { "inferred" }),
        [first, ..] => (Some((*first).clone()), "ambiguous"),
    }
}

/// useを1つ作る。検索に結べれば、その検索語を強める。
pub fn make_use(ws: &Workspace, s: &Session, memory_id: &str, kind: &str, weight: f64, search_id: Option<String>) -> UseRecord {
    let terms = search_id.as_deref().map(|sid| query::search_terms(ws.conn(), sid).unwrap_or_default()).unwrap_or_default();
    // 接続をまたいで同じ参照を読み直しても、検索元の一会話（C0なら元接続）の一回分。
    let scope_key = if matches!(kind, KIND_RECALLED | "rejected") {
        search_id
            .as_deref()
            .and_then(|sid| {
                ws.conn()
                    .query_row("SELECT COALESCE(conversation_id, 'conn:' || conn_id) FROM searches WHERE search_id=?1", [sid], |r| r.get::<_, String>(0))
                    .ok()
            })
            .unwrap_or_else(|| s.scope_key())
    } else {
        s.scope_key()
    };
    UseRecord { memory_id: memory_id.into(), kind: kind.into(), weight, search_id, scope_key, terms }
}

pub struct UsedOutcome {
    pub uses: Vec<UseRecord>,
    pub notes: Vec<String>,
}

/// 書込の `used` と、取手の重なり（anchored_outcome）から uses を作る。
pub fn uses_for_write(ws: &Workspace, s: &Session, used: &[String], new_paths: &[String]) -> UsedOutcome {
    let read = reads_in_scope(ws, s);
    let mut uses = vec![];
    let mut notes = vec![];
    let mut cited = std::collections::HashSet::new();
    for (i, id) in used.iter().enumerate() {
        if i >= USED_LIMIT {
            notes.push(format!("{id}（1回の書込で数えるのは{USED_LIMIT}件まで）"));
            continue;
        }
        match read.get(id) {
            Some(sid) => {
                uses.push(make_use(ws, s, id, "cited", W_CITED, sid.clone()));
                cited.insert(id.clone());
                notes.push(format!("{id}(cited)"));
            }
            None => notes.push(format!("{id}（この会話で読んでいないため数えない）")),
        }
    }
    if !new_paths.is_empty() {
        for (mid, sid) in &read {
            if cited.contains(mid) {
                continue;
            }
            let anchors = query::anchors_of(ws.conn(), mid).unwrap_or_default();
            let overlap = anchors.iter().any(|a| match &a.anchor {
                am_domain::Anchor::File { path, external: false, .. } => new_paths.contains(path),
                _ => false,
            });
            if overlap {
                uses.push(make_use(ws, s, mid, "anchored_outcome", W_ANCHORED, sid.clone()));
            }
        }
    }
    UsedOutcome { uses, notes }
}

/// 2つの問いが「言い換え」か。同じ語の組（同じ検索のやり直し）でも、重なりの無い別件でもなく、
/// 語の重なり（Jaccard）が REPHRASE_MIN 以上のときだけ（R5）。
pub const REPHRASE_MIN: f64 = 0.25;

pub fn is_rephrase(old: &[String], new: &[String]) -> bool {
    let a: std::collections::HashSet<&String> = old.iter().collect();
    let b: std::collections::HashSet<&String> = new.iter().collect();
    if a.is_empty() || b.is_empty() || a == b {
        return false;
    }
    let inter = a.intersection(&b).count() as f64;
    let union = a.union(&b).count() as f64;
    inter / union >= REPHRASE_MIN
}

/// 読んだ後に辿らず使わず、2分以内に言い換えた検索をした（C1/C2のときだけ）。
/// 次の検索が別件なら、前に読んだ記憶を否定しない。
pub fn reformulated(ws: &Workspace, s: &Session, level: Level, new_terms: &[String]) -> Vec<UseRecord> {
    if level == Level::C0 || new_terms.is_empty() {
        return vec![];
    }
    let two_min = (chrono::Utc::now() - chrono::Duration::minutes(2)).to_rfc3339_opts(chrono::SecondsFormat::Millis, true);
    let Ok(rows) = query::reads_in_scope(ws.conn(), Some(&s.conn_id), s.conversation_id().as_deref(), s.gen_seen, &two_min) else {
        return vec![];
    };
    let mut out = vec![];
    let scope = s.scope_key();
    for (mid, sid, at) in rows.into_iter().filter(|(_, sid, at)| sid.is_some() && at.as_str() >= two_min.as_str()) {
        let later: i64 = ws
            .conn()
            .query_row(
                "SELECT count(*) FROM uses WHERE memory_id=?1 AND scope_key=?2 AND created_at >= ?3 AND kind != 'recalled'",
                rusqlite::params![mid, scope, at],
                |r| r.get(0),
            )
            .unwrap_or(1);
        let old_terms = sid.as_deref().map(|x| query::search_terms(ws.conn(), x).unwrap_or_default()).unwrap_or_default();
        if later == 0 && is_rephrase(&old_terms, new_terms) {
            out.push(make_use(ws, s, &mid, "reformulated", W_REFORMULATED, sid));
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    fn t(v: &[&str]) -> Vec<String> {
        v.iter().map(|s| s.to_string()).collect()
    }

    #[test]
    fn rephrase_only_when_related() {
        assert!(is_rephrase(&t(&["接地", "振動", "閾値"]), &t(&["接地", "振動", "揺れ"])));
        // 同じ検索のやり直し・別件・空は言い換えではない。
        assert!(!is_rephrase(&t(&["接地", "振動"]), &t(&["振動", "接地"])));
        assert!(!is_rephrase(&t(&["接地", "振動"]), &t(&["大会", "改善"])));
        assert!(!is_rephrase(&t(&["接地"]), &[]));
    }

    #[test]
    fn strength_caps_and_decays() {
        let now = chrono::Utc::now().to_rfc3339();
        let old = (chrono::Utc::now() - chrono::Duration::days(90)).to_rfc3339();
        let row = |m: &str, raw: f64, at: &str, class: &str| (m.to_string(), "接地".to_string(), class.to_string(), raw, Some(at.to_string()));
        let s = strengths(&[row("a", 5.0, &now, "useful"), row("b", 1.0, &old, "useful"), row("c", 1.0, &now, "recall")]);
        assert!((s["a"][0].1 - STRENGTH_CAP).abs() < 1e-6, "上限");
        assert!((s["b"][0].1 - 0.5).abs() < 1e-3, "90日で半分");
        assert!(!s.contains_key("c"), "recall は有用性に入れない");
        assert!(bonus(&s["a"]) <= BONUS_CAP);
        assert!(recall_strengths(&[row("c", 3.0, &now, "recall")])["c"][0].1 <= RECALL_CAP);
    }
}
