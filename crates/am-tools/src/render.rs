//! 表示の部品。v3の search-output.mjs の見積りと抜粋を移したもの。

use crate::terms::query_terms;

/// tokenの概算。CJKは1字1token、それ以外は4字で1token（v3と同じ式。UTF-16単位で数える）。
pub fn estimate_tokens(text: &str) -> usize {
    let mut cjk = 0usize;
    let mut total = 0usize;
    for c in text.chars() {
        let units = c.len_utf16();
        total += units;
        let u = c as u32;
        if (0x3000..=0x30ff).contains(&u) || (0x3400..=0x4dbf).contains(&u) || (0x4e00..=0x9fff).contains(&u) || (0xf900..=0xfaff).contains(&u) {
            cjk += 1;
        }
    }
    cjk + (total - cjk).div_ceil(4)
}

/// UTF-16の位置で区切った本文。表示の `[start:end]` はv3と同じくUTF-16の位置。
pub struct Utf16Text {
    units: Vec<u16>,
}

impl Utf16Text {
    pub fn new(s: &str) -> Self {
        Utf16Text { units: s.encode_utf16().collect() }
    }
    pub fn len(&self) -> usize {
        self.units.len()
    }
    pub fn is_empty(&self) -> bool {
        self.units.is_empty()
    }
    pub fn slice(&self, a: usize, b: usize) -> String {
        String::from_utf16_lossy(&self.units[a.min(self.len())..b.min(self.len())])
    }
    /// 指定位置を次の文字境界へ。部分読取りでもサロゲートを割らない。
    pub fn boundary(&self, i: usize) -> usize {
        let i = i.min(self.len());
        if self.is_low_surrogate(i) {
            i + 1
        } else {
            i
        }
    }
    fn is_low_surrogate(&self, i: usize) -> bool {
        self.units.get(i).is_some_and(|u| (0xDC00..=0xDFFF).contains(u))
    }
}

pub struct Excerpt {
    pub complete: bool,
    pub ranges: Vec<(usize, usize)>,
}

fn caveat_index(s: &str) -> Option<usize> {
    const WORDS: &[&str] = &["未決定", "未調査", "未確認", "未修正", "未接続", "未対応", "ではない", "混同しない", "禁止", "ただし", "stop", "unknown"];
    let folded = s.to_lowercase();
    WORDS.iter().filter_map(|w| folded.find(w)).min().map(|byte| folded[..byte].encode_utf16().count())
}

/// 問いの語を含む文と、制約（ただし・禁止・未確認…）を含む文を、原文のまま抜き出す。
pub fn query_excerpts(content: &str, query: &str, limit: usize) -> Excerpt {
    let text = Utf16Text::new(content);
    if text.len() <= limit {
        return Excerpt { complete: true, ranges: vec![(0, text.len())] };
    }
    let terms = query_terms(query);
    let is_end = |c: char| matches!(c, '。' | '！' | '？' | '!' | '?' | '\n');
    // 文：終わりの記号以外が続き、その後に終わりの記号が続く区間（v3の正規表現と同じ）。
    struct Sent {
        start: usize,
        end: usize,
        hits: Vec<String>,
        caveat: Option<usize>,
    }
    let mut sents: Vec<Sent> = vec![];
    let chars: Vec<char> = content.chars().collect();
    let mut pos16 = 0usize;
    let mut i = 0usize;
    while i < chars.len() {
        if is_end(chars[i]) {
            pos16 += chars[i].len_utf16();
            i += 1;
            continue;
        }
        let start = pos16;
        let mut s = String::new();
        while i < chars.len() && !is_end(chars[i]) {
            s.push(chars[i]);
            pos16 += chars[i].len_utf16();
            i += 1;
        }
        while i < chars.len() && is_end(chars[i]) {
            s.push(chars[i]);
            pos16 += chars[i].len_utf16();
            i += 1;
        }
        let folded = s.to_lowercase();
        sents.push(Sent { start, end: pos16, hits: terms.iter().filter(|t| folded.contains(t.as_str())).cloned().collect(), caveat: caveat_index(&s) });
    }
    if sents.is_empty() {
        sents.push(Sent { start: 0, end: text.len(), hits: vec![], caveat: None });
    }
    let weight = |s: &Sent| s.hits.iter().map(|t| t.encode_utf16().count().min(8)).sum::<usize>();
    // 同点は先に現れた文（JSの安定sortと同じ）。
    let mut best = 0usize;
    for (k, s) in sents.iter().enumerate() {
        if weight(s) > weight(&sents[best]) {
            best = k;
        }
    }
    let mut constraint: Option<usize> = None;
    for (k, s) in sents.iter().enumerate().filter(|(_, s)| s.caveat.is_some()) {
        constraint = match constraint {
            None => Some(k),
            Some(c) => {
                let (wc, wk) = (weight(&sents[c]), weight(s));
                // 重みが大きい方、同じなら後ろの文。
                if wk > wc || (wk == wc && s.start > sents[c].start) {
                    Some(k)
                } else {
                    Some(c)
                }
            }
        };
    }
    let mut chosen = vec![best];
    if let Some(c) = constraint.filter(|c| *c != best) {
        chosen.push(c);
    }
    // 質問・見出しだけの一致で余った枠には、続く結果も原文のまま入れる。
    // 制約文の枠は残す。半端な48文字を二つ作るより一続きの文脈を優先する。
    let allowance = limit / chosen.len();
    let mut ranges = vec![];
    for &k in &chosen {
        let s = &sents[k];
        let (mut start, mut end) = (s.start, s.end);
        // 見出しの行（「## 未確認」など）だけでは中身が無い。続く1文まで含める。
        if text.slice(start, end).trim_start().starts_with('#') {
            if let Some(next) = sents.get(k + 1) {
                end = next.end;
            }
        }
        let mut next = k + 1;
        while let Some(n) = sents.get(next) {
            if n.start - start >= allowance || chosen.iter().any(|&c| c != k && sents[c].start < n.end && sents[c].start >= start) {
                break;
            }
            end = n.end.min(start + allowance);
            next += 1;
        }
        if end - start > allowance {
            let focus = if Some(k) == constraint {
                s.caveat.unwrap_or(0)
            } else {
                let raw = text.slice(start, end).to_lowercase();
                s.hits.first().and_then(|h| raw.find(h.as_str()).map(|b| raw[..b].encode_utf16().count())).unwrap_or(0)
            };
            start = (start + focus.saturating_sub(allowance / 3)).max(start);
            end = end.min(start + allowance);
        }
        // サロゲートの組を、抜粋の境目で割らない。
        if start > 0 && text.is_low_surrogate(start) {
            start += 1;
        }
        if end < text.len() && text.is_low_surrogate(end) {
            end -= 1;
        }
        ranges.push((start, end));
    }
    ranges.sort();
    Excerpt { complete: false, ranges }
}

