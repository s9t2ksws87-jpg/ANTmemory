//! daemon と shim / hook の間の約束（設計案v2 §2）。named pipeの上を、1行1つのJSONで話す。
//!
//! 最初の1行が `{"am": ...}` の制御。hello の後は、MCPのJSON-RPCをそのまま中継する。

use std::io::{BufRead, BufReader, Write};
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

use am_tools::workspace::{Layout, Resolved};
use interprocess::local_socket::{prelude::*, GenericNamespaced, Stream};
use serde::{Deserialize, Serialize};

pub const PROTOCOL: u32 = 1;

/// 同じ版のbuildかを比べる刻印。新しいbuildほど大きい。
pub fn build_stamp() -> u64 {
    env!("AM_BUILD_STAMP").parse().unwrap_or(0)
}

pub fn build_id() -> String {
    format!("am-cli {}+{}", env!("CARGO_PKG_VERSION"), build_stamp())
}

/// 配置（ANTmemoryのroot）の識別子。正規化したrootのsha256の先頭8桁。
/// 別の配置（開発のcheckout・公開版・試験のコピー）のdaemonへ取り違えてつながらないよう、pipe名とhelloで使う（R7）。
pub fn deployment_id(layout: &Layout) -> String {
    use sha2::Digest;
    let root = std::fs::canonicalize(&layout.antmemory_root).unwrap_or_else(|_| layout.antmemory_root.clone());
    let s = root.display().to_string();
    let s = s.strip_prefix(r"\\?\").unwrap_or(&s).replace('/', "\\").trim_end_matches('\\').to_lowercase();
    hex::encode(sha2::Sha256::digest(s.as_bytes()))[..8].to_string()
}

pub fn pipe_name(layout: &Layout) -> String {
    let user = std::env::var("USERNAME").or_else(|_| std::env::var("USER")).unwrap_or_else(|_| "user".into());
    let user: String = user.chars().map(|c| if c.is_ascii_alphanumeric() { c } else { '_' }).collect();
    std::env::var("ANT_MEMORY_PIPE").unwrap_or_else(|_| format!("antmemory-{user}-{}", deployment_id(layout)))
}

/// ANTmemoryのrepo。ANT_MEMORY_ROOT、無ければ実行ファイルから上へ辿って stores/ と runtime/（v3の頃の配置なら server/）を持つ所。
pub fn layout() -> anyhow::Result<Layout> {
    if let Some(r) = std::env::var_os("ANT_MEMORY_ROOT") {
        return Ok(Layout { antmemory_root: PathBuf::from(r) });
    }
    let exe = std::env::current_exe()?;
    let mut dir = exe.parent().map(Path::to_path_buf);
    while let Some(d) = dir {
        if d.join("stores").is_dir() && (d.join("runtime").is_dir() || d.join("server").is_dir()) {
            return Ok(Layout { antmemory_root: d });
        }
        dir = d.parent().map(Path::to_path_buf);
    }
    anyhow::bail!("ANTmemoryのrootが分からない（ANT_MEMORY_ROOT を設定してください）")
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Hello {
    pub am: String,
    pub protocol: u32,
    pub build: String,
    pub stamp: u64,
    pub client: String,
    pub cwd: String,
    pub workspace: Resolved,
    pub shim_pid: u32,
    pub parents: serde_json::Value,
    /// clientが渡した会話の識別子（(client, session)）。
    pub conversation: Option<(String, String)>,
    pub model: Option<String>,
    pub provider: Option<String>,
    /// 観測用：会話・作業空間の手がかりになりそうな環境変数の名前（値は送らない）。
    pub env_names: Vec<String>,
    /// 接続元の配置（deployment_id）。daemonの配置と違えば拒否する。
    #[serde(default)]
    pub deployment: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct HelloReply {
    pub am: String,
    pub ok: bool,
    pub build: String,
    pub stamp: u64,
    pub conn_id: Option<String>,
    pub reason: Option<String>,
    pub level: Option<String>,
}

pub fn connect(layout: &Layout) -> std::io::Result<Stream> {
    let name = pipe_name(layout).to_ns_name::<GenericNamespaced>()?;
    Stream::connect(name)
}

/// daemonにつながるまで待つ。つながらなければ、排他してdaemonを起動する（上限5秒）。
pub fn connect_or_spawn(layout: &Layout) -> anyhow::Result<Stream> {
    if let Ok(s) = connect(layout) {
        return Ok(s);
    }
    spawn_daemon(layout)?;
    let start = Instant::now();
    while start.elapsed() < Duration::from_secs(5) {
        if let Ok(s) = connect(layout) {
            return Ok(s);
        }
        std::thread::sleep(Duration::from_millis(50));
    }
    anyhow::bail!("DAEMON_UNAVAILABLE: daemonを起動したが5秒以内につながらない（{}を確認）", daemon_log(layout).display())
}

pub fn runtime_dir(layout: &Layout) -> PathBuf {
    layout.antmemory_root.join("runtime").join("v4")
}

pub fn daemon_log(layout: &Layout) -> PathBuf {
    runtime_dir(layout).join("daemon.log")
}

/// 自分の標準入出力を、起動するdaemonに継がせない。
/// 継がせると、clientがshim（やhook）の出力の終わりを受け取れず、終わったshimを待ち続ける。
fn no_inherit_stdio() {
    #[cfg(windows)]
    {
        use windows_sys::Win32::Foundation::{SetHandleInformation, HANDLE_FLAG_INHERIT, INVALID_HANDLE_VALUE};
        use windows_sys::Win32::System::Console::{GetStdHandle, STD_ERROR_HANDLE, STD_INPUT_HANDLE, STD_OUTPUT_HANDLE};
        for which in [STD_INPUT_HANDLE, STD_OUTPUT_HANDLE, STD_ERROR_HANDLE] {
            // SAFETY: 自分のプロセスの標準ハンドルの属性を変えるだけで、ハンドルは閉じない。
            unsafe {
                let h = GetStdHandle(which);
                if !h.is_null() && h != INVALID_HANDLE_VALUE {
                    SetHandleInformation(h, HANDLE_FLAG_INHERIT, 0);
                }
            }
        }
    }
}

fn spawn_daemon(layout: &Layout) -> anyhow::Result<()> {
    let dir = runtime_dir(layout);
    std::fs::create_dir_all(&dir)?;
    // 起動の排他。同時に起動されても、daemon自身もlockで1つに絞る。
    let lock = dir.join("spawn.lock");
    if let Ok(meta) = std::fs::metadata(&lock) {
        if meta.modified().ok().and_then(|m| m.elapsed().ok()).is_some_and(|e| e < Duration::from_secs(10)) {
            return Ok(());
        }
    }
    std::fs::write(&lock, std::process::id().to_string())?;
    no_inherit_stdio();
    let exe = std::env::current_exe()?;
    let mut cmd = std::process::Command::new(exe);
    cmd.arg("daemon")
        .env("ANT_MEMORY_ROOT", &layout.antmemory_root)
        .stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null());
    #[cfg(windows)]
    {
        use std::os::windows::process::CommandExt;
        const DETACHED_PROCESS: u32 = 0x0000_0008;
        const CREATE_NEW_PROCESS_GROUP: u32 = 0x0000_0200;
        const CREATE_NO_WINDOW: u32 = 0x0800_0000;
        cmd.creation_flags(DETACHED_PROCESS | CREATE_NEW_PROCESS_GROUP | CREATE_NO_WINDOW);
    }
    cmd.spawn()?;
    Ok(())
}

pub fn send_line(w: &mut impl Write, v: &impl Serialize) -> std::io::Result<()> {
    let mut s = serde_json::to_string(v).map_err(std::io::Error::other)?;
    s.push('\n');
    w.write_all(s.as_bytes())?;
    w.flush()
}

pub fn read_line(r: &mut BufReader<impl std::io::Read>) -> std::io::Result<Option<String>> {
    let mut s = String::new();
    if r.read_line(&mut s)? == 0 {
        return Ok(None);
    }
    Ok(Some(s.trim_end_matches(['\n', '\r']).to_string()))
}

/// 1つの制御メッセージを送り、1行の返事を受け取る（hook・status・drain用）。
pub fn request(msg: &serde_json::Value, layout: &Layout, spawn: bool) -> anyhow::Result<serde_json::Value> {
    let stream = if spawn { connect_or_spawn(layout)? } else { connect(layout)? };
    let mut msg = msg.clone();
    msg["deployment"] = serde_json::json!(deployment_id(layout));
    let (mut r, mut send) = split(stream);
    send_line(&mut send, &msg)?;
    let line = read_line(&mut r)?.ok_or_else(|| anyhow::anyhow!("daemonが返事をせずに閉じた"))?;
    Ok(serde_json::from_str(&line)?)
}

/// 親プロセスの連鎖（pid と exe名）。Windowsでは1回のPowerShellで取る。
pub fn parent_chain() -> serde_json::Value {
    // PowerShellの起動に約0.5秒かかるので、観測するときだけ取る（ANT_MEMORY_TRACE_PARENTS=1）。
    if !cfg!(windows) || std::env::var("ANT_MEMORY_TRACE_PARENTS").as_deref() != Ok("1") {
        return serde_json::json!([]);
    }
    let script = format!(
        "$id={};$o=@();for($i=0;$i -lt 5;$i++){{$p=Get-CimInstance Win32_Process -Filter \"ProcessId=$id\";if(-not $p){{break}};$o+=\"$($p.ProcessId) $($p.Name)\";if($p.ParentProcessId -eq $id){{break}};$id=$p.ParentProcessId}};$o -join \"`n\"",
        std::process::id()
    );
    let out = am_anchor::proc::silent("powershell.exe")
        .args(["-NoProfile", "-NonInteractive", "-Command", &script])
        .stdin(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .output();
    let text = out.map(|o| String::from_utf8_lossy(&o.stdout).into_owned()).unwrap_or_default();
    let v: Vec<serde_json::Value> = text
        .lines()
        .filter_map(|l| l.trim().split_once(' ').map(|(p, n)| serde_json::json!({ "pid": p.parse::<u64>().ok(), "exe": n })))
        .collect();
    serde_json::Value::Array(v)
}

/// named pipeの1接続。読む側と書く側を別のthreadで使えるよう、Arcで共有する。
#[derive(Clone)]
pub struct Pipe(pub std::sync::Arc<Stream>);

impl std::io::Read for Pipe {
    fn read(&mut self, buf: &mut [u8]) -> std::io::Result<usize> {
        (&*self.0).read(buf)
    }
}

impl std::io::Write for Pipe {
    fn write(&mut self, buf: &[u8]) -> std::io::Result<usize> {
        (&*self.0).write(buf)
    }
    fn flush(&mut self) -> std::io::Result<()> {
        (&*self.0).flush()
    }
}

pub fn split(s: Stream) -> (BufReader<Pipe>, Pipe) {
    let p = Pipe(std::sync::Arc::new(s));
    (BufReader::new(p.clone()), p)
}
