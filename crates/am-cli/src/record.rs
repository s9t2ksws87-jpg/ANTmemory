//! S0：MCPのstdioをそのまま中継し、1行ずつ記録する。
//!
//! 中継する内容は書き換えない。記録は観測用の一時的なもので、記憶としては扱わない。
//! 検索語や記憶の本文が入るので、出力先はGit管理外にする（logs/ など）。

use std::fs::{self, File, OpenOptions};
use std::io::{BufRead, BufReader, BufWriter, Read, Write};
use std::path::Path;
use std::process::{Command, Stdio};
use std::sync::{Arc, Mutex};
use std::thread;

use anyhow::Context;
use serde_json::json;

fn now() -> String {
    chrono::Utc::now().to_rfc3339_opts(chrono::SecondsFormat::Micros, true)
}

/// 1行を記録の形にする。UTF-8でなければhexで持つ（書き換えないため）。
fn raw_value(line: &[u8]) -> serde_json::Value {
    let body = line.strip_suffix(b"\n").unwrap_or(line);
    let body = body.strip_suffix(b"\r").unwrap_or(body);
    match std::str::from_utf8(body) {
        Ok(s) => json!(s),
        Err(_) => json!({ "$hex": hex::encode(body) }),
    }
}

type Log = Arc<Mutex<BufWriter<File>>>;

fn write_log(log: &Log, value: serde_json::Value) {
    // 記録に失敗しても中継は止めない。失敗はstderrに出す（黙って捨てない）。
    let mut w = match log.lock() {
        Ok(w) => w,
        Err(p) => p.into_inner(),
    };
    if let Err(e) = writeln!(w, "{value}").and_then(|_| w.flush()) {
        eprintln!("[antmemory record] 記録の書込に失敗: {e}");
    }
}

/// `from` から1行ずつ読み、`to` へそのまま流しながら記録する。
fn pump<R: Read, W: Write>(from: R, mut to: W, log: Log, dir: &'static str) {
    let mut reader = BufReader::new(from);
    let mut line = Vec::new();
    loop {
        line.clear();
        match reader.read_until(b'\n', &mut line) {
            Ok(0) => break,
            Ok(_) => {
                write_log(&log, json!({ "t": now(), "dir": dir, "raw": raw_value(&line) }));
                if to.write_all(&line).and_then(|_| to.flush()).is_err() {
                    break;
                }
            }
            Err(e) => {
                write_log(&log, json!({ "t": now(), "dir": "meta", "event": "read_error", "from": dir, "error": e.to_string() }));
                break;
            }
        }
    }
}

pub fn run(log_dir: &Path, command: &[String]) -> anyhow::Result<i32> {
    let (program, args) = command.split_first().context("起動するコマンドが無い")?;
    fs::create_dir_all(log_dir).with_context(|| format!("{} を作れない", log_dir.display()))?;
    let client = std::env::var("ANT_MEMORY_CLIENT").unwrap_or_else(|_| "unknown".into());
    let stamp = chrono::Utc::now().format("%Y%m%dT%H%M%S%.3fZ");
    let path = log_dir.join(format!("{stamp}_{client}_{}.jsonl", std::process::id()));
    let file = OpenOptions::new().create_new(true).append(true).open(&path)
        .with_context(|| format!("{} を作れない", path.display()))?;
    let log: Log = Arc::new(Mutex::new(BufWriter::new(file)));

    let mut child = Command::new(program)
        .args(args)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::inherit())
        .spawn()
        .with_context(|| format!("{program} を起動できない"))?;

    write_log(&log, json!({
        "t": now(), "dir": "meta", "event": "start",
        "client": client, "pid": std::process::id(), "child_pid": child.id(),
        "cwd": std::env::current_dir().ok().map(|p| p.display().to_string()),
        "program": program, "args": args,
    }));

    let child_in = child.stdin.take().context("子プロセスのstdinが無い")?;
    let child_out = child.stdout.take().context("子プロセスのstdoutが無い")?;

    let log_in = log.clone();
    // stdinの読み手は、clientが閉じるまで戻らない。子が先に終わった場合はprocess::exitで終える。
    thread::spawn(move || pump(std::io::stdin().lock(), child_in, log_in, "c2s"));
    let log_out = log.clone();
    let out_thread = thread::spawn(move || pump(child_out, std::io::stdout().lock(), log_out, "s2c"));

    let status = child.wait().context("子プロセスを待てない")?;
    // 子のstdoutを最後まで流してから終わる。
    if out_thread.join().is_err() {
        eprintln!("[antmemory record] stdoutの中継threadが異常終了した");
    }
    let code = status.code().unwrap_or(1);
    write_log(&log, json!({ "t": now(), "dir": "meta", "event": "exit", "code": code }));
    Ok(code)
}
