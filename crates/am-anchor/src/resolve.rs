//! 取手を現物と照合する（設計案v2 §3.5の手順）。読むだけで、何も書かない。
//!
//! 1. 記録した場所にある
//! 2. 無ければ、最後にそのパスを触ったcommitで改名・移動を辿る
//! 3. 辿れなければ、消える直前の中身（blob）と同じファイルを今の索引から探す
//! 4. それも無ければ、gitの履歴から記録時の版を読めるかを確かめる

use std::collections::HashMap;
use std::path::{Path, PathBuf};

use am_domain::{Anchor, AnchorStatus};

const MAX_RENAME_HOPS: u32 = 5;

pub struct Resolver {
    root: PathBuf,
    git: bool,
    /// 今の索引（HEADの作業ツリーに登録されたファイル）の blob → パス。
    index_by_blob: HashMap<String, Vec<String>>,
    /// パス → blob（今の索引）。
    index_by_path: HashMap<String, String>,
    rename_cache: HashMap<String, Vec<(String, String)>>,
    path_cache: HashMap<String, AnchorStatus>,
}

fn run_git(root: &Path, args: &[&str]) -> Option<String> {
    let out = crate::proc::silent("git").arg("-C").arg(root).args(args).output().ok()?;
    if !out.status.success() {
        return None;
    }
    String::from_utf8(out.stdout).ok()
}

impl Resolver {
    pub fn new(root: &Path) -> Resolver {
        let git = run_git(root, &["rev-parse", "--is-inside-work-tree"]).is_some_and(|s| s.trim() == "true");
        let mut index_by_blob: HashMap<String, Vec<String>> = HashMap::new();
        let mut index_by_path = HashMap::new();
        if git {
            // -z：日本語のパスを引用符やエスケープなしで受け取る。
            if let Some(out) = run_git(root, &["-c", "core.quotepath=false", "ls-files", "-s", "-z"]) {
                for entry in out.split('\0').filter(|e| !e.is_empty()) {
                    // "<mode> <blob> <stage>\t<path>"
                    if let Some((meta, path)) = entry.split_once('\t') {
                        if let Some(blob) = meta.split(' ').nth(1) {
                            index_by_blob.entry(blob.to_string()).or_default().push(path.to_string());
                            index_by_path.insert(path.to_string(), blob.to_string());
                        }
                    }
                }
            }
        }
        Resolver {
            root: root.to_path_buf(),
            git,
            index_by_blob,
            index_by_path,
            rename_cache: HashMap::new(),
            path_cache: HashMap::new(),
        }
    }

    pub fn is_git(&self) -> bool {
        self.git
    }

    pub fn root(&self) -> &Path {
        &self.root
    }

    /// 今のgit indexにある、そのパスのblob。
    pub fn index_blob(&self, path: &str) -> Option<&str> {
        self.index_by_path.get(path).map(|s| s.as_str())
    }

    pub fn resolve(&mut self, anchor: &Anchor) -> AnchorStatus {
        match anchor {
            Anchor::File { path, external: true, .. } => {
                if Path::new(path).exists() {
                    AnchorStatus::Present { blob: None }
                } else {
                    AnchorStatus::Missing { reason: "workspaceの外のパスで、記録した場所に無い".into() }
                }
            }
            Anchor::File { path, external: false, .. } => {
                // resolverの索引は短時間再利用するが、今そこにあるかは毎回見る。
                if self.root.join(path).exists() {
                    return AnchorStatus::Present { blob: self.index_by_path.get(path).cloned() };
                }
                if let Some(s) = self.path_cache.get(path) {
                    let still_valid = match s {
                        AnchorStatus::Present { .. } => false,
                        AnchorStatus::Moved { to, .. } => self.root.join(to).exists(),
                        AnchorStatus::SameContentAt { paths, blob } => paths.iter().all(|p| self.matches_blob(p, blob)),
                        _ => true,
                    };
                    if still_valid { return s.clone(); }
                }
                let s = self.resolve_path(path);
                self.path_cache.insert(path.clone(), s.clone());
                s
            }
            Anchor::Commit { sha, .. } => {
                if !self.git {
                    return AnchorStatus::NotChecked { reason: "workspaceがgit管理外".into() };
                }
                let spec = format!("{sha}^{{commit}}");
                match run_git(&self.root, &["rev-parse", "--verify", "--quiet", &spec]) {
                    Some(full) => AnchorStatus::Present { blob: Some(full.trim().to_string()) },
                    None => AnchorStatus::Missing { reason: "このworkspaceのrepoに、そのcommitが無い".into() },
                }
            }
            Anchor::Conversation { .. } => AnchorStatus::NotChecked { reason: "会話ログの照合はS4で行う".into() },
            Anchor::Url { .. } => AnchorStatus::NotChecked { reason: "URLは取得しない".into() },
            Anchor::Opaque { .. } => AnchorStatus::NotChecked { reason: "取手として解釈できない文字列".into() },
        }
    }

