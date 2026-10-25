//! 本番切替の同期点（§10.2）と、切戻し（§10.4）・点検（doctor）・配置（install）。
//!
//! v3のプロセスを止める・clientの設定を変えるなど、戻しにくい操作は、
//! 明示のフラグが無ければ一覧を出すだけで止まる。

#![allow(clippy::type_complexity)] // 書き戻しは、表の列の組をそのまま読む。

use std::fs;
use std::path::{Path, PathBuf};

use am_store::{query, Store};
use am_tools::workspace::Layout;
use anyhow::{bail, Context};
use rusqlite::params;
use serde_json::json;

use crate::ipc;
use crate::migrate::stamp;

/// 動いているv3のプロセス（ランチャーかサーバー本体を引数に持つnode）。
pub fn v3_processes(layout: &Layout) -> anyhow::Result<Vec<(u32, String)>> {
    if !cfg!(windows) {
        bail!("このOSのv3プロセス列挙は未対応。停止済みとは判断しない");
    }
    let script = "$ErrorActionPreference='Stop'; [Console]::OutputEncoding=[System.Text.UTF8Encoding]::new($false); $p=@(Get-CimInstance Win32_Process -ErrorAction Stop | Where-Object { $_.Name -match '^node(\\.exe)?$' -and $_.CommandLine -match 'ant-memory\\.mjs|native-server\\.mjs' } | Select-Object ProcessId,CommandLine); ConvertTo-Json -InputObject $p -Compress";
    enumerate_with(layout, am_anchor::proc::silent("powershell.exe").args(["-NoProfile", "-NonInteractive", "-Command", script]))
}

fn enumerate_with(layout: &Layout, command: &mut std::process::Command) -> anyhow::Result<Vec<(u32, String)>> {
    let out = command.output().context("v3プロセス調査を起動できない。停止済みとは判断しない")?;
    decode_processes(layout, out.status.success(), &out.stdout, &out.stderr)
}

fn decode_processes(layout: &Layout, success: bool, stdout: &[u8], stderr: &[u8]) -> anyhow::Result<Vec<(u32, String)>> {
    if !success {
        bail!("v3プロセス調査に失敗: {}", String::from_utf8_lossy(stderr));
    }
    #[derive(serde::Deserialize)]
    #[serde(rename_all = "PascalCase")]
    struct Process {
        process_id: u32,
        command_line: String,
    }
    let rows: Vec<Process> = serde_json::from_slice(stdout).context("v3プロセス調査の出力が不正。停止済みとは判断しない")?;
    let root = layout.antmemory_root.display().to_string().to_lowercase().replace('/', "\\");
    let root = root.trim_end_matches('\\');
    let paths = [
        format!("{root}\\bin\\ant-memory.mjs"),
        format!("{root}\\server\\native-server.mjs"),
        format!("{root}\\legacy\\v3\\server\\native-server.mjs"),
        format!("{root}\\legacy\\v2\\server\\native-server.mjs"),
    ];
    Ok(rows
        .into_iter()
        .filter(|r| r.process_id != std::process::id())
        .filter(|r| {
            let cmd = r.command_line.to_lowercase().replace('/', "\\");
            paths.iter().any(|path| {
                cmd.match_indices(path).any(|(i, _)| {
                    let before = cmd[..i].chars().next_back();
                    let after = cmd[i + path.len()..].chars().next();
                    before.is_none_or(|c| c.is_whitespace() || c == '"') && after.is_none_or(|c| c.is_whitespace() || c == '"')
                })
            })
        })
        .map(|r| (r.process_id, r.command_line))
        .collect())
}

fn targets(layout: &Layout, store: Option<&str>, all: bool) -> anyhow::Result<Vec<String>> {
    if let Some(s) = store {
        return Ok(vec![s.to_string()]);
    }
    if !all {
        bail!("--store <slug> か --all を指定する");
    }
    let mut v = vec![];
    for e in fs::read_dir(layout.stores())?.flatten() {
        if e.path().join("store.db").is_file() {
            v.push(e.file_name().to_string_lossy().into_owned());
        }
    }
    v.sort();
    Ok(v)
}

/// V3_FROZEN の中身。`phase` は blocking（起動は止めた・v3が残っているかもしれない）か
/// quiescent（v3の停止を確かめた。最終移行してよい）。
pub fn frozen_phase(layout: &Layout, slug: &str) -> Option<(String, String)> {
    let t = fs::read_to_string(layout.store_dir(slug).join("V3_FROZEN")).ok()?;
    let v: serde_json::Value = serde_json::from_str(&t).ok()?;
    Some((v["phase"].as_str().unwrap_or("blocking").to_string(), v["frozen_at"].as_str().unwrap_or("").to_string()))
}

