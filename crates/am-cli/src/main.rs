//! antmemory — ANTmemory v4 のコマンド。
//!
//! - `serve`   : clientから起動されるshim。stdioのMCPをdaemonへ中継する
//! - `daemon`  : マシンに1つの常駐。書き手・embedding・接続の登録簿を持つ（shimが自動で起動する）
//! - `hook`    : Claude Code のhookから呼ばれ、会話の観測をdaemonへ渡す
//! - `migrate` : v3のstoreをv4へ（予行・本番・凍結後の差分）
//! - `cutover` : 本番切替の凍結と解除
//! - `export-v3` / `doctor` / `install` / `status` / `call` / `record`

mod cutover;
mod daemon;
mod hook;
mod ipc;
mod maintain;
mod migrate;
mod record;
mod serve;
mod viewer;

use std::path::PathBuf;

use clap::{Parser, Subcommand};

pub const BUILD: &str = concat!("am-cli ", env!("CARGO_PKG_VERSION"));

#[derive(Parser)]
#[command(name = "antmemory", version, about = "ANTmemory v4")]
struct Cli {
    #[command(subcommand)]
    cmd: Cmd,
}

#[derive(Subcommand)]
enum Cmd {
    /// 人間用の読取り専用ビューア（ブラウザで開く）。
    Viewer {
        #[arg(long, default_value_t = 3737)]
        port: u16,
        /// ブラウザを自動で開かない。
        #[arg(long)]
        no_open: bool,
    },
    /// clientから起動されるshim。daemonが無ければ起動してつなぐ。
    Serve {
        /// テスト用：daemonを通さず、このv4 DBを直接開く（同じDBをdaemonが使っていないときだけ）。
        #[arg(long)]
        direct: Option<PathBuf>,
        #[arg(long)]
        root: Option<PathBuf>,
        #[arg(long, default_value = "test")]
        client: String,
    },
    /// 常駐のdaemon。通常はshimが起動する。
    Daemon,
    /// Claude Code のhook（stdinのJSONを読む）。
    Hook,
    /// daemonの状態。
    Status,
    /// daemonに、処理中の呼出しを終えてから終了するよう頼む。
    Drain,
    /// テスト用：v4 DBに対してtoolを1回呼ぶ。
    Call {
        #[arg(long)]
        db: PathBuf,
        #[arg(long)]
        root: Option<PathBuf>,
        #[arg(long)]
        tool: String,
        #[arg(long, default_value = "{}")]
        args: String,
        #[arg(long, default_value = "test")]
        client: String,
    },
    /// stdioのMCPサーバーを子プロセスとして起動し、行き来するJSON-RPCを記録する（S0）。
    Record {
        #[arg(long)]
        log_dir: PathBuf,
        #[arg(trailing_var_arg = true, required = true, num_args = 1..)]
        command: Vec<String>,
    },
    /// v3のstoreをv4へ移す。
    Migrate {
        /// 予行：snapshotから別のディレクトリへ（v3には触らない）。
        #[arg(long)]
        rehearse: bool,
        /// 本番：凍結したstoreを、同じディレクトリの v4.db へ。
        #[arg(long = "final")]
        final_: bool,
        /// 凍結の後にv3へ書かれた記憶だけを取り込む。
        #[arg(long)]
        late: bool,
        #[arg(long)]
        from: Option<PathBuf>,
        #[arg(long)]
        out_dir: Option<PathBuf>,
        #[arg(long)]
        workspace_root: Option<PathBuf>,
        #[arg(long)]
        registry: Option<PathBuf>,
        #[arg(long)]
        store: Option<String>,
        #[arg(long)]
        all: bool,
        /// embeddingの一致を確かめる標本の数（予行）。
        #[arg(long, default_value_t = 0)]
        embed_sample: usize,
    },
    /// 本番切替の凍結（freeze）と解除（thaw）。thaw-v4 は切戻しのためのv4の凍結を解く。
    Cutover {
        #[arg(value_parser = ["freeze", "thaw", "thaw-v4"])]
        action: String,
        #[arg(long)]
        store: Option<String>,
        #[arg(long)]
        all: bool,
        /// 動いているv3のプロセスを止めてよい。
        #[arg(long)]
        confirm_stop: bool,
    },
    /// 切戻し：v4で増えたものを、凍結時のv3のコピーへ書き戻す。
    ExportV3 {
        #[arg(long)]
        store: String,
        #[arg(long)]
        out: PathBuf,
    },
    /// 点検。
    Doctor {
        #[arg(long)]
        store: Option<String>,
        /// v4.db のある全てのstore。
        #[arg(long)]
        all: bool,
    },
    /// 手入れ：点検（quick_check）・今日のバックアップ・WALの書き戻し。daemonも6時間ごとに行う。
    Maintain {
        #[arg(long)]
        store: Option<String>,
        #[arg(long)]
        all: bool,
    },
    /// このexeを runtime/bin へ配置し、clientの設定例を出す。
    Install {
        #[arg(long)]
        to: Option<PathBuf>,
    },
}

