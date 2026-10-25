//! v4のstore。DBを変える経路は `Store::apply` だけ。
//!
//! 読取り専用の処理（v3の読込み、照合、検索・表示用の読取り）は別moduleに置き、
//! 書込のSQLはこのfileにだけ書く。

mod schema;
pub mod query;
pub mod v3read;
pub mod verify;

use std::path::Path;

use am_domain::{
    AnchorObservation, AnchorStatus, ApplyCtx, Basis, Command, GoalClose, MemoryKind, NewAnchor, NewGoal, NewMemory,
    Proposal, UseRecord, Utterance, V3Batch, IMPORTANCE, VISIBILITY,
};
use rusqlite::{params, Connection, OpenFlags, OptionalExtension, Transaction, TransactionBehavior};
use serde_json::json;

pub use schema::SCHEMA_VERSION;

#[derive(Debug, thiserror::Error)]
pub enum StoreError {
    #[error("sqlite: {0}")]
    Sqlite(#[from] rusqlite::Error),
    #[error("v4 DBが既にある: {0}")]
    AlreadyExists(String),
    #[error("schema_versionが合わない: DB={found}, このbuild={expected}")]
    SchemaMismatch { found: i64, expected: i64 },
    #[error("{0}")]
    Precondition(String),
    #[error("CAS_CONFLICT key={key}\ncurrent={current}\nexpected={expected}")]
    CasConflict { key: String, current: String, expected: String },
    #[error("v3の値を読めない: {0}")]
    BadLegacyValue(String),
}

pub type Result<T> = std::result::Result<T, StoreError>;

fn pre<T>(msg: impl Into<String>) -> Result<T> {
    Err(StoreError::Precondition(msg.into()))
}

/// `apply` の結果。呼んだ側が確認できるよう、件数とイベントのseqを返す。
#[derive(Debug, Default)]
#[must_use]
pub struct Applied {
    pub command: &'static str,
    pub event_seqs: Vec<i64>,
    pub counts: Vec<(&'static str, usize)>,
    /// 命令ごとの補足（変更前の値、作ったid、捨てた理由など）。
    pub detail: serde_json::Value,
}

pub struct Store {
    conn: Connection,
    /// これがあれば書込を拒否する（切戻しのための凍結。DBと同じディレクトリの V4_FROZEN）。
    freeze_marker: Option<std::path::PathBuf>,
}

pub const V4_FROZEN: &str = "V4_FROZEN";
const LEARNING_RULES_VERSION: &str = "2";

pub fn now() -> String {
    chrono::Utc::now().to_rfc3339_opts(chrono::SecondsFormat::Millis, true)
}

fn tune(conn: &Connection) -> Result<()> {
    conn.pragma_update(None, "foreign_keys", "ON")?;
    conn.pragma_update(None, "busy_timeout", 5000)?;
    Ok(())
}

impl Store {
    /// 新しいv4 DBを作る。同じパスに既にあれば拒否する（上書きしない）。
    pub fn create(path: &Path, build: &str) -> Result<Store> {
        Self::create_with_model(path, build, None)
    }

    /// `active_model_id` を決めて作る。v3から取り込む場合は、取込みがv3のモデルを設定する。
    pub fn create_with_model(path: &Path, build: &str, active_model_id: Option<&str>) -> Result<Store> {
        if path.exists() {
            return Err(StoreError::AlreadyExists(path.display().to_string()));
        }
        let conn = Connection::open(path)?;
        conn.pragma_update(None, "journal_mode", "WAL")?;
        tune(&conn)?;
        conn.execute_batch(schema::SCHEMA)?;
        conn.execute(
            "INSERT INTO meta(key, value) VALUES ('schema_version', ?1), ('created_at', ?2), ('created_by_build', ?3)",
            params![SCHEMA_VERSION.to_string(), now(), build],
        )?;
        conn.execute("INSERT INTO meta(key,value) VALUES ('learning_rules_version',?1)", [LEARNING_RULES_VERSION])?;
        if let Some(m) = active_model_id {
            conn.execute("INSERT INTO meta(key, value) VALUES ('active_model_id', ?1)", [m])?;
        }
        Ok(Store { conn, freeze_marker: path.parent().map(|p| p.join(V4_FROZEN)) })
    }

