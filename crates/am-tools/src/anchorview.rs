//! 取手を、思い出すときに再観測して見せる（§3.5の表）。記録された移動先を事実として扱わない。

use am_anchor::capture::{compare, span, Sameness};
use am_domain::{Anchor, AnchorStatus, Capture, NewAnchor};
use am_store::query::StoredAnchor;

use crate::ctx::Workspace;

pub struct Seen {
    pub label: String,
    pub segment: String,
    pub origin: String,
    /// 今の該当箇所（locatorがあり、読めたとき）。(パス, 先頭の行, 本文)
    pub excerpt_from: Option<(String, Option<String>)>,
    pub status: Option<AnchorStatus>,
}

fn short(sha: &str) -> &str {
    &sha[..sha.len().min(7)]
}

/// 旧parserが空白で切った取手は、原文から実在するファイルを見直す。
/// 保存済みの取手・指紋は書き換えない。旧パスが実在するか指紋があるなら、その証拠を優先する。
fn reparsed_if_uncaptured<'a>(ws: &Workspace, a: &'a StoredAnchor) -> std::borrow::Cow<'a, StoredAnchor> {
    let original = std::borrow::Cow::Borrowed(a);
    let Some(root) = &ws.root else { return original };
    let Anchor::File { path, external, .. } = &a.anchor else { return original };
    if a.capture.as_ref().is_some_and(|c| c.blob.is_some()) {
        return original;
    }
    let full = |p: &str, external: bool| if external { std::path::PathBuf::from(p) } else { root.join(p) };
    if full(path, *external).is_file() {
        return original;
    }
    let parsed = am_anchor::parse::parse(&a.segment, root);
    if parsed != a.anchor && matches!(&parsed, Anchor::File { path, external, .. } if full(path, *external).is_file()) {
        let mut updated = a.clone();
        updated.anchor = parsed;
        updated.capture = None; // 記録時の内容が同一だったとは推定しない。
        std::borrow::Cow::Owned(updated)
    } else {
        original
    }
}

/// 1つの取手を今の現物と照合する。
pub fn observe(ws: &mut Workspace, a: &StoredAnchor) -> Seen {
    let effective = reparsed_if_uncaptured(ws, a);
    let a = effective.as_ref();
    let root = ws.root.clone();
    let seg = a.segment.clone();
    let origin = a.origin.clone();
    let mk = |label: String, excerpt_from, status| Seen { label, segment: seg.clone(), origin: origin.clone(), excerpt_from, status };
    match &a.anchor {
        Anchor::File { path, external, locator, .. } => {
            let Some(resolver) = ws.resolver() else {
                return mk("[未照合: workspace rootが分からない]".into(), None, None);
            };
            let status = resolver.resolve(&a.anchor);
            let full_of = |p: &str| if *external { std::path::PathBuf::from(p) } else { root.clone().unwrap_or_default().join(p) };
            let label = match &status {
                AnchorStatus::Present { .. } => {
                    let same = compare(&full_of(path), locator.as_deref(), a.capture.as_ref());
                    match same {
                        Sameness::Same => "[同一]".to_string(),
                        Sameness::SpanSame => "[範囲は同一・ファイルは変更あり]".to_string(),
                        Sameness::Changed => {
                            let at = a.capture.as_ref().and_then(|c| c.commit.as_deref()).map(|c| format!(" 記録時 @{}", short(c))).unwrap_or_default();
                            format!("[変更あり{at}]")
                        }
                        Sameness::Unknown => {
                            if *external { "[現物あり・workspace外]".into() } else { "[現物あり]".into() }
                        }
                    }
                }
                AnchorStatus::Moved { to, .. } => format!("[移動 → {to}]"),
                AnchorStatus::SameContentAt { paths, .. } => format!("[同じ内容がここに: {}]", paths.join(", ")),
                AnchorStatus::RecoverableFromGit { commit, path } => {
                    format!("[現物なし → git show {}:{}]", short(commit), path)
                }
                AnchorStatus::Missing { .. } => {
                    if ws.resolver().is_some_and(|r| r.is_git()) { "[現物なし]".into() } else { "[現物なし・旧版なし]".into() }
                }
                AnchorStatus::NotChecked { reason } => format!("[未照合: {reason}]"),
            };
            let excerpt_path = match &status {
                AnchorStatus::Present { .. } => Some(path.clone()),
                AnchorStatus::Moved { to, .. } => Some(to.clone()),
                _ => None,
            };
            let dirty = a.capture.as_ref().is_some_and(|c| c.dirty && c.blob.is_some());
            let label = if dirty && !label.starts_with("[同一") { format!("{label}（記録時はcommit前の変更を含んでいた）") } else { label };
            mk(label, excerpt_path.map(|p| (p, locator.clone())), Some(status))
        }
        Anchor::Commit { .. } => {
            let status = ws.resolver().map(|r| r.resolve(&a.anchor));
            let label = match &status {
                Some(AnchorStatus::Present { .. }) => "[commitあり]".to_string(),
                Some(AnchorStatus::Missing { .. }) => "[commitが見つからない]".to_string(),
                Some(AnchorStatus::NotChecked { reason }) => format!("[未照合: {reason}]"),
                _ => "[未照合]".to_string(),
            };
            mk(label, None, status)
        }
        Anchor::Conversation { .. } => mk("[会話]".into(), None, None),
        Anchor::Url { .. } => mk("[URL・取得しない]".into(), None, None),
        Anchor::Opaque { .. } => mk("[解釈できない取手]".into(), None, None),
    }
}

