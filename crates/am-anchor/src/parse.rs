//! v3の `source_ref` は自由な文字列なので、形から取手の種類を読む。
//! 読めないものは `Opaque` として元の文字列のまま持つ（推測で埋めない）。

use std::path::Path;

use am_domain::Anchor;

fn is_hex_sha(s: &str) -> bool {
    (7..=40).contains(&s.len()) && s.chars().all(|c| c.is_ascii_hexdigit())
}

fn first_sha(s: &str) -> Option<String> {
    s.split(|c: char| !c.is_ascii_alphanumeric())
        .find(|w| is_hex_sha(w) && w.chars().any(|c| c.is_ascii_digit()))
        .map(|w| w.to_ascii_lowercase())
}

fn percent_decode(s: &str) -> String {
    let bytes = s.as_bytes();
    let mut out = Vec::with_capacity(bytes.len());
    let mut i = 0;
    while i < bytes.len() {
        if bytes[i] == b'%' && i + 2 < bytes.len() {
            // % の次が日本語でもUTF-8の途中をstrで切らない。壊れたescapeは原文のまま残す。
            if let (Some(hi), Some(lo)) = ((bytes[i + 1] as char).to_digit(16), (bytes[i + 2] as char).to_digit(16)) {
                out.push((hi * 16 + lo) as u8);
                i += 3;
                continue;
            }
        }
        out.push(bytes[i]);
        i += 1;
    }
    String::from_utf8(out).unwrap_or_else(|_| s.to_string())
}

fn is_windows_abs(s: &str) -> bool {
    let b = s.as_bytes();
    b.len() >= 3 && b[0].is_ascii_alphabetic() && b[1] == b':' && (b[2] == b'/' || b[2] == b'\\')
}

fn looks_like_path(s: &str) -> bool {
    if s.is_empty() || s.contains("://") {
        return false;
    }
    if is_windows_abs(s) {
        return true;
    }
    let has_sep = s.contains('/') || s.contains('\\');
    let has_ext = has_file_extension(s);
    // 拡張子を持つファイル名なら、空白の数で切り捨てない。
    has_ext || (has_sep && s.split_whitespace().nth(3).is_none())
}

fn has_file_extension(s: &str) -> bool {
    Path::new(s).extension().is_some_and(|e| {
        let e = e.to_string_lossy();
        !e.is_empty() && e.len() <= 6 && e.chars().all(|c| c.is_ascii_alphanumeric())
    })
}

/// パスと位置を分ける。`#見出し`、`#L10-L20`、`:L10`、` (注記)`、`@sha` を位置として読む。
fn split_locator(s: &str) -> (String, Option<String>, Option<String>) {
    let mut path = s.to_string();
    let mut locator: Option<String> = None;
    let mut commit: Option<String> = None;

    if let Some(i) = path.rfind(" (").filter(|_| path.ends_with(')')) {
        locator = Some(path[i + 1..].trim().to_string());
        path.truncate(i);
    }
    if let Some(i) = path.rfind('@') {
        let tail = path[i + 1..].trim();
        if is_hex_sha(tail) {
            commit = Some(tail.to_ascii_lowercase());
            path.truncate(i);
        }
    }
    if let Some(i) = path.find('#') {
        let tail = path[i + 1..].trim().to_string();
        path.truncate(i);
        if is_hex_sha(&tail) {
            commit = Some(tail.to_ascii_lowercase());
        } else if !tail.is_empty() {
            locator = Some(match locator {
                Some(l) => format!("{tail} {l}"),
                None => tail,
            });
        }
    }
    if let Some(i) = path.rfind(":L") {
        if path[i + 2..].chars().next().is_some_and(|c| c.is_ascii_digit()) {
            locator = Some(path[i + 1..].to_string());
            path.truncate(i);
        }
    }
    // `file.py:1044`、`a.md:55,172`、`x.py:655-671` の行番号。ドライブの `F:` は対象外（先頭2文字より後だけ見る）。
    if let Some(i) = path.rfind(':').filter(|&i| i > 1) {
        let tail = &path[i + 1..];
        if !tail.is_empty() && tail.chars().all(|c| c.is_ascii_digit() || c == ',' || c == '-') {
            locator = Some(match locator {
                Some(l) => format!("{tail} {l}"),
                None => tail.to_string(),
            });
            path.truncate(i);
        }
    }
    (path.trim().trim_end_matches(['/', '\\']).to_string(), locator, commit)
}

