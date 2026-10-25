//! 検索語の取り出し。v3の query-terms.mjs を移したもの。
//!
//! v3は `Intl.Segmenter('ja', {granularity:'word'})`（V8のICU）で語に分ける。
//! ここではICU4Xの辞書つきの分かち書きを使う。辞書の版が違えば区切りが少し違い得る。

use std::collections::HashSet;
use std::sync::OnceLock;

use icu_segmenter::WordSegmenter;
use unicode_normalization::UnicodeNormalization;

const STOP: &[&str] = &[
    "これ", "それ", "これから", "ここ", "そこ", "どこ", "どう", "なぜ", "する", "した", "して", "してい", "てい", "しない",
    "いる", "ある", "こと", "もの", "ため", "よう", "だけ", "ごと", "とき", "場合", "教え", "教えて", "たい", "必要", "the",
    "a", "an", "is", "are", "was", "were", "of", "to", "in", "for", "and", "or", "what", "how", "does", "do", "did",
    "please",
];

thread_local! {
    // WordSegmenter はthreadを跨いで共有できないので、threadごとに1つ持つ。
    static SEGMENTER: WordSegmenter = WordSegmenter::new_auto();
}

fn id_regex() -> &'static regex::Regex {
    static R: OnceLock<regex::Regex> = OnceLock::new();
    R.get_or_init(|| regex::Regex::new(r"[a-z0-9_]+(?:[._:-][a-z0-9_]+)*").expect("regex"))
}

/// JSの `String.length`（UTF-16の単位数）。
pub fn js_len(s: &str) -> usize {
    s.encode_utf16().count()
}

fn single_hiragana(s: &str) -> bool {
    let mut it = s.chars();
    matches!((it.next(), it.next()), (Some(c), None) if ('\u{3041}'..='\u{309F}').contains(&c))
}

pub fn normalize(q: &str) -> String {
    q.nfkc().collect::<String>().to_lowercase()
}

pub fn query_terms(query: &str) -> Vec<String> {
    let input = normalize(query);
    let stop: HashSet<&str> = STOP.iter().copied().collect();
    let mut out: Vec<String> = vec![];
    let mut seen = HashSet::new();
    let mut add = |s: &str| {
        if js_len(s) >= 2 && !stop.contains(s) && seen.insert(s.to_string()) {
            out.push(s.to_string());
        }
    };
    // コードの識別子やハイフンつきの名前は、そのまま1語として残す。
    for m in id_regex().find_iter(&input) {
        add(m.as_str());
    }
    // 語として扱う区間だけを (開始, 終了) で持つ。
    let mut words: Vec<(usize, usize)> = vec![];
    let mut prev = 0usize;
    SEGMENTER.with(|seg| {
        for (i, wt) in seg.segment_str(&input).iter_with_word_type() {
            // ICU4Xは辞書で分けた日本語の区間に語の種類を付けない（None）。文字か数字を含めば語とする。
            if i > prev && (wt.is_word_like() || input[prev..i].chars().any(char::is_alphanumeric)) {
                words.push((prev, i));
            }
            prev = i;
        }
    });
    for i in 0..words.len() {
        let (start, mut end) = words[i];
        let w = &input[start..end];
        add(w);
        if stop.contains(w) || single_hiragana(w) {
            continue;
        }
        // 隣り合う語だけをつなぐ（助詞や記号を黙って消さない）。第一回・参加者・ハイパー化など。
        for &(ns, ne) in words.iter().take((i + 3).min(words.len())).skip(i + 1) {
            let next = &input[ns..ne];
            if ns != end || stop.contains(next) || single_hiragana(next) {
                break;
            }
            end = ne;
            add(&input[start..end]);
        }
    }
    out.truncate(40);
    out
}

/// FTSのtrigramが引ける語（3字以上）と、部分一致で探す語（2字）に分ける。
pub fn split_long_short(terms: &[String]) -> (Vec<String>, Vec<String>) {
    let long = terms.iter().filter(|t| t.chars().count() >= 3).cloned().collect();
    let short = terms.iter().filter(|t| t.chars().count() == 2).cloned().collect();
    (long, short)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn japanese_and_ids() {
        let t = query_terms("こんにゃく 接地 振動");
        assert!(t.contains(&"こんにゃく".to_string()), "{t:?}");
        assert!(t.contains(&"接地".to_string()), "{t:?}");
        assert!(t.contains(&"振動".to_string()), "{t:?}");
        let t = query_terms("search_context の top_k");
        assert!(t.contains(&"search_context".to_string()), "{t:?}");
        assert!(t.contains(&"top_k".to_string()), "{t:?}");
        // 1字の語・止め語は入らない
        assert!(!t.contains(&"の".to_string()));
        let t = query_terms("Ｇｏａｌ　共有");
        assert!(t.contains(&"goal".to_string()), "{t:?}");
    }
}
