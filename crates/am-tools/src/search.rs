//! 検索の順位。v3の native-search.mjs と retrieval-core.mjs の式を移し、学習tagsを足す（§6.5）。

use std::collections::HashMap;

use am_store::query::{self, Filter};

use crate::ctx::Workspace;
use crate::terms::{query_terms, split_long_short};
use crate::useful;

#[derive(Debug, Clone)]
pub struct SearchArgs {
    pub query: String,
    pub filter: Filter,
    pub top_k: usize,
    pub min_score: f64,
    pub mode: String,
}

#[derive(Debug, Clone)]
pub struct Hit {
    pub id: String,
    /// 候補内の相対値（最良=1.0、小数2桁）。v3の relative_score と同じ意味。
    pub score: f64,
    pub reasons: Vec<String>,
    pub learned: Vec<(String, f64)>,
    /// 検索由来タグ（この語の検索から選んで読まれた実績）。
    pub recall: Vec<(String, f64)>,
}

#[derive(Debug, Default)]
pub struct Semantic {
    pub used: bool,
    pub pending: i64,
    pub note: Option<String>,
}

pub struct Ranked {
    pub hits: Vec<Hit>,
    pub terms: Vec<String>,
    pub semantic: Semantic,
}

fn round2(x: f64) -> f64 {
    (x * 100.0).round() / 100.0
}

