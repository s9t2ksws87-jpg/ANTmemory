//! 人間用の読取り専用ビューア。Agent toolを呼ばず、閲覧を検索学習や作業の焦点へ混ぜない。
use std::collections::BTreeMap;
use std::path::PathBuf;

use am_store::{query, Store};
use am_tools::{ctx::Workspace, workspace::Layout};
use anyhow::{anyhow, bail, Context};
use rusqlite::{params_from_iter, types::Value as SqlValue};
use serde_json::{json, Value};
use tiny_http::{Header, Method, Response, Server};

type Params = BTreeMap<String, String>;

fn header(key: &str, value: &str) -> Header {
    Header::from_bytes(key, value).expect("static HTTP header")
}

pub fn run(layout: Layout, port: u16, open: bool) -> anyhow::Result<()> {
    let server = match Server::http(("127.0.0.1", port)) {
        Ok(s) => s,
        Err(e) => {
            if port != 0 && existing_viewer(port, &layout) {
                let url = format!("http://127.0.0.1:{port}");
                println!("起動済みのANTmemory Viewer: {url}");
                if open {
                    open_browser(&url);
                }
                return Ok(());
            }
            return Err(anyhow!("ビューアを起動できません: {e}（別のポートは --port 3738）"));
        }
    };
    let address = server.server_addr().to_ip().context("TCP address")?;
    let authority = format!("127.0.0.1:{}", address.port());
    let url = format!("http://{authority}");
    println!("ANTmemory Viewer: {url}\n読取り専用。終了は Ctrl+C");
    if open {
        open_browser(&url);
    }
    for request in server.incoming_requests() {
        let host = request.headers().iter().find(|h| h.field.equiv("Host")).map(|h| h.value.as_str());
        let origin = request.headers().iter().find(|h| h.field.equiv("Origin")).map(|h| h.value.as_str());
        let local_host = format!("localhost:{}", address.port());
        let local_origin = format!("http://{local_host}");
        let permitted = matches!(host, Some(h) if h == authority || h == local_host) && origin.is_none_or(|o| o == url || o == local_origin);
        let (status, mime, bytes) = if !permitted {
            (403, "application/json; charset=utf-8", br#"{"error":"local origin required"}"#.to_vec())
        } else if request.method() != &Method::Get {
            (405, "application/json; charset=utf-8", br#"{"error":"read only"}"#.to_vec())
        } else {
            route(&layout, request.url())
        };
        let response = Response::from_data(bytes)
            .with_status_code(status)
            .with_header(header("Content-Type", mime))
            .with_header(header("Cache-Control", "no-store"))
            .with_header(header("X-Content-Type-Options", "nosniff"))
            .with_header(header("Referrer-Policy", "no-referrer"))
            .with_header(header("Content-Security-Policy", "default-src 'self'; script-src 'self'; style-src 'self'; img-src 'self' data:; connect-src 'self'; frame-ancestors 'none'; base-uri 'none'; form-action 'none'"));
        let _sent = request.respond(response);
    }
    Ok(())
}

fn open_browser(url: &str) {
    #[cfg(windows)]
    {
        use std::os::windows::process::CommandExt;
        let _opened = std::process::Command::new("rundll32.exe").creation_flags(0x08000000).args(["url.dll,FileProtocolHandler", url]).spawn();
    }
    #[cfg(not(windows))]
    let _opened = std::process::Command::new("xdg-open").arg(url).spawn();
}

fn existing_viewer(port: u16, layout: &Layout) -> bool {
    use std::io::{Read, Write};
    let Ok(mut stream) = std::net::TcpStream::connect_timeout(&([127, 0, 0, 1], port).into(), std::time::Duration::from_millis(500)) else { return false };
    if stream.set_read_timeout(Some(std::time::Duration::from_secs(1))).is_err() {
        return false;
    }
    if write!(stream, "GET /api/ping HTTP/1.1\r\nHost: 127.0.0.1:{port}\r\nConnection: close\r\n\r\n").is_err() {
        return false;
    }
    let mut bytes = vec![];
    if stream.take(8192).read_to_end(&mut bytes).is_err() {
        return false;
    }
    let text = String::from_utf8_lossy(&bytes);
    text.starts_with("HTTP/1.1 200")
        && text.contains("\"app\":\"ANTmemory.Viewer\"")
        && text.contains(&format!("\"deployment\":\"{}\"", crate::ipc::deployment_id(layout)))
}

fn decode(s: &str) -> anyhow::Result<String> {
    let bytes = s.as_bytes();
    let mut out = vec![];
    let mut i = 0;
    while i < bytes.len() {
        match bytes[i] {
            b'%' => {
                let pair = bytes.get(i + 1..i + 3).context("URLの%エンコードが不完全です")?;
                let a = (pair[0] as char).to_digit(16).context("URLの%エンコードが不正です")?;
                let b = (pair[1] as char).to_digit(16).context("URLの%エンコードが不正です")?;
                out.push((a * 16 + b) as u8);
                i += 3;
            }
            b'+' => {
                out.push(b' ');
                i += 1;
            }
            b => {
                out.push(b);
                i += 1;
            }
        }
    }
    Ok(String::from_utf8(out)?)
}

fn route(layout: &Layout, url: &str) -> (u16, &'static str, Vec<u8>) {
    let (path, query_string) = url.split_once('?').unwrap_or((url, ""));
    let asset: Option<(&str, &[u8])> = match path {
        "/" | "/index.html" => Some(("text/html; charset=utf-8", include_bytes!("../viewer/index.html"))),
        "/app.js" => Some(("text/javascript; charset=utf-8", include_bytes!("../viewer/app.js"))),
        "/styles.css" => Some(("text/css; charset=utf-8", include_bytes!("../viewer/styles.css"))),
        "/icon.svg" | "/favicon.ico" => Some(("image/svg+xml", br##"<svg xmlns="http://www.w3.org/2000/svg" viewBox="0 0 24 24"><rect width="24" height="24" rx="4" fill="#181818"/><path d="M4 19 12 4l8 15H4M7 13h10" stroke="#4daafc" stroke-width="2" fill="none"/></svg>"##)),
        _ => None,
    };
    if let Some((mime, data)) = asset {
        return (200, mime, data.to_vec());
    }
    let result = (|| {
        let mut p = Params::new();
        for pair in query_string.split('&').filter(|s| !s.is_empty()) {
            let (key, value) = pair.split_once('=').unwrap_or((pair, ""));
            p.insert(decode(key)?, decode(value)?);
        }
        api(layout, path, &p)
    })();
    match result {
        Ok(value) => (200, "application/json; charset=utf-8", value.to_string().into_bytes()),
        Err(e) => (400, "application/json; charset=utf-8", json!({"error":e.to_string()}).to_string().into_bytes()),
    }
}

fn param<'a>(p: &'a Params, key: &str) -> &'a str {
    p.get(key).map(String::as_str).unwrap_or("")
}

fn workspace(layout: &Layout, slug: &str) -> anyhow::Result<Workspace> {
    if slug.is_empty() || slug == "." || slug == ".." || slug.contains(['/', '\\', ':']) {
        bail!("作業空間の名前が不正です");
    }
    let stores = std::fs::canonicalize(layout.stores())?;
    let path = std::fs::canonicalize(layout.store_dir(slug).join("v4.db")).context("V4の記憶が見つかりません")?;
    if !path.starts_with(&stores) {
        bail!("作業空間の外のDBは開けません");
    }
    let store = Store::open(&path, true)?;
    store.reader().pragma_update(None, "query_only", true)?;
    store.reader().busy_timeout(std::time::Duration::from_secs(2))?;
    let registry: BTreeMap<String, PathBuf> = std::fs::read(layout.registry()).ok().and_then(|b| serde_json::from_slice(&b).ok()).unwrap_or_default();
    Ok(Workspace::new(slug, registry.get(slug).cloned(), store, None, None, &crate::ipc::build_id())?)
}

fn memory(v: &query::MemoryView, full: bool) -> Value {
    json!({"id":v.id,"topic":v.topic,"content":if full {v.content.clone()} else {v.content.chars().take(220).collect::<String>()},
        "tags":v.tags,"agent":v.author,"created":v.created,"updated":v.updated,"status":v.status,
        "current":v.current,"visibility":v.visibility,"importance":v.importance,"source_ref":v.source_ref,"expires_at":v.expires_at})
}

fn number(ws: &Workspace, sql: &str) -> anyhow::Result<i64> {
    Ok(ws.conn().query_row(sql, [], |r| r.get(0))?)
}

fn api(layout: &Layout, path: &str, p: &Params) -> anyhow::Result<Value> {
    if path == "/api/ping" {
        return Ok(json!({"app":"ANTmemory.Viewer","read_only":true,"deployment":crate::ipc::deployment_id(layout)}));
    }
    if path == "/api/workspaces" {
        let mut items = vec![];
        for entry in std::fs::read_dir(layout.stores())? {
            let entry = entry?;
            if !entry.path().join("v4.db").is_file() {
                continue;
            }
            let slug = entry.file_name().to_string_lossy().to_string();
            match workspace(layout, &slug) {
                Ok(ws) => items.push(json!({"name":slug,"root":ws.root,"count":query::population(ws.conn())?,"error":null})),
                Err(e) => items.push(json!({"name":slug,"count":0,"error":e.to_string()})),
            }
        }
        items.sort_by(|a, b| a["name"].as_str().cmp(&b["name"].as_str()));
        return Ok(json!({"items":items}));
    }
    let mut ws = workspace(layout, param(p, "workspace"))?;
    // 1回の応答は同じread transactionから作る。WALでdaemonの書込と共存する。
    ws.conn().execute_batch("BEGIN DEFERRED")?;
    match path {
        "/api/summary" => {
            let mut st = ws.conn().prepare("SELECT author, count(*) FROM memories GROUP BY author ORDER BY count(*) DESC, author")?;
            let agents = st.query_map([], |r| Ok(json!({"name":r.get::<_,String>(0)?,"count":r.get::<_,i64>(1)?})))?.collect::<Result<Vec<_>, _>>()?;
            let mut st =
                ws.conn().prepare("SELECT j.value,count(*) FROM memories m,json_each(m.tags_json) j GROUP BY j.value ORDER BY count(*) DESC,j.value")?;
            let tags = st.query_map([], |r| Ok(json!({"name":r.get::<_,String>(0)?,"count":r.get::<_,i64>(1)?})))?.collect::<Result<Vec<_>, _>>()?;
            Ok(json!({"memories":query::population(ws.conn())?,"goals":number(&ws,"SELECT count(*) FROM goals WHERE state='posted'")?,
                "unknowns":number(&ws,"SELECT count(*) FROM memories m JOIN memory_state s ON s.id=m.id WHERE m.kind='unknown' AND s.status='active' AND (s.expires_at IS NULL OR s.expires_at>strftime('%Y-%m-%dT%H:%M:%fZ','now')) AND NOT EXISTS(SELECT 1 FROM relations r WHERE r.dst=m.id AND r.kind='resolves')")?,
                "agents":agents,"tags":tags,"root":ws.root,"read_only":true}))
        }
        "/api/memories" => memories(&ws, p),
        "/api/memory" => detail(&mut ws, param(p, "id")),
        "/api/goals" => {
            let goals = query::goals(ws.conn())?
                .into_iter()
                .map(|g| {
                    let basis = g
                        .basis
                        .iter()
                        .map(|b| {
                            let mut value = json!(b);
                            if let Some(id) = value["utterance_id"].as_str() {
                                if let Ok(Some((client, session, file, line, quote))) = query::utterance(ws.conn(), id) {
                                    value["source"] = json!({"client":client,"session":session,"file":file,"line":line,"quote":quote});
                                }
                            }
                            value
                        })
                        .collect::<Vec<_>>();
                    json!({"id":g.id,"title":g.title,"kind":g.kind,"parent":g.parent,"state":g.state,"agent":g.posted_by,
                    "created":g.posted_at,"closed":g.closed_at,"closed_by":g.closed_by,"reason":g.close_reason,"basis":basis,"merged_into":g.merged_into})
                })
                .collect::<Vec<_>>();
            let proposals = query::open_proposals(ws.conn())?
                .into_iter()
                .map(|g| json!({"id":g.id,"title":g.title,"quote":g.quote,"action":g.action,"goal_id":g.goal_id,"agent":g.proposed_by}))
                .collect::<Vec<_>>();
            Ok(json!({"items":goals,"proposals":proposals}))
        }
        "/api/goal" => {
            let id = param(p, "id");
            if !query::goals(ws.conn())?.iter().any(|g| g.id == id) {
                bail!("札が見つかりません: {id}");
            }
            let links = query::goal_links(ws.conn(), id)?;
            let total = links.len();
            let ids = links.iter().rev().take(100).map(|l| l.0.clone()).collect::<Vec<_>>();
            let views = query::views(ws.conn(), &ids)?;
            let reports = ids.iter().filter_map(|id| views.get(id)).map(|v| memory(v, false)).collect::<Vec<_>>();
            Ok(json!({"reports":reports,"total":total,"history":history(&ws,id)?}))
        }
        "/api/states" => states(&ws, p),
        _ => bail!("ページが見つかりません"),
    }
}

fn memories(ws: &Workspace, p: &Params) -> anyhow::Result<Value> {
    let mut conditions = vec!["1=1".to_string()];
    let mut args: Vec<SqlValue> = vec![];
    for (key, column) in [("agent", "m.author"), ("importance", "s.importance"), ("visibility", "s.visibility")] {
        let value = param(p, key);
        if !value.is_empty() {
            conditions.push(format!("{column}=?"));
            args.push(value.to_string().into());
        }
    }
    let now = am_store::now();
    let effective = "CASE WHEN s.status='active' AND s.expires_at IS NOT NULL AND s.expires_at<=? THEN 'expired' ELSE s.status END";
    let status = param(p, "status");
    if status != "all" {
        conditions.push(format!("{effective}=?"));
        args.push(now.into());
        args.push(if status.is_empty() { "active" } else { status }.to_string().into());
    }
    let tag = param(p, "tag");
    if !tag.is_empty() {
        conditions.push("EXISTS(SELECT 1 FROM json_each(m.tags_json) j WHERE j.value=?)".into());
        args.push(tag.to_string().into());
    }
    if param(p, "kind") == "unknown" {
        conditions.push("m.kind='unknown'".into());
        if param(p, "resolved") != "true" {
            conditions.push("NOT EXISTS(SELECT 1 FROM relations r WHERE r.dst=m.id AND r.kind='resolves')".into());
        }
    }
    let terms = am_tools::terms::query_terms(param(p, "q"));
    if !param(p, "q").trim().is_empty() {
        let (long, short) = am_tools::terms::split_long_short(&terms);
        let mut found = vec!["m.id=?".to_string()];
        args.push(param(p, "q").to_string().into());
        if !long.is_empty() {
            found.push("m.id IN (SELECT id FROM memories_fts WHERE memories_fts MATCH ?)".into());
            args.push(long.iter().map(|t| format!("\"{}\"", t.replace('"', ""))).collect::<Vec<_>>().join(" OR ").into());
        }
        for t in short {
            found.push("instr(lower(m.topic||' '||m.content||' '||m.tags_json||' '||m.author),?)>0".into());
            args.push(t.into());
        }
        conditions.push(format!("({})", found.join(" OR ")));
    }
    let base = format!("FROM memories m JOIN memory_state s ON s.id=m.id WHERE {}", conditions.join(" AND "));
    let total: i64 = ws.conn().query_row(&format!("SELECT count(*) {base}"), params_from_iter(&args), |r| r.get(0))?;
    let limit = param(p, "limit").parse::<i64>().unwrap_or(30).clamp(1, 100);
    let page = param(p, "page").parse::<i64>().unwrap_or(0).clamp(0, 1_000_000).min((total - 1).max(0) / limit);
    let order = match param(p, "sort") {
        "oldest" => "m.created_at ASC,m.id ASC",
        "created" => "m.created_at DESC,m.id ASC",
        _ => "s.updated_at DESC,m.id ASC",
    };
    args.push(limit.into());
    args.push((page * limit).into());
    let mut st = ws.conn().prepare(&format!("SELECT m.id {base} ORDER BY {order} LIMIT ? OFFSET ?"))?;
    let ids = st.query_map(params_from_iter(&args), |r| r.get::<_, String>(0))?.collect::<Result<Vec<_>, _>>()?;
    let views = query::views(ws.conn(), &ids)?;
    let items = ids.iter().filter_map(|id| views.get(id)).map(|v| memory(v, false)).collect::<Vec<_>>();
    Ok(json!({"items":items,"total":total,"page":page,"limit":limit,"search":"全文の語句検索"}))
}

fn states(ws: &Workspace, p: &Params) -> anyhow::Result<Value> {
    let q = param(p, "q").to_lowercase();
    let total: i64 = ws.conn().query_row("SELECT count(*) FROM state WHERE instr(lower(key||' '||value_json),?1)>0", [&q], |r| r.get(0))?;
    let limit = param(p, "limit").parse::<i64>().unwrap_or(30).clamp(1, 100);
    let page = param(p, "page").parse::<i64>().unwrap_or(0).clamp(0, 1_000_000).min((total - 1).max(0) / limit);
    let mut st = ws
        .conn()
        .prepare("SELECT key,value_json,updated_at FROM state WHERE instr(lower(key||' '||value_json),?1)>0 ORDER BY updated_at DESC,key LIMIT ?2 OFFSET ?3")?;
    let items=st.query_map(rusqlite::params![q,limit,page*limit],|r|{
        let value: String=r.get(1)?;
        Ok(json!({"key":r.get::<_,String>(0)?,"value":serde_json::from_str::<Value>(&value).unwrap_or(Value::String(value)),"updated":r.get::<_,String>(2)?}))
    })?.collect::<Result<Vec<_>,_>>()?;
    Ok(json!({"items":items,"total":total,"page":page,"limit":limit}))
}

fn history(ws: &Workspace, id: &str) -> anyhow::Result<Value> {
    let (events, relations) = query::history(ws.conn(), id)?;
    Ok(
        json!({"events":events.into_iter().map(|e|json!({"kind":e.kind,"agent":e.actor,"reason":e.reason,"old":e.old_value,"new":e.new_value,"at":e.created_at})).collect::<Vec<_>>(),
        "relations":relations.into_iter().map(|r|json!({"kind":r.kind,"id":r.other,"out":r.out,"reason":r.reason})).collect::<Vec<_>>()}),
    )
}

fn detail(ws: &mut Workspace, id: &str) -> anyhow::Result<Value> {
    let v = query::view(ws.conn(), id)?.context("記憶が見つかりません")?;
    let mut result = memory(&v, true);
    result["history"] = history(ws, id)?;
    result["env"] = query::recorded_env(ws.conn(), id)?.unwrap_or(Value::Null);
    let anchors = query::anchors_of(ws.conn(), id)?;
    result["anchors"] = json!(anchors
        .iter()
        .map(|a| {
            let seen = am_tools::anchorview::observe(ws, a);
            json!({"raw":a.raw,"segment":a.segment,"label":seen.label,"anchor":a.anchor,"capture":a.capture,"observed":seen.status})
        })
        .collect::<Vec<_>>());
    let rows = query::learned_tags_for_memory(ws.conn(), id)?;
    result["recall"] = json!(rows.iter().filter(|r| r.2 == "recall").map(|r| r.1.clone()).collect::<Vec<_>>());
    Ok(result)
}

#[cfg(test)]
mod tests {
    use super::*;
    use am_domain::{ApplyCtx, Command, NewMemory};

    #[test]
    fn viewer_filters_before_paging_and_never_writes() {
        let d = tempfile::tempdir().unwrap();
        let layout = Layout { antmemory_root: d.path().into() };
        std::fs::create_dir_all(layout.store_dir("project")).unwrap();
        let db = layout.store_dir("project").join("v4.db");
        let mut store = Store::create(&db, "test").unwrap();
        for (id, topic) in [("aaaaaaaaaa", "接地の記録"), ("bbbbbbbbbb", "別の記録"), ("cccccccccc", "撤回した接地")] {
            let mem = NewMemory {
                id: id.into(),
                topic: topic.into(),
                content: "<script>alert(1)</script> ant-memory".into(),
                tags: vec!["a_%".into()],
                importance: "medium".into(),
                expires_at: None,
                source_ref: None,
                anchors: vec![],
                env: None,
            };
            let _a = store.apply(&ApplyCtx::default(), Command::Write { mem, reason: "fixture".into(), lineage: None, goal: None, uses: vec![] }).unwrap();
        }
        let _a = store.apply(&ApplyCtx::default(), Command::Retract { id: "cccccccccc".into(), reason: "試験".into() }).unwrap();
        drop(store);
        let p = Params::from([("workspace".into(), "project".into()), ("q".into(), "接地".into()), ("tag".into(), "a_%".into()), ("limit".into(), "1".into())]);
        let before = std::fs::read(&db).unwrap();
        let out = api(&layout, "/api/memories", &p).unwrap();
        assert_eq!(out["total"], 1);
        assert_eq!(out["items"][0]["id"], "aaaaaaaaaa");
        let mut p = p.clone();
        p.insert("id".into(), "aaaaaaaaaa".into());
        assert!(api(&layout, "/api/memory", &p).unwrap()["content"].as_str().unwrap().contains("<script>"));
        p.insert("status".into(), "all".into());
        assert_eq!(api(&layout, "/api/memories", &p).unwrap()["total"], 2);
        assert_eq!(std::fs::read(&db).unwrap(), before);
        assert!(workspace(&layout, "../project").is_err());
        assert!(decode("%あ").is_err());
        assert_eq!(decode("%E8%A8%98%E6%86%B6+a").unwrap(), "記憶 a");
    }
}