    fn resolve_path(&mut self, path: &str) -> AnchorStatus {
        if self.root.join(path).exists() {
            return AnchorStatus::Present { blob: self.index_by_path.get(path).cloned() };
        }
        if !self.git {
            return AnchorStatus::Missing { reason: "記録した場所に無く、workspaceがgit管理外".into() };
        }

        let mut current = path.to_string();
        let mut hops = 0;
        loop {
            let Some(last) = run_git(&self.root, &["log", "--all", "-n1", "--format=%H", "--", &current])
                .map(|s| s.trim().to_string())
                .filter(|s| !s.is_empty())
            else {
                return if hops == 0 {
                    AnchorStatus::Missing { reason: "記録した場所に無く、gitの履歴にも無い".into() }
                } else {
                    AnchorStatus::Missing { reason: format!("改名を{hops}回辿った先 {current} が履歴に無い") }
                };
            };

            if let Some(to) = self.renamed_in(&last, &current) {
                hops += 1;
                if self.root.join(&to).exists() {
                    return AnchorStatus::Moved { to, via_commit: last, hops };
                }
                if hops >= MAX_RENAME_HOPS {
                    return AnchorStatus::Missing { reason: format!("改名を{hops}回辿っても現物に届かない（最後: {to}）") };
                }
                current = to;
                continue;
            }

            // 改名ではなく削除だった。消える直前の中身を探す。
            let before = format!("{last}^:{current}");
            let at = format!("{last}:{current}");
            let (blob, version_commit) = match run_git(&self.root, &["rev-parse", "--verify", "--quiet", &before]) {
                Some(b) => (b.trim().to_string(), format!("{last}^")),
                None => match run_git(&self.root, &["rev-parse", "--verify", "--quiet", &at]) {
                    Some(b) => (b.trim().to_string(), last.clone()),
                    None => {
                        return AnchorStatus::Missing { reason: format!("{last} の前後どちらにも中身が無い") };
                    }
                },
            };
            if let Some(paths) = self.index_by_blob.get(&blob) {
                let paths: Vec<String> = paths.iter().filter(|p| self.matches_blob(p, &blob)).cloned().collect();
                if !paths.is_empty() { return AnchorStatus::SameContentAt { paths, blob }; }
            }
            return AnchorStatus::RecoverableFromGit { commit: version_commit, path: current };
        }
    }

    fn matches_blob(&self, path: &str, blob: &str) -> bool {
        std::fs::read(self.root.join(path)).is_ok_and(|bytes| crate::capture::git_blob_sha(&bytes) == blob)
    }