fn write_marker(layout: &Layout, slug: &str, v: &serde_json::Value) -> anyhow::Result<()> {
    let f = layout.store_dir(slug).join("V3_FROZEN");
    let tmp = f.with_extension("tmp");
    fs::write(&tmp, serde_json::to_string_pretty(v)?)?;
    fs::rename(&tmp, &f)?;
    Ok(())
}

/// 凍結（§10.2、R9）。段階を分ける：
/// 1. 印を置き、v3のランチャーの新しい起動を止める（blocking）
/// 2. 動いているv3を止め、本当に止まったかを数え直す
/// 3. 残りが無ければ quiescent にする。残れば成功にしない（最終移行も受け付けない）
pub fn freeze(layout: &Layout, store: Option<&str>, all: bool, confirm_stop: bool) -> anyhow::Result<i32> {
    freeze_with(layout, store, all, confirm_stop, v3_processes)
}

fn freeze_with(
    layout: &Layout, store: Option<&str>, all: bool, confirm_stop: bool,
    mut processes: impl FnMut(&Layout) -> anyhow::Result<Vec<(u32, String)>>,
) -> anyhow::Result<i32> {
    let slugs = targets(layout, store, all)?;
    let now = chrono::Utc::now().to_rfc3339_opts(chrono::SecondsFormat::Millis, true);
    for slug in &slugs {
        let frozen_at = frozen_phase(layout, slug).map(|(_, at)| at).unwrap_or_else(|| now.clone());
        write_marker(layout, slug, &json!({ "frozen_at": frozen_at, "phase": "blocking", "build": ipc::build_id() }))?;
        println!("{slug}: v3の新しい起動を止めた（blocking）");
    }
    let procs = processes(layout)?;
    if !procs.is_empty() && !confirm_stop {
        println!("\n動いているv3のプロセス {}件（このままでは最終移行できない）:", procs.len());
        for (pid, cmd) in &procs {
            println!("- {pid} {}", cmd.chars().take(160).collect::<String>());
        }
        println!("\n止めてよければ --confirm-stop を付けて実行し直す（clientを閉じて止めてもよい）。凍結を取り消すなら `antmemory cutover thaw`。");
        return Ok(3);
    }
    let mut stopped = 0usize;
    let mut failed = vec![];
    for (pid, _) in &procs {
        match am_anchor::proc::silent("taskkill").args(["/PID", &pid.to_string(), "/F"]).output() {
            Ok(o) if o.status.success() => stopped += 1,
            Ok(o) => failed.push(format!("{pid}: {}", String::from_utf8_lossy(&o.stderr).trim())),
            Err(e) => failed.push(format!("{pid}: taskkillを起動できない（{e}）")),
        }
    }
    std::thread::sleep(std::time::Duration::from_millis(800));
    let remaining = processes(layout)?;
    if !failed.is_empty() || !remaining.is_empty() {
        println!("v3が止まり切っていない。凍結は blocking のまま（最終移行は受け付けない）:");
        for f in &failed {
            println!("- 停止に失敗 {f}");
        }
        for (pid, cmd) in &remaining {
            println!("- 残っている {pid} {}", cmd.chars().take(160).collect::<String>());
        }
        return Ok(4);
    }
    let at = chrono::Utc::now().to_rfc3339_opts(chrono::SecondsFormat::Millis, true);
    for slug in &slugs {
        let (_, frozen_at) = frozen_phase(layout, slug).unwrap_or_default();
        write_marker(layout, slug, &json!({ "frozen_at": frozen_at, "phase": "quiescent", "quiescent_at": at, "stopped": stopped, "build": ipc::build_id() }))?;
        println!("{slug}: v3の停止を確かめた（quiescent。止めたプロセス {stopped}件）");
    }
    Ok(0)
}

pub fn thaw(layout: &Layout, store: Option<&str>, all: bool) -> anyhow::Result<()> {
    for slug in targets(layout, store, all)? {
        let f = layout.store_dir(&slug).join("V3_FROZEN");
        if f.exists() {
            // 凍結の記録は消さずに名前を変えて残す。
            fs::rename(&f, layout.store_dir(&slug).join(format!("V3_FROZEN.thawed-{}", stamp())))?;
            println!("{slug}: v3の凍結を解いた");
        }
    }
    Ok(())
}

/// v4の凍結を解く（切戻しをやめて、v4を使い続けるとき）。
pub fn thaw_v4(layout: &Layout, slug: &str) -> anyhow::Result<()> {
    let f = layout.store_dir(slug).join(am_store::V4_FROZEN);
    if f.exists() {
        fs::rename(&f, layout.store_dir(slug).join(format!("V4_FROZEN.thawed-{}", stamp())))?;
        println!("{slug}: v4の凍結を解いた");
    }
    Ok(())
}

