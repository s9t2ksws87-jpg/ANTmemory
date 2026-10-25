//! daemon（マシンに1つ）。workspaceごとの書き手・embeddingのworker・接続の登録簿を持つ（§2.1）。
//!
//! 書き手は daemon 1つだけ。workspaceごとの `Mutex<Workspace>` が、そのDBへの書込を直列にする。

use std::collections::{HashMap, HashSet};
use std::io::Write;
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::{mpsc, Arc, Mutex};
use std::time::{Duration, Instant};

use am_domain::{ApplyCtx, Command, ConnectionRecord};
use am_embed::Embedder;
use am_store::{query, Store};
use am_tools::ctx::{new_id, Session, Workspace};
use am_tools::mcp::Conn;
use am_tools::workspace::{lookup_root, Layout, Resolved};
use interprocess::local_socket::{prelude::*, GenericNamespaced, ListenerOptions, Stream};

use crate::ipc::{self, Hello, HelloReply};

/// shimが1つも無い（呼出しが無い）状態がこれだけ続いたら終了する。
const IDLE_EXIT: Duration = Duration::from_secs(30 * 60);
const EMBED_BATCH: i64 = 16;
/// 手入れ（バックアップ・checkpoint・ログの回転）の間隔。
const MAINTAIN_EVERY: Duration = Duration::from_secs(6 * 3600);
/// 計算・公開に失敗した記憶を、次に試すまでの間。
const EMBED_RETRY_AFTER: Duration = Duration::from_secs(10 * 60);

struct Daemon {
    layout: Layout,
    workspaces: Mutex<HashMap<String, Arc<Mutex<Workspace>>>>,
    embedder: Option<Arc<Embedder>>,
    embed_note: Option<String>,
    embed_tx: Mutex<mpsc::Sender<String>>,
    connections: AtomicUsize,
    in_call: AtomicUsize,
    last_active: Mutex<Instant>,
    draining: AtomicBool,
    log: Mutex<Option<std::fs::File>>,
}

impl Daemon {
    fn log(&self, line: &str) {
        let ts = chrono::Utc::now().to_rfc3339_opts(chrono::SecondsFormat::Millis, true);
        if let Ok(mut g) = self.log.lock() {
            if let Some(f) = g.as_mut() {
                writeln!(f, "{ts} {line}").ok();
            }
        }
    }

    fn touch(&self) {
        if let Ok(mut t) = self.last_active.lock() {
            *t = Instant::now();
        }
    }

    /// workspaceのv4 DBを開く。v3のstore.dbだけがある間は開かない（記録がv3とv4に分かれないように）。
    fn workspace(&self, r: &Resolved) -> Result<Arc<Mutex<Workspace>>, String> {
        let mut map = self.workspaces.lock().map_err(|_| "workspaceの表が壊れた".to_string())?;
        // 同じslugで別のフォルダが届いたら、そのstoreを渡さない（別のworkspaceの記憶が混ざらないように。独立レビュー I1）。
        let mismatch = |known: Option<&std::path::Path>| match (known, r.root.as_deref()) {
            (Some(k), Some(got)) if !am_tools::workspace::same_root(k, got) => Some(format!(
                "slug「{}」は {} のstoreだが、{} から接続された。registry（runtime/workspaces.json）の対応を確かめる",
                r.slug,
                k.display(),
                got.display()
            )),
            _ => None,
        };
        if let Some(w) = map.get(&r.slug) {
            let mut cached = lock_ws(w);
            let known = cached.root.clone().or_else(|| lookup_root(&self.layout, &r.slug));
            if let Some(why) = mismatch(known.as_deref()) {
                self.log(&format!("refused: {why}"));
                return Err(why);
            }
            // 名前だけで先に開いていたstoreにも、検証できた実フォルダを渡す。
            // DBだけ合っていてGit・取手の再観測先が不明のままにならないようにする。
            if cached.root.is_none() {
                cached.root = r.root.clone().or(known);
            }
            return Ok(w.clone());
        }
        if let Some(why) = mismatch(lookup_root(&self.layout, &r.slug).as_deref()) {
            self.log(&format!("refused: {why}"));
            return Err(why);
        }
        let dir = self.layout.store_dir(&r.slug);
        let v4 = dir.join("v4.db");
        let store = if v4.exists() {
            Store::open(&v4, false).map_err(|e| format!("{}: {e}", v4.display()))?
        } else if dir.join("store.db").exists() {
            return Err(format!(
                "workspace「{}」はまだv3のstore.dbで動いている。`antmemory cutover freeze` → `antmemory migrate --final` で移してから使う（v3とv4で記録が分かれないように）",
                r.slug
            ));
        } else {
            std::fs::create_dir_all(&dir).map_err(|e| e.to_string())?;
            Store::create_with_model(&v4, &ipc::build_id(), Some(am_store::v3read::MODEL_ID)).map_err(|e| e.to_string())?
        };
        let root = r.root.clone().or_else(|| lookup_root(&self.layout, &r.slug));
        let mut ws = Workspace::new(&r.slug, root, store, self.embedder.clone(), self.embed_note.clone(), &ipc::build_id()).map_err(|e| e.to_string())?;
        let tx = self.embed_tx.lock().map_err(|_| "worker".to_string())?.clone();
        ws.notify_write = Some(Box::new(move |slug: &str, _id: &str| {
            tx.send(slug.to_string()).ok();
        }));
        let w = Arc::new(Mutex::new(ws));
        map.insert(r.slug.clone(), w.clone());
        self.log(&format!("open workspace slug={} source={}", r.slug, r.source));
        if let Ok(tx) = self.embed_tx.lock() {
            tx.send(r.slug.clone()).ok();
        }
        Ok(w)
    }
}

