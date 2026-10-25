//! 取手（anchor）。現物の写しではなく、現物をもう一度見つけるための情報。

use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum Anchor {
    /// ファイル。`path` はworkspace rootからの相対（`external` ならrootの外の絶対パス）。
    File {
        path: String,
        external: bool,
        locator: Option<String>,
        commit: Option<String>,
    },
    Commit {
        sha: String,
        repo: Option<String>,
    },
    Conversation {
        raw: String,
    },
    Url {
        url: String,
    },
    /// 解釈できない取手。元の文字列のまま持つ。
    Opaque {
        raw: String,
    },
}

/// 記憶に付いた取手の1つ。`raw` は記録された文字列そのもの。
/// 1つの `raw` に複数の取手が書かれていれば、区切った部分ごとに1行にし、`segment` に持つ。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct AnchorRow {
    pub memory_id: String,
    pub ord: u32,
    pub raw: String,
    pub segment: String,
    pub anchor: Anchor,
    pub recorded_at: String,
}

/// 照合の結果。事実ではなく、その時点の観測として記録する。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "status", rename_all = "snake_case")]
pub enum AnchorStatus {
    /// 記録した場所にある。v3由来の取手は指紋を持たないので、中身が同じかは分からない。
    Present { blob: Option<String> },
    /// gitの改名・移動を辿った先にある。
    Moved { to: String, via_commit: String, hops: u32 },
    /// 同じ中身（blob）のファイルが別の場所にある。候補が複数なら全て出す。
    SameContentAt { paths: Vec<String>, blob: String },
    /// 今は無いが、gitの履歴から記録時の版を読める。
    RecoverableFromGit { commit: String, path: String },
    /// どこにも見つからない。
    Missing { reason: String },
    /// ファイルではない取手（commit、会話、URL）は、このS1では照合しない。
    NotChecked { reason: String },
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct AnchorObservation {
    pub memory_id: String,
    pub ord: u32,
    pub observed_at: String,
    pub status: AnchorStatus,
}

/// 記録したときの現物の指紋。daemonが自動で取る（§3.5）。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, Default)]
pub struct Capture {
    /// 記録時のファイルのgit blob sha（gitの外でも、同じ式で計算する）。
    pub blob: Option<String>,
    /// 記録時のHEAD。
    pub commit: Option<String>,
    /// 記録時に、indexの版から変わっていた（またはgitの管理外だった）。
    pub dirty: bool,
    /// locatorが指す範囲の指紋（正規化した本文のsha256）。
    pub span_fp: Option<String>,
    /// ファイルが無かった・読めなかったときの理由。
    pub note: Option<String>,
}

/// これから記録する取手。`origin` は agent（Agentが指定）/ touched（hookで観測した編集）/ v3_source_ref。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct NewAnchor {
    pub raw: String,
    pub segment: String,
    pub anchor: Anchor,
    pub capture: Option<Capture>,
    pub origin: String,
}