/// embeddingのモデルを root/models へ写す（v3の node_modules を片付けても意味検索が止まらないように）。
/// 一時ディレクトリで全てのファイルを揃え、大きさまで確かめてから公開する。途中で欠けた配置は作り直す（独立レビュー I3）。
fn ensure_model(layout: &Layout) -> anyhow::Result<()> {
    const FILES: [&str; 2] = ["onnx/model.onnx", "tokenizer.json"];
    let models = layout.antmemory_root.join("models");
    let src = am_embed::model_dir(&layout.antmemory_root);
    let name = src.file_name().ok_or_else(|| anyhow::anyhow!("モデルフォルダ名が無い: {}", src.display()))?;
    let dst = models.join(name);
    let src_ok = FILES.iter().all(|f| src.join(f).is_file());
    let same = |f: &str| match (fs::read(src.join(f)), fs::read(dst.join(f))) {
        (Ok(a), Ok(b)) => !a.is_empty() && a == b,
        (Err(_), Ok(b)) => !b.is_empty(),
        _ => false,
    };
    if FILES.iter().all(|f| same(f)) {
        return Ok(());
    }
    if !src_ok || src == dst {
        println!("注意: embeddingのモデルが揃っていない（{}）。意味検索なしで動く", src.display());
        return Ok(());
    }
    let st = format!("{}-{}", stamp(), am_tools::ctx::new_id("install-model"));
    let tmp = models.join(format!(".partial-{st}"));
    fs::create_dir_all(tmp.join("onnx"))?;
    for f in FILES {
        fs::copy(src.join(f), tmp.join(f)).with_context(|| format!("モデルを写せない: {f}"))?;
        anyhow::ensure!(fs::metadata(src.join(f))?.len() == fs::metadata(tmp.join(f))?.len(), "写したモデルの大きさが合わない: {f}");
    }
    if dst.exists() {
        // 欠けた配置は消さずに名前を変えて退ける。
        fs::rename(&dst, models.join(format!("{}.broken-{st}", name.to_string_lossy())))?;
    }
    fs::rename(&tmp, &dst)?;
    println!("モデルを写した: {}", dst.display());
    Ok(())
}

/// ortはDLLを動的に読む。exeだけ配ると、別配置では意味検索が黙って失われる。
fn ensure_runtime(layout: &Layout, dir: &Path) -> anyhow::Result<()> {
    let Some(src) = am_embed::find_onnxruntime_dll(&layout.antmemory_root) else {
        println!("注意: onnxruntime.dll が無い。意味検索なしで動く（runtime/binに配置してinstallを再実行）");
        return Ok(());
    };
    let dst = dir.join("onnxruntime.dll");
    if dst.is_file() && fs::read(&src)? == fs::read(&dst)? { return Ok(()); }
    let st = format!("{}-{}", stamp(), am_tools::ctx::new_id("install-runtime"));
    let fresh = dir.join(format!("onnxruntime.new-{st}.dll"));
    fs::copy(&src, &fresh)?;
    anyhow::ensure!(fs::read(&src)? == fs::read(&fresh)?, "写したDLLの内容が違う");
    let old = dir.join(format!("onnxruntime.old-{st}.dll"));
    let had_old = dst.exists();
    if had_old { fs::rename(&dst, &old)?; }
    if let Err(e) = fs::rename(&fresh, &dst) {
        if had_old { fs::rename(&old, &dst).ok(); }
        fs::remove_file(&fresh).ok();
        return Err(e.into());
    }
    println!("推論runtimeを写した: {}", dst.display());
    Ok(())
}

/// このexeを配置する。新しいexeを一時の名前で置き、確かめてから入れ替える。
/// 配置済みのexe自身から実行しても、入れ替えに失敗しても、起動先（antmemory.exe）を失わない（独立レビュー I2）。
pub fn install(layout: &Layout, to: Option<PathBuf>) -> anyhow::Result<()> {
    let dir = to.unwrap_or_else(|| layout.antmemory_root.join("runtime").join("bin"));
    fs::create_dir_all(&dir)?;
    let dst = dir.join("antmemory.exe");
    ensure_model(layout)?;
    ensure_runtime(layout, &dir)?;
    let me = std::env::current_exe()?;
    let st = format!("{}-{}", stamp(), am_tools::ctx::new_id("install-exe"));
    let fresh = dir.join(format!("antmemory.new-{st}.exe"));
    fs::copy(&me, &fresh).with_context(|| format!("{} を写せない", me.display()))?;
    anyhow::ensure!(fs::metadata(&me)?.len() == fs::metadata(&fresh)?.len(), "写したexeの大きさが合わない");
    let old = dir.join(format!("antmemory.old-{st}.exe"));
    let had_old = dst.exists();
    if had_old {
        // 動いているexeは上書きできないが、名前は変えられる。
        fs::rename(&dst, &old)?;
    }
    if let Err(e) = fs::rename(&fresh, &dst) {
        if had_old {
            fs::rename(&old, &dst).ok();
        }
        fs::remove_file(&fresh).ok();
        anyhow::bail!("新しいexeを置けない（元のexeに戻した）: {e}");
    }
    // 前に置き換えた古いexeは、動いていなければ消せる（動いているもの・今退けたものは残る）。
    for e in fs::read_dir(&dir)?.flatten() {
        let n = e.file_name().to_string_lossy().into_owned();
        if (n.starts_with("antmemory.old-") || n.starts_with("antmemory.new-")) && n.ends_with(".exe") && e.path() != old {
            fs::remove_file(e.path()).ok();
        }
    }
    let exe = dst.display().to_string();
    println!("配置した: {exe}（{}）", ipc::build_id());
    println!("\nclientの設定（書き換えはユーザーの確認を取ってから）:");
    println!("- Claude Code (~/.claude.json の mcpServers.ant-memory): {{\"command\": {}, \"args\": [\"serve\"], \"env\": {{\"ANT_MEMORY_CLIENT\": \"claude-code\", \"ANT_MEMORY_ROOT\": {}}}}}", json!(exe), json!(layout.antmemory_root));
    println!("- Codex (~/.codex/config.toml): [mcp_servers.ant-memory] command = {} args = [\"serve\"] env = {{ ANT_MEMORY_CLIENT = \"codex\" }}", json!(exe));
    println!("- Claude Code hooks (C2): SessionStart / UserPromptSubmit / PostToolUse(Read|Edit|Write|MultiEdit|NotebookEdit) → command {} hook", json!(exe));
    Ok(())
}