/// 開いている全workspaceの手入れ。失敗はログに残し、daemonは止めない。
fn maintenance(d: &Daemon) {
    let list: Vec<(String, Arc<Mutex<Workspace>>)> =
        d.workspaces.lock().map(|m| m.iter().map(|(k, v)| (k.clone(), v.clone())).collect()).unwrap_or_default();
    for (slug, w) in list {
        let dir = d.layout.store_dir(&slug);
        let ws = lock_ws(&w);
        match crate::maintain::quick_check(&ws.store) {
            Ok(p) if p.is_empty() => {}
            Ok(p) => {
                // 壊れた版をバックアップにしない（古い良い版を押し出さない）。
                d.log(&format!("INTEGRITY slug={slug}: 点検で問題 {}件。バックアップを取らない: {}", p.len(), p.join("; ")));
                continue;
            }
            Err(e) => {
                d.log(&format!("quick_check failed slug={slug}: {e}"));
                continue;
            }
        }
        match crate::maintain::daily_backup(&dir, &ws.store) {
            Ok(Some(p)) => d.log(&format!("backup slug={slug} → {}", p.display())),
            Ok(None) => {}
            Err(e) => d.log(&format!("backup failed slug={slug}: {e}")),
        }
        match crate::maintain::checkpoint(&ws.store) {
            Ok(cp) if !cp.complete() => {
                d.log(&format!("checkpoint deferred slug={slug}: {}/{}ページ（読んでいる接続がある）", cp.done_pages, cp.wal_pages))
            }
            Ok(_) => {}
            Err(e) => d.log(&format!("checkpoint failed slug={slug}: {e}")),
        }
    }
    let path = ipc::daemon_log(&d.layout);
    if let Ok(mut g) = d.log.lock() {
        if std::fs::metadata(&path).map(|m| m.len() > crate::maintain::LOG_MAX_BYTES).unwrap_or(false) {
            *g = None;
            crate::maintain::rotate(&path);
            *g = std::fs::OpenOptions::new().create(true).append(true).open(&path).ok();
        }
    }
}

fn lock_ws(w: &Arc<Mutex<Workspace>>) -> std::sync::MutexGuard<'_, Workspace> {
    match w.lock() {
        Ok(g) => g,
        Err(p) => p.into_inner(),
    }
}