    /// 既存DBを開く。v2は書込用接続で派生索引だけ更新する。他の版・読取専用では拒否する。
    pub fn open(path: &Path, read_only: bool) -> Result<Store> {
        let flags = if read_only {
            OpenFlags::SQLITE_OPEN_READ_ONLY | OpenFlags::SQLITE_OPEN_NO_MUTEX
        } else {
            OpenFlags::SQLITE_OPEN_READ_WRITE | OpenFlags::SQLITE_OPEN_NO_MUTEX
        };
        let mut conn = Connection::open_with_flags(path, flags)?;
        if !read_only {
            tune(&conn)?;
        }
        let mut found: i64 = conn
            .query_row("SELECT value FROM meta WHERE key='schema_version'", [], |r| r.get::<_, String>(0))?
            .parse()
            .map_err(|_| StoreError::BadLegacyValue("meta.schema_version".into()))?;
        // 古いschemaは、派生物（索引・集計）だけを1つのtransactionで足して上げる。本文・id・イベントは変えない。
        while (2..SCHEMA_VERSION).contains(&found) && !read_only {
            let tx = conn.transaction_with_behavior(TransactionBehavior::Immediate)?;
            if path.parent().is_some_and(|p| p.join(V4_FROZEN).exists()) {
                return pre("V4_FROZEN: 凍結中は検索索引も更新しない。thaw-v4後に開き直す");
            }
            // もう一つの接続が先に更新した場合は再実行しない。
            let current: i64 = tx
                .query_row("SELECT value FROM meta WHERE key='schema_version'", [], |r| r.get::<_, String>(0))?
                .parse()
                .map_err(|_| StoreError::BadLegacyValue("meta.schema_version".into()))?;
            match current {
                2 => tx.execute_batch("DROP TABLE memories_fts;
                    CREATE VIRTUAL TABLE memories_fts USING fts5(id UNINDEXED, topic, tags, content, author, tokenize='trigram');
                    INSERT INTO memories_fts(id,topic,tags,content,author)
                    SELECT id,topic,COALESCE((SELECT group_concat(value,' ') FROM json_each(tags_json)),''),content,author FROM memories;
                    UPDATE meta SET value='3' WHERE key='schema_version';")?,
                3 => {
                    // 長く使ったときの重さ：会話ごとの検索・読取りの索引と、学習tagsの集計。
                    tx.execute_batch(schema::UPGRADE_3_TO_4)?;
                    rebuild_learned(&tx, None)?;
                    tx.execute("INSERT OR REPLACE INTO meta(key,value) VALUES ('learning_rules_version',?1)", [LEARNING_RULES_VERSION])?;
                    tx.execute("UPDATE meta SET value='4' WHERE key='schema_version'", [])?;
                }
                v if v == SCHEMA_VERSION => {}
                v => return pre(format!("索引更新中にschema_versionが変わった: {v}")),
            }
            tx.commit()?;
            found = conn
                .query_row("SELECT value FROM meta WHERE key='schema_version'", [], |r| r.get::<_, String>(0))?
                .parse()
                .map_err(|_| StoreError::BadLegacyValue("meta.schema_version".into()))?;
        }
        if found != SCHEMA_VERSION {
            return Err(StoreError::SchemaMismatch { found, expected: SCHEMA_VERSION });
        }
        // 表の構造は同じ。旧版で記録した否定も効くよう、派生した学習集計だけを一度再計算する。
        if !read_only && query::meta(&conn, "learning_rules_version")?.as_deref() != Some(LEARNING_RULES_VERSION) {
            let tx = conn.transaction_with_behavior(TransactionBehavior::Immediate)?;
            if path.parent().is_some_and(|p| p.join(V4_FROZEN).exists()) {
                return pre("V4_FROZEN: 凍結中は学習集計も更新しない");
            }
            if query::meta(&tx, "learning_rules_version")?.as_deref() != Some(LEARNING_RULES_VERSION) {
                rebuild_learned(&tx, None)?;
                tx.execute("INSERT OR REPLACE INTO meta(key,value) VALUES ('learning_rules_version',?1)", [LEARNING_RULES_VERSION])?;
            }
            tx.commit()?;
        }
        Ok(Store { conn, freeze_marker: path.parent().map(|p| p.join(V4_FROZEN)) })
    }

    /// 読取り用。書込はできない（`&Connection` からは apply を呼べない）。
    pub fn reader(&self) -> &Connection {
        &self.conn
    }

    /// 唯一の書込口。1つのtransactionで、前提の検査からイベントの記録まで行う。
    pub fn apply(&mut self, cx: &ApplyCtx, cmd: Command) -> Result<Applied> {
        let kind = cmd.kind();
        let tx = self.conn.transaction_with_behavior(TransactionBehavior::Immediate)?;
        // 書込のlockを取った後で確かめる。凍結の前に始まった書込は、凍結した側がlockを取る前に終わっている。
        if self.freeze_marker.as_ref().is_some_and(|m| m.exists()) {
            return pre("V4_FROZEN: このworkspaceは切戻しのため凍結されている（書込を受け付けない）。`antmemory cutover thaw-v4` で解く");
        }
        let w = W { tx: &tx, cx, ts: now() };
        let applied = match cmd {
            Command::ImportV3(batch) => import_v3(&w, *batch, false)?,
            Command::ImportV3Late(batch) => import_v3(&w, *batch, true)?,
            Command::RecordAnchorObservations(obs) => w.anchor_observations(obs)?,
            Command::Write { mem, reason, lineage, goal, uses } => w.write(mem, &reason, lineage, goal, uses)?,
            Command::Supersede { old_id, mem, reason, goal, uses } => w.supersede(&old_id, mem, &reason, goal, uses)?,
            Command::Retract { id, reason } => w.retract(&id, &reason)?,
            Command::Restore { id, reason } => w.restore(&id, &reason)?,
            Command::ChangeImportance { id, importance, reason } => w.change_importance(&id, &importance, &reason)?,
            Command::SetVisibility { id, visibility, reason } => w.set_visibility(&id, &visibility, &reason)?,
            Command::ExpireDue => w.expire_due()?,
            Command::SetState { key, value, expected } => w.set_state(&key, value, expected)?,
            Command::PostGoal(g) => w.post_goal(g)?,
            Command::EditGoal { id, title, basis_note } => w.edit_goal(&id, title, basis_note)?,
            Command::CloseGoal { id, how, reason, utterance } => w.close_goal(&id, how, reason, utterance)?,
            Command::ProposeGoal(p) => w.propose_goal(p)?,
            Command::RecordConnection(c) => w.record_connection(c)?,
            Command::BumpConversation { conversation_id, client, session, source, bump } => {
                w.bump_conversation(&conversation_id, &client, &session, &source, bump)?
            }
            Command::RecordSearch(s) => w.record_search(s)?,
            Command::RecordRead(r) => w.record_read(r)?,
            Command::RecordUses(u) => w.record_uses(u, None)?,
            Command::ActivateModel { model_id } => {
                if model_id.trim().is_empty() { return pre("model_idは空にできない"); }
                let old: Option<String> = tx.query_row("SELECT value FROM meta WHERE key='active_model_id'", [], |r| r.get(0)).optional()?;
                if old.as_deref() == Some(&model_id) {
                    Applied { detail: json!({"unchanged": true}), ..Default::default() }
                } else {
                    tx.execute("INSERT INTO meta(key,value) VALUES ('active_model_id',?1) ON CONFLICT(key) DO UPDATE SET value=excluded.value", [&model_id])?;
                    let seq = w.event("model_activated", None, json!({"old": old, "new": model_id}))?;
                    Applied { event_seqs: vec![seq], ..Default::default() }
                }
            }
            Command::PublishEmbedding { id, model_id, content_hash, vector, computed_by } => {
                w.publish_embedding(&id, &model_id, &content_hash, &vector, &computed_by)?
            }
        };
        tx.commit()?;
        Ok(Applied { command: kind, ..applied })
    }
}

/// 1つのtransactionの中の書込。
struct W<'a> {
    tx: &'a Transaction<'a>,
    cx: &'a ApplyCtx,
    ts: String,
}

fn applied(seqs: Vec<i64>, detail: serde_json::Value) -> Applied {
    Applied { command: "", event_seqs: seqs, counts: vec![], detail }
}

/// CASの比較だけで使う見方。JSONのobject・配列を文字列にして渡すclientがあるので、
/// 文字列の中身がobject・配列ならそれとして比べる（保存する値は渡されたまま）。
fn cas_view(v: &serde_json::Value) -> std::borrow::Cow<'_, serde_json::Value> {
    if let serde_json::Value::String(s) = v {
        if let Ok(p @ (serde_json::Value::Object(_) | serde_json::Value::Array(_))) = serde_json::from_str(s) {
            return std::borrow::Cow::Owned(p);
        }
    }
    std::borrow::Cow::Borrowed(v)
}

impl W<'_> {
    fn event(&self, kind: &str, subject: Option<&str>, payload: serde_json::Value) -> Result<i64> {
        self.tx
            .prepare_cached(
                "INSERT INTO events(kind, subject, conn_id, conversation_id, actor, model, provider, build, payload_json, created_at)
                 VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10)",
            )?
            .execute(params![
                kind, subject, self.cx.conn_id, self.cx.conversation_id, self.cx.actor, self.cx.model,
                self.cx.provider, self.cx.build, payload.to_string(), self.ts
            ])?;
        Ok(self.tx.last_insert_rowid())
    }

    /// 記憶のイベント。payloadはv3の memory_events と同じ形（reason, old_value, new_value）。
    fn mem_event(&self, kind: &str, id: &str, reason: Option<&str>, old: Option<&str>, new: Option<&str>) -> Result<i64> {
        self.event(kind, Some(id), json!({ "reason": reason, "old_value": old, "new_value": new }))
    }

    fn require_reason(&self, op: &str, reason: &str) -> Result<()> {
        if reason.trim().is_empty() {
            return pre(format!("{op} には理由(reason)が必須です"));
        }
        Ok(())
    }

    fn status(&self, id: &str) -> Result<Option<String>> {
        Ok(self.tx.query_row("SELECT status FROM memory_state WHERE id=?1", [id], |r| r.get(0)).optional()?)
    }

    fn successor(&self, id: &str) -> Result<Option<String>> {
        Ok(self
            .tx
            .query_row(
                "SELECT src FROM relations WHERE dst=?1 AND kind='supersedes' ORDER BY created_at DESC LIMIT 1",
                [id],
                |r| r.get(0),
            )
            .optional()?)
    }

    fn set_status(&self, id: &str, status: &str, seq: i64) -> Result<()> {
        self.tx.execute(
            "UPDATE memory_state SET status=?2, status_origin='v4', updated_at=?3, updated_seq=?4 WHERE id=?1",
            params![id, status, self.ts, seq],
        )?;
        Ok(())
    }

    fn relation(&self, src: &str, dst: &str, kind: &str, reason: Option<&str>) -> Result<()> {
        self.tx.execute(
            "INSERT OR IGNORE INTO relations(src, dst, kind, actor, reason, created_at) VALUES (?1, ?2, ?3, ?4, ?5, ?6)",
            params![src, dst, kind, self.cx.actor, reason, self.ts],
        )?;
        Ok(())
    }

    fn insert_memory(&self, m: &NewMemory, reason: &str) -> Result<i64> {
        if m.topic.trim().is_empty() {
            return pre("topic は必須です");
        }
        if m.content.trim().is_empty() {
            return pre("content は必須です");
        }
        if !IMPORTANCE.contains(&m.importance.as_str()) {
            return pre(format!("不正な重要度: {}", m.importance));
        }
        let tags_json = serde_json::to_string(&m.tags).expect("tags serialize");
        let hash = am_domain::content_hash(&m.topic, &tags_json, &m.content);
        let kind = MemoryKind::from_tags(&m.tags);
        let n = self.tx.execute(
            "INSERT OR IGNORE INTO memories(id, kind, topic, content, tags_json, author, source_ref, content_hash, author_conn, author_conversation, created_at, v3_updated_at)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, NULL)",
            params![
                m.id, kind.as_str(), m.topic, m.content, tags_json, self.cx.actor, m.source_ref, hash,
                self.cx.conn_id, self.cx.conversation_id, self.ts
            ],
        )?;
        if n == 0 {
            return pre(format!("idが衝突した: {}（もう一度書いてください）", m.id));
        }
        let seq = self.mem_event("created", &m.id, Some(reason), None, None)?;
        self.tx.execute(
            "INSERT INTO memory_state(id, status, status_origin, visibility, visibility_origin, importance, expires_at, updated_at, updated_seq)
             VALUES (?1, 'active', 'v4', 'warm', 'v4', ?2, ?3, ?4, ?5)",
            params![m.id, m.importance, m.expires_at, self.ts, seq],
        )?;
        self.tx.execute(
            "INSERT INTO memories_fts(id, topic, tags, content, author) VALUES (?1, ?2, ?3, ?4, ?5)",
            params![m.id, m.topic, m.tags.join(" "), m.content, self.cx.actor],
        )?;
        if let Some(src) = m.source_ref.as_deref().filter(|s| !s.trim().is_empty()) {
            self.mem_event("source_observed", &m.id, Some(reason), None, Some(src.trim()))?;
        }
        if let Some(env) = &m.env {
            self.event("recorded_env", Some(&m.id), env.clone())?;
        }
        self.insert_anchors(&m.id, &m.anchors)?;
        // 書いた直後に期限を過ぎている極短TTLでも、activeのまま漏らさない（v3と同じ）。
        if let Some(exp) = &m.expires_at {
            if exp.as_str() <= self.ts.as_str() {
                let s = self.mem_event("expired", &m.id, Some("TTL失効"), None, None)?;
                self.set_status(&m.id, "expired", s)?;
            }
        }
        Ok(seq)
    }

    fn insert_anchors(&self, id: &str, anchors: &[NewAnchor]) -> Result<()> {
        let start: i64 =
            self.tx.query_row("SELECT COALESCE(MAX(ord)+1, 0) FROM anchors WHERE memory_id=?1", [id], |r| r.get(0))?;
        let mut ins = self.tx.prepare_cached(
            "INSERT INTO anchors(memory_id, ord, raw, segment, anchor_json, capture_json, origin, recorded_at)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8)",
        )?;
        for (i, a) in anchors.iter().enumerate() {
            let anchor = serde_json::to_string(&a.anchor).expect("anchor serialize");
            let capture = a.capture.as_ref().map(|c| serde_json::to_string(c).expect("capture serialize"));
            ins.execute(params![id, start + i as i64, a.raw, a.segment, anchor, capture, a.origin, self.ts])?;
        }
        Ok(())
    }

    fn link_goal(&self, goal: &Option<String>, id: &str) -> Result<()> {
        if let Some(g) = goal {
            let exists: Option<String> =
                self.tx.query_row("SELECT id FROM goals WHERE id=?1 AND state='posted'", [g], |r| r.get(0)).optional()?;
            if exists.is_some() {
                self.tx.execute(
                    "INSERT OR IGNORE INTO goal_links(goal_id, memory_id, conn_id, created_at) VALUES (?1, ?2, ?3, ?4)",
                    params![g, id, self.cx.conn_id, self.ts],
                )?;
            }
        }
        Ok(())
    }

    fn write(
        &self,
        m: NewMemory,
        reason: &str,
        lineage: Option<am_domain::Lineage>,
        goal: Option<String>,
        uses: Vec<UseRecord>,
    ) -> Result<Applied> {
        let kind = MemoryKind::from_tags(&m.tags);
        if let Some(l) = &lineage {
            let parent_tags: Option<String> =
                self.tx.query_row("SELECT tags_json FROM memories WHERE id=?1", [&l.parent_id], |r| r.get(0)).optional()?;
            let Some(parent_tags) = parent_tags else {
                return pre(format!("parent_id が見つかりません: {}", l.parent_id));
            };
            let parent_is_unknown = parent_tags.contains("\"tk:unknown\"");
            if kind == MemoryKind::Hypothesis && !parent_is_unknown {
                return pre(format!("hypothesis の parent_id は UNKNOWN である必要があります: {}", l.parent_id));
            }
            if l.resolves && !parent_is_unknown {
                return pre(format!("resolves_unknown=true の parent_id は UNKNOWN である必要があります: {}", l.parent_id));
            }
        } else if kind == MemoryKind::Hypothesis {
            return pre("hypothesis には parent_id（対象UNKNOWN）が必須です");
        }
        let seq = self.insert_memory(&m, reason)?;
        if let Some(l) = &lineage {
            let k = reason.trim_start_matches("observe_context:");
            let r = l.reason.clone().unwrap_or_else(|| format!("{k} derived from {}", l.parent_id));
            self.relation(&m.id, &l.parent_id, "derived_from", Some(&r))?;
            if l.resolves {
                let r = l.reason.clone().unwrap_or_else(|| format!("reobservation resolves UNKNOWN {}", l.parent_id));
                self.relation(&m.id, &l.parent_id, "resolves", Some(&r))?;
                let r = l.reason.clone().unwrap_or_else(|| "再観測によってUNKNOWNを解決".into());
                self.mem_event("unknown_resolved", &l.parent_id, Some(&r), None, Some(&m.id))?;
            }
        }
        self.link_goal(&goal, &m.id)?;
        let used = self.record_uses(uses, Some(seq))?;
        Ok(applied(vec![seq], json!({ "id": m.id, "uses": used.detail })))
    }

    fn supersede(
        &self,
        old_id: &str,
        m: NewMemory,
        reason: &str,
        goal: Option<String>,
        uses: Vec<UseRecord>,
    ) -> Result<Applied> {
        self.require_reason("supersede_context", reason)?;
        let Some(status) = self.status(old_id)? else {
            return pre(format!("置換対象が見つかりません: {old_id}"));
        };
        let successor = self.successor(old_id)?;
        if status != "active" || successor.is_some() {
            return pre(format!(
                "置換対象 {old_id} は active ではありません (status={status})"
            ) + &successor.map(|s| format!("。後継 {s} を確認してください")).unwrap_or_default());
        }
        let seq = self.insert_memory(&m, &format!("supersedes {old_id}: {reason}"))?;
        self.relation(&m.id, old_id, "supersedes", Some(reason))?;
        let s2 = self.mem_event("superseded", old_id, Some(reason), None, Some(&m.id))?;
        self.set_status(old_id, "superseded", s2)?;
        // 学習tagsの強度は、後継へ半分を引き継ぐ（§6.4）。
        self.tx.execute(
            "INSERT INTO learned_evidence(memory_id, term, scope_key, weight, use_id, created_at)
             SELECT ?2, term, scope_key, weight * 0.5, use_id, created_at FROM learned_evidence WHERE memory_id=?1",
            params![old_id, m.id],
        )?;
        rebuild_learned(self.tx, Some(&m.id))?;
        self.link_goal(&goal, &m.id)?;
        let used = self.record_uses(uses, Some(seq))?;
        Ok(applied(vec![seq, s2], json!({ "id": m.id, "old_id": old_id, "uses": used.detail })))
    }

    fn retract(&self, id: &str, reason: &str) -> Result<Applied> {
        self.require_reason("retract_context", reason)?;
        match self.status(id)?.as_deref() {
            None => return pre(format!("対象が見つかりません: {id}")),
            Some("retracted") => return Ok(applied(vec![], json!({ "unchanged": true }))),
            _ => {}
        }
        let seq = self.mem_event("retracted", id, Some(reason), None, None)?;
        self.set_status(id, "retracted", seq)?;
        Ok(applied(vec![seq], json!({})))
    }

    fn restore(&self, id: &str, reason: &str) -> Result<Applied> {
        self.require_reason("restore_context", reason)?;
        let Some(prev) = self.status(id)? else {
            return pre(format!("復帰対象が見つかりません: {id}"));
        };
        if prev == "active" {
            return Ok(applied(vec![], json!({ "unchanged": true })));
        }
        if let Some(s) = self.successor(id)? {
            return pre(format!(
                "記録 {id} には後継 {s} があるため current へ復帰できません。旧版の本文は read_context で参照できます"
            ));
        }
        let mut seqs = vec![];
        let expires: Option<String> =
            self.tx.query_row("SELECT expires_at FROM memory_state WHERE id=?1", [id], |r| r.get(0))?;
        // 明示的に戻したものが、次の読取りでまた期限切れにならないよう、期限を外す。
        if let Some(e) = expires.filter(|e| e.as_str() <= self.ts.as_str()) {
            let s = self.mem_event("ttl_cleared", id, Some(reason), Some(&e), None)?;
            self.tx.execute("UPDATE memory_state SET expires_at=NULL WHERE id=?1", [id])?;
            seqs.push(s);
        }
        let seq = self.mem_event("restored", id, Some(reason), Some(&prev), Some("active"))?;
        self.set_status(id, "active", seq)?;
        seqs.push(seq);
        Ok(applied(seqs, json!({ "from": prev })))
    }

    fn change_importance(&self, id: &str, importance: &str, reason: &str) -> Result<Applied> {
        self.require_reason("change_importance", reason)?;
        if !IMPORTANCE.contains(&importance) {
            return pre(format!("不正な重要度: {importance}"));
        }
        let old: Option<String> =
            self.tx.query_row("SELECT importance FROM memory_state WHERE id=?1", [id], |r| r.get(0)).optional()?;
        let Some(old) = old else { return pre(format!("対象が見つかりません: {id}")) };
        let seq = self.mem_event("importance_changed", id, Some(reason), Some(&old), Some(importance))?;
        self.tx.execute(
            "UPDATE memory_state SET importance=?2, updated_at=?3, updated_seq=?4 WHERE id=?1",
            params![id, importance, self.ts, seq],
        )?;
        Ok(applied(vec![seq], json!({ "from": old, "to": importance })))
    }

    fn set_visibility(&self, id: &str, visibility: &str, reason: &str) -> Result<Applied> {
        self.require_reason("set_visibility", reason)?;
        if !VISIBILITY.contains(&visibility) {
            return pre(format!("不正なvisibility: {visibility}"));
        }
        let old: Option<String> =
            self.tx.query_row("SELECT visibility FROM memory_state WHERE id=?1", [id], |r| r.get(0)).optional()?;
        let Some(old) = old else { return pre(format!("対象が見つかりません: {id}")) };
        if old == visibility {
            return Ok(applied(vec![], json!({ "unchanged": true, "from": old, "to": visibility })));
        }
        let seq = self.mem_event("visibility_changed", id, Some(reason), Some(&old), Some(visibility))?;
        self.tx.execute(
            "UPDATE memory_state SET visibility=?2, visibility_origin='v4', updated_seq=?3 WHERE id=?1",
            params![id, visibility, seq],
        )?;
        Ok(applied(vec![seq], json!({ "from": old, "to": visibility })))
    }

    fn expire_due(&self) -> Result<Applied> {
        let due: Vec<String> = self
            .tx
            .prepare("SELECT id FROM memory_state WHERE status='active' AND expires_at IS NOT NULL AND expires_at <= ?1")?
            .query_map([&self.ts], |r| r.get(0))?
            .collect::<std::result::Result<_, _>>()?;
        let mut seqs = vec![];
        for id in &due {
            let s = self.mem_event("expired", id, Some("TTL失効"), None, None)?;
            self.set_status(id, "expired", s)?;
            seqs.push(s);
        }
        Ok(applied(seqs, json!({ "expired": due })))
    }

    fn set_state(&self, key: &str, value: serde_json::Value, expected: Option<serde_json::Value>) -> Result<Applied> {
        if let Some(expected) = expected {
            let cur: Option<String> =
                self.tx.query_row("SELECT value_json FROM state WHERE key=?1", [key], |r| r.get(0)).optional()?;
            let current: serde_json::Value = match &cur {
                Some(s) => serde_json::from_str(s).map_err(|e| StoreError::BadLegacyValue(format!("state {key}: {e}")))?,
                None => serde_json::Value::Null,
            };
            if cas_view(&current) != cas_view(&expected) {
                return Err(StoreError::CasConflict {
                    key: key.into(),
                    current: current.to_string(),
                    expected: expected.to_string(),
                });
            }
        }
        let value_json = value.to_string();
        let seq = self.event("state_set", Some(key), json!({ "value": value }))?;
        self.tx.execute(
            "INSERT INTO state(key, value_json, updated_at, updated_seq) VALUES (?1, ?2, ?3, ?4)
             ON CONFLICT(key) DO UPDATE SET value_json=excluded.value_json, updated_at=excluded.updated_at, updated_seq=excluded.updated_seq",
            params![key, value_json, self.ts, seq],
        )?;
        Ok(applied(vec![seq], json!({ "updated_at": self.ts })))
    }

    fn insert_utterance(&self, u: &Utterance) -> Result<()> {
        self.tx.execute(
            "INSERT OR IGNORE INTO utterances(id, client, session, file, line_no, line_sha256, quoted_span, said_at, verified_at, verified_by)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, 'daemon')",
            params![u.id, u.client, u.session, u.file, u.line_no, u.line_sha256, u.quoted_span, u.said_at, self.ts],
        )?;
        Ok(())
    }

    fn goal_row(&self, id: &str) -> Result<Option<(String, String, Option<String>)>> {
        Ok(self
            .tx
            .query_row("SELECT kind, state, parent FROM goals WHERE id=?1", [id], |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)))
            .optional()?)
    }

    fn post_goal(&self, g: NewGoal) -> Result<Applied> {
        let title_len = g.title.chars().count();
        if g.title.trim().is_empty() || title_len > 24 {
            return pre(format!("札の名前は1〜24字（今は{title_len}字）。切り詰めずに返すので、短くして掲げ直してください"));
        }
        match g.kind.as_str() {
            "overall" => {
                if g.parent.is_some() {
                    return pre("全体目標は親を持てない");
                }
                if g.utterance.is_none() || !g.basis.iter().any(|b| matches!(b, Basis::Utterance { .. })) {
                    return pre("全体目標には、照合済みのユーザー発言が必須");
                }
            }
            "work" => {
                if let Some(p) = &g.parent {
                    match self.goal_row(p)? {
                        None => return pre(format!("親の札が無い: {p}")),
                        Some((_, state, _)) if state != "posted" => {
                            return pre(format!("親の札 {p} は下ろされている（{state}）"))
                        }
                        _ => {}
                    }
                } else if g.basis.is_empty() {
                    return pre("最上位の作業目標には根拠（quote か note）が必須");
                }
            }
            k => return pre(format!("不正なkind: {k}（overall / work）")),
        }
        if let Some(u) = &g.utterance {
            self.insert_utterance(u)?;
        }
        let mut basis = g.basis.clone();
        if let Some(p) = &g.parent {
            basis.insert(0, Basis::Parent { goal_id: p.clone() });
        }
        let basis_json = serde_json::to_string(&basis).expect("basis serialize");
        let seq = self.event(
            "goal_posted",
            Some(&g.id),
            json!({ "kind": g.kind, "parent": g.parent, "title": g.title, "basis": basis }),
        )?;
        self.tx.execute(
            "INSERT INTO goals(id, kind, parent, title, basis_json, state, merged_into, posted_by, posted_conn, posted_conversation, posted_at, updated_seq)
             VALUES (?1, ?2, ?3, ?4, ?5, 'posted', NULL, ?6, ?7, ?8, ?9, ?10)",
            params![g.id, g.kind, g.parent, g.title, basis_json, self.cx.actor, self.cx.conn_id, self.cx.conversation_id, self.ts, seq],
        )?;
        if let Some(p) = &g.resolves_proposal {
            self.tx.execute("UPDATE goal_proposals SET resolved_seq=?2 WHERE id=?1", params![p, seq])?;
        }
        Ok(applied(vec![seq], json!({ "id": g.id })))
    }

    fn edit_goal(&self, id: &str, title: Option<String>, basis_note: Option<String>) -> Result<Applied> {
        let Some((kind, state, _)) = self.goal_row(id)? else { return pre(format!("札が無い: {id}")) };
        if state != "posted" {
            return pre(format!("札 {id} は下ろされている（{state}）。下ろした札は編集しない"));
        }
        if let Some(t) = &title {
            let n = t.chars().count();
            if t.trim().is_empty() || n > 24 {
                return pre(format!("札の名前は1〜24字（今は{n}字）"));
            }
            if kind == "overall" {
                return pre("全体目標の名前は、照合した発言を根拠に掲げ直す（post_goal kind=overall）");
            }
        }
        let seq = self.event("goal_edited", Some(id), json!({ "title": title, "note": basis_note }))?;
        if let Some(t) = &title {
            self.tx.execute("UPDATE goals SET title=?2, updated_seq=?3 WHERE id=?1", params![id, t, seq])?;
        }
        if let Some(n) = &basis_note {
            let cur: String = self.tx.query_row("SELECT basis_json FROM goals WHERE id=?1", [id], |r| r.get(0))?;
            let mut basis: Vec<Basis> = serde_json::from_str(&cur).unwrap_or_default();
            basis.push(Basis::Initiative { note: n.clone(), from: None });
            self.tx.execute(
                "UPDATE goals SET basis_json=?2, updated_seq=?3 WHERE id=?1",
                params![id, serde_json::to_string(&basis).expect("basis"), seq],
            )?;
        }
        Ok(applied(vec![seq], json!({})))
    }

    fn close_goal(&self, id: &str, how: GoalClose, reason: Option<String>, utterance: Option<Utterance>) -> Result<Applied> {
        let Some((kind, state, _)) = self.goal_row(id)? else { return pre(format!("札が無い: {id}")) };
        if state != "posted" {
            return pre(format!("札 {id} は既に下ろされている（{state}）"));
        }
        if kind == "overall" && utterance.is_none() {
            return pre("全体目標を下ろすにも、照合済みのユーザー発言が必須");
        }
        let (new_state, into) = match &how {
            GoalClose::Done => ("done", None),
            GoalClose::Dropped => ("dropped", None),
            GoalClose::Merged { into } => {
                if into == id {
                    return pre("自分自身へは統合できない");
                }
                match self.goal_row(into)? {
                    None => return pre(format!("統合先の札が無い: {into}")),
                    Some((_, s, _)) if s != "posted" => return pre(format!("統合先 {into} は下ろされている（{s}）")),
                    _ => {}
                }
                ("merged", Some(into.clone()))
            }
        };
        let close_basis = utterance.as_ref().map(|u| {
            serde_json::to_string(&Basis::Utterance { utterance_id: u.id.clone(), quote: u.quoted_span.clone() })
                .expect("basis")
        });
        if let Some(u) = &utterance {
            self.insert_utterance(u)?;
        }
        let seq = self.event(
            "goal_closed",
            Some(id),
            json!({ "state": new_state, "into": into, "reason": reason, "utterance": utterance.as_ref().map(|u| &u.id) }),
        )?;
        self.tx.execute(
            "UPDATE goals SET state=?2, merged_into=?3, closed_at=?4, closed_by=?5, close_reason=?6, close_basis_json=?7, updated_seq=?8 WHERE id=?1",
            params![id, new_state, into, self.ts, self.cx.actor, reason, close_basis, seq],
        )?;
        Ok(applied(vec![seq], json!({ "state": new_state })))
    }

    fn propose_goal(&self, p: Proposal) -> Result<Applied> {
        let seq = self.event(
            "goal_proposed",
            Some(&p.id),
            json!({ "action": p.action, "goal_id": p.goal_id, "title": p.title, "quote": p.quote, "reason": p.reason }),
        )?;
        self.tx.execute(
            "INSERT INTO goal_proposals(id, action, goal_id, parent, title, quote, reason, proposed_by, conn_id, created_at, resolved_seq)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, NULL)",
            params![p.id, p.action, p.goal_id, p.parent, p.title, p.quote, p.reason, self.cx.actor, self.cx.conn_id, self.ts],
        )?;
        Ok(applied(vec![seq], json!({ "id": p.id })))
    }

    fn record_connection(&self, c: am_domain::ConnectionRecord) -> Result<Applied> {
        let conv = c.conversation.as_ref().map(|(client, session)| format!("{client}:{session}"));
        if let (Some(id), Some((client, session))) = (&conv, &c.conversation) {
            self.tx.execute(
                "INSERT INTO conversations(conversation_id, client, session, gen, source, first_seen, last_seen)
                 VALUES (?1, ?2, ?3, 0, 'env', ?4, ?4) ON CONFLICT(conversation_id) DO UPDATE SET last_seen=excluded.last_seen",
                params![id, client, session, self.ts],
            )?;
        }
        let seq = self.event("connection_opened", Some(&c.conn_id), json!({ "client": c.client, "level": c.level, "conversation": conv }))?;
        self.tx.execute(
            "INSERT OR REPLACE INTO connections(conn_id, client, cwd, shim_pid, parents_json, conversation_id, level, build, started_at)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9)",
            params![c.conn_id, c.client, c.cwd, c.shim_pid, c.parents.to_string(), conv, c.level, c.build, self.ts],
        )?;
        Ok(applied(vec![seq], json!({})))
    }

    fn bump_conversation(&self, id: &str, client: &str, session: &str, source: &str, bump: bool) -> Result<Applied> {
        self.tx.execute(
            "INSERT INTO conversations(conversation_id, client, session, gen, source, first_seen, last_seen)
             VALUES (?1, ?2, ?3, 0, ?4, ?5, ?5) ON CONFLICT(conversation_id) DO UPDATE SET last_seen=excluded.last_seen, source=excluded.source",
            params![id, client, session, source, self.ts],
        )?;
        if bump {
            self.tx.execute("UPDATE conversations SET gen=gen+1 WHERE conversation_id=?1", [id])?;
        }
        let gen: i64 = self.tx.query_row("SELECT gen FROM conversations WHERE conversation_id=?1", [id], |r| r.get(0))?;
        let seq = self.event("conversation_seen", Some(id), json!({ "source": source, "bump": bump, "gen": gen }))?;
        Ok(applied(vec![seq], json!({ "gen": gen })))
    }

    fn record_search(&self, s: am_domain::SearchRecord) -> Result<Applied> {
        self.tx.execute(
            "INSERT INTO searches(search_id, conn_id, conversation_id, gen, legacy_session_id, query_raw, query_terms_json, focus_goal, created_at, origin)
             VALUES (?1, ?2, ?3, ?4, NULL, ?5, ?6, ?7, ?8, 'v4')",
            params![
                s.search_id, self.cx.conn_id, self.cx.conversation_id, s.gen, s.query_raw,
                serde_json::to_string(&s.query_terms).expect("terms"), s.focus_goal, self.ts
            ],
        )?;
        let mut ins = self.tx.prepare_cached(
            "INSERT INTO search_results(result_id, search_id, rank, memory_id, base_score, reasons_json) VALUES (?1, ?2, ?3, ?4, ?5, ?6)",
        )?;
        for (i, (id, score, reasons)) in s.results.iter().enumerate() {
            ins.execute(params![
                format!("{}:{}", s.search_id, i + 1), s.search_id, (i + 1) as i64, id, score,
                serde_json::to_string(reasons).expect("reasons")
            ])?;
        }
        Ok(applied(vec![], json!({})))
    }

    fn record_read(&self, r: am_domain::ReadRecord) -> Result<Applied> {
        self.tx.execute(
            "INSERT INTO reads(conn_id, conversation_id, gen, memory_id, search_id, link, created_at) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7)",
            params![self.cx.conn_id, self.cx.conversation_id, r.gen, r.memory_id, r.search_id, r.link, self.ts],
        )?;
        Ok(applied(vec![], json!({ "read_id": self.tx.last_insert_rowid() })))
    }

    /// uses を記録し、検索語があれば learned_evidence へ結ぶ。存在しない記憶は捨てて理由を返す。
    fn record_uses(&self, uses: Vec<UseRecord>, by_seq: Option<i64>) -> Result<Applied> {
        let mut recorded = vec![];
        let mut skipped = vec![];
        for u in uses {
            let exists: Option<String> =
                self.tx.query_row("SELECT id FROM memories WHERE id=?1", [&u.memory_id], |r| r.get(0)).optional()?;
            if exists.is_none() {
                skipped.push(json!({ "id": u.memory_id, "reason": "記憶が無い" }));
                continue;
            }
            self.tx.execute(
                "INSERT INTO uses(memory_id, kind, weight, search_id, scope_key, by_event_seq, created_at) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7)",
                params![u.memory_id, u.kind, u.weight, u.search_id, u.scope_key, by_seq, self.ts],
            )?;
            let use_id = self.tx.last_insert_rowid();
            for t in &u.terms {
                self.tx.execute(
                    "INSERT INTO learned_evidence(memory_id, term, scope_key, weight, use_id, created_at) VALUES (?1, ?2, ?3, ?4, ?5, ?6)",
                    params![u.memory_id, t, u.scope_key, u.weight, use_id, self.ts],
                )?;
                bump_learned(self.tx, &u.memory_id, t, &u.scope_key, &u.kind, u.weight, &self.ts)?;
            }
            recorded.push(json!({ "id": u.memory_id, "kind": u.kind }));
        }
        Ok(applied(vec![], json!({ "recorded": recorded, "skipped": skipped })))
    }

    fn publish_embedding(&self, id: &str, model_id: &str, hash: &str, vector: &[u8], computed_by: &str) -> Result<Applied> {
        let active: Option<String> =
            self.tx.query_row("SELECT value FROM meta WHERE key='active_model_id'", [], |r| r.get(0)).optional()?;
        let cur_hash: Option<String> =
            self.tx.query_row("SELECT content_hash FROM memories WHERE id=?1", [id], |r| r.get(0)).optional()?;
        let why = if active.as_deref() != Some(model_id) {
            Some(format!("model_idが今のものと違う（active={active:?}）"))
        } else if cur_hash.as_deref() != Some(hash) {
            Some("content_hashが記憶と合わない".to_string())
        } else {
            None
        };
        if let Some(why) = why {
            let seq = self.event("embedding_discarded", Some(id), json!({ "model_id": model_id, "reason": why }))?;
            return Ok(applied(vec![seq], json!({ "discarded": why })));
        }
        self.tx.execute(
            "INSERT OR REPLACE INTO embeddings(id, model_id, content_hash, vector, computed_by) VALUES (?1, ?2, ?3, ?4, ?5)",
            params![id, model_id, hash, vector, computed_by],
        )?;
        Ok(applied(vec![], json!({})))
    }

    fn anchor_observations(&self, obs: Vec<AnchorObservation>) -> Result<Applied> {
        let seq = self.event("anchor_observed", None, json!({ "count": obs.len() }))?;
        let mut ins = self.tx.prepare_cached(
            "INSERT INTO anchor_observations(memory_id, ord, observed_at, status, detail_json, seq)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6)",
        )?;
        for o in &obs {
            let detail = serde_json::to_string(&o.status).expect("AnchorStatus is always serializable");
            // 外部キーで、存在しない取手への観測はここで失敗する。
            ins.execute(params![o.memory_id, o.ord, o.observed_at, status_name(&o.status), detail, seq])?;
        }
        Ok(Applied { command: "", event_seqs: vec![seq], counts: vec![("anchor_observations", obs.len())], detail: json!({}) })
    }
}