/// 切戻し：凍結時のv3 snapshotのコピーへ、v4にあってv3に無いものを書き戻す（§10.4）。
///
/// 同期点（R10）：先に V4_FROZEN を置いて新しい書込を止め、`BEGIN IMMEDIATE` で処理中の書込が
/// 終わるのを待つ。その1つのtransactionの中で全ての表を読むので、途中で増えた記憶が混ざらない。
/// 書き戻す対象は「切戻し先に無いid」（R8。lateで取り込んだ記憶も入る）。最後に全ての記憶・状態・
/// 関係・stateを照合し、合わなければ出力を未検証として残し、成功にしない。
pub fn export_v3(layout: &Layout, slug: &str, out: &Path) -> anyhow::Result<bool> {
    let dir = layout.store_dir(slug);
    let base = dir.join("v3-final-snapshot.db");
    if !base.is_file() {
        bail!("{} が無い（本番移行の snapshot）", base.display());
    }
    if out.exists() {
        bail!("{} が既にある（上書きしない）", out.display());
    }
    let marker = dir.join(am_store::V4_FROZEN);
    if !marker.exists() {
        fs::write(
            &marker,
            serde_json::to_string_pretty(&json!({ "frozen_at": chrono::Utc::now().to_rfc3339(), "reason": "export-v3", "build": ipc::build_id() }))?,
        )?;
        println!("{slug}: v4への書込を止めた（V4_FROZEN）");
    }
    let mut v4 = rusqlite::Connection::open(dir.join("v4.db"))?;
    v4.busy_timeout(std::time::Duration::from_secs(30))?;
    let r = v4.transaction_with_behavior(rusqlite::TransactionBehavior::Immediate)?;

    fs::copy(&base, out)?;
    let mut o = rusqlite::Connection::open(out)?;
    let tx = o.transaction()?;
    let mut n = json!({});
    let in_v3: std::collections::HashSet<String> =
        tx.prepare("SELECT id FROM chunks")?.query_map([], |x| x.get(0))?.collect::<Result<_, _>>()?;

    // 記憶：切戻し先に無いidを全て書く。
    let mems: Vec<(String, String, String, String, String, String, String, Option<String>, String, String, String, Option<Vec<u8>>)> = r
        .prepare(
            "SELECT m.id, m.topic, m.author, m.tags_json, s.importance, m.content, m.created_at, s.expires_at, s.status, s.visibility,
                    COALESCE(m.v3_updated_at, s.updated_at),
                    (SELECT vector FROM embeddings e WHERE e.id=m.id AND e.content_hash=m.content_hash AND e.model_id=?1)
               FROM memories m JOIN memory_state s ON s.id=m.id",
        )?
        .query_map([am_store::v3read::MODEL_ID], |x| {
            Ok((x.get(0)?, x.get(1)?, x.get(2)?, x.get(3)?, x.get(4)?, x.get(5)?, x.get(6)?, x.get(7)?, x.get(8)?, x.get(9)?, x.get(10)?, x.get(11)?))
        })?
        .collect::<Result<_, _>>()?;
    let mut added = 0;
    for (id, topic, agent, tags, imp, content, created, expires, _, _, updated, emb) in &mems {
        if in_v3.contains(id) {
            continue;
        }
        tx.execute(
            "INSERT INTO chunks(id, topic, agent, tags, importance, content, embedding, created_at, updated_at, expires_at) VALUES (?1,?2,?3,?4,?5,?6,?7,?8,?9,?10)",
            params![id, topic, agent, tags, imp, content, emb, created, updated, expires],
        )?;
        added += 1;
    }
    n["memories_added"] = json!(added);
    // 状態：全ての記憶について、v4の値に揃える（v3の行が無かった記憶にも行を作る）。
    for (id, _, _, _, imp, _, _, expires, status, vis, updated, _) in &mems {
        tx.execute("INSERT OR REPLACE INTO memory_lifecycle(chunk_id, status, updated_at) VALUES (?1, ?2, ?3)", params![id, status, updated])?;
        tx.execute(
            "INSERT INTO memory_visibility(chunk_id, visibility, reason, changed_at, actor) VALUES (?1, ?2, 'v4から書き戻し', ?3, 'antmemory-export')
             ON CONFLICT(chunk_id) DO UPDATE SET visibility=excluded.visibility WHERE memory_visibility.visibility != excluded.visibility",
            params![id, vis, updated],
        )?;
        tx.execute("UPDATE chunks SET importance=?2, expires_at=?3 WHERE id=?1", params![id, imp, expires])?;
    }

    // イベント：v3由来はv3のevent_idで、v4のものは v4-<seq> で。既にあるものは重ねない。
    let mut events = 0;
    {
        let rows: Vec<(i64, String, String, String, Option<String>, Option<String>, String, String)> = r
            .prepare(
                "SELECT seq, kind, subject, actor, model, provider, payload_json, created_at FROM events
                  WHERE subject IN (SELECT id FROM memories)",
            )?
            .query_map([], |x| Ok((x.get(0)?, x.get(1)?, x.get(2)?, x.get(3)?, x.get(4)?, x.get(5)?, x.get(6)?, x.get(7)?)))?
            .collect::<Result<_, _>>()?;
        for (seq, kind, subject, actor, model, provider, payload, at) in rows {
            let p: serde_json::Value = serde_json::from_str(&payload).unwrap_or_default();
            let s = |k: &str| p.get(k).and_then(|v| v.as_str()).map(String::from);
            let (event_id, event_type) = match kind.strip_prefix("v3.") {
                Some(t) => (s("v3_event_id").unwrap_or_else(|| format!("v3-{seq}")), t.to_string()),
                None => (format!("v4-{seq}"), kind.clone()),
            };
            events += tx.execute(
                "INSERT OR IGNORE INTO memory_events(event_id, chunk_id, event_type, actor, model, provider, reason, old_value, new_value, created_at)
                 VALUES (?1,?2,?3,?4,?5,?6,?7,?8,?9,?10)",
                params![event_id, subject, event_type, actor, model, provider, s("reason"), s("old_value"), s("new_value"), at],
            )?;
        }
    }
    n["events_added"] = json!(events);
    let rels: Vec<(String, String, String, String, Option<String>, String)> = r
        .prepare("SELECT src, dst, kind, actor, reason, created_at FROM relations")?
        .query_map([], |x| Ok((x.get(0)?, x.get(1)?, x.get(2)?, x.get(3)?, x.get(4)?, x.get(5)?)))?
        .collect::<Result<_, _>>()?;
    let mut rel_added = 0;
    for (a, b, k, actor, reason, at) in &rels {
        rel_added += tx.execute(
            "INSERT OR IGNORE INTO memory_relations(source_id, target_id, relation_type, actor, reason, created_at) VALUES (?1,?2,?3,?4,?5,?6)",
            params![a, b, k, actor, reason, at],
        )?;
    }
    n["relations_added"] = json!(rel_added);
    let states: Vec<(String, String, String)> = r
        .prepare("SELECT key, value_json, updated_at FROM state")?
        .query_map([], |x| Ok((x.get(0)?, x.get(1)?, x.get(2)?)))?
        .collect::<Result<_, _>>()?;
    for (k, v, at) in &states {
        tx.execute("INSERT OR REPLACE INTO state(key, value, updated_at) VALUES (?1, ?2, ?3)", params![k, v, at])?;
    }
    // 想起ログ：v3由来は元のrecall_idで、v4の検索結果はresult_idで。
    let recalls: Vec<(String, String, Option<String>, Option<String>, String, Option<f64>, Option<i64>, String)> = r
        .prepare(
            "SELECT r.result_id, r.memory_id, COALESCE(s.conversation_id, s.legacy_session_id), s.query_raw, r.reasons_json, r.base_score, r.rank, s.created_at
               FROM search_results r JOIN searches s ON s.search_id=r.search_id",
        )?
        .query_map([], |x| Ok((x.get(0)?, x.get(1)?, x.get(2)?, x.get(3)?, x.get(4)?, x.get(5)?, x.get(6)?, x.get(7)?)))?
        .collect::<Result<_, _>>()?;
    let mut rec_added = 0;
    for (rid, mid, sess, q, reason, score, rank, at) in &recalls {
        rec_added += tx.execute(
            "INSERT OR IGNORE INTO memory_recall_log(recall_id, chunk_id, session_id, query, reason, score, rank, created_at) VALUES (?1,?2,?3,?4,?5,?6,?7,?8)",
            params![rid, mid, sess, q, reason, score, rank, at],
        )?;
    }
    n["recall_log_added"] = json!(rec_added);

    // v3の形で表せないもの（札・照合した発言・uses・取手と観測など）は、payloadのJSONとして記憶に残す。落とさない。
    let dump = |sql: &str| -> anyhow::Result<Vec<serde_json::Value>> {
        let mut st = r.prepare(sql)?;
        let cols: Vec<String> = st.column_names().iter().map(|c| c.to_string()).collect();
        let rows = st.query_map([], |x| {
            let mut m = serde_json::Map::new();
            for (i, c) in cols.iter().enumerate() {
                let v: rusqlite::types::Value = x.get(i)?;
                m.insert(
                    c.clone(),
                    match v {
                        rusqlite::types::Value::Null => serde_json::Value::Null,
                        rusqlite::types::Value::Integer(i) => json!(i),
                        rusqlite::types::Value::Real(f) => json!(f),
                        rusqlite::types::Value::Text(t) => json!(t),
                        rusqlite::types::Value::Blob(b) => json!({ "$blob_hex": hex::encode(b) }),
                    },
                );
            }
            Ok(serde_json::Value::Object(m))
        })?;
        Ok(rows.collect::<Result<_, _>>()?)
    };
    let now = chrono::Utc::now().to_rfc3339_opts(chrono::SecondsFormat::Millis, true);
    let mut payload_ids = vec![];
    for (tag, topic, sql) in [
        ("tk:goal", "v4の札（goals）", "SELECT * FROM goals"),
        ("tk:goal", "v4の札の候補（goal_proposals）", "SELECT * FROM goal_proposals"),
        ("tk:goal", "v4の札の報告（goal_links）", "SELECT * FROM goal_links"),
        ("tk:utterance", "v4で照合したユーザー発言（utterances）", "SELECT * FROM utterances"),
        ("tk:v4-payload", "v4の「役に立った」の観測（uses）", "SELECT * FROM uses"),
        ("tk:v4-payload", "v4の学習tagsの証拠（learned_evidence）", "SELECT * FROM learned_evidence"),
        ("tk:v4-payload", "v4の取手（anchors）", "SELECT * FROM anchors"),
        ("tk:v4-payload", "v4の取手の観測（anchor_observations）", "SELECT * FROM anchor_observations"),
        ("tk:v4-payload", "v4の読取り（reads）", "SELECT * FROM reads"),
        ("tk:v4-payload", "v4の会話（conversations）", "SELECT * FROM conversations"),
    ] {
        let rows = dump(sql)?;
        if rows.is_empty() {
            continue;
        }
        let id = am_tools::ctx::new_id(topic);
        tx.execute(
            "INSERT INTO chunks(id, topic, agent, tags, importance, content, embedding, created_at, updated_at, expires_at) VALUES (?1,?2,'antmemory-export',?3,'low',?4,NULL,?5,?5,NULL)",
            params![id, format!("{topic} {}件（切戻し時の書き戻し）", rows.len()), json!([tag, "v4-export"]).to_string(), serde_json::to_string(&rows)?, now],
        )?;
        tx.execute("INSERT OR REPLACE INTO memory_lifecycle(chunk_id, status, updated_at) VALUES (?1, 'active', ?2)", params![id, now])?;
        tx.execute(
            "INSERT OR REPLACE INTO memory_visibility(chunk_id, visibility, reason, changed_at, actor) VALUES (?1, 'cold', 'v4の書き戻し', ?2, 'antmemory-export')",
            params![id, now],
        )?;
        payload_ids.push(id);
    }
    n["payload_memories"] = json!(payload_ids.len());
    tx.commit()?;
    // v3の検索索引（chunksを元にした外部内容表）を作り直す。失敗は成功にしない。
    o.execute("INSERT INTO chunks_fts(chunks_fts) VALUES('rebuild')", []).context("v3の検索索引を作り直せない")?;

    // 照合：全ての記憶・状態・関係・stateが、切戻し先から同じに読めるか。
    let mut miss: Vec<String> = vec![];
    for (id, topic, agent, tags, imp, content, _, expires, status, vis, _, _) in &mems {
        let got: Option<(String, String, String, String, String, Option<String>, Option<String>, Option<String>)> = o
            .query_row(
                "SELECT c.topic, c.agent, c.tags, c.importance, c.content, c.expires_at, l.status, v.visibility FROM chunks c
                   LEFT JOIN memory_lifecycle l ON l.chunk_id=c.id LEFT JOIN memory_visibility v ON v.chunk_id=c.id WHERE c.id=?1",
                [id],
                |x| Ok((x.get(0)?, x.get(1)?, x.get(2)?, x.get(3)?, x.get(4)?, x.get(5)?, x.get(6)?, x.get(7)?)),
            )
            .ok();
        match got {
            None => miss.push(format!("記憶 {id} が無い")),
            Some(g) => {
                if (&g.0, &g.1, &g.2, &g.3, &g.4, &g.5) != (topic, agent, tags, imp, content, expires) {
                    miss.push(format!("記憶 {id} の本文か属性が違う"));
                }
                if g.6.as_deref() != Some(status.as_str()) || g.7.as_deref() != Some(vis.as_str()) {
                    miss.push(format!("記憶 {id} の状態が違う（{:?}/{:?} ≠ {status}/{vis}）", g.6, g.7));
                }
            }
        }
    }
    for (a, b, k, ..) in &rels {
        let ok: bool = o
            .query_row("SELECT EXISTS(SELECT 1 FROM memory_relations WHERE source_id=?1 AND target_id=?2 AND relation_type=?3)", params![a, b, k], |x| x.get(0))
            .unwrap_or(false);
        if !ok {
            miss.push(format!("関係 {a} -{k}-> {b} が無い"));
        }
    }
    for (k, v, _) in &states {
        let got: Option<String> = o.query_row("SELECT value FROM state WHERE key=?1", [k], |x| x.get(0)).ok();
        if got.as_deref() != Some(v.as_str()) {
            miss.push(format!("state {k} が違う"));
        }
    }
    let fts_ok: i64 = o.query_row("SELECT count(*) FROM chunks_fts", [], |x| x.get(0)).unwrap_or(-1);
    let chunks: i64 = o.query_row("SELECT count(*) FROM chunks", [], |x| x.get(0))?;
    if fts_ok != chunks {
        miss.push(format!("v3の検索索引の件数が合わない（{fts_ok} / {chunks}）"));
    }
    r.rollback()?;
    drop(o);
    println!("書き戻した: {}", serde_json::to_string_pretty(&n)?);
    println!("照合: v4の記憶 {} 件・関係 {} 件・state {} 件を、切戻し先で確かめた。不一致 {} 件", mems.len(), rels.len(), states.len(), miss.len());
    if !miss.is_empty() {
        let bad = out.with_extension(format!("unverified-{}.db", stamp()));
        fs::rename(out, &bad)?;
        for m in miss.iter().take(30) {
            println!("- {m}");
        }
        println!("切戻し先は未検証として {} に残した。切戻しには使わない。v4は凍結したまま（`antmemory cutover thaw-v4` で戻せる）", bad.display());
        return Ok(false);
    }
    println!(
        "切り戻すには: clientの設定をv3へ戻し、{} を stores/{slug}/store.db と入れ替え、`antmemory cutover thaw --store {slug}` でv3の凍結を外す。\nv4は凍結したまま（書込を受け付けない）。切戻しをやめるなら `antmemory cutover thaw-v4 --store {slug}`",
        out.display()
    );
    Ok(true)
}