/// vectorの無い記憶を、少しずつ計算して公開する。計算の間はworkspaceのlockを持たない。
/// 失敗した記憶はしばらく除いて次へ進む。1回のバッチで1件も公開できなければ、そのworkspaceは打ち切って
/// 次のworkspaceへ移る（同じ失敗を回し続けて、他のworkspaceを待たせない。R4）。
fn embed_worker(d: Arc<Daemon>, rx: mpsc::Receiver<String>) {
    let Some(e) = d.embedder.clone() else { return };
    let mut pending: HashSet<String> = HashSet::new();
    let mut failed: HashMap<(String, String), Instant> = HashMap::new();
    loop {
        if !wait_for_embedding_work(&rx, &mut pending, &mut failed, EMBED_RETRY_AFTER) {
            return;
        }
        let slugs: Vec<String> = pending.drain().collect();
        for slug in slugs {
            let Some(w) = d.workspaces.lock().ok().and_then(|m| m.get(&slug).cloned()) else {
                continue;
            };
            loop {
                let skip = failed.keys().filter(|(s, _)| *s == slug).count() as i64;
                let (model, batch) = {
                    let ws = lock_ws(&w);
                    let b = query::missing_embeddings(ws.conn(), &ws.model_id, EMBED_BATCH + skip).unwrap_or_default();
                    (ws.model_id.clone(), b)
                };
                let batch: Vec<_> = batch.into_iter().filter(|r| !failed.contains_key(&(slug.clone(), r.0.clone()))).take(EMBED_BATCH as usize).collect();
                if batch.is_empty() {
                    break;
                }
                let n = batch.len();
                let mut published = 0usize;
                for (id, topic, tags_json, content, hash) in batch {
                    let tags: Vec<String> = serde_json::from_str(&tags_json).unwrap_or_default();
                    let text = am_embed::prepare_text(&topic, &tags, &content);
                    let v = match e.embed_document(&text) {
                        Ok(v) => v,
                        Err(err) => {
                            d.log(&format!("embed failed slug={slug} id={id}: {err}"));
                            failed.insert((slug.clone(), id), Instant::now());
                            continue;
                        }
                    };
                    let mut ws = lock_ws(&w);
                    let cx = ApplyCtx { actor: "daemon-embed".into(), build: ipc::build_id(), ..Default::default() };
                    let cmd = Command::PublishEmbedding {
                        id: id.clone(),
                        model_id: model.clone(),
                        content_hash: hash,
                        vector: am_embed::to_blob(&v),
                        computed_by: "v4-ort".into(),
                    };
                    match ws.apply(&cx, cmd) {
                        Ok(r) if r.detail.get("discarded").is_none() => published += 1,
                        Ok(r) => {
                            d.log(&format!("embedding discarded slug={slug} id={id}: {}", r.detail["discarded"]));
                            failed.insert((slug.clone(), id), Instant::now());
                        }
                        Err(err) => {
                            d.log(&format!("publish embedding failed slug={slug} id={id}: {err}"));
                            failed.insert((slug.clone(), id), Instant::now());
                        }
                    }
                }
                if published == 0 {
                    d.log(&format!("embedding: slug={slug} で進まない（{n}件とも失敗）。{}分後に再試行する", EMBED_RETRY_AFTER.as_secs() / 60));
                    break;
                }
                if (n as i64) < EMBED_BATCH {
                    break;
                }
            }
        }
    }
}

/// 期限になったworkspace自身を再投入する。新しい書込や他のworkspaceの通知を必要としない。
fn wait_for_embedding_work(
    rx: &mpsc::Receiver<String>,
    pending: &mut HashSet<String>,
    failed: &mut HashMap<(String, String), Instant>,
    retry_after: Duration,
) -> bool {
    let next = failed.values().map(|at| retry_after.saturating_sub(at.elapsed())).min();
    let received = match next {
        Some(wait) => rx.recv_timeout(wait),
        None => rx.recv().map_err(|_| mpsc::RecvTimeoutError::Disconnected),
    };
    match received {
        Ok(slug) => {
            pending.insert(slug);
        }
        Err(mpsc::RecvTimeoutError::Timeout) => {}
        Err(mpsc::RecvTimeoutError::Disconnected) => return false,
    }
    while let Ok(slug) = rx.try_recv() {
        pending.insert(slug);
    }
    failed.retain(|(slug, _), at| {
        if at.elapsed() >= retry_after {
            pending.insert(slug.clone());
            false
        } else {
            true
        }
    });
    true
}

