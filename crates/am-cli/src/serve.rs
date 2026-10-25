//! shim（clientごとに1つ）。stdioのMCPを、daemonのnamed pipeへそのまま中継する（§2.3）。
//!
//! shimは書込をしない（書き手をdaemon 1つに保つ）。daemonにつながらない間は、
//! initialize・tools/list だけを自分で答え、toolの呼出しには理由つきのエラーを返す。

use std::collections::HashSet;
use std::io::{BufRead, BufReader, Write};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use am_tools::workspace::Resolved;
use crate::ipc::Pipe;
use serde_json::{json, Value};

use crate::ipc::{self, Hello, HelloReply};

const REPLAY_ID: &str = "am-replay-initialize";

struct Shared {
    stdout: Mutex<std::io::Stdout>,
    pending: Mutex<HashSet<String>>,
    connected: Mutex<bool>,
}

impl Shared {
    fn out(&self, line: &str) {
        let mut o = match self.stdout.lock() {
            Ok(o) => o,
            Err(p) => p.into_inner(),
        };
        o.write_all(line.as_bytes()).ok();
        o.write_all(b"\n").ok();
        o.flush().ok();
    }
}

fn id_key(v: &Value) -> Option<String> {
    v.get("id").filter(|i| !i.is_null()).map(|i| i.to_string())
}

/// daemonが使えないときの応答。toolの呼出しは、MCPのtoolエラーとして理由を返す（黙って成功にしない）。
fn unavailable(msg: &Value, reason: &str) -> Option<String> {
    let id = msg.get("id")?.clone();
    let method = msg.get("method").and_then(|m| m.as_str()).unwrap_or("");
    let result = match method {
        "initialize" => {
            let asked = msg.pointer("/params/protocolVersion").and_then(|v| v.as_str()).unwrap_or("");
            let version = if am_tools::mcp::SUPPORTED.contains(&asked) { asked } else { am_tools::mcp::SUPPORTED[0] };
            json!({ "protocolVersion": version, "capabilities": { "tools": { "listChanged": false } },
                    "serverInfo": { "name": "ant-memory", "version": am_tools::mcp::SERVER_VERSION },
                    "instructions": am_tools::tools::INSTRUCTIONS })
        }
        "tools/list" => am_tools::tools::list(),
        "ping" => json!({}),
        "tools/call" => json!({ "content": [{ "type": "text", "text": format!("DAEMON_UNAVAILABLE: {reason}") }], "isError": true }),
        _ => return Some(json!({ "jsonrpc": "2.0", "id": id, "error": { "code": -32000, "message": format!("DAEMON_UNAVAILABLE: {reason}") } }).to_string()),
    };
    Some(json!({ "jsonrpc": "2.0", "id": id, "result": result }).to_string())
}

fn lost(msg_id: &str) -> String {
    let id: Value = serde_json::from_str(msg_id).unwrap_or(Value::Null);
    json!({ "jsonrpc": "2.0", "id": id, "result": { "content": [{ "type": "text",
        "text": "DAEMON_UNAVAILABLE: 処理中にdaemonとの接続が切れた。この呼出しが記録されたかは不明。read_context / list_context で再観測してから続ける" }],
        "isError": true } })
    .to_string()
}

fn hello_msg(resolved: &Resolved, deployment: String) -> Hello {
    let names: Vec<String> = std::env::vars().map(|(k, _)| k).collect();
    let client = am_tools::workspace::infer_client(&|k| std::env::var(k).ok(), &names);
    // 会話の識別子：clientが自分の環境変数で渡しているときだけ使う（推測で結び付けない）。
    let conversation = match (client.as_str(), std::env::var("CLAUDE_CODE_SESSION_ID")) {
        ("claude-code", Ok(s)) if !s.trim().is_empty() => Some(("claude-code".to_string(), s)),
        _ => None,
    };
    let env_names: Vec<String> = std::env::vars()
        .map(|(k, _)| k)
        .filter(|k| {
            let u = k.to_uppercase();
            ["CLAUDE", "CODEX", "ANTIGRAVITY", "GEMINI", "SESSION", "THREAD", "CONVERSATION"].iter().any(|p| u.contains(p))
        })
        .collect();
    Hello {
        am: "hello".into(),
        protocol: ipc::PROTOCOL,
        build: ipc::build_id(),
        stamp: ipc::build_stamp(),
        client,
        cwd: std::env::current_dir().map(|p| p.display().to_string()).unwrap_or_default(),
        workspace: resolved.clone(),
        shim_pid: std::process::id(),
        parents: ipc::parent_chain(),
        conversation,
        model: std::env::var("ANT_MEMORY_MODEL").ok(),
        provider: std::env::var("ANT_MEMORY_PROVIDER").ok(),
        env_names,
        deployment,
    }
}