pub fn search(ws: &Workspace, a: &SearchArgs) -> am_store::Result<Ranked> {
    let terms = query_terms(&a.query);
    let mut semantic = Semantic::default();
    if a.query.trim().is_empty() {
        return Ok(Ranked { hits: vec![], terms, semantic });
    }
    let conn = ws.conn();

    // 語彙の候補（v3の searchFTS）。
    let (long, short) = split_long_short(&terms);
    let mut fts = query::fts_candidates(conn, &long, &short, &a.filter)?;
    let population = query::population(conn)? as f64;
    let mut short_hits: HashMap<&str, usize> = HashMap::new();
    for row in &fts {
        for t in &short {
            if row.body_folded.contains(t.as_str()) {
                *short_hits.entry(t.as_str()).or_default() += 1;
            }
        }
    }
    for row in fts.iter_mut() {
        let title = row.topic.to_lowercase();
        let matched_long: Vec<&String> = long.iter().filter(|t| row.body_folded.contains(t.as_str())).collect();
        for t in &short {
            if !row.body_folded.contains(t.as_str()) || matched_long.iter().any(|l| l.contains(t.as_str())) {
                continue;
            }
            let n = *short_hits.get(t.as_str()).unwrap_or(&0) as f64;
            let idf = (1.0 + (population - n + 0.5) / (n + 0.5)).ln();
            row.score += idf * if title.contains(t.as_str()) { 2.0 } else { 1.0 };
        }
    }
    fts.sort_by(|x, y| y.score.partial_cmp(&x.score).unwrap_or(std::cmp::Ordering::Equal).then(x.id.cmp(&y.id)));

    // 意味の候補。queryのvectorが作れなければBM25だけで行い、そう表示する。
    let mut qvec: Option<Vec<f32>> = None;
    if a.mode != "bm25" {
        match &ws.embedder {
            Some(e) => match e.embed_query(&a.query) {
                Ok(v) => qvec = Some(v),
                Err(err) => semantic.note = Some(format!("queryのvectorを作れない: {err}")),
            },
            None => semantic.note = Some(ws.embed_note.clone().unwrap_or_else(|| "embeddingのモデルが無い".into())),
        }
    }
    semantic.pending = query::count_missing_embeddings(conn, &ws.model_id).unwrap_or(0);
    let mut vec_hits: Vec<(String, f64)> = vec![];
    if let Some(q) = &qvec {
        semantic.used = true;
        for (id, blob) in query::vectors(conn, &ws.model_id, &a.filter)? {
            let v = am_embed::from_blob(&blob);
            if v.len() != q.len() {
                continue;
            }
            let s = am_embed::dot(q, &v) as f64;
            if s > 0.0 {
                vec_hits.push((id, s));
            }
        }
        vec_hits.sort_by(|x, y| y.1.partial_cmp(&x.1).unwrap_or(std::cmp::Ordering::Equal));
    }

    let effective = if qvec.is_some() { a.mode.as_str() } else { "bm25" };
    let mut fused: Vec<(String, f64, Vec<String>)> = match effective {
        "semantic" => vec_hits.iter().map(|(id, s)| (id.clone(), *s, vec!["semantic".to_string()])).collect(),
        "bm25" => fts.iter().map(|r| (r.id.clone(), r.score, vec!["bm25".to_string()])).collect(),
        _ => {
            // 語彙の強さで上位を決め、意味は上限つきの補助にする（v3の hybrid と同じ式）。
            let vs: HashMap<&str, f64> = vec_hits.iter().map(|(id, s)| (id.as_str(), *s)).collect();
            let best_lex = fts.iter().map(|r| r.score).fold(0.0f64, f64::max);
            let best_sem = vec_hits.iter().map(|(_, s)| *s).fold(0.0f64, f64::max);
            let mut m: Vec<(String, f64, Vec<String>)> = vec![];
            let mut seen = std::collections::HashSet::new();
            if best_lex > 0.0 {
                for r in &fts {
                    let cos = vs.get(r.id.as_str()).copied().unwrap_or(0.0).max(0.0);
                    let support = if best_sem > 0.0 { cos / best_sem } else { 0.0 };
                    let score = (r.score / best_lex).max(0.8 * support) + 0.1 * support;
                    let why = if cos > 0.0 { "bm25+semantic" } else { "bm25" };
                    m.push((r.id.clone(), score, vec![why.to_string()]));
                    seen.insert(r.id.clone());
                }
                for (id, s) in &vec_hits {
                    if !seen.contains(id) && *s >= 0.45 {
                        m.push((id.clone(), 0.9 * s, vec!["semantic".to_string()]));
                    }
                }
            } else {
                for (id, s) in &vec_hits {
                    if *s >= 0.45 {
                        m.push((id.clone(), *s, vec!["semantic".to_string()]));
                    }
                }
            }
            m.sort_by(|x, y| y.1.partial_cmp(&x.1).unwrap_or(std::cmp::Ordering::Equal));
            m
        }
    };

    // 正規化（最良を1.0にした相対値）と足切り（retrieval-core の normalizeScores）。
    let raw: Vec<f64> = fused.iter().map(|x| if x.1.is_finite() { x.1 } else { 0.0 }).collect();
    let flip = !raw.is_empty() && raw.iter().all(|v| *v <= 0.0) && raw.iter().any(|v| *v < 0.0);
    let rel: Vec<f64> = raw.iter().map(|v| if flip { -v } else { *v }).collect();
    let max = rel.iter().copied().fold(f64::MIN, f64::max);
    let mut base: Vec<Hit> = if fused.is_empty() || max <= 0.0 {
        if a.min_score > 0.0 {
            vec![]
        } else {
            fused.drain(..).map(|(id, _, reasons)| Hit { id, score: 0.0, reasons, learned: vec![], recall: vec![] }).collect()
        }
    } else {
        fused
            .drain(..)
            .zip(rel)
            .map(|((id, _, reasons), r)| Hit { id, score: round2(r / max), reasons, learned: vec![], recall: vec![] })
            .filter(|h| h.score >= a.min_score)
            .collect()
    };

    // 学習tags（§6.5）と検索由来タグ。どちらも順位に少し足し、本文で当たらない記憶も2件までは候補に入れる。
    // 検索由来タグは「この語で探して選ばれた」実績で、tagsを書かなくても思い出せるようにする。
    let tags = query::learned_tags(conn, &terms)?;
    let strengths = useful::strengths(&tags);
    let recalls = useful::recall_strengths(&tags);
    let in_base: std::collections::HashSet<String> = base.iter().map(|h| h.id.clone()).collect();
    for h in base.iter_mut() {
        if let Some(s) = strengths.get(&h.id) {
            h.learned = s.clone();
        }
        if let Some(r) = recalls.get(&h.id) {
            h.recall = r.clone();
        }
    }
    let key = |h: &Hit| h.score + useful::bonus(&h.learned) + useful::recall_bonus(&h.recall);
    let mut extra: Vec<Hit> = vec![];
    let candidates: std::collections::HashSet<String> = strengths.keys().chain(recalls.keys()).cloned().collect();
    if !candidates.is_empty() {
        let allowed: std::collections::HashSet<String> = query::eligible_ids(conn, &a.filter, candidates.iter().cloned().collect())?.into_iter().collect();
        for id in candidates.iter().filter(|id| !in_base.contains(*id) && allowed.contains(*id)) {
            let learned = strengths.get(id).cloned().unwrap_or_default();
            let recall = recalls.get(id).cloned().unwrap_or_default();
            let mut reasons: Vec<String> = learned.iter().map(|(t, _)| format!("learned_tag:{t}")).collect();
            reasons.extend(recall.iter().map(|(t, _)| format!("recalled_tag:{t}")));
            extra.push(Hit { id: id.clone(), score: 0.0, reasons, learned, recall });
        }
        extra.sort_by(|x, y| key(y).partial_cmp(&key(x)).unwrap_or(std::cmp::Ordering::Equal).then(x.id.cmp(&y.id)));
        extra.truncate(useful::LEARNED_ONLY_SLOTS);
    }
    base.extend(extra);
    // 明示された解決関係だけを順位へ反映する。日付や似た文章から勝手に旧版と確定しない。
    let resolved: std::collections::HashSet<String> = {
        let mut st = conn.prepare("SELECT DISTINCT dst FROM relations WHERE kind='resolves'")?;
        let rows = st.query_map([], |r| r.get(0))?;
        rows.collect::<Result<_, _>>()?
    };
    let rank = |h: &Hit| key(h) - if resolved.contains(&h.id) { 0.15 } else { 0.0 };
    base.sort_by(|x, y| rank(y).partial_cmp(&rank(x)).unwrap_or(std::cmp::Ordering::Equal).then(x.id.cmp(&y.id)));
    base.truncate(a.top_k);
    Ok(Ranked { hits: base, terms, semantic })
}