fn handle(d: Arc<Daemon>, stream: Stream) {
    let (mut r, mut send) = ipc::split(stream);
    let Ok(Some(first)) = ipc::read_line(&mut r) else {
        return;
    };
    let v: serde_json::Value = match serde_json::from_str(&first) {
        Ok(v) => v,
        Err(e) => {
            ipc::send_line(&mut send, &serde_json::json!({ "am": "error", "reason": format!("最初の行がJSONでない: {e}") })).ok();
            return;
        }
    };
    // 別の配置のshim・hookは受け付けない（同じslugでも、別の配置のDBを読み書きしないため。R7）。
    let mine = ipc::deployment_id(&d.layout);
    let theirs = v.get("deployment").and_then(|x| x.as_str()).unwrap_or("");
    if theirs != mine {
        let reason = format!(
            "配置が違う（接続元={theirs}・daemon={mine} root={}）。ANT_MEMORY_ROOT か ANT_MEMORY_PIPE を確かめる",
            d.layout.antmemory_root.display()
        );
        d.log(&format!("refused: {reason}"));
        // helloにもhook・status・drainにも読める形（am / ok / reason）で1行だけ返す。
        ipc::send_line(&mut send, &reply_err(&reason)).ok();
        return;
    }
    match v.get("am").and_then(|x| x.as_str()) {
        Some("hello") => {}
        Some("hook") => {
            let r = hook(&d, &v);
            ipc::send_line(&mut send, &serde_json::json!({ "am": "hook_ok", "ok": r.is_ok(), "note": r.unwrap_or_else(|e| e) })).ok();
            return;
        }
        Some("status") => {
            let ws: Vec<String> = d.workspaces.lock().map(|m| m.keys().cloned().collect()).unwrap_or_default();
            ipc::send_line(
                &mut send,
                &serde_json::json!({ "am": "status", "build": ipc::build_id(), "stamp": ipc::build_stamp(), "pid": std::process::id(),
                    "pipe": ipc::pipe_name(&d.layout), "root": d.layout.antmemory_root.display().to_string(),
                    "connections": d.connections.load(Ordering::SeqCst), "workspaces": ws,
                    "embedding": d.embedder.is_some(), "embed_note": d.embed_note }),
            )
            .ok();
            return;
        }
        Some("drain") => {
            d.draining.store(true, Ordering::SeqCst);
            ipc::send_line(&mut send, &serde_json::json!({ "am": "drain_ok" })).ok();
            return;
        }
        _ => {
            ipc::send_line(&mut send, &serde_json::json!({ "am": "error", "reason": "知らない制御" })).ok();
            return;
        }
    }
    let mut hello: Hello = match serde_json::from_value(v) {
        Ok(h) => h,
        Err(e) => {
            ipc::send_line(&mut send, &reply_err(&format!("helloを読めない: {e}"))).ok();
            return;
        }
    };
    if hello.protocol != ipc::PROTOCOL {
        ipc::send_line(&mut send, &reply_err(&format!("protocolが合わない: shim={} daemon={}", hello.protocol, ipc::PROTOCOL))).ok();
        return;
    }
    if d.draining.load(Ordering::SeqCst) {
        ipc::send_line(&mut send, &reply_err("draining")).ok();
        return;
    }
    if hello.stamp > ipc::build_stamp() {
        // shimの方が新しい：新しい呼出しを止め、処理中の書込を終えてから終了する。
        d.log(&format!("newer shim build={} → drain", hello.build));
        d.draining.store(true, Ordering::SeqCst);
        ipc::send_line(&mut send, &reply_err("draining")).ok();
        return;
    }
    hello.workspace = match am_tools::workspace::refresh_binding(&d.layout, std::path::Path::new(&hello.cwd), &hello.workspace) {
        Ok(r) => r,
        Err(e) => {
            ipc::send_line(&mut send, &reply_err(&e)).ok();
            return;
        }
    };
    let ws = match d.workspace(&hello.workspace) {
        Ok(w) => w,
        Err(e) => {
            d.log(&format!("hello refused slug={}: {e}", hello.workspace.slug));
            ipc::send_line(&mut send, &reply_err(&e)).ok();
            return;
        }
    };
    let conn_id = format!("c{}", new_id(&hello.client));
    let mut session = Session::new(&conn_id, &hello.client, &ipc::build_id());
    session.cwd = Some(hello.cwd.clone());
    if hello.workspace.source.starts_with("fallback(") {
        session.workspace_notice = Some(am_tools::workspace::FALLBACK_NOTICE.into());
    }
    session.conversation = hello.conversation.clone();
    session.model = hello.model.clone();
    session.provider = hello.provider.clone();
    let level = {
        let mut g = lock_ws(&ws);
        let level = session.level(&g);
        let rec = ConnectionRecord {
            conn_id: conn_id.clone(),
            client: hello.client.clone(),
            cwd: Some(hello.cwd.clone()),
            shim_pid: Some(hello.shim_pid as i64),
            parents: serde_json::json!({ "chain": hello.parents, "env_names": hello.env_names, "workspace_source": hello.workspace.source }),
            conversation: hello.conversation.clone(),
            level: level.as_str().into(),
            build: hello.build.clone(),
        };
        if let Err(e) = g.apply(&session.apply_ctx(), Command::RecordConnection(rec)) {
            d.log(&format!("record connection failed: {e}"));
        }
        level
    };
    d.log(&format!(
        "conn {conn_id} client={} slug={} level={} conversation={:?} env={:?}",
        hello.client,
        hello.workspace.slug,
        level.as_str(),
        hello.conversation,
        hello.env_names
    ));
    let ok = HelloReply {
        am: "hello_ok".into(),
        ok: true,
        build: ipc::build_id(),
        stamp: ipc::build_stamp(),
        conn_id: Some(conn_id.clone()),
        reason: None,
        level: Some(level.as_str().into()),
    };
    if ipc::send_line(&mut send, &ok).is_err() {
        return;
    }
    let open = Counted::new(&d.connections);
    let mut conn = Conn { ws, session };
    while let Ok(Some(line)) = ipc::read_line(&mut r) {
        if line.trim().is_empty() {
            continue;
        }
        d.touch();
        let call = Counted::new(&d.in_call);
        let out = conn.handle_line(&line);
        drop(call);
        if let Some(o) = out {
            let mut s = o;
            s.push('\n');
            if send.write_all(s.as_bytes()).and_then(|_| send.flush()).is_err() {
                break;
            }
        }
    }
    drop(open);
    d.touch();
    d.log(&format!("conn {conn_id} closed"));
}

