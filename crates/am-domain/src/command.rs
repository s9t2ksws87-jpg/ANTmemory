//! DBを変える命令。`am-store` の `Store::apply` だけが受け取る。

use serde::{Deserialize, Serialize};

use crate::anchor::{AnchorObservation, NewAnchor};
use crate::legacy::V3Batch;

pub const IMPORTANCE: [&str; 4] = ["low", "medium", "high", "critical"];
pub const VISIBILITY: [&str; 4] = ["hot", "warm", "cold", "archive"];

/// 新しい記憶の本文。idは呼ぶ側が作る（v3と同じ10桁のhex）。
#[derive(Debug, Clone)]
pub struct NewMemory {
    pub id: String,
    pub topic: String,
    pub content: String,
    pub tags: Vec<String>,
    pub importance: String,
    pub expires_at: Option<String>,
    pub source_ref: Option<String>,
    pub anchors: Vec<NewAnchor>,
    /// 書いたときの環境（workspace・repoの地点など）。取手が無い記録にも付ける。本文には混ぜない。
    pub env: Option<serde_json::Value>,
}

/// observe_context の系譜。`resolves` はparentのUNKNOWNを解決する観測。
#[derive(Debug, Clone)]
pub struct Lineage {
    pub parent_id: String,
    pub resolves: bool,
    pub reason: Option<String>,
}

/// 「役に立った」の観測1つ（§6.3）。`terms` はそのuseを結ぶ検索語（無ければ語は強めない）。
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct UseRecord {
    pub memory_id: String,
    pub kind: String,
    pub weight: f64,
    pub search_id: Option<String>,
    pub scope_key: String,
    pub terms: Vec<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SearchRecord {
    pub search_id: String,
    pub query_raw: String,
    pub query_terms: Vec<String>,
    pub gen: Option<i64>,
    pub focus_goal: Option<String>,
    /// (memory_id, base_score, reasons)
    pub results: Vec<(String, f64, Vec<String>)>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ReadRecord {
    pub memory_id: String,
    pub search_id: Option<String>,
    /// explicit / inferred / inferred_conn / ambiguous / direct
    pub link: String,
    pub gen: Option<i64>,
}

/// 照合したユーザー発言（§4.3）。全文は持たない。
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Utterance {
    pub id: String,
    pub client: String,
    pub session: String,
    pub file: String,
    pub line_no: i64,
    pub line_sha256: String,
    pub quoted_span: String,
    pub said_at: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum Basis {
    Utterance { utterance_id: String, quote: String },
    UnverifiedQuote { quote: String },
    Parent { goal_id: String },
    Initiative { note: String, from: Option<String> },
}

#[derive(Debug, Clone)]
pub struct NewGoal {
    pub id: String,
    pub kind: String,
    pub parent: Option<String>,
    pub title: String,
    pub basis: Vec<Basis>,
    pub utterance: Option<Utterance>,
    /// これを掲げたことで解決する候補（goal_proposals.id）。
    pub resolves_proposal: Option<String>,
}

#[derive(Debug, Clone)]
pub enum GoalClose {
    Done,
    Dropped,
    Merged { into: String },
}

#[derive(Debug, Clone)]
pub struct Proposal {
    pub id: String,
    /// post / close
    pub action: String,
    pub goal_id: Option<String>,
    pub parent: Option<String>,
    pub title: Option<String>,
    pub quote: String,
    pub reason: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ConnectionRecord {
    pub conn_id: String,
    pub client: String,
    pub cwd: Option<String>,
    pub shim_pid: Option<i64>,
    pub parents: serde_json::Value,
    pub conversation: Option<(String, String)>,
    pub level: String,
    pub build: String,
}

/// DBを変える命令。新しい操作はここへvariantを足し、`apply` のmatchで扱う。
#[derive(Debug)]
pub enum Command {
    /// v3の1つのstoreを丸ごと取り込む。空のv4 DBに対してだけ受け付ける。
    ImportV3(Box<V3Batch>),
    /// 凍結の後にv3へ書かれた記憶だけを取り込む（§10.3）。既にあるidは触らない。
    ImportV3Late(Box<V3Batch>),
    /// 取手を現物と照合した結果を追記する。
    RecordAnchorObservations(Vec<AnchorObservation>),
    /// write_context / observe_context。
    Write { mem: NewMemory, reason: String, lineage: Option<Lineage>, goal: Option<String>, uses: Vec<UseRecord> },
    /// correct_context / supersede_context。旧版は superseded になり、本文は残る。
    Supersede { old_id: String, mem: NewMemory, reason: String, goal: Option<String>, uses: Vec<UseRecord> },
    Retract { id: String, reason: String },
    Restore { id: String, reason: String },
    ChangeImportance { id: String, importance: String, reason: String },
    SetVisibility { id: String, visibility: String, reason: String },
    /// 期限を過ぎたactiveな記憶を expired にする。
    ExpireDue,
    /// `expected` があればCAS（今の値がJSONとして等しいときだけ書く）。
    SetState { key: String, value: serde_json::Value, expected: Option<serde_json::Value> },
    PostGoal(NewGoal),
    EditGoal { id: String, title: Option<String>, basis_note: Option<String> },
    CloseGoal { id: String, how: GoalClose, reason: Option<String>, utterance: Option<Utterance> },
    ProposeGoal(Proposal),
    RecordConnection(ConnectionRecord),
    /// 会話の世代を進める（圧縮・clear）。焦点と保持版はdaemon側で捨てる。
    BumpConversation { conversation_id: String, client: String, session: String, source: String, bump: bool },
    RecordSearch(SearchRecord),
    RecordRead(ReadRecord),
    RecordUses(Vec<UseRecord>),
    /// モデル切替も凍結検査・イベント記録を通す。以前のvectorは残す。
    ActivateModel { model_id: String },
    /// worker が計算したvectorを公開する。model_idとcontent_hashが合わなければ捨てる。
    PublishEmbedding { id: String, model_id: String, content_hash: String, vector: Vec<u8>, computed_by: String },
}

impl Command {
    pub fn kind(&self) -> &'static str {
        match self {
            Command::ImportV3(_) => "import_v3",
            Command::ImportV3Late(_) => "import_v3_late",
            Command::RecordAnchorObservations(_) => "record_anchor_observations",
            Command::Write { .. } => "write",
            Command::Supersede { .. } => "supersede",
            Command::Retract { .. } => "retract",
            Command::Restore { .. } => "restore",
            Command::ChangeImportance { .. } => "change_importance",
            Command::SetVisibility { .. } => "set_visibility",
            Command::ExpireDue => "expire_due",
            Command::SetState { .. } => "set_state",
            Command::PostGoal(_) => "post_goal",
            Command::EditGoal { .. } => "edit_goal",
            Command::CloseGoal { .. } => "close_goal",
            Command::ProposeGoal(_) => "propose_goal",
            Command::RecordConnection(_) => "record_connection",
            Command::BumpConversation { .. } => "bump_conversation",
            Command::RecordSearch(_) => "record_search",
            Command::RecordRead(_) => "record_read",
            Command::RecordUses(_) => "record_uses",
            Command::ActivateModel { .. } => "activate_model",
            Command::PublishEmbedding { .. } => "publish_embedding",
        }
    }
}

/// `apply` を呼ぶ側の身元。イベントに残す。
#[derive(Debug, Clone, Default)]
pub struct ApplyCtx {
    pub actor: String,
    pub build: String,
    pub conn_id: Option<String>,
    pub conversation_id: Option<String>,
    pub model: Option<String>,
    pub provider: Option<String>,
}
