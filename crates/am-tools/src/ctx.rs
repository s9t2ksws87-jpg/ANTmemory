//! toolを動かす文脈：workspace（store・現物・embedding）と、接続ごとのsession。

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Arc;
use std::time::{Duration, Instant};

use am_anchor::Resolver;
use am_domain::{ApplyCtx, Command};
use am_embed::Embedder;
use am_store::{query, Applied, Store};

/// gitの索引を読み直す間隔。取手の照合は毎回行うが、索引の読込みは重いので少し使い回す。
const RESOLVER_TTL: Duration = Duration::from_secs(60);

/// 書いた記憶を知らせる先（workspaceのslug、記憶のid）。
pub type WriteNotify = Box<dyn Fn(&str, &str) + Send>;

pub struct Workspace {
    pub slug: String,
    pub root: Option<PathBuf>,
    pub store: Store,
    pub model_id: String,
    pub embedder: Option<Arc<Embedder>>,
    pub embed_note: Option<String>,
    resolver: Option<(Instant, Resolver)>,
    /// hookで観測した、会話ごとの「前回の書込以降に編集したファイル」（§3.5 touched）。
    pub touched: HashMap<String, Vec<String>>,
    /// 書いた記憶のidを、embeddingのworkerへ知らせる。
    pub notify_write: Option<WriteNotify>,
}

impl Workspace {
    pub fn new(
        slug: &str,
        root: Option<PathBuf>,
        mut store: Store,
        embedder: Option<Arc<Embedder>>,
        embed_note: Option<String>,
        build: &str,
    ) -> am_store::Result<Self> {
        let model_id = if let Some(e) = &embedder {
            let m = e.model_id().to_string();
            let current = query::meta(store.reader(), "active_model_id")?;
            if current.as_deref() != Some(&m) {
                let cx = ApplyCtx { actor: "embedding".into(), build: build.into(), ..Default::default() };
                let _applied = store.apply(&cx, Command::ActivateModel { model_id: m.clone() })?;
            }
            m
        } else {
            query::meta(store.reader(), "active_model_id")?.unwrap_or_else(|| am_store::v3read::MODEL_ID.to_string())
        };
        Ok(Workspace { slug: slug.to_string(), root, store, model_id, embedder, embed_note, resolver: None, touched: HashMap::new(), notify_write: None })
    }

    pub fn resolver(&mut self) -> Option<&mut Resolver> {
        let root = self.root.clone()?;
        let stale = self.resolver.as_ref().is_none_or(|(at, _)| at.elapsed() > RESOLVER_TTL);
        if stale {
            self.resolver = Some((Instant::now(), Resolver::new(&root)));
        }
        self.resolver.as_mut().map(|(_, r)| r)
    }

    /// 書いた直後の照合が、古い索引の結果を使わないようにする。
    pub fn invalidate_resolver(&mut self) {
        self.resolver = None;
    }

    pub fn apply(&mut self, cx: &ApplyCtx, cmd: Command) -> am_store::Result<Applied> {
        self.store.apply(cx, cmd)
    }

    pub fn conn(&self) -> &rusqlite::Connection {
        self.store.reader()
    }

    pub fn root_path(&self) -> Option<&Path> {
        self.root.as_deref()
    }
}

/// 連携段階（§5.2）。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Level {
    C0,
    C1,
    C2,
}

impl Level {
    pub fn as_str(self) -> &'static str {
        match self {
            Level::C0 => "C0",
            Level::C1 => "C1",
            Level::C2 => "C2",
        }
    }
}

/// その接続で、札のツリーをどこまで見せたか（保持している版）。
#[derive(Debug, Clone)]
pub struct Held {
    pub seq: i64,
    pub full_at: Instant,
    pub complete: bool,
    pub root: Option<String>,
}

pub struct Session {
    pub conn_id: String,
    pub client: String,
    pub cwd: Option<String>,
    /// workspaceが未特定だった接続の最初の成功応答で一度だけ知らせる。
    pub workspace_notice: Option<String>,
    /// (client, session)。検証できる経路（clientが渡した環境変数・hook）で届いたときだけ持つ。
    pub conversation: Option<(String, String)>,
    pub model: Option<String>,
    pub provider: Option<String>,
    pub build: String,
    pub focus: Option<String>,
    pub held: Option<Held>,
    /// この接続が全体を見た共有文脈（v3のstate）の版と、そのとき。
    pub context_held: Option<(String, Instant)>,
    /// C2 の同じ文脈世代で実際に返した検索表示だけ。接続・世代をまたいで共有しない。
    pub memory_held: HashMap<String, String>,
    pub gen_seen: Option<i64>,
}

static COUNTER: AtomicU64 = AtomicU64::new(0);

/// 10桁のhex（v3と同じ形）。時刻・プロセス・連番から作る。
pub fn new_id(seed: &str) -> String {
    use sha1::Digest;
    let n = COUNTER.fetch_add(1, Ordering::Relaxed);
    let nanos = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map(|d| d.as_nanos()).unwrap_or(0);
    let mut h = sha1::Sha1::new();
    h.update(format!("{seed}:{nanos}:{}:{n}", std::process::id()).as_bytes());
    hex::encode(h.finalize())[..10].to_string()
}

impl Session {
    pub fn new(conn_id: &str, client: &str, build: &str) -> Session {
        Session {
            conn_id: conn_id.into(),
            client: client.into(),
            cwd: None,
            workspace_notice: None,
            conversation: None,
            model: None,
            provider: None,
            build: build.into(),
            focus: None,
            held: None,
            context_held: None,
            memory_held: HashMap::new(),
            gen_seen: None,
        }
    }

    pub fn conversation_id(&self) -> Option<String> {
        self.conversation.as_ref().map(|(c, s)| format!("{c}:{s}"))
    }

    /// 「役に立った」を数える単位。会話が分かればそれ、分からなければ接続。
    pub fn scope_key(&self) -> String {
        self.conversation_id().unwrap_or_else(|| format!("conn:{}", self.conn_id))
    }

    pub fn apply_ctx(&self) -> ApplyCtx {
        ApplyCtx {
            actor: self.client.clone(),
            build: self.build.clone(),
            conn_id: Some(self.conn_id.clone()),
            conversation_id: self.conversation_id(),
            model: self.model.clone(),
            provider: self.provider.clone(),
        }
    }

    /// 連携段階。会話が分かり、そのclientのhookが届いていればC2。
    pub fn level(&self, ws: &Workspace) -> Level {
        let Some(id) = self.conversation_id() else { return Level::C0 };
        let hooked: bool = ws.conn().query_row("SELECT source='hook' FROM conversations WHERE conversation_id=?1", [&id], |r| r.get(0)).unwrap_or(false);
        if hooked {
            Level::C2
        } else {
            Level::C1
        }
    }

    /// 会話の世代が進んでいたら（圧縮・clear）、焦点と保持版を捨てる。
    pub fn sync_generation(&mut self, ws: &Workspace) -> bool {
        let Some(id) = self.conversation_id() else { return false };
        let gen = query::conversation_gen(ws.conn(), &id).ok().flatten();
        let changed = self.gen_seen.is_some() && gen != self.gen_seen;
        if changed {
            self.focus = None;
            self.held = None;
            self.context_held = None;
            self.memory_held.clear();
        }
        self.gen_seen = gen;
        changed
    }
}