/// 数を1つ増やし、抜けるときに必ず戻す（panicで抜けても drain が待ち続けないように）。
struct Counted<'a>(&'a AtomicUsize);

impl<'a> Counted<'a> {
    fn new(n: &'a AtomicUsize) -> Self {
        n.fetch_add(1, Ordering::SeqCst);
        Counted(n)
    }
}

impl Drop for Counted<'_> {
    fn drop(&mut self) {
        self.0.fetch_sub(1, Ordering::SeqCst);
    }
}

fn reply_err(reason: &str) -> HelloReply {
    HelloReply { am: "hello_err".into(), ok: false, build: ipc::build_id(), stamp: ipc::build_stamp(), conn_id: None, reason: Some(reason.into()), level: None }
}

fn hook(d: &Daemon, v: &serde_json::Value) -> Result<String, String> {
    let r: Resolved = serde_json::from_value(v.get("workspace").cloned().unwrap_or_default()).map_err(|e| e.to_string())?;
    let ev: am_tools::hooks::HookEvent = serde_json::from_value(v.get("event").cloned().unwrap_or_default()).map_err(|e| e.to_string())?;
    let ws = d.workspace(&r)?;
    let mut g = lock_ws(&ws);
    let note = am_tools::hooks::apply(&mut g, &ev, &ipc::build_id())?;
    d.log(&format!("hook {} slug={} {note}", ev.session_id, r.slug));
    Ok(note)
}