/// 今の該当箇所の抜粋。`limit` 字まで。
pub fn excerpt(ws: &Workspace, path: &str, locator: Option<&str>, limit: usize) -> Option<String> {
    let full = if std::path::Path::new(path).is_absolute() { path.into() } else { ws.root.as_ref()?.join(path) };
    let text = std::fs::read_to_string(full).ok()?;
    let (line, body) = span(&text, locator?)?;
    let cut: String = body.chars().take(limit).collect();
    let more = if body.chars().count() > limit { "…" } else { "" };
    Some(format!("L{line}〜: {cut}{more}"))
}

/// 1行の取手の表示（検索の表で使う）。
pub fn line(ws: &mut Workspace, anchors: &[StoredAnchor], max: usize) -> Option<String> {
    if anchors.is_empty() {
        return None;
    }
    let mut parts = vec![];
    for a in anchors.iter().take(max) {
        let seen = observe(ws, a);
        let tag = if a.origin == "touched" { "（編集）" } else { "" };
        parts.push(format!("{} {}{tag}", seen.label, a.segment));
    }
    if anchors.len() > max {
        parts.push(format!("ほか{}件（read_context）", anchors.len() - max));
    }
    Some(parts.join(" · "))
}

/// 普段の検索に出す短い取手：今どこにあるかだけ。git の追跡の詳細は read_context で見る。
/// ファイルでない取手（commit・会話・URL）は、ここでは出さない。
pub fn short_line(ws: &mut Workspace, anchors: &[StoredAnchor], max: usize) -> Option<String> {
    let mut parts: Vec<String> = vec![];
    // source_ref から付いた取手と、明示した取手が同じファイルを指すことがある。同じ場所は1回だけ。
    let mut seen = std::collections::HashSet::new();
    let effective: Vec<_> = anchors.iter().map(|a| reparsed_if_uncaptured(ws, a)).collect();
    let files: Vec<&StoredAnchor> = effective
        .iter()
        .map(|a| a.as_ref())
        .filter(|a| matches!(&a.anchor, Anchor::File { path, locator, .. } if seen.insert((path.clone(), locator.clone()))))
        .collect();
    for a in &files {
        if parts.len() >= max {
            break;
        }
        let Anchor::File { path, locator, .. } = &a.anchor else { continue };
        let loc = locator.as_deref().map(|l| format!("#{l}")).unwrap_or_default();
        // 普段は場所だけ。本文の指紋・範囲比較は read_context / verbose で行う。
        let status = ws.resolver().map(|r| r.resolve(&a.anchor));
        let s = match &status {
            Some(AnchorStatus::Moved { to, .. }) => format!("{to}{loc}（移動先）"),
            Some(AnchorStatus::SameContentAt { paths, .. }) => format!("{}{loc}（同じ内容）", paths.first().cloned().unwrap_or_default()),
            Some(AnchorStatus::RecoverableFromGit { .. }) | Some(AnchorStatus::Missing { .. }) => format!("{path}{loc}（今は無い）"),
            None | Some(AnchorStatus::NotChecked { .. }) => format!("{path}{loc}（場所は未照合）"),
            _ => format!("{path}{loc}"),
        };
        parts.push(s);
    }
    if parts.is_empty() {
        return None;
    }
    if files.len() > parts.len() {
        parts.push(format!("ほか{}件", files.len() - parts.len()));
    }
    Some(parts.join(" · "))
}

/// 書込で受け取った取手の文字列を解釈し、指紋を取る。
pub fn prepare(ws: &mut Workspace, raws: &[String], origin: &str, head: Option<&str>) -> Vec<NewAnchor> {
    let root = ws.root.clone();
    let parse_root = root.clone().unwrap_or_default();
    let mut out = vec![];
    for raw in raws.iter().map(|r| r.trim()).filter(|r| !r.is_empty()) {
        for (segment, anchor) in am_anchor::parse::parse_all(raw, &parse_root) {
            let capture: Option<Capture> = match (&root, &anchor) {
                (Some(r), Anchor::File { path, .. }) => {
                    let index_blob = ws.resolver().and_then(|res| res.index_blob(path).map(String::from));
                    am_anchor::capture::capture(r, &anchor, head, index_blob.as_deref())
                }
                _ => None,
            };
            out.push(NewAnchor { raw: raw.to_string(), segment, anchor, capture, origin: origin.to_string() });
        }
    }
    out
}

pub fn file_paths(anchors: &[NewAnchor]) -> Vec<String> {
    anchors
        .iter()
        .filter_map(|a| match &a.anchor {
            Anchor::File { path, external: false, .. } => Some(path.clone()),
            _ => None,
        })
        .collect()
}
