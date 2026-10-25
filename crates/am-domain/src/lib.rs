//! ANTmemory v4 の型。I/Oを持たない。
//!
//! 記憶の本文はidごとに不変（v3と同じ）。変わるのは状態だけで、
//! DBを変える経路は `Command` を `am-store` の `apply` へ渡すことだけにする。

pub mod anchor;
pub mod command;
pub mod legacy;

use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

pub use anchor::{Anchor, AnchorObservation, AnchorRow, AnchorStatus, Capture, NewAnchor};
pub use command::*;
pub use legacy::V3Batch;

/// 本文のhash。topic・tags（保存された文字列のまま）・contentから作る。
/// 長さが異なる区切り方で同じ値にならないよう、JSON配列にしてからhashする。
pub fn content_hash(topic: &str, tags_json: &str, content: &str) -> String {
    let canonical = serde_json::json!(["am-memory-v1", topic, tags_json, content]).to_string();
    hex::encode(Sha256::digest(canonical.as_bytes()))
}

/// tagsの `tk:*` から観測の種類を読む。どれも無ければ通常の記録。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum MemoryKind {
    Note,
    Observation,
    Unknown,
    Hypothesis,
}

impl MemoryKind {
    pub fn from_tags(tags: &[String]) -> Self {
        let has = |t: &str| tags.iter().any(|x| x == t);
        if has("tk:unknown") {
            MemoryKind::Unknown
        } else if has("tk:hypothesis") {
            MemoryKind::Hypothesis
        } else if has("tk:observation") {
            MemoryKind::Observation
        } else {
            MemoryKind::Note
        }
    }

    pub fn as_str(self) -> &'static str {
        match self {
            MemoryKind::Note => "note",
            MemoryKind::Observation => "observation",
            MemoryKind::Unknown => "unknown",
            MemoryKind::Hypothesis => "hypothesis",
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn hash_separates_fields() {
        assert_ne!(content_hash("ab", "[]", "c"), content_hash("a", "[]", "bc"));
        assert_eq!(content_hash("a", "[]", "b"), content_hash("a", "[]", "b"));
    }

    #[test]
    fn kind_from_tags() {
        let t = |v: &[&str]| v.iter().map(|s| s.to_string()).collect::<Vec<_>>();
        assert_eq!(MemoryKind::from_tags(&t(&["tk:unknown", "x"])), MemoryKind::Unknown);
        assert_eq!(MemoryKind::from_tags(&t(&["tk:observation"])), MemoryKind::Observation);
        assert_eq!(MemoryKind::from_tags(&t(&["design"])), MemoryKind::Note);
    }
}
