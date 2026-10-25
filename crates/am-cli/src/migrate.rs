//! v3 → v4 の移行（設計案v2 §10）。
//!
//! - `rehearse`：snapshotから別のディレクトリへ変換して照合する。何度でもやり直せる。v3には触らない。
//! - `final_`  ：凍結（V3_FROZEN）したstoreを、同じディレクトリの v4.db へ移す。不一致なら使わない。
//! - `late`    ：凍結の後にv3へ書かれた記憶だけを取り込む（§10.3）。

use std::collections::BTreeMap;
use std::fs;
use std::path::{Path, PathBuf};

use am_anchor::Resolver;
use am_domain::{Anchor, AnchorObservation, AnchorRow, ApplyCtx, Command, V3Batch};
use am_store::{status_name, v3read, verify, Store};
use am_tools::workspace::Layout;
use anyhow::{bail, Context};
use rusqlite::params;
use serde_json::json;

use crate::ipc;

fn now() -> String {
    chrono::Utc::now().to_rfc3339_opts(chrono::SecondsFormat::Millis, true)
}

pub fn stamp() -> String {
    chrono::Utc::now().format("%Y%m%dT%H%M%SZ").to_string()
}

fn cx() -> ApplyCtx {
    ApplyCtx { actor: "antmemory-migrate".into(), build: ipc::build_id(), ..Default::default() }
}

/// registry（slug → root）から、store.dbの親ディレクトリ名でrootを引く。
fn root_from_registry(from: &Path, registry: &Path) -> anyhow::Result<Option<PathBuf>> {
    let Some(slug) = from.parent().and_then(|p| p.file_name()).map(|s| s.to_string_lossy().into_owned()) else {
        return Ok(None);
    };
    let text = fs::read_to_string(registry).with_context(|| format!("{} を読めない", registry.display()))?;
    let map: BTreeMap<String, String> = serde_json::from_str(&text).context("registryがJSONの対応表でない")?;
    Ok(map.get(&slug).map(PathBuf::from))
}

fn anchor_kind(a: &Anchor) -> &'static str {
    match a {
        Anchor::File { external: false, .. } => "file",
        Anchor::File { external: true, .. } => "file_external",
        Anchor::Commit { .. } => "commit",
        Anchor::Conversation { .. } => "conversation",
        Anchor::Url { .. } => "url",
        Anchor::Opaque { .. } => "opaque",
    }
}

/// chunkごとの source_observed を古い順に並べ、取手にする。記憶の無いsource_refは数だけ返す。
fn anchors_from_sources(batch: &V3Batch, root: &Path) -> (Vec<AnchorRow>, usize) {
    let mut by_chunk: BTreeMap<&str, Vec<(&str, &str)>> = BTreeMap::new();
    for e in batch.events.iter().filter(|e| e.event_type == "source_observed") {
        if let Some(v) = e.new_value.as_deref() {
            by_chunk.entry(e.chunk_id.as_str()).or_default().push((e.created_at.as_str(), v));
        }
    }
    let known: std::collections::HashSet<&str> = batch.chunks.iter().map(|c| c.id.as_str()).collect();
    let mut anchors = Vec::new();
    let mut orphan = 0usize;
    for (chunk, mut refs) in by_chunk {
        if !known.contains(chunk) {
            orphan += refs.len();
            continue;
        }
        refs.sort();
        let mut ord = 0u32;
        for (at, raw) in refs {
            for (segment, anchor) in am_anchor::parse::parse_all(raw, root) {
                anchors.push(AnchorRow { memory_id: chunk.to_string(), ord, raw: raw.to_string(), segment, anchor, recorded_at: at.to_string() });
                ord += 1;
            }
        }
    }
    (anchors, orphan)
}

/// 取手を現物と照合して観測として追記し、種類×結果の表を返す。
fn observe(store: &mut Store, anchors: &[AnchorRow], root: Option<&Path>) -> anyhow::Result<serde_json::Value> {
    let Some(root) = root else {
        return Ok(json!({ "skipped": "workspace rootが分からないため照合していない" }));
    };
    let mut resolver = Resolver::new(root);
    let at = now();
    let obs: Vec<AnchorObservation> = anchors
        .iter()
        .map(|a| AnchorObservation { memory_id: a.memory_id.clone(), ord: a.ord, observed_at: at.clone(), status: resolver.resolve(&a.anchor) })
        .collect();
    let mut table: BTreeMap<String, BTreeMap<String, usize>> = BTreeMap::new();
    let mut examples: BTreeMap<String, Vec<serde_json::Value>> = BTreeMap::new();
    for (a, o) in anchors.iter().zip(&obs) {
        let k = anchor_kind(&a.anchor).to_string();
        let s = status_name(&o.status).to_string();
        *table.entry(k.clone()).or_default().entry(s.clone()).or_default() += 1;
        let ex = examples.entry(format!("{k}/{s}")).or_default();
        if ex.len() < 5 {
            ex.push(json!({ "segment": a.segment, "status": o.status }));
        }
    }
    let rec = store.apply(&cx(), Command::RecordAnchorObservations(obs))?;
    Ok(json!({ "workspace_root": root.display().to_string(), "git": resolver.is_git(),
        "by_kind_and_status": table, "examples": examples, "observation_event_seq": rec.event_seqs }))
}