/// daemonへつなぎ、helloを交わす。daemonの方が古ければ、入れ替わるのを待つ。
fn open(layout: &am_tools::workspace::Layout, hello: &Hello) -> Result<(BufReader<Pipe>, Pipe), String> {
    let start = Instant::now();
    loop {
        let stream = ipc::connect_or_spawn(layout).map_err(|e| e.to_string())?;
        let (mut r, mut send) = ipc::split(stream);
        ipc::send_line(&mut send, hello).map_err(|e| e.to_string())?;
        let line = ipc::read_line(&mut r).map_err(|e| e.to_string())?.ok_or("daemonがhelloに答えずに閉じた")?;
        let reply: HelloReply = serde_json::from_str(&line).map_err(|e| e.to_string())?;
        if reply.ok {
            return Ok((r, send));
        }
        let reason = reply.reason.unwrap_or_default();
        if reason == "draining" && start.elapsed() < Duration::from_secs(8) {
            std::thread::sleep(Duration::from_millis(150));
            continue;
        }
        return Err(reason);
    }
}

pub fn run() -> anyhow::Result<()> {
    let layout = ipc::layout()?;
    let cwd = std::env::current_dir()?;
    let resolved = am_tools::workspace::resolve(&layout, &cwd, &|k| std::env::var(k).ok()).map_err(anyhow::Error::msg)?;
    eprintln!("[antmemory] workspace=\"{}\" ({})", resolved.slug, resolved.source);
    let hello = hello_msg(&resolved, ipc::deployment_id(&layout));
    let shared = Arc::new(Shared { stdout: Mutex::new(std::io::stdout()), pending: Mutex::new(HashSet::new()), connected: Mutex::new(false) });
    let mut send: Option<Pipe> = None;
    let mut init_line: Option<String> = None;
    let mut initialized_line: Option<String> = None;
    let mut last_error: Option<String> = None;

    let stdin = std::io::stdin();
    for line in stdin.lock().lines() {
        let line = line?;
        if line.trim().is_empty() {
            continue;
        }
        let msg: Value = serde_json::from_str(&line).unwrap_or(Value::Null);
        let method = msg.get("method").and_then(|m| m.as_str()).unwrap_or("").to_string();
        if method == "initialize" {
            init_line = Some(line.clone());
        } else if method == "notifications/initialized" {
            initialized_line = Some(line.clone());
        }
        let alive = *shared.connected.lock().unwrap_or_else(|p| p.into_inner());
        if !alive {
            send = None;
        }
        if send.is_none() {
            match open(&layout, &hello) {
                Ok((r, mut s)) => {
                    // つなぎ直したdaemonは、この接続の initialize を知らない。応答は捨てて送り直す。
                    if method != "initialize" {
                        if let Some(init) = &init_line {
                            if let Ok(mut v) = serde_json::from_str::<Value>(init) {
                                v["id"] = json!(REPLAY_ID);
                                ipc::send_line(&mut s, &v).ok();
                            }
                            if let Some(n) = &initialized_line {
                                s.write_all(format!("{n}\n").as_bytes()).ok();
                            }
                        }
                    }
                    *shared.connected.lock().unwrap_or_else(|p| p.into_inner()) = true;
                    let sh = shared.clone();
                    std::thread::spawn(move || pump(r, sh));
                    send = Some(s);
                    last_error = None;
                }
                Err(e) => {
                    last_error = Some(e);
                }
            }
        }
        let Some(s) = send.as_mut() else {
            let reason = last_error.clone().unwrap_or_else(|| "daemonにつながらない".into());
            if let Some(out) = unavailable(&msg, &reason) {
                shared.out(&out);
            }
            continue;
        };
        if let Some(k) = id_key(&msg) {
            shared.pending.lock().unwrap_or_else(|p| p.into_inner()).insert(k);
        }
        if s.write_all(format!("{line}\n").as_bytes()).and_then(|_| s.flush()).is_err() {
            *shared.connected.lock().unwrap_or_else(|p| p.into_inner()) = false;
            send = None;
            if let Some(k) = id_key(&msg) {
                shared.pending.lock().unwrap_or_else(|p| p.into_inner()).remove(&k);
                shared.out(&lost(&k));
            }
        }
    }
    // stdinが閉じても、答えを待っている呼出しの返事は届ける（上限30秒）。
    let start = Instant::now();
    while !shared.pending.lock().unwrap_or_else(|p| p.into_inner()).is_empty() && start.elapsed() < Duration::from_secs(30) {
        std::thread::sleep(Duration::from_millis(20));
    }
    Ok(())
}