/// 学習tagsの集計（schema 4）。uses と learned_evidence は証拠として残し、検索のたびに数え直さないよう、
/// (記憶, 語, 種類) ごとの合計を持つ。種類は useful（役に立った）と recall（検索から選んで読まれた）。
/// 数え方は §6.4 と同じ：会話（scope）ごとに正の重みは最大の1回、goal_done は足し、否定は引く。
fn learned_class(kind: &str) -> &'static str {
    if kind == "recalled" { "recall" } else { "useful" }
}

fn bump_learned(c: &Connection, memory: &str, term: &str, scope: &str, kind: &str, w: f64, at: &str) -> Result<()> {
    bump_learned_class(c, memory, term, scope, kind, learned_class(kind), w, at)?;
    if kind == "rejected" && w < 0.0 {
        // 「選んで読んだ」の後の「無関係」は、思い出す語にも反映する。有用性とは別に記録する。
        bump_learned_class(c, memory, term, scope, kind, "recall", w, at)?;
    }
    Ok(())
}

#[allow(clippy::too_many_arguments)]
fn bump_learned_class(c: &Connection, memory: &str, term: &str, scope: &str, kind: &str, class: &str, w: f64, at: &str) -> Result<()> {
    let old: Option<(f64, f64, f64)> = c
        .query_row(
            "SELECT max_pos, additive, neg FROM learned_scope WHERE memory_id=?1 AND term=?2 AND class=?3 AND scope_key=?4",
            params![memory, term, class, scope],
            |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)),
        )
        .optional()?;
    let (mut max_pos, mut additive, mut neg) = old.unwrap_or((0.0, 0.0, 0.0));
    let before = max_pos + additive + neg;
    if w < 0.0 {
        if class == "recall" {
            // 同じ検索参照を別接続で否定し直しても、同じscopeの否定を積み増さない。
            neg = neg.min(w);
        } else {
            neg += w;
        }
    } else if kind == "goal_done" {
        additive += w;
    } else {
        max_pos = max_pos.max(w);
    }
    let pos_at = (w > 0.0).then_some(at);
    c.execute(
        "INSERT INTO learned_scope(memory_id, term, class, scope_key, max_pos, additive, neg, last_pos_at) VALUES (?1,?2,?3,?4,?5,?6,?7,?8)
         ON CONFLICT(memory_id, term, class, scope_key) DO UPDATE SET max_pos=excluded.max_pos, additive=excluded.additive, neg=excluded.neg,
           last_pos_at=COALESCE(excluded.last_pos_at, learned_scope.last_pos_at)",
        params![memory, term, class, scope, max_pos, additive, neg, pos_at],
    )?;
    let delta = max_pos + additive + neg - before;
    c.execute(
        "INSERT INTO learned_tags(memory_id, term, class, raw, last_pos_at) VALUES (?1,?2,?3,?4,?5)
         ON CONFLICT(memory_id, term, class) DO UPDATE SET raw=learned_tags.raw + excluded.raw,
           last_pos_at=CASE WHEN excluded.last_pos_at IS NOT NULL AND (learned_tags.last_pos_at IS NULL OR excluded.last_pos_at > learned_tags.last_pos_at)
                            THEN excluded.last_pos_at ELSE learned_tags.last_pos_at END",
        params![memory, term, class, delta, pos_at],
    )?;
    Ok(())
}