/// 同じMiniLMについて、v3（transformers.js）とv4（ort）のvectorを比べる。
fn embedding_sample(layout: Option<&Layout>, store: &Store, n: usize) -> serde_json::Value {
    let Some(l) = layout else { return json!({ "skipped": "rootが分からない" }) };
    let Some(model) = am_embed::v3_model_dir(&l.antmemory_root) else { return json!({"skipped":"同値検査用のMiniLMが無い"}); };
    let e = match am_embed::Embedder::load(&model) {
        Ok(e) => e,
        Err(err) => return json!({ "skipped": err.to_string() }),
    };
    let rows: Vec<(String, String, String, Vec<u8>)> = store
        .reader()
        .prepare("SELECT m.topic, m.tags_json, m.content, e.vector FROM memories m JOIN embeddings e ON e.id=m.id WHERE e.model_id=?1 AND e.content_hash=m.content_hash ORDER BY random() LIMIT ?2")
        .and_then(|mut st| st.query_map(params![am_store::v3read::MODEL_ID, n as i64], |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?)))?.collect())
        .unwrap_or_default();
    let mut sims: Vec<f32> = vec![];
    for (topic, tags, content, blob) in &rows {
        let tags: Vec<String> = serde_json::from_str(tags).unwrap_or_default();
        if let Ok(v) = e.embed(&am_embed::prepare_text(topic, &tags, content)) {
            sims.push(am_embed::dot(&v, &am_embed::from_blob(blob)));
        }
    }
    sims.sort_by(|a, b| a.partial_cmp(b).unwrap_or(std::cmp::Ordering::Equal));
    if sims.is_empty() {
        return json!({ "sampled": 0 });
    }
    json!({ "sampled": sims.len(), "min": sims[0], "median": sims[sims.len() / 2],
        "below_0_999": sims.iter().filter(|s| **s < 0.999).count() })
}

struct Outcome {
    ok: bool,
    report: serde_json::Value,
}

/// snapshotから読み、v4 DBを作って取り込み、取手を照合し、照合する。
fn convert(from: &Path, snap_path: &Path, v4_path: &Path, root: Option<PathBuf>, layout: Option<&Layout>, sample: usize) -> anyhow::Result<Outcome> {
    let snap_sha = v3read::snapshot(from, snap_path).context("snapshotを取れない")?;
    let snap = v3read::open_read_only(snap_path)?;
    let mut batch = v3read::read(&snap, &from.display().to_string(), &snap_sha)?;
    let (anchors, orphan) = anchors_from_sources(&batch, &root.clone().unwrap_or_default());
    batch.anchors = anchors.clone();
    let mut store = Store::create(v4_path, &ipc::build_id())?;
    let imported = store.apply(&cx(), Command::ImportV3(Box::new(batch)))?;
    let resolution = observe(&mut store, &anchors, root.as_deref())?;
    let report = verify::verify(&snap, store.reader())?;
    let emb = if sample > 0 { embedding_sample(layout, &store, sample) } else { json!({ "skipped": "標本なし" }) };
    Ok(Outcome {
        ok: report.ok(),
        report: json!({
            "build": ipc::build_id(), "generated_at": now(), "from": from.display().to_string(),
            "snapshot": snap_path.display().to_string(), "snapshot_sha256": snap_sha, "v4": v4_path.display().to_string(),
            "imported": imported.counts.iter().map(|(k, v)| (k.to_string(), json!(v))).collect::<serde_json::Map<_, _>>(),
            "orphan_source_refs": orphan, "verify": report.to_json(), "anchors": resolution, "embedding_equivalence": emb,
        }),
    })
}

fn write_reports(dir: &Path, prefix: &str, r: &serde_json::Value) -> anyhow::Result<()> {
    fs::write(dir.join(format!("{prefix}.json")), serde_json::to_string_pretty(r)?)?;
    fs::write(dir.join(format!("{prefix}.md")), render_md(r))?;
    Ok(())
}