/// 強度を ●●○ で表す（最大3.0）。
pub fn dots(strength: f64) -> String {
    let n = ((strength / 3.0) * 3.0).round().clamp(1.0, 3.0) as usize;
    format!("{}{}", "●".repeat(n), "○".repeat(3 - n))
}

/// 保存内容ではなく表示用の短い1行。Rawを変更しない。
pub fn short(text: &str, chars: usize) -> String {
    let mut it = text.chars();
    let mut out: String = it.by_ref().take(chars).map(|c| if c.is_whitespace() { ' ' } else { c }).collect();
    if it.next().is_some() {
        out.push('…');
    }
    out
}

/// 表示テキストのページ。UTF-16 offset を返し、長い1行の根拠も欠落なく辿れる。
pub fn page(text: &str, offset: usize, budget: usize, next_call: impl Fn(usize) -> String) -> (String, bool) {
    let t = Utf16Text::new(text);
    let start = t.boundary(offset);
    let rest = t.slice(start, t.len());
    if estimate_tokens(&rest) <= budget {
        return (rest, start == 0);
    }
    let mut lo = start;
    let mut hi = t.len();
    while lo < hi {
        let mid = lo + (hi - lo).div_ceil(2);
        let end = t.boundary(mid);
        let trial = format!("{}\n続き: {}", t.slice(start, end), next_call(end));
        if estimate_tokens(&trial) <= budget {
            lo = mid;
        } else {
            hi = mid - 1;
        }
    }
    let end = t.boundary(lo);
    if end == start {
        return ("予算不足。token_budgetを増やしてください".into(), false);
    }
    (format!("{}\n続き: {}", t.slice(start, end), next_call(end)), false)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn tokens() {
        assert_eq!(estimate_tokens("abcd"), 1);
        assert_eq!(estimate_tokens("記憶"), 2);
        assert_eq!(estimate_tokens("記憶ab"), 3);
    }

    #[test]
    fn excerpts_pick_hit_and_caveat() {
        let body = format!("{}接地の閾値は0.3にした。{}ただし振動は未確認。{}", "前置き。".repeat(40), "間。".repeat(40), "後ろ。".repeat(40));
        let e = query_excerpts(&body, "接地 閾値", 120);
        assert!(!e.complete);
        let t = Utf16Text::new(&body);
        let parts: Vec<String> = e.ranges.iter().map(|(a, b)| t.slice(*a, *b)).collect();
        assert!(parts.iter().any(|p| p.contains("接地の閾値")), "{parts:?}");
        assert!(parts.iter().any(|p| p.contains("未確認")), "{parts:?}");
    }

    #[test]
    fn question_match_uses_remaining_space_for_the_result() {
        let body = format!("検索時にコンテキストを圧迫しない？測定結果は初回1377、再送1166だった。{}ただし実Usageは未確認。", "説明が続く。".repeat(100));
        let e = query_excerpts(&body, "検索 コンテキスト 圧迫", 200);
        let t = Utf16Text::new(&body);
        let shown = e.ranges.iter().map(|(a, b)| t.slice(*a, *b)).collect::<Vec<_>>().join("\n");
        assert!(shown.contains("1377") && shown.contains("未確認"), "{shown}");
    }
}

#[cfg(test)]
mod heading_tests {
    use super::*;

    #[test]
    fn heading_only_excerpt_takes_the_next_sentence() {
        let body = format!("{}\n## 未確認\n画面のちらつきが消えたかは見ていない。\n{}", "前置き。".repeat(60), "後書き。".repeat(60));
        let e = query_excerpts(&body, "ちらつき", 120);
        let t = Utf16Text::new(&body);
        let shown: Vec<String> = e.ranges.iter().map(|(a, b)| t.slice(*a, *b)).collect();
        assert!(shown.iter().all(|s| s.trim() != "## 未確認"), "{shown:?}");
    }
}
