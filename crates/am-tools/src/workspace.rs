//! workspaceの解決。v3のランチャー（bin/ant-memory.mjs）と同じ規則で、同じregistryを使う。
//!
//!   1. ANT_MEMORY_WORKSPACE（絶対パス or 名前）
//!   2. 作業フォルダの .antmemory-workspace（別の作業場所から同じ記憶を使う参照）
//!   3. cwdから上へ辿って目印（.git / CLAUDE.md など）を探す
//!   4. 環境変数のヒント（IDEから起動された場合）
//!   5. Antigravity IDE の workspaceStorage
//!   6. どれも無ければ _shared

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use sha1::Digest as _;

const MARKERS: &[&str] = &[".git", "CLAUDE.md", "AGENTS.md", "GEMINI.md", ".agents", ".claude", "package.json", "pyproject.toml", "Cargo.toml", "go.mod"];
const ENV_HINTS: &[&str] =
    &["VSCODE_WORKSPACE_FOLDER", "WORKSPACE_FOLDER", "WORKSPACE_ROOT", "PROJECT_ROOT", "ANTIGRAVITY_WORKSPACE", "GEMINI_WORKSPACE", "INIT_CWD", "PWD"];

#[derive(Debug, Clone, serde::Serialize, serde::Deserialize, PartialEq, Eq)]
pub struct Resolved {
    pub slug: String,
    pub root: Option<PathBuf>,
    pub source: String,
}

pub const WORKSPACE_LINK: &str = ".antmemory-workspace";
pub const FALLBACK_NOTICE: &str = "接続先: _shared（作業先未特定）。.antmemory-workspace で参照先を指定できます。";

/// 明示されたローカル参照だけを読む。他repoや親のユーザーフォルダの設定を推測で流用しない。
pub fn local_binding(layout: &Layout, cwd: &Path) -> Result<Option<Resolved>, String> {
    for dir in cwd.ancestors() {
        if deny_roots(layout).contains(&norm(dir)) {
            break;
        }
        let link = dir.join(WORKSPACE_LINK);
        if link.exists() {
            let text = std::fs::read_to_string(&link).map_err(|e| format!("{} を読めません: {e}", link.display()))?;
            let value = text.trim().trim_start_matches('\u{feff}').trim();
            if value.is_empty() || value.lines().count() != 1 {
                return Err(format!("{} に参照先フォルダを1行で指定してください", link.display()));
            }
            let target = Path::new(value);
            let target = if target.is_absolute() { target.to_path_buf() } else { dir.join(target) };
            let target = std::fs::canonicalize(&target).map_err(|e| format!("{} の参照先 {} を開けません: {e}", link.display(), target.display()))?;
            let target = plain_canonical_path(&target);
            if !target.is_dir() || deny_roots(layout).contains(&norm(&target)) {
                return Err(format!("{} の参照先は作業フォルダを指定してください", link.display()));
            }
            return Ok(Some(Resolved { slug: slug_for(layout, &target), root: Some(target), source: format!("binding:{}", link.display()) }));
        }
        if MARKERS.iter().any(|m| dir.join(m).exists()) {
            break;
        }
    }
    Ok(None)
}