/// 証拠（learned_evidence × uses.kind）から集計を作り直す。`memory` が無ければ全部。
fn rebuild_learned(c: &Connection, memory: Option<&str>) -> Result<()> {
    c.execute("DELETE FROM learned_scope WHERE ?1 IS NULL OR memory_id=?1", [memory])?;
    c.execute("DELETE FROM learned_tags WHERE ?1 IS NULL OR memory_id=?1", [memory])?;
    let rows: Vec<(String, String, String, f64, String, String)> = c
        .prepare(
            "SELECT le.memory_id, le.term, le.scope_key, le.weight, le.created_at, COALESCE(u.kind, '') FROM learned_evidence le
               LEFT JOIN uses u ON u.use_id=le.use_id WHERE ?1 IS NULL OR le.memory_id=?1 ORDER BY le.rowid",
        )?
        .query_map([memory], |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?, r.get(4)?, r.get(5)?)))?
        .collect::<std::result::Result<_, _>>()?;
    for (m, t, scope, w, at, kind) in rows {
        bump_learned(c, &m, &t, &scope, &kind, w, &at)?;
    }
    Ok(())
}

pub fn status_name(s: &AnchorStatus) -> &'static str {
    match s {
        AnchorStatus::Present { .. } => "present",
        AnchorStatus::Moved { .. } => "moved",
        AnchorStatus::SameContentAt { .. } => "same_content_at",
        AnchorStatus::RecoverableFromGit { .. } => "recoverable_from_git",
        AnchorStatus::Missing { .. } => "missing",
        AnchorStatus::NotChecked { .. } => "not_checked",
    }
}