/// 点検。projectionをイベントから作り直して比べ、照合の限界も明記する。
pub fn doctor(layout: &Layout, slug: &str) -> anyhow::Result<bool> {
    let dir = layout.store_dir(slug);
    let store = Store::open(&dir.join("v4.db"), true)?;
    let c = store.reader();
    let mut ok = true;
    let mut line = |good: bool, s: String| {
        if !good {
            ok = false;
        }
        println!("{} {s}", if good { "✓" } else { "✗" });
    };
    let one = |sql: &str| -> i64 { c.query_row(sql, [], |r| r.get(0)).unwrap_or(-1) };
    let (m, s, f) = (one("SELECT count(*) FROM memories"), one("SELECT count(*) FROM memory_state"), one("SELECT count(*) FROM memories_fts"));
    line(m == s && m == f, format!("記憶 {m} / 状態 {s} / 索引 {f}"));
    let mut bad_hash = 0;
    {
        let mut st = c.prepare("SELECT topic, tags_json, content, content_hash FROM memories")?;
        let rows = st.query_map([], |r| Ok((r.get::<_, String>(0)?, r.get::<_, String>(1)?, r.get::<_, String>(2)?, r.get::<_, String>(3)?)))?;
        for row in rows {
            let (t, g, b, h) = row?;
            if am_domain::content_hash(&t, &g, &b) != h {
                bad_hash += 1;
            }
        }
    }
    line(bad_hash == 0, format!("content_hashの作り直しと不一致 {bad_hash}"));
    let model = query::meta(c, "active_model_id")?.unwrap_or_default();
    let missing = query::count_missing_embeddings(c, &model)?;
    line(true, format!("今のモデル {model} のvectorが無い記憶 {missing}（daemonのworkerが計算する）"));
    // 状態のprojectionを、イベントの最後の状態変化と比べる。
    let mut mismatch = 0;
    {
        let mut st = c.prepare(
            "SELECT s.id, s.status, (SELECT e.kind FROM events e WHERE e.subject=s.id AND e.kind IN
               ('superseded','retracted','restored','expired','v3.superseded','v3.retracted','v3.restored','v3.expired')
               ORDER BY e.created_at DESC, e.seq DESC LIMIT 1) FROM memory_state s",
        )?;
        let rows = st.query_map([], |r| Ok((r.get::<_, String>(0)?, r.get::<_, String>(1)?, r.get::<_, Option<String>>(2)?)))?;
        for row in rows {
            let (_, status, last) = row?;
            let expected = match last.as_deref().map(|k| k.trim_start_matches("v3.")) {
                Some("superseded") => "superseded",
                Some("retracted") => "retracted",
                Some("expired") => "expired",
                _ => "active",
            };
            if status != expected && !(status == "active" && last.is_none()) {
                mismatch += 1;
            }
        }
    }
    line(mismatch == 0, format!("状態とイベントの最後の変化が合わない記憶 {mismatch}（v3由来の欠落を含み得る。U8）"));
    let orphan_rel = one("SELECT count(*) FROM relations r WHERE NOT EXISTS (SELECT 1 FROM memories m WHERE m.id=r.src) OR NOT EXISTS (SELECT 1 FROM memories m WHERE m.id=r.dst)");
    line(true, format!("記憶の無い側を持つ関係 {orphan_rel}（v3由来なら残す）"));
    let bad_overall =
        one("SELECT count(*) FROM goals g WHERE g.kind='overall' AND NOT EXISTS (SELECT 1 FROM utterances u WHERE instr(g.basis_json, u.id) > 0)");
    line(bad_overall == 0, format!("照合した発言の記録が無い全体目標 {bad_overall}"));
    line(true, format!("未確認の札の候補 {}", one("SELECT count(*) FROM goal_proposals WHERE resolved_seq IS NULL")));
    if let Ok(t) = fs::read_to_string(dir.join("V3_FROZEN")) {
        let at = serde_json::from_str::<serde_json::Value>(&t).ok().and_then(|v| v["frozen_at"].as_str().map(String::from)).unwrap_or_default();
        if dir.join("store.db").is_file() {
            let v3 = am_store::v3read::open_read_only(&dir.join("store.db"))?;
            let ids: Vec<String> = v3
                .prepare("SELECT id FROM chunks WHERE created_at > ?1")?
                .query_map([&at], |r| r.get(0))?
                .collect::<Result<_, _>>()?;
            let late = ids.iter().filter(|id| !query::exists(c, id).unwrap_or(false)).count();
            line(late == 0, format!("凍結（{at}）の後にv3へ書かれた記憶 {late}（あれば `antmemory migrate --late`）"));
        }
    }
    println!("\n照合の限界：ユーザー発言の照合は、Agentの読み違い・推測・会話の取り違えを防ぐ。会話ログを書き換えられる手元のプロセス（shellを持つAgentを含む）は防がない。");
    Ok(ok)
}