pub fn rehearse(from: &Path, out_dir: &Path, workspace_root: Option<PathBuf>, registry: Option<PathBuf>, sample: usize) -> anyhow::Result<bool> {
    if !from.is_file() {
        bail!("{} が無い", from.display());
    }
    let v4_path = out_dir.join("v4.db");
    if v4_path.exists() {
        bail!("{} が既にある。予行は毎回新しいディレクトリへ出す", v4_path.display());
    }
    fs::create_dir_all(out_dir)?;
    let root = match (workspace_root, registry) {
        (Some(r), _) => Some(r),
        (None, Some(reg)) => root_from_registry(from, &reg)?,
        (None, None) => None,
    };
    let layout = ipc::layout().ok();
    let o = convert(from, &out_dir.join("snapshot.v3.db"), &v4_path, root, layout.as_ref(), sample)?;
    write_reports(out_dir, "report", &o.report)?;
    println!("{}", render_md(&o.report));
    Ok(o.ok)
}

/// v3がまだDBを使っていないか。BEGIN EXCLUSIVE が取れれば、他の接続は無い。
fn quiescent(v3: &Path) -> anyhow::Result<()> {
    let c = rusqlite::Connection::open(v3)?;
    c.busy_timeout(std::time::Duration::from_millis(500))?;
    c.execute_batch("BEGIN EXCLUSIVE; ROLLBACK;")
        .map_err(|e| anyhow::anyhow!("v3のDBがまだ使われている（{e}）。v3のプロセスが残っていないか確かめる"))?;
    Ok(())
}

pub fn final_(layout: &Layout, slug: &str) -> anyhow::Result<bool> {
    let dir = layout.store_dir(slug);
    let v3 = dir.join("store.db");
    let v4 = dir.join("v4.db");
    if !v3.is_file() {
        bail!("{} が無い", v3.display());
    }
    match crate::cutover::frozen_phase(layout, slug) {
        None => bail!("{slug} は凍結されていない。先に `antmemory cutover freeze --store {slug}`"),
        Some((phase, _)) if phase != "quiescent" => {
            bail!("{slug} の凍結は {phase}（v3の停止を確かめていない）。`antmemory cutover freeze --store {slug} --confirm-stop` で止め切る")
        }
        _ => {}
    }
    // 凍結の後に起動されたv3が残っていないか、移す直前にもう一度数える（R9）。
    let procs = crate::cutover::v3_processes(layout)?;
    if !procs.is_empty() {
        bail!("v3のプロセスが {}件 動いている（{:?}）。止めてから移す", procs.len(), procs.iter().map(|p| p.0).collect::<Vec<_>>());
    }
    if v4.exists() {
        bail!("{} が既にある（移行済み）", v4.display());
    }
    quiescent(&v3)?;
    let snap = dir.join("v3-final-snapshot.db");
    if snap.exists() {
        fs::rename(&snap, dir.join(format!("v3-final-snapshot.{}.db", stamp())))?;
    }
    let root = am_tools::workspace::lookup_root(layout, slug);
    let o = convert(&v3, &snap, &v4, root, Some(layout), 30)?;
    write_reports(&dir, "v4-migration-report", &o.report)?;
    if !o.ok {
        // 不一致のDBは使わない。消さずに名前を変えて残す。
        let st = stamp();
        for suffix in ["", "-wal", "-shm"] {
            let p = dir.join(format!("v4.db{suffix}"));
            if p.exists() {
                fs::rename(&p, dir.join(format!("v4.db.failed-{st}{suffix}")))?;
            }
        }
        println!("{slug}: 照合が一致しないので使わない（v4.db.failed-{st} に残した）。報告: {}", dir.join("v4-migration-report.md").display());
        return Ok(false);
    }
    let r = &o.report["imported"];
    println!("{slug}: 移行して照合が一致した（記憶 {}）", r["memories"]);
    Ok(true)
}