/// v3のstoreを取り込む。`late` は凍結後の差分で、既にある記憶には触らない。
fn import_v3(w: &W, b: V3Batch, late: bool) -> Result<Applied> {
    let tx = w.tx;
    let existing: i64 = tx.query_row("SELECT count(*) FROM memories", [], |r| r.get(0))?;
    let imported: Option<String> =
        tx.query_row("SELECT value FROM meta WHERE key='imported_from'", [], |r| r.get(0)).optional()?;
    if !late && (existing > 0 || imported.is_some()) {
        return pre("ImportV3 は空のv4 DBにだけ行える");
    }
    if late && imported.is_none() {
        return pre("ImportV3Late は、v3から取り込んだv4 DBにだけ行える");
    }
    let known: std::collections::HashSet<String> = if late {
        tx.prepare("SELECT id FROM memories")?.query_map([], |r| r.get(0))?.collect::<std::result::Result<_, _>>()?
    } else {
        Default::default()
    };
    let chunks: Vec<_> = b.chunks.iter().filter(|c| !known.contains(&c.id)).collect();
    let new_ids: std::collections::HashSet<&str> = chunks.iter().map(|c| c.id.as_str()).collect();
    let keep = |chunk_id: &str| !late || new_ids.contains(chunk_id);

    // v3の履歴イベントは、元の時刻の順に先に積む。
    let mut v3_events: Vec<_> = b.events.iter().filter(|e| keep(&e.chunk_id)).collect();
    v3_events.sort_by(|a, b| a.created_at.cmp(&b.created_at).then(a.event_id.cmp(&b.event_id)));
    {
        let mut ins = tx.prepare_cached(
            "INSERT INTO events(kind, subject, conn_id, conversation_id, actor, model, provider, build, payload_json, created_at)
             VALUES (?1, ?2, NULL, NULL, ?3, ?4, ?5, 'v3', ?6, ?7)",
        )?;
        for e in &v3_events {
            let payload = json!({
                "v3_event_id": e.event_id, "reason": e.reason,
                "old_value": e.old_value, "new_value": e.new_value,
            });
            ins.execute(params![
                format!("v3.{}", e.event_type), e.chunk_id, e.actor, e.model, e.provider,
                payload.to_string(), e.created_at
            ])?;
        }
    }

    let import_seq = w.event(
        if late { "import_v3_late" } else { "import_v3" },
        None,
        json!({
            "source_path": b.source_path, "snapshot_sha256": b.snapshot_sha256,
            "chunks": chunks.len(), "events": v3_events.len(),
        }),
    )?;

    let mut defaults_status = 0usize;
    let mut defaults_visibility = 0usize;
    let mut embeddings = 0usize;
    {
        let mut ins_mem = tx.prepare_cached(
            "INSERT INTO memories(id, kind, topic, content, tags_json, author, source_ref, content_hash, author_conn, author_conversation, created_at, v3_updated_at)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, NULL, NULL, ?9, ?10)",
        )?;
        let mut ins_state = tx.prepare_cached(
            "INSERT INTO memory_state(id, status, status_origin, visibility, visibility_origin, importance, expires_at, updated_at, updated_seq)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9)",
        )?;
        let mut ins_emb = tx.prepare_cached(
            "INSERT INTO embeddings(id, model_id, content_hash, vector, computed_by) VALUES (?1, ?2, ?3, ?4, 'v3-transformers.js')",
        )?;
        let mut ins_fts = tx.prepare_cached("INSERT INTO memories_fts(id, topic, tags, content, author) VALUES (?1, ?2, ?3, ?4, ?5)")?;
        for c in &chunks {
            let tags: Vec<String> = serde_json::from_str(&c.tags_json)
                .map_err(|e| StoreError::BadLegacyValue(format!("chunk {} tags: {e}", c.id)))?;
            let kind = MemoryKind::from_tags(&tags);
            let hash = am_domain::content_hash(&c.topic, &c.tags_json, &c.content);
            ins_mem.execute(params![
                c.id, kind.as_str(), c.topic, c.content, c.tags_json, c.agent,
                c.source_ref, hash, c.created_at, c.updated_at
            ])?;
            let (status, status_origin) = match &c.status {
                Some(s) => (s.as_str(), "v3_row"),
                None => {
                    defaults_status += 1;
                    ("active", "v3_default")
                }
            };
            let (vis, vis_origin) = match &c.visibility {
                Some(v) => (v.as_str(), "v3_row"),
                None => {
                    defaults_visibility += 1;
                    ("warm", "v3_default")
                }
            };
            ins_state.execute(params![
                c.id, status, status_origin, vis, vis_origin, c.importance, c.expires_at, c.updated_at, import_seq
            ])?;
            if let Some(v) = &c.embedding {
                ins_emb.execute(params![c.id, b.embedding_model, hash, v])?;
                embeddings += 1;
            }
            ins_fts.execute(params![c.id, c.topic, tags.join(" "), c.content, c.agent])?;
        }
    }

    let mut relations = 0usize;
    {
        let mut ins = tx.prepare_cached(
            "INSERT OR IGNORE INTO relations(src, dst, kind, actor, reason, created_at) VALUES (?1, ?2, ?3, ?4, ?5, ?6)",
        )?;
        for r in b.relations.iter().filter(|r| keep(&r.source_id)) {
            relations += ins.execute(params![r.source_id, r.target_id, r.relation_type, r.actor, r.reason, r.created_at])?;
        }
    }
    if !late {
        let mut ins =
            tx.prepare_cached("INSERT INTO state(key, value_json, updated_at, updated_seq) VALUES (?1, ?2, ?3, ?4)")?;
        for s in &b.state {
            ins.execute(params![s.key, s.value, s.updated_at, import_seq])?;
        }
    }

    // v3の想起ログは、1回の検索が同じ (query, session, 時刻) で書かれている（logRecall）。
    let mut searches = 0usize;
    let mut results = 0usize;
    {
        let mut ins_search = tx.prepare_cached(
            "INSERT OR IGNORE INTO searches(search_id, conn_id, conversation_id, gen, legacy_session_id, query_raw, query_terms_json, focus_goal, created_at, origin)
             VALUES (?1, NULL, NULL, NULL, ?2, ?3, NULL, NULL, ?4, 'v3_recall_log')",
        )?;
        let mut ins_result = tx.prepare_cached(
            "INSERT OR IGNORE INTO search_results(result_id, search_id, rank, memory_id, base_score, reasons_json)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6)",
        )?;
        for r in &b.recalls {
            let key = json!([r.query, r.session_id, r.created_at]).to_string();
            let search_id = format!("v3:{}", &sha256_hex(&key)[..16]);
            searches += ins_search.execute(params![search_id, r.session_id, r.query, r.created_at])?;
            results += ins_result.execute(params![r.recall_id, search_id, r.rank, r.chunk_id, r.score, r.reason])?;
        }
    }
    if !late {
        let mut ins = tx.prepare_cached("INSERT INTO legacy_rows(table_name, row_json) VALUES (?1, ?2)")?;
        for l in &b.legacy {
            ins.execute(params![l.table, l.row_json])?;
        }
    }
    {
        let mut ins = tx.prepare_cached(
            "INSERT INTO anchors(memory_id, ord, raw, segment, anchor_json, capture_json, origin, recorded_at)
             VALUES (?1, ?2, ?3, ?4, ?5, NULL, 'v3_source_ref', ?6)",
        )?;
        for a in b.anchors.iter().filter(|a| keep(&a.memory_id)) {
            let json = serde_json::to_string(&a.anchor).expect("Anchor is always serializable");
            ins.execute(params![a.memory_id, a.ord, a.raw, a.segment, json, a.recorded_at])?;
        }
    }

    if late {
        tx.execute(
            "INSERT OR REPLACE INTO meta(key, value) VALUES ('late_imported_at', ?1)",
            params![w.ts],
        )?;
    } else {
        tx.execute(
            "INSERT INTO meta(key, value) VALUES ('imported_from', ?1), ('imported_snapshot_sha256', ?2), ('imported_at', ?3), ('v3_embedding_model', ?4)",
            params![b.source_path, b.snapshot_sha256, w.ts, b.embedding_model],
        )?;
        tx.execute("INSERT OR IGNORE INTO meta(key, value) VALUES ('active_model_id', ?1)", [&b.embedding_model])?;
    }

    Ok(Applied {
        command: "",
        event_seqs: vec![import_seq],
        counts: vec![
            ("memories", chunks.len()),
            ("status_defaulted", defaults_status),
            ("visibility_defaulted", defaults_visibility),
            ("embeddings", embeddings),
            ("v3_events", v3_events.len()),
            ("relations", relations),
            ("state", if late { 0 } else { b.state.len() }),
            ("searches", searches),
            ("search_results", results),
            ("legacy_rows", if late { 0 } else { b.legacy.len() }),
            ("anchors", b.anchors.iter().filter(|a| keep(&a.memory_id)).count()),
        ],
        detail: json!({}),
    })
}

fn sha256_hex(s: &str) -> String {
    use sha2::Digest;
    hex::encode(sha2::Sha256::digest(s.as_bytes()))
}