pub fn run() -> anyhow::Result<()> {
    let layout = ipc::layout()?;
    let dir = ipc::runtime_dir(&layout);
    std::fs::create_dir_all(&dir)?;
    // daemonは1つだけ。lockを取れなければ、別のdaemonが動いている。
    let lock = std::fs::OpenOptions::new().create(true).truncate(false).write(true).open(dir.join("daemon.lock"))?;
    if lock.try_lock().is_err() {
        eprintln!("[antmemory] 別のdaemonが動いている");
        return Ok(());
    }
    let log = std::fs::OpenOptions::new().create(true).append(true).open(ipc::daemon_log(&layout)).ok();
    let (embedder, embed_note) = match Embedder::load(&am_embed::model_dir(&layout.antmemory_root)) {
        Ok(e) => (Some(Arc::new(e)), None),
        Err(e) => (None, Some(format!("embeddingのモデルを読めない: {e}"))),
    };
    let (tx, rx) = mpsc::channel();
    let d = Arc::new(Daemon {
        layout,
        workspaces: Mutex::new(HashMap::new()),
        embedder,
        embed_note,
        embed_tx: Mutex::new(tx),
        connections: AtomicUsize::new(0),
        in_call: AtomicUsize::new(0),
        last_active: Mutex::new(Instant::now()),
        draining: AtomicBool::new(false),
        log: Mutex::new(log),
    });
    d.log(&format!("start pid={} build={} pipe={} embedding={}", std::process::id(), ipc::build_id(), ipc::pipe_name(&d.layout), d.embedder.is_some()));
    {
        let d2 = d.clone();
        std::thread::spawn(move || embed_worker(d2, rx));
    }
    {
        // 手入れ：起動の2分後と、その後6時間ごとに、開いているworkspaceのバックアップ・checkpoint、ログの回転。
        let d2 = d.clone();
        std::thread::spawn(move || {
            std::thread::sleep(Duration::from_secs(120));
            loop {
                maintenance(&d2);
                std::thread::sleep(MAINTAIN_EVERY);
            }
        });
    }
    {
        // 待機中の終了と、drain（処理中の呼出しが終わったら終了）。
        let d2 = d.clone();
        std::thread::spawn(move || loop {
            std::thread::sleep(Duration::from_millis(200));
            if d2.draining.load(Ordering::SeqCst) && d2.in_call.load(Ordering::SeqCst) == 0 {
                d2.log("drained → exit");
                std::process::exit(0);
            }
            let idle = d2.last_active.lock().map(|t| t.elapsed()).unwrap_or_default();
            if d2.connections.load(Ordering::SeqCst) == 0 && idle > IDLE_EXIT {
                d2.log("idle → exit");
                std::process::exit(0);
            }
        });
    }
    let name = ipc::pipe_name(&d.layout).to_ns_name::<GenericNamespaced>()?;
    let listener = ListenerOptions::new().name(name).create_sync()?;
    std::fs::remove_file(dir.join("spawn.lock")).ok();
    for conn in listener.incoming() {
        match conn {
            Ok(s) => {
                let d2 = d.clone();
                std::thread::spawn(move || handle(d2, s));
            }
            Err(e) => d.log(&format!("accept failed: {e}")),
        }
    }
    drop(lock);
    Ok(())
}

#[cfg(test)]
mod retry_tests {
    use super::*;
    #[test]
    fn retries_failed_workspace_without_new_notifications() {
        let (tx, rx) = mpsc::channel();
        let mut pending = HashSet::new();
        let mut failed = HashMap::from([(("A".to_string(), "id".to_string()), Instant::now())]);
        let retry = Duration::from_millis(30);
        tx.send("B".into()).unwrap();
        assert!(wait_for_embedding_work(&rx, &mut pending, &mut failed, retry));
        assert!(pending.remove("B"));
        // OSのスケジューリングで既に期限になった場合も正しい。未到来なら通知なしで待つ。
        if !pending.contains("A") {
            assert!(wait_for_embedding_work(&rx, &mut pending, &mut failed, retry));
        }
        assert!(pending.contains("A") && failed.is_empty());
        drop(tx);
        assert!(!wait_for_embedding_work(&rx, &mut pending, &mut failed, retry));
    }
}