pub fn late(layout: &Layout, slug: &str) -> anyhow::Result<()> {
    let dir = layout.store_dir(slug);
    let frozen = fs::read_to_string(dir.join("V3_FROZEN")).context("V3_FROZEN が無い")?;
    let frozen_at = serde_json::from_str::<serde_json::Value>(&frozen)
        .ok()
        .and_then(|v| v["frozen_at"].as_str().map(String::from))
        .context("V3_FROZEN に frozen_at が無い")?;
    let tmp = dir.join(format!("v3-late-snapshot.{}.db", stamp()));
    let sha = v3read::snapshot(&dir.join("store.db"), &tmp)?;
    let snap = v3read::open_read_only(&tmp)?;
    let mut batch = v3read::read(&snap, &dir.join("store.db").display().to_string(), &sha)?;
    let mut store = Store::open(&dir.join("v4.db"), false)?;
    let root = am_tools::workspace::lookup_root(layout, slug);
    let (anchors, _) = anchors_from_sources(&batch, &root.clone().unwrap_or_default());
    batch.anchors = anchors;
    // 既にある記憶への、凍結後の変化（状態・関係）は自動では決めず、一覧で示す。
    let known: std::collections::HashSet<String> =
        store.reader().prepare("SELECT id FROM memories")?.query_map([], |r| r.get(0))?.collect::<Result<_, _>>()?;
    let conflicts: Vec<String> = batch
        .events
        .iter()
        .filter(|e| e.created_at > frozen_at && known.contains(&e.chunk_id))
        .map(|e| format!("{} {} {} by {}", e.created_at, e.event_type, e.chunk_id, e.actor))
        .collect();
    let new = batch.chunks.iter().filter(|c| !known.contains(&c.id)).count();
    let r = store.apply(&cx(), Command::ImportV3Late(Box::new(batch)))?;
    println!("凍結後にv3へ書かれた記憶 {new}件を取り込んだ: {:?}", r.counts);
    if !conflicts.is_empty() {
        println!("既にある記憶への凍結後の変化 {}件（自動では反映しない）:", conflicts.len());
        for c in conflicts {
            println!("- {c}");
        }
    }
    Ok(())
}

pub fn render_md(r: &serde_json::Value) -> String {
    let mut s = String::new();
    s.push_str("# v3 → v4 移行\n\n");
    s.push_str(&format!("- 元: `{}`\n", r["from"].as_str().unwrap_or("")));
    s.push_str(&format!("- snapshot sha256: `{}`\n", r["snapshot_sha256"].as_str().unwrap_or("")));
    s.push_str(&format!("- build: {}\n\n", r["build"].as_str().unwrap_or("")));
    let v = &r["verify"];
    s.push_str(&format!(
        "## 照合: {}\n\n| 項目 | v3 | v4 | 一致 |\n| --- | --- | --- | --- |\n",
        if v["ok"].as_bool() == Some(true) { "全て一致" } else { "**不一致あり**" }
    ));
    for c in v["checks"].as_array().into_iter().flatten() {
        s.push_str(&format!(
            "| {} | {} | {} | {} |\n",
            c["name"].as_str().unwrap_or(""),
            c["v3"],
            c["v4"],
            if c["ok"].as_bool() == Some(true) { "✓" } else { "✗" }
        ));
    }
    if let Some(m) = v["mismatches"].as_array().filter(|m| !m.is_empty()) {
        s.push_str(&format!("\n不一致 {} 件（先頭のみ）:\n", v["mismatch_total"]));
        for x in m {
            s.push_str(&format!("- {}\n", x.as_str().unwrap_or("")));
        }
    }
    s.push_str("\n## 既定値で補った状態\n\n");
    let imp = &r["imported"];
    s.push_str(&format!(
        "- lifecycle行が無くactiveとした記憶: {}\n- visibility行が無くwarmとした記憶: {}\n- 記憶が無いのに付いていたsource_ref: {}\n",
        imp["status_defaulted"], imp["visibility_defaulted"], r["orphan_source_refs"]
    ));
    let e = &r["embedding_equivalence"];
    if e.get("skipped").is_none() {
        s.push_str(&format!(
            "\n## embedding（v3のvectorとv4の計算の一致）\n\n標本 {}・最小 {}・中央値 {}・0.999未満 {}\n",
            e["sampled"], e["min"], e["median"], e["below_0_999"]
        ));
    }
    s.push_str("\n## 取手の照合\n\n");
    let a = &r["anchors"];
    if let Some(skip) = a["skipped"].as_str() {
        s.push_str(&format!("{skip}\n"));
        return s;
    }
    s.push_str(&format!("workspace root: `{}`（git: {}）\n\n", a["workspace_root"].as_str().unwrap_or(""), a["git"]));
    s.push_str("| 種類 | present | moved | same_content_at | recoverable_from_git | missing | not_checked |\n| --- | --- | --- | --- | --- | --- | --- |\n");
    if let Some(t) = a["by_kind_and_status"].as_object() {
        for (kind, st) in t {
            let g = |k: &str| st.get(k).and_then(|v| v.as_u64()).unwrap_or(0);
            s.push_str(&format!(
                "| {kind} | {} | {} | {} | {} | {} | {} |\n",
                g("present"),
                g("moved"),
                g("same_content_at"),
                g("recoverable_from_git"),
                g("missing"),
                g("not_checked")
            ));
        }
    }
    s
}
