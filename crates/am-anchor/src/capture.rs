//! 記録するときの現物の指紋（§3.5）。思い出すときの「同一 / 変更あり」の判定にも使う。
//!
//! 写しは持たない。持つのは blob（gitと同じ式のsha1）、HEAD、indexからの変更の有無、
//! locatorが指す範囲の指紋だけ。

use std::path::Path;

use am_domain::{Anchor, Capture};
use sha1::Digest as _;

/// gitの blob sha（`git hash-object` と同じ値）。gitの外のファイルにも同じ式を使う。
pub fn git_blob_sha(bytes: &[u8]) -> String {
    let mut h = sha1::Sha1::new();
    h.update(format!("blob {}\0", bytes.len()).as_bytes());
    h.update(bytes);
    hex::encode(h.finalize())
}

/// locator が指す範囲。行の指定（`L43-L75`、`1044`、`55,172`）か、見出しの文字列。
/// 返すのは (先頭の行番号, 範囲の本文)。見つからなければ None（推測で近い所を選ばない）。
pub fn span(text: &str, locator: &str) -> Option<(usize, String)> {
    let lines: Vec<&str> = text.lines().collect();
    let loc = locator.trim();
    let num = |s: &str| s.trim().trim_start_matches(['L', 'l']).parse::<usize>().ok();
    // 行番号の並び（55,172,208）
    if loc.contains(',') && loc.split(',').all(|p| num(p).is_some()) {
        let picked: Vec<(usize, &str)> = loc
            .split(',')
            .filter_map(num)
            .filter(|n| *n >= 1 && *n <= lines.len())
            .map(|n| (n, lines[n - 1]))
            .collect();
        let first = picked.first()?.0;
        return Some((first, picked.iter().map(|(_, l)| *l).collect::<Vec<_>>().join("\n")));
    }
    // 行の範囲（L43-L75、43-75、1044）
    let range = loc.split_once('-').map(|(a, b)| (num(a), num(b))).unwrap_or((num(loc), num(loc)));
    if let (Some(a), Some(b)) = range {
        if a >= 1 && a <= b && a <= lines.len() {
            let b = b.min(lines.len());
            return Some((a, lines[a - 1..b].join("\n")));
        }
        return None;
    }
    // 見出し：locatorの文字列を含むmarkdownの見出しから、同じ以上の階層の次の見出しまで。
    let want = loc.trim_start_matches('#').trim().to_lowercase();
    if want.is_empty() {
        return None;
    }
    let level = |l: &str| {
        let n = l.chars().take_while(|c| *c == '#').count();
        (n > 0 && l[n..].starts_with(' ')).then_some(n)
    };
    let start = lines.iter().position(|l| level(l).is_some() && l.to_lowercase().contains(&want))?;
    let lv = level(lines[start]).expect("heading");
    let end = lines[start + 1..]
        .iter()
        .position(|l| level(l).is_some_and(|n| n <= lv))
        .map(|i| start + 1 + i)
        .unwrap_or(lines.len());
    Some((start + 1, lines[start..end].join("\n")))
}

/// 範囲の指紋。改行と行末の空白の違いでは変わらないようにする。
pub fn span_fingerprint(span_text: &str) -> String {
    use sha2::Digest;
    let norm: String = span_text.lines().map(|l| l.trim_end()).collect::<Vec<_>>().join("\n");
    hex::encode(sha2::Sha256::digest(norm.trim().as_bytes()))
}

pub fn head_commit(root: &Path) -> Option<String> {
    let out = crate::proc::silent("git").arg("-C").arg(root).args(["rev-parse", "HEAD"]).output().ok()?;
    out.status.success().then(|| String::from_utf8_lossy(&out.stdout).trim().to_string())
}

/// 書いたときのrepoの地点（取手が無い記録にも付ける）。gitを1回だけ呼ぶ。
#[derive(Debug, Clone, Default, serde::Serialize)]
pub struct GitEnv {
    pub head: Option<String>,
    pub branch: Option<String>,
    /// 追跡しているファイルに、commitしていない変更がある。
    pub dirty: bool,
    pub changed: usize,
}