fn normalize(p: &str) -> String {
    p.replace('\\', "/")
}

/// `root` 配下なら相対パスにする。Windowsでは大文字小文字を区別しない。
fn relativize(abs: &str, root: &Path) -> Option<String> {
    let a = normalize(abs);
    let r = normalize(&root.to_string_lossy()).trim_end_matches('/').to_string();
    let (al, rl) = (a.to_lowercase(), r.to_lowercase());
    if al == rl {
        return Some(String::new());
    }
    if al.starts_with(&(rl.clone() + "/")) {
        return Some(a[r.len() + 1..].to_string());
    }
    None
}

/// 1つの文字列に `;` 区切りで複数の取手が書かれていることがある（実データで多い）。
/// 区切った各部分と、その解釈を返す。URLは区切らない。
pub fn parse_all(raw: &str, workspace_root: &Path) -> Vec<(String, Anchor)> {
    let s = raw.trim();
    let lower = s.to_lowercase();
    if lower.starts_with("http://") || lower.starts_with("https://") || !(s.contains(';') || s.contains('；')) {
        return vec![(s.to_string(), parse(s, workspace_root))];
    }
    s.split([';', '；'])
        .map(str::trim)
        .filter(|p| !p.is_empty())
        .map(|p| (p.to_string(), parse(p, workspace_root)))
        .collect()
}

/// 平文のパスの後ろに付いた注記（`: 説明`、` 3.3`、` §2`）を位置として切り分ける。
/// パス中の空白は保つ。注記の明示記号か、ファイル名が終わった後の空白でだけ分ける。
fn split_plain_annotation(s: &str) -> (String, Option<String>) {
    let spaces: Vec<usize> = s.char_indices().filter_map(|(i, c)| c.is_whitespace().then_some(i)).collect();
    for &i in &spaces {
        let head = s[..i].trim_end();
        let rest = s[i..].trim();
        let marked = head.ends_with([':', '：', ',', '、']);
        let head = head.trim_end_matches([':', '：', ',', '、']);
        let (path, _, _) = split_locator(head);
        let section = !rest.is_empty() && rest.chars().all(|c| c.is_ascii_digit() || c == '.') && rest.chars().any(|c| c.is_ascii_digit());
        if has_file_extension(&path) && (marked || section || rest.starts_with('§')) {
            return (head.to_string(), Some(rest.to_string()));
        }
    }
    // #見出しや:L行はsplit_locatorが扱う。空白を含む見出しもここでは分断しない。
    let (path, _, _) = split_locator(s);
    if has_file_extension(&path) {
        return (s.to_string(), None);
    }
    // 旧記録の「file.md 説明」も読む。右から探し、パス内の空白を先に切らない。
    for &i in spaces.iter().rev() {
        let head = s[..i].trim_end();
        let (path, _, _) = split_locator(head);
        if has_file_extension(&path) {
            return (head.to_string(), Some(s[i..].trim().to_string()));
        }
    }
    (s.to_string(), None)
}

