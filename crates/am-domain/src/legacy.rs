//! v3のstoreから読んだ行。値は読んだまま持ち、ここでは解釈しない。

use crate::anchor::AnchorRow;

#[derive(Debug, Clone)]
pub struct V3Chunk {
    pub id: String,
    pub topic: String,
    pub agent: String,
    pub tags_json: String,
    pub importance: String,
    pub content: String,
    pub embedding: Option<Vec<u8>>,
    pub created_at: String,
    pub updated_at: String,
    pub expires_at: Option<String>,
    /// 最新の `source_observed` イベントの値（v3のreadRecordと同じ取り方）。
    pub source_ref: Option<String>,
    /// lifecycle行が無ければNone（v3はactiveとして扱っていた）。
    pub status: Option<String>,
    /// visibility行が無ければNone（v3はwarmとして扱っていた）。
    pub visibility: Option<String>,
}

#[derive(Debug, Clone)]
pub struct V3Event {
    pub event_id: String,
    pub chunk_id: String,
    pub event_type: String,
    pub actor: String,
    pub model: Option<String>,
    pub provider: Option<String>,
    pub reason: Option<String>,
    pub old_value: Option<String>,
    pub new_value: Option<String>,
    pub created_at: String,
}

#[derive(Debug, Clone)]
pub struct V3Relation {
    pub source_id: String,
    pub target_id: String,
    pub relation_type: String,
    pub actor: String,
    pub reason: Option<String>,
    pub created_at: String,
}

#[derive(Debug, Clone)]
pub struct V3State {
    pub key: String,
    pub value: String,
    pub updated_at: String,
}

#[derive(Debug, Clone)]
pub struct V3Recall {
    pub recall_id: String,
    pub chunk_id: String,
    pub session_id: Option<String>,
    pub query: Option<String>,
    pub reason: String,
    pub score: Option<f64>,
    pub rank: Option<i64>,
    pub created_at: String,
}

/// v4に専用の表がまだ無い行。表名と、行のJSONをそのまま持つ。
#[derive(Debug, Clone)]
pub struct LegacyRow {
    pub table: String,
    pub row_json: String,
}

#[derive(Debug, Clone, Default)]
pub struct V3Batch {
    pub source_path: String,
    pub snapshot_sha256: String,
    pub embedding_model: String,
    pub chunks: Vec<V3Chunk>,
    pub events: Vec<V3Event>,
    pub relations: Vec<V3Relation>,
    pub state: Vec<V3State>,
    pub recalls: Vec<V3Recall>,
    pub legacy: Vec<LegacyRow>,
    pub anchors: Vec<AnchorRow>,
}