pub fn git_env(root: &Path) -> Option<GitEnv> {
    let out = crate::proc::silent("git").arg("-C").arg(root).args(["status", "--porcelain=v2", "--branch", "-uno"]).output().ok()?;
    if !out.status.success() {
        return None;
    }
    let text = String::from_utf8_lossy(&out.stdout);
    let mut env = GitEnv::default();
    for l in text.lines() {
        if let Some(v) = l.strip_prefix("# branch.oid ") {
            env.head = (v != "(initial)").then(|| v.trim().to_string());
        } else if let Some(v) = l.strip_prefix("# branch.head ") {
            env.branch = (v != "(detached)").then(|| v.trim().to_string());
        } else if !l.starts_with('#') && !l.trim().is_empty() {
            env.changed += 1;
        }
    }
    env.dirty = env.changed > 0;
    Some(env)
}

/// 記録するときの指紋を取る。`index_blob` はgitのindexにあるそのパスのblob（無ければ管理外）。
pub fn capture(root: &Path, anchor: &Anchor, head: Option<&str>, index_blob: Option<&str>) -> Option<Capture> {
    let Anchor::File { path, external, locator, .. } = anchor else { return None };
    let full = if *external { std::path::PathBuf::from(path) } else { root.join(path) };
    let bytes = match std::fs::read(&full) {
        Ok(b) => b,
        Err(e) => {
            return Some(Capture { note: Some(format!("記録時に読めなかった: {e}")), commit: head.map(String::from), ..Default::default() })
        }
    };
    let blob = git_blob_sha(&bytes);
    let dirty = index_blob != Some(blob.as_str());
    let span_fp = locator.as_deref().and_then(|loc| {
        let text = String::from_utf8_lossy(&bytes);
        span(&text, loc).map(|(_, s)| span_fingerprint(&s))
    });
    let note = match (locator, &span_fp) {
        (Some(_), None) => Some("locatorの位置をファイルの中に見つけられなかった".into()),
        _ => None,
    };
    Some(Capture { blob: Some(blob), commit: head.map(String::from), dirty, span_fp, note })
}

/// 思い出すときの比較。記録時の指紋と今のファイルを比べる。
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Sameness {
    /// 範囲（無ければファイル全体）が記録時と同じ。
    Same,
    /// ファイルは変わったが、locatorの範囲は同じ。
    SpanSame,
    Changed,
    /// 記録時の指紋が無い（v3から移した取手など）。
    Unknown,
}

pub fn compare(full: &Path, locator: Option<&str>, cap: Option<&Capture>) -> Sameness {
    let Some(cap) = cap else { return Sameness::Unknown };
    let Ok(bytes) = std::fs::read(full) else { return Sameness::Unknown };
    let blob = git_blob_sha(&bytes);
    if cap.blob.as_deref() == Some(blob.as_str()) {
        return Sameness::Same;
    }
    if let (Some(loc), Some(fp)) = (locator, &cap.span_fp) {
        let text = String::from_utf8_lossy(&bytes);
        if span(&text, loc).map(|(_, s)| span_fingerprint(&s)).as_deref() == Some(fp.as_str()) {
            return Sameness::SpanSame;
        }
    }
    if cap.blob.is_none() {
        return Sameness::Unknown;
    }
    Sameness::Changed
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn blob_matches_git() {
        // `printf 'hello\n' | git hash-object --stdin`
        assert_eq!(git_blob_sha(b"hello\n"), "ce013625030ba8dba906f756967f9e9ca394464a");
    }

    #[test]
    fn spans() {
        let t = "# A\nx\n## 保存ログ\ny\nz\n## 次\nw\n# B\n";
        assert_eq!(span(t, "保存ログ").unwrap(), (3, "## 保存ログ\ny\nz".into()));
        assert_eq!(span(t, "L2-L3").unwrap(), (2, "x\n## 保存ログ".into()));
        assert_eq!(span(t, "7").unwrap(), (7, "w".into()));
        assert_eq!(span(t, "2,4").unwrap(), (2, "x\ny".into()));
        assert!(span(t, "無い見出し").is_none());
        assert!(span(t, "L90").is_none());
    }
}