pub fn parse(raw: &str, workspace_root: &Path) -> Anchor {
    let s = raw.trim();
    let lower = s.to_lowercase();

    if lower.starts_with("http://") || lower.starts_with("https://") {
        return Anchor::Url { url: s.to_string() };
    }
    if ["conversation", "thread", "user-question", "user question"].iter().any(|p| lower.starts_with(p)) {
        return Anchor::Conversation { raw: s.to_string() };
    }
    if lower.starts_with("commit") || lower.starts_with("git") {
        if let Some(sha) = first_sha(s) {
            return Anchor::Commit { sha, repo: None };
        }
        return Anchor::Opaque { raw: s.to_string() };
    }

    let (body, note) = if lower.starts_with("file:///") {
        (percent_decode(&s[8..]), None)
    } else if lower.starts_with("file://") {
        (percent_decode(&s[7..]), None)
    } else if lower.starts_with("file:") {
        split_plain_annotation(&s[5..])
    } else {
        split_plain_annotation(s)
    };
    let (path, locator, mut commit) = split_locator(&body);
    // 注記の先頭が `@sha` なら、それはcommit（例: `path#L43-L75 @1c7a94a`）。
    let note = match note {
        Some(n) if n.starts_with('@') => {
            let (head, rest) = n[1..].split_once(char::is_whitespace).unwrap_or((&n[1..], ""));
            if is_hex_sha(head) && commit.is_none() {
                commit = Some(head.to_ascii_lowercase());
                Some(rest.trim().to_string()).filter(|r| !r.is_empty())
            } else {
                Some(n)
            }
        }
        other => other,
    };
    let locator = match (locator, note) {
        (Some(l), Some(n)) => Some(format!("{l} {n}")),
        (l, n) => l.or(n),
    };
    if !looks_like_path(&path) {
        return Anchor::Opaque { raw: s.to_string() };
    }
    let path = normalize(&path);
    if is_windows_abs(&path) {
        return match relativize(&path, workspace_root) {
            Some(rel) => Anchor::File { path: rel, external: false, locator, commit },
            None => Anchor::File { path, external: true, locator, commit },
        };
    }
    Anchor::File { path: path.trim_start_matches("./").to_string(), external: false, locator, commit }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn root() -> &'static Path {
        Path::new("F:/ANTIGRABTY/デュエマしようぜ！")
    }

    #[test]
    fn relative_and_absolute_paths() {
        assert_eq!(
            parse("doc/作業所/CPU作成計画！/00_現在地と逆算.md", root()),
            Anchor::File { path: "doc/作業所/CPU作成計画！/00_現在地と逆算.md".into(), external: false, locator: None, commit: None }
        );
        assert_eq!(
            parse("f:\\ANTIGRABTY\\デュエマしようぜ！\\doc\\a.md#見出し", root()),
            Anchor::File { path: "doc/a.md".into(), external: false, locator: Some("見出し".into()), commit: None }
        );
        assert_eq!(
            parse("file:///F:/ANTIGRABTY/ANTmemory/logs/launcher.log (2026-10-23T21:14Z)", root()),
            Anchor::File {
                path: "F:/ANTIGRABTY/ANTmemory/logs/launcher.log".into(),
                external: true,
                locator: Some("(2026-10-23T21:14Z)".into()),
                commit: None
            }
        );
    }

    #[test]
    fn repo_with_commit_and_lines() {
        assert_eq!(
            parse("F:/悪の根源/テルメールゲームブック#6fb099b", Path::new("F:/悪の根源/テルメールゲームブック")),
            Anchor::File { path: "".into(), external: false, locator: None, commit: Some("6fb099b".into()) }
        );
        assert_eq!(
            parse("server/lib/register-v2-tools.mjs#L43-L75 @1c7a94a", root()),
            Anchor::File {
                path: "server/lib/register-v2-tools.mjs".into(),
                external: false,
                locator: Some("L43-L75".into()),
                commit: Some("1c7a94a".into())
            }
        );
    }

    #[test]
    fn several_refs_and_annotations() {
        let parts = parse_all(
            "F:/ANTIGRABTY/デュエマしようぜ！/doc/current/GUIDE.md; workbench.bat; doc/a.md#Q1",
            root(),
        );
        let paths: Vec<_> = parts
            .iter()
            .map(|(_, a)| match a {
                Anchor::File { path, .. } => path.clone(),
                other => panic!("{other:?}"),
            })
            .collect();
        assert_eq!(paths, vec!["doc/current/GUIDE.md", "workbench.bat", "doc/a.md"]);

        assert_eq!(
            parse("doc/作業所/03_逐次観測ログ.md: 2026-10-01 最新ユーザー指定", root()),
            Anchor::File {
                path: "doc/作業所/03_逐次観測ログ.md".into(),
                external: false,
                locator: Some("2026-10-01 最新ユーザー指定".into()),
                commit: None
            }
        );
        assert_eq!(
            parse("doc/作業所/計画/00_現在地と逆算.md 3.3", root()),
            Anchor::File { path: "doc/作業所/計画/00_現在地と逆算.md".into(), external: false, locator: Some("3.3".into()), commit: None }
        );
        assert_eq!(
            parse("Duel_Masters_Forge/receiver/runtime.py:1044", root()),
            Anchor::File { path: "Duel_Masters_Forge/receiver/runtime.py".into(), external: false, locator: Some("1044".into()), commit: None }
        );
        assert_eq!(
            parse("doc/a.md:55,172,208", root()),
            Anchor::File { path: "doc/a.md".into(), external: false, locator: Some("55,172,208".into()), commit: None }
        );
        assert_eq!(
            parse("file:F:/ANTIGRABTY/デュエマしようぜ！/doc/b.md", root()),
            Anchor::File { path: "doc/b.md".into(), external: false, locator: None, commit: None }
        );
        assert_eq!(
            parse("file:///F:/x/%E4%BA%BA%E6%B0%97%20MCP.md", root()),
            Anchor::File { path: "F:/x/人気 MCP.md".into(), external: true, locator: None, commit: None }
        );
    }

    #[test]
    fn non_files_stay_as_they_are() {
        assert!(matches!(parse("conversation 2026-10-24 ユーザー発言", root()), Anchor::Conversation { .. }));
        assert_eq!(parse("commit 57a2e65c", root()), Anchor::Commit { sha: "57a2e65c".into(), repo: None });
        assert!(matches!(parse("working tree after the third flip and more", root()), Anchor::Opaque { .. }));
        assert!(matches!(parse("https://github.com/x/y", root()), Anchor::Url { .. }));
    }

    #[test]
    fn spaces_belong_to_the_path_until_an_annotation() {
        for raw in [
            "doc/作業 記録.md",
            "doc/a folder with many words/作業 記録.md",
            "doc/plan (draft).md",
            "doc/old.md backup.md",
        ] {
            assert_eq!(parse(raw, root()), Anchor::File { path: raw.into(), external: false, locator: None, commit: None });
        }
        for (raw, loc) in [
            ("doc/作業 記録.md: 説明", "説明"),
            ("doc/作業 記録.md 3.3", "3.3"),
            ("doc/作業 記録.md §2", "§2"),
            ("doc/作業 記録.md 詳しい説明", "詳しい説明"),
            ("doc/作業 記録.md#保存した 記録", "保存した 記録"),
            ("doc/作業 記録.md:L2-L3", "L2-L3"),
        ] {
            assert_eq!(
                parse(raw, root()),
                Anchor::File { path: "doc/作業 記録.md".into(), external: false, locator: Some(loc.into()), commit: None },
                "{raw}"
            );
        }
        assert_eq!(
            parse("file:F:/ANTIGRABTY/デュエマしようぜ！/doc/作業 記録.md", root()),
            Anchor::File { path: "doc/作業 記録.md".into(), external: false, locator: None, commit: None }
        );
    }

    #[test]
    fn percent_decoding_preserves_invalid_utf8_escapes_without_panicking() {
        for name in ["100%完成.md", "100%2完成.md", "100%.md", "100%GG.md", "絵%🧠.md"] {
            assert_eq!(
                parse(&format!("file:///F:/x/{name}"), root()),
                Anchor::File { path: format!("F:/x/{name}"), external: true, locator: None, commit: None }
            );
        }
        assert_eq!(percent_decode("100%25%E5%AE%8C%E6%88%90.md"), "100%完成.md");
        assert_eq!(percent_decode("%FF"), "%FF");
    }
}