/// --store か --all で、v4.db のあるstoreを選ぶ。
fn v4_stores(l: &am_tools::workspace::Layout, store: Option<String>, all: bool) -> anyhow::Result<Vec<String>> {
    if let Some(s) = store {
        return Ok(vec![s]);
    }
    if !all {
        anyhow::bail!("--store <slug> か --all");
    }
    let mut v: Vec<String> = std::fs::read_dir(l.stores())?
        .flatten()
        .filter(|e| e.path().join("v4.db").is_file())
        .map(|e| e.file_name().to_string_lossy().into_owned())
        .collect();
    v.sort();
    Ok(v)
}

fn main() -> anyhow::Result<()> {
    match Cli::parse().cmd {
        Cmd::Viewer { port, no_open } => viewer::run(ipc::layout()?, port, !no_open),
        Cmd::Serve { direct: Some(db), root, client } => serve::run_direct(&db, root, &client),
        Cmd::Serve { .. } => serve::run(),
        Cmd::Daemon => daemon::run(),
        Cmd::Hook => std::process::exit(hook::run()),
        Cmd::Status => {
            let l = ipc::layout()?;
            match ipc::request(&serde_json::json!({ "am": "status" }), &l, false) {
                Ok(v) => println!("{}", serde_json::to_string_pretty(&v)?),
                Err(e) => println!("daemonは動いていない（{e}）"),
            }
            Ok(())
        }
        Cmd::Drain => {
            let l = ipc::layout()?;
            println!("{}", ipc::request(&serde_json::json!({ "am": "drain" }), &l, false)?);
            Ok(())
        }
        Cmd::Call { db, root, tool, args, client } => {
            let ws = serve::open_direct(&db, root)?;
            let mut s = am_tools::Session::new("call", &client, &ipc::build_id());
            s.cwd = std::env::current_dir().ok().map(|p| p.display().to_string());
            let mut ws = ws;
            let r = am_tools::tools::call(&mut ws, &mut s, &tool, &serde_json::from_str(&args)?);
            println!("{}", r.text);
            if r.is_error {
                std::process::exit(1);
            }
            Ok(())
        }
        Cmd::Record { log_dir, command } => {
            let code = record::run(&log_dir, &command)?;
            std::process::exit(code);
        }
        Cmd::Migrate { rehearse, final_, late, from, out_dir, workspace_root, registry, store, all, embed_sample } => {
            if rehearse {
                let (Some(from), Some(out)) = (from, out_dir) else { anyhow::bail!("--rehearse には --from と --out-dir") };
                let ok = migrate::rehearse(&from, &out, workspace_root, registry, embed_sample)?;
                std::process::exit(if ok { 0 } else { 2 });
            }
            let l = ipc::layout()?;
            if final_ {
                let slugs: Vec<String> = if all {
                    std::fs::read_dir(l.stores())?
                        .flatten()
                        .filter(|e| e.path().join("V3_FROZEN").is_file() && !e.path().join("v4.db").exists() && e.path().join("store.db").is_file())
                        .map(|e| e.file_name().to_string_lossy().into_owned())
                        .collect()
                } else {
                    vec![store.ok_or_else(|| anyhow::anyhow!("--store <slug> か --all"))?]
                };
                let mut all_ok = true;
                for s in slugs {
                    match migrate::final_(&l, &s) {
                        Ok(ok) => all_ok &= ok,
                        Err(e) => {
                            println!("{s}: {e}");
                            all_ok = false;
                        }
                    }
                }
                std::process::exit(if all_ok { 0 } else { 2 });
            }
            if late {
                return migrate::late(&l, &store.ok_or_else(|| anyhow::anyhow!("--store <slug>"))?);
            }
            anyhow::bail!("--rehearse / --final / --late のどれか")
        }
        Cmd::Cutover { action, store, all, confirm_stop } => {
            let l = ipc::layout()?;
            if action == "freeze" {
                std::process::exit(cutover::freeze(&l, store.as_deref(), all, confirm_stop)?);
            }
            if action == "thaw-v4" {
                return cutover::thaw_v4(&l, &store.ok_or_else(|| anyhow::anyhow!("--store <slug>"))?);
            }
            cutover::thaw(&l, store.as_deref(), all)
        }
        Cmd::ExportV3 { store, out } => {
            let ok = cutover::export_v3(&ipc::layout()?, &store, &out)?;
            std::process::exit(if ok { 0 } else { 2 });
        }
        Cmd::Doctor { store, all } => {
            let l = ipc::layout()?;
            let mut ok = true;
            for s in v4_stores(&l, store, all)? {
                println!("
## {s}");
                match cutover::doctor(&l, &s) {
                    Ok(good) => ok &= good,
                    Err(e) => {
                        println!("✗ 開けない: {e}");
                        ok = false;
                    }
                }
            }
            std::process::exit(if ok { 0 } else { 2 });
        }
        Cmd::Maintain { store, all } => {
            let l = ipc::layout()?;
            let mut ok = true;
            for s in v4_stores(&l, store, all)? {
                match maintain::run_store(&l.store_dir(&s)) {
                    Ok(msg) => println!("{s}: {msg}"),
                    Err(e) => {
                        println!("{s}: ✗ {e}");
                        ok = false;
                    }
                }
            }
            std::process::exit(if ok { 0 } else { 2 });
        }
        Cmd::Install { to } => cutover::install(&ipc::layout()?, to),
    }
}