/// daemon → stdout。切れたら、答えの無い呼出しに理由つきのエラーを返す。
fn pump(mut r: BufReader<Pipe>, sh: Arc<Shared>) {
    while let Ok(Some(line)) = ipc::read_line(&mut r) {
                let v: Value = serde_json::from_str(&line).unwrap_or(Value::Null);
                if v.get("id").and_then(|i| i.as_str()) == Some(REPLAY_ID) {
                    continue;
                }
                if let Some(k) = id_key(&v) {
                    sh.pending.lock().unwrap_or_else(|p| p.into_inner()).remove(&k);
                }
                sh.out(&line);
    }
    *sh.connected.lock().unwrap_or_else(|p| p.into_inner()) = false;
    let ids: Vec<String> = sh.pending.lock().unwrap_or_else(|p| p.into_inner()).drain().collect();
    for k in ids {
        sh.out(&lost(&k));
    }
}

/// テスト用：daemonを通さず、このプロセスの中で1つのv4 DBを開いて答える。
/// 書き手が2つにならないよう、daemonが同じDBを使っていない時だけ使う。
pub fn run_direct(db: &std::path::Path, root: Option<std::path::PathBuf>, client: &str) -> anyhow::Result<()> {
    let ws = open_direct(db, root)?;
    let mut conn = am_tools::mcp::Conn { ws: Arc::new(Mutex::new(ws)), session: am_tools::Session::new("direct", client, &ipc::build_id()) };
    conn.session.cwd = std::env::current_dir().ok().map(|p| p.display().to_string());
    let stdin = std::io::stdin();
    let mut out = std::io::stdout();
    for line in stdin.lock().lines() {
        let line = line?;
        if let Some(o) = conn.handle_line(&line) {
            writeln!(out, "{o}")?;
            out.flush()?;
        }
    }
    Ok(())
}

pub fn open_direct(db: &std::path::Path, root: Option<std::path::PathBuf>) -> anyhow::Result<am_tools::Workspace> {
    let store = am_store::Store::open(db, false)?;
    let layout = ipc::layout().ok();
    let (e, note) = match layout.as_ref().map(|l| am_embed::Embedder::load(&am_embed::model_dir(&l.antmemory_root))) {
        Some(Ok(e)) => (Some(Arc::new(e)), None),
        Some(Err(e)) => (None, Some(e.to_string())),
        None => (None, Some("ANTmemoryのrootが分からない".into())),
    };
    let slug = db.parent().and_then(|p| p.file_name()).map(|s| s.to_string_lossy().into_owned()).unwrap_or_default();
    Ok(am_tools::Workspace::new(&slug, root, store, e, note, &ipc::build_id())?)
}