    /// そのcommitで `path` が改名されていれば、新しいパスを返す。
    fn renamed_in(&mut self, commit: &str, path: &str) -> Option<String> {
        if !self.rename_cache.contains_key(commit) {
            let out = run_git(
                &self.root,
                &["-c", "core.quotepath=false", "show", "-M", "--name-status", "-z", "--format=", commit],
            )
            .unwrap_or_default();
            // -z では "R100\0old\0new\0" のように並ぶ。
            let mut pairs = Vec::new();
            let mut it = out.split('\0').filter(|s| !s.is_empty());
            while let Some(status) = it.next() {
                if status.starts_with('R') || status.starts_with('C') {
                    if let (Some(old), Some(new)) = (it.next(), it.next()) {
                        if status.starts_with('R') {
                            pairs.push((old.to_string(), new.to_string()));
                        }
                    }
                } else {
                    it.next();
                }
            }
            self.rename_cache.insert(commit.to_string(), pairs);
        }
        self.rename_cache[commit].iter().find(|(old, _)| old == path).map(|(_, new)| new.clone())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::process::Command;

    fn git(dir: &Path, args: &[&str]) {
        let ok = Command::new("git").arg("-C").arg(dir).args(args).status().unwrap().success();
        assert!(ok, "git {args:?}");
    }

    fn repo() -> tempfile::TempDir {
        let d = tempfile::tempdir().unwrap();
        git(d.path(), &["init", "-q"]);
        git(d.path(), &["config", "user.email", "t@example.com"]);
        git(d.path(), &["config", "user.name", "t"]);
        std::fs::create_dir_all(d.path().join("doc")).unwrap();
        std::fs::write(d.path().join("doc/計画.md"), "# 計画\n中身\n").unwrap();
        std::fs::write(d.path().join("doc/消す.md"), "消える中身\n").unwrap();
        std::fs::write(d.path().join("doc/複製元.md"), "同じ中身\n").unwrap();
        git(d.path(), &["add", "."]);
        git(d.path(), &["commit", "-qm", "init"]);
        d
    }

    fn file(p: &str) -> Anchor {
        Anchor::File { path: p.into(), external: false, locator: None, commit: None }
    }

    #[test]
    fn current_location_is_rechecked_while_index_is_cached() {
        let d = tempfile::tempdir().unwrap();
        let mut r = Resolver::new(d.path());
        let p = d.path().join("memo.md");
        assert!(matches!(r.resolve(&file("memo.md")), AnchorStatus::Missing { .. }));
        std::fs::write(&p, "memo").unwrap();
        assert!(matches!(r.resolve(&file("memo.md")), AnchorStatus::Present { .. }));
        std::fs::remove_file(&p).unwrap();
        assert!(matches!(r.resolve(&file("memo.md")), AnchorStatus::Missing { .. }));
    }

    #[test]
    fn present_moved_recoverable() {
        let d = repo();
        std::fs::create_dir_all(d.path().join("archive")).unwrap();
        git(d.path(), &["mv", "doc/計画.md", "archive/計画.md"]);
        git(d.path(), &["rm", "-q", "doc/消す.md"]);
        git(d.path(), &["commit", "-qm", "archive"]);

        let mut r = Resolver::new(d.path());
        assert!(matches!(r.resolve(&file("doc/複製元.md")), AnchorStatus::Present { blob: Some(_) }));
        match r.resolve(&file("doc/計画.md")) {
            AnchorStatus::Moved { to, hops, .. } => {
                assert_eq!(to, "archive/計画.md");
                assert_eq!(hops, 1);
            }
            other => panic!("{other:?}"),
        }
        match r.resolve(&file("doc/消す.md")) {
            AnchorStatus::RecoverableFromGit { path, .. } => assert_eq!(path, "doc/消す.md"),
            other => panic!("{other:?}"),
        }
        assert!(matches!(r.resolve(&file("doc/無い.md")), AnchorStatus::Missing { .. }));
    }

    #[test]
    fn same_content_elsewhere() {
        let d = repo();
        std::fs::write(d.path().join("doc/写し.md"), "同じ中身\n").unwrap();
        git(d.path(), &["add", "."]);
        git(d.path(), &["commit", "-qm", "copy"]);
        // 写しを残して元を消す。rename検出が働かないよう、別のcommitで消す。
        git(d.path(), &["rm", "-q", "doc/複製元.md"]);
        git(d.path(), &["commit", "-qm", "remove original"]);

        let mut r = Resolver::new(d.path());
        match r.resolve(&file("doc/複製元.md")) {
            AnchorStatus::SameContentAt { paths, .. } => assert_eq!(paths, vec!["doc/写し.md".to_string()]),
            other => panic!("{other:?}"),
        }
        // 索引のblobが同じでも、作業ツリーが変わった後は「同じ内容」と断定しない。
        std::fs::write(d.path().join("doc/写し.md"), "書き換えた内容").unwrap();
        assert!(matches!(r.resolve(&file("doc/複製元.md")), AnchorStatus::RecoverableFromGit { .. }));
    }
}