fn plain_canonical_path(path: &Path) -> PathBuf {
    let text = path.display().to_string();
    if let Some(unc) = text.strip_prefix(r"\\?\UNC\") {
        return PathBuf::from(format!(r"\\{unc}"));
    }
    PathBuf::from(text.strip_prefix(r"\\?\").unwrap_or(&text))
}

/// 古いshimのfallback接続も、daemon側で新しいローカル参照を読み直せる。
pub fn refresh_binding(layout: &Layout, cwd: &Path, resolved: &Resolved) -> Result<Resolved, String> {
    if !resolved.source.starts_with("env:ANT_MEMORY_WORKSPACE(") {
        return Ok(local_binding(layout, cwd)?.unwrap_or_else(|| {
            if resolved.source.starts_with("binding:") {
                try_candidate(layout, &cwd.display().to_string(), "cwd")
                    .unwrap_or_else(|| Resolved { slug: "_shared".into(), root: None, source: format!("fallback(cwd={})", cwd.display()) })
            } else {
                resolved.clone()
            }
        }));
    }
    Ok(resolved.clone())
}

pub struct Layout {
    /// ANTmemoryのrepo（stores/、runtime/ を持つ）。
    pub antmemory_root: PathBuf,
}

impl Layout {
    pub fn stores(&self) -> PathBuf {
        self.antmemory_root.join("stores")
    }
    pub fn registry(&self) -> PathBuf {
        std::env::var_os("ANT_MEMORY_REGISTRY").map(PathBuf::from).unwrap_or_else(|| self.antmemory_root.join("runtime/workspaces.json"))
    }
    pub fn store_dir(&self, slug: &str) -> PathBuf {
        self.stores().join(slug)
    }
}

fn norm(p: &Path) -> String {
    let s = p.display().to_string();
    s.trim_end_matches(['\\', '/']).to_lowercase().replace('/', "\\")
}

pub fn sanitize(name: &str) -> String {
    let s: String = name.chars().map(|c| if "<>:\"/\\|?*".contains(c) || (c as u32) < 0x20 { '_' } else { c }).collect();
    let lead = s.chars().take_while(|c| *c == '.').count();
    let s = if lead > 0 { format!("_{}", &s[lead..]) } else { s };
    let s = s.trim_end_matches(['.', ' ']).trim().to_string();
    if s.is_empty() {
        "workspace".into()
    } else {
        s
    }
}

fn deny_roots(layout: &Layout) -> Vec<String> {
    let mut v: Vec<PathBuf> = vec![
        layout.stores(),
        layout.antmemory_root.join("server"),
        layout.antmemory_root.join("legacy"),
        layout.antmemory_root.join("runtime"),
        layout.antmemory_root.join("logs"),
    ];
    for k in ["USERPROFILE", "APPDATA", "LOCALAPPDATA", "ProgramFiles", "ProgramFiles(x86)", "ProgramData", "windir"] {
        if let Some(p) = std::env::var_os(k) {
            v.push(PathBuf::from(p));
        }
    }
    v.iter().map(|p| norm(p)).collect()
}

fn find_root(start: &Path) -> Option<(PathBuf, &'static str)> {
    let mut dir = Some(start.to_path_buf());
    while let Some(d) = dir {
        for m in MARKERS {
            if d.join(m).exists() {
                return Some((d, m));
            }
        }
        dir = d.parent().map(Path::to_path_buf);
    }
    None
}

fn short_hash(p: &Path) -> String {
    hex::encode(sha1::Sha1::digest(norm(p).as_bytes()))[..8].to_string()
}

/// registryの対応（slug → 絶対パス）を引く。無ければ作って記録する（v3と同じファイル）。
///
/// 読む→空いているslugを選ぶ→書く、の全体をプロセス間でlockする。lockが無いと、同じ名前の別フォルダが
/// 同時に初めてつながったとき、どちらも古い対応を読んで同じslugを選び、別のworkspaceが同じDBへ入る（独立レビュー I1）。
pub fn slug_for(layout: &Layout, abs: &Path) -> String {
    let reg_path = layout.registry();
    if let Some(dir) = reg_path.parent() {
        std::fs::create_dir_all(dir).ok();
    }
    let lock = std::fs::OpenOptions::new().create(true).truncate(false).write(true).open(reg_path.with_extension("json.lock"));
    let _guard = match &lock {
        Ok(f) => match f.lock() {
            Ok(()) => Some(f),
            Err(e) => {
                eprintln!("[antmemory] registryのlockを取れない（lockなしで続ける）: {e}");
                None
            }
        },
        Err(e) => {
            eprintln!("[antmemory] registryのlockファイルを開けない（lockなしで続ける）: {e}");
            None
        }
    };
    let mut reg: BTreeMap<String, String> = std::fs::read_to_string(&reg_path).ok().and_then(|t| serde_json::from_str(&t).ok()).unwrap_or_default();
    for (slug, recorded) in &reg {
        if norm(Path::new(recorded)) == norm(abs) {
            return slug.clone();
        }
    }
    let base = sanitize(&abs.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default());
    let slug = if reg.contains_key(&base) { format!("{base}-{}", short_hash(abs)) } else { base };
    reg.insert(slug.clone(), abs.display().to_string());
    if let Some(dir) = reg_path.parent() {
        if let Err(e) = std::fs::create_dir_all(dir) {
            eprintln!("[antmemory] registryのディレクトリを作れない: {e}");
        }
    }
    if let Ok(text) = serde_json::to_string_pretty(&reg) {
        // 記録に失敗しても、slugは決まっているので動作は続ける（v3と同じ）。理由は出す。
        // 別のshimが同時に書いても壊れないよう、一時ファイルに書いてから置き換える。
        let tmp = reg_path.with_extension(format!("json.{}.tmp", std::process::id()));
        if let Err(e) = std::fs::write(&tmp, text + "\n").and_then(|_| std::fs::rename(&tmp, &reg_path)) {
            eprintln!("[antmemory] registryに書けない: {e}");
        }
    }
    if let Some(f) = _guard {
        f.unlock().ok();
    }
    slug
}

/// 2つのrootが同じフォルダか（大文字小文字・区切り・末尾の区切りを無視）。
pub fn same_root(a: &Path, b: &Path) -> bool {
    norm(a) == norm(b)
}

pub fn lookup_root(layout: &Layout, slug: &str) -> Option<PathBuf> {
    let reg: BTreeMap<String, String> = std::fs::read_to_string(layout.registry()).ok().and_then(|t| serde_json::from_str(&t).ok()).unwrap_or_default();
    reg.get(slug).map(PathBuf::from)
}

fn try_candidate(layout: &Layout, dir: &str, label: &str) -> Option<Resolved> {
    let d = dir.trim();
    if d.is_empty() {
        return None;
    }
    let p = Path::new(d);
    if !p.exists() {
        return None;
    }
    let (root, marker) = find_root(p)?;
    if deny_roots(layout).contains(&norm(&root)) {
        return None;
    }
    Some(Resolved { slug: slug_for(layout, &root), root: Some(root), source: format!("{label}:{marker}") })
}

/// Antigravity IDE：開いているウィンドウのフォルダを workspaceStorage から探す（v3と同じ規則）。
fn antigravity_workspace() -> Option<(PathBuf, String)> {
    std::env::var_os("ANTIGRAVITY_EDITOR_APP_ROOT")?;
    let mut bases = vec![];
    if let Some(cache) = std::env::var_os("VSCODE_CODE_CACHE_PATH") {
        if let Some(prod) = Path::new(&cache).parent().and_then(|p| p.parent()) {
            bases.push(prod.join("User/workspaceStorage"));
        }
    }
    if let Some(app) = std::env::var_os("APPDATA") {
        bases.push(Path::new(&app).join("Antigravity IDE/User/workspaceStorage"));
    }
    let mut cands: Vec<(PathBuf, String, std::time::SystemTime)> = vec![];
    for b in bases {
        let Ok(rd) = std::fs::read_dir(&b) else { continue };
        for e in rd.flatten() {
            let ws = e.path().join("workspace.json");
            let Some(folder) = std::fs::read_to_string(&ws)
                .ok()
                .and_then(|t| serde_json::from_str::<serde_json::Value>(&t).ok())
                .and_then(|v| v.get("folder").and_then(|f| f.as_str()).map(String::from))
            else {
                continue;
            };
            let Some(dir) = file_url_to_path(&folder) else { continue };
            if !dir.exists() {
                continue;
            }
            let mtime = [e.path(), e.path().join("state.vscdb")]
                .iter()
                .filter_map(|p| std::fs::metadata(p).and_then(|m| m.modified()).ok())
                .max()
                .unwrap_or(std::time::UNIX_EPOCH);
            cands.push((dir, folder, mtime));
        }
    }
    if cands.is_empty() {
        return None;
    }
    if cands.len() > 1 {
        if let Some((id, how)) = antigravity_workspace_id() {
            let fid = id.to_lowercase();
            let re = regex::Regex::new("[^A-Za-z0-9]+").expect("regex");
            for (dir, folder, _) in &cands {
                let h = hex::encode(sha2::Sha256::digest(folder.as_bytes()));
                let slug = re.replace_all(folder, "_").to_lowercase();
                if h == fid || slug == fid {
                    return Some((dir.clone(), how));
                }
            }
        }
    }
    let best = cands.into_iter().max_by_key(|c| c.2)?;
    Some((best.0, "recent".into()))
}

fn antigravity_workspace_id() -> Option<(String, String)> {
    if !cfg!(windows) {
        return None;
    }
    let script = format!(
        "$ErrorActionPreference = 'SilentlyContinue'\n$id = {}\nfor ($i = 0; $i -lt 6; $i++) {{\n  $p = Get-CimInstance Win32_Process -Filter \"ProcessId=$id\"\n  if (-not $p) {{ break }}\n  if ($p.CommandLine -match '--workspace_id[\\s=]+([0-9a-zA-Z_]+)') {{ Write-Output \"CHAIN $($matches[1])\" }}\n  if ($p.ParentProcessId -eq $id) {{ break }}\n  $id = $p.ParentProcessId\n}}\nGet-CimInstance Win32_Process -Filter \"Name LIKE 'language_server%'\" | ForEach-Object {{\n  if ($_.CommandLine -match '--workspace_id[\\s=]+([0-9a-zA-Z_]+)') {{ Write-Output \"LSP $($matches[1])\" }}\n}}",
        std::process::id()
    );
    let out =
        am_anchor::proc::silent("powershell.exe").args(["-NoProfile", "-NonInteractive", "-ExecutionPolicy", "Bypass", "-Command", &script]).output().ok()?;
    let text = String::from_utf8_lossy(&out.stdout);
    let mut chain = vec![];
    let mut lsp = std::collections::BTreeSet::new();
    for l in text.lines() {
        if let Some(x) = l.strip_prefix("CHAIN ") {
            chain.push(x.trim().to_string());
        } else if let Some(x) = l.strip_prefix("LSP ") {
            lsp.insert(x.trim().to_lowercase());
        }
    }
    if let Some(c) = chain.first() {
        return Some((c.clone(), "workspace_id".into()));
    }
    if lsp.len() == 1 {
        return Some((lsp.into_iter().next()?, "workspace_id(単一ウィンドウ)".into()));
    }
    None
}

pub(crate) fn file_url_to_path(url: &str) -> Option<PathBuf> {
    let rest = url.strip_prefix("file:///")?;
    let mut bytes = vec![];
    let b = rest.as_bytes();
    let mut i = 0;
    while i < b.len() {
        if b[i] == b'%' && i + 2 < b.len() {
            bytes.push(u8::from_str_radix(std::str::from_utf8(&b[i + 1..i + 3]).ok()?, 16).ok()?);
            i += 3;
        } else {
            bytes.push(b[i]);
            i += 1;
        }
    }
    Some(PathBuf::from(String::from_utf8(bytes).ok()?.replace('/', "\\")))
}

/// 接続してきたclient。ANT_MEMORY_CLIENT が無ければ、clientが自分で置く環境変数から推す
/// （設定に書き忘れても、記録の書き手が unknown にならないように）。
pub fn infer_client(env: &dyn Fn(&str) -> Option<String>, names: &[String]) -> String {
    if let Some(c) = env("ANT_MEMORY_CLIENT").filter(|c| !c.trim().is_empty()) {
        return c;
    }
    if env("CLAUDECODE").is_some() || env("CLAUDE_CODE_SESSION_ID").is_some() || env("CLAUDE_PROJECT_DIR").is_some() {
        return "claude-code".into();
    }
    if env("ANTIGRAVITY_EDITOR_APP_ROOT").is_some() || env("ANTIGRAVITY_AGENTAPI_EXE").is_some() {
        return "antigravity".into();
    }
    if names.iter().any(|n| n.to_uppercase().starts_with("CODEX_")) {
        return "codex".into();
    }
    "unknown".into()
}

pub fn resolve(layout: &Layout, cwd: &Path, env: &dyn Fn(&str) -> Option<String>) -> Result<Resolved, String> {
    if let Some(v) = env("ANT_MEMORY_WORKSPACE").map(|v| v.trim().to_string()).filter(|v| !v.is_empty()) {
        let p = Path::new(&v);
        if p.is_absolute() {
            return Ok(Resolved { slug: slug_for(layout, p), root: Some(p.to_path_buf()), source: "env:ANT_MEMORY_WORKSPACE(path)".into() });
        }
        return Ok(Resolved { slug: sanitize(&v), root: None, source: "env:ANT_MEMORY_WORKSPACE(name)".into() });
    }
    if let Some(r) = local_binding(layout, cwd)? {
        return Ok(r);
    }
    if let Some(r) = try_candidate(layout, &cwd.display().to_string(), "cwd") {
        return Ok(r);
    }
    for k in ENV_HINTS {
        if let Some(v) = env(k) {
            if let Some(r) = try_candidate(layout, &v, &format!("env:{k}")) {
                return Ok(r);
            }
        }
    }
    if let Some((dir, how)) = antigravity_workspace() {
        if let Some(r) = try_candidate(layout, &dir.display().to_string(), &format!("antigravity:{how}")) {
            return Ok(r);
        }
    }
    Ok(Resolved { slug: "_shared".into(), root: None, source: format!("fallback(cwd={})", cwd.display()) })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn sanitize_like_v3() {
        assert_eq!(sanitize("a:b"), "a_b");
        assert_eq!(sanitize("..x"), "_x");
        assert_eq!(sanitize("x. "), "x");
        assert_eq!(sanitize(""), "workspace");
        assert_eq!(file_url_to_path("file:///f%3A/ANTIGRABTY/%E3%83%9F").unwrap(), PathBuf::from("f:\\ANTIGRABTY\\ミ"));
    }

    #[test]
    fn local_link_binds_scratch_and_respects_explicit_workspace_and_repo_boundaries() {
        let d = tempfile::tempdir().unwrap();
        let layout = Layout { antmemory_root: d.path().join("deploy") };
        let target = d.path().join("actual-project");
        let scratch = d.path().join("scratch");
        let child = scratch.join("nested");
        std::fs::create_dir_all(&target).unwrap();
        std::fs::create_dir_all(&child).unwrap();
        std::fs::write(target.join("Cargo.toml"), "").unwrap();
        std::fs::write(scratch.join(WORKSPACE_LINK), "../actual-project\n").unwrap();
        let bound = resolve(&layout, &child, &|_| None).unwrap();
        assert_eq!(bound.slug, "actual-project");
        // Windows CI may expose TEMP through an 8.3 alias; bindings return its canonical path.
        let canonical_target = plain_canonical_path(&std::fs::canonicalize(&target).unwrap());
        assert!(same_root(bound.root.as_ref().unwrap(), &canonical_target));
        let explicit = resolve(&layout, &child, &|k| (k == "ANT_MEMORY_WORKSPACE").then(|| "chosen".into())).unwrap();
        assert_eq!(explicit.slug, "chosen");
        let own_repo = child.join("independent");
        std::fs::create_dir_all(&own_repo).unwrap();
        std::fs::write(own_repo.join("Cargo.toml"), "").unwrap();
        assert_eq!(resolve(&layout, &own_repo, &|_| None).unwrap().slug, "independent");
        std::fs::write(scratch.join(WORKSPACE_LINK), "../missing-project").unwrap();
        assert!(resolve(&layout, &child, &|_| None).unwrap_err().contains("参照先"));
    }

    #[test]
    fn old_fallback_hello_is_rebound_but_explicit_shared_is_not() {
        let d = tempfile::tempdir().unwrap();
        let layout = Layout { antmemory_root: d.path().join("deploy") };
        let target = d.path().join("project");
        let scratch = d.path().join("scratch");
        std::fs::create_dir_all(&target).unwrap();
        std::fs::create_dir_all(&scratch).unwrap();
        let old = Resolved { slug: "_shared".into(), root: None, source: "fallback(cwd=scratch)".into() };
        assert_eq!(refresh_binding(&layout, &scratch, &old).unwrap().slug, "_shared");
        std::fs::write(scratch.join(WORKSPACE_LINK), target.display().to_string()).unwrap();
        let bound = refresh_binding(&layout, &scratch, &old).unwrap();
        assert_eq!(bound.slug, "project");
        let ancestor = Resolved { slug: "drive-root".into(), root: Some(d.path().into()), source: "cwd:.agents".into() };
        assert_eq!(refresh_binding(&layout, &scratch, &ancestor).unwrap().slug, "project");
        let explicit = Resolved { source: "env:ANT_MEMORY_WORKSPACE(name)".into(), ..old.clone() };
        assert_eq!(refresh_binding(&layout, &scratch, &explicit).unwrap(), explicit);
        std::fs::remove_file(scratch.join(WORKSPACE_LINK)).unwrap();
        assert_eq!(refresh_binding(&layout, &scratch, &bound).unwrap().slug, "_shared");
    }

    #[test]
    fn canonical_network_path_keeps_the_network_root() {
        let root = plain_canonical_path(Path::new(r"\\?\UNC\server\share\project"));
        assert_eq!(root, PathBuf::from(r"\\server\share\project"));
        assert_eq!(plain_canonical_path(Path::new(r"\\?\F:\project")), PathBuf::from(r"F:\project"));
    }

    #[test]
    fn removing_a_link_in_a_repo_restores_that_repos_own_workspace() {
        let d = tempfile::tempdir().unwrap();
        let layout = Layout { antmemory_root: d.path().join("deploy") };
        let own = d.path().join("own-project");
        let target = d.path().join("other-project");
        std::fs::create_dir_all(&own).unwrap();
        std::fs::create_dir_all(&target).unwrap();
        std::fs::write(own.join("Cargo.toml"), "").unwrap();
        std::fs::write(own.join(WORKSPACE_LINK), target.display().to_string()).unwrap();
        let linked = resolve(&layout, &own, &|_| None).unwrap();
        assert_eq!(linked.slug, "other-project");
        std::fs::remove_file(own.join(WORKSPACE_LINK)).unwrap();
        let current = refresh_binding(&layout, &own, &linked).unwrap();
        assert_eq!(current.slug, "own-project");
        assert!(same_root(current.root.as_ref().unwrap(), &own));
    }
}

#[cfg(test)]
mod race_tests {
    use super::*;

    /// 同じ名前の別フォルダが同時に初めて登録されても、slugは全て別になる（独立レビュー I1）。
    #[test]
    fn concurrent_first_registration_gets_distinct_slugs() {
        let d = tempfile::tempdir().unwrap();
        let root = d.path().join("deploy");
        std::fs::create_dir_all(root.join("runtime")).unwrap();
        let dirs: Vec<PathBuf> = (0..8).map(|i| d.path().join(format!("p{i}")).join("same-name")).collect();
        for p in &dirs {
            std::fs::create_dir_all(p).unwrap();
        }
        let slugs: Vec<String> = std::thread::scope(|sc| {
            let hs: Vec<_> = dirs
                .iter()
                .map(|p| {
                    let root = root.clone();
                    sc.spawn(move || slug_for(&Layout { antmemory_root: root }, p))
                })
                .collect();
            hs.into_iter().map(|h| h.join().unwrap()).collect()
        });
        let distinct: std::collections::HashSet<&String> = slugs.iter().collect();
        assert_eq!(distinct.len(), 8, "{slugs:?}");
        let reg: BTreeMap<String, String> = serde_json::from_str(&std::fs::read_to_string(root.join("runtime/workspaces.json")).unwrap()).unwrap();
        assert_eq!(reg.len(), 8, "{reg:?}");
    }
}