#[cfg(test)]
mod process_tests {
    use super::*;
    #[test]
    fn failed_enumeration_never_leaves_quiescent_marker() {
        let d = tempfile::tempdir().unwrap();
        let layout = Layout { antmemory_root: d.path().into() };
        fs::create_dir_all(layout.store_dir("test")).unwrap();
        for fail_at in [1, 2] {
            write_marker(&layout, "test", &json!({"phase":"quiescent","frozen_at":"old"})).unwrap();
            let mut n = 0;
            let result = freeze_with(&layout, Some("test"), false, false, |_| {
                n += 1;
                if n == fail_at { bail!("injected process enumeration failure"); }
                Ok(vec![])
            });
            assert!(result.is_err());
            assert_eq!(frozen_phase(&layout, "test"), Some(("blocking".into(), "old".into())));
        }
    }
    #[test]
    fn enumeration_errors_are_not_zero_processes() {
        let layout = Layout { antmemory_root: PathBuf::from("F:/memory") };
        assert!(enumerate_with(&layout, &mut am_anchor::proc::silent("antmemory-nonexistent-command-9147")).is_err());
        assert!(decode_processes(&layout, false, b"[]", b"CIM failed").is_err());
        for bad in [b"".as_slice(), b"junk", b"null", b"{}", b"[{\"ProcessId\":12}]"] {
            assert!(decode_processes(&layout, true, bad, b"").is_err());
        }
        assert!(decode_processes(&layout, true, b"[]", b"").unwrap().is_empty());
        let rows = json!([
            {"ProcessId":10,"CommandLine":"node F:/memory/server/native-server.mjs"},
            {"ProcessId":11,"CommandLine":"node F:/memory-copy/server/native-server.mjs"},
            {"ProcessId":12,"CommandLine":"node F:/memory/server/native-server.mjs.backup"},
            {"ProcessId":13,"CommandLine":"node \"F:/memory/bin/ant-memory.mjs\""}
        ]);
        assert_eq!(decode_processes(&layout, true, rows.to_string().as_bytes(), b"").unwrap().iter().map(|r| r.0).collect::<Vec<_>>(), vec![10, 13]);
    }
}
