//! v4のschema。
//!
//! v1（S1の予行だけで使った）からの変更は、記憶の操作・札・会話・検索の観測の表を足したこと。
//! 本番のv4 DBはまだ無いので、v1のDBは移行せず、予行をやり直す。

// v3: authorを全文索引へ戻す。v2からは派生索引だけをtransaction内で再構築する。
pub const SCHEMA_VERSION: i64 = 4;

pub const SCHEMA: &str = r#"
CREATE TABLE meta (
  key   TEXT PRIMARY KEY,
  value TEXT NOT NULL
);

-- 本文はidごとに不変。追記のみ。
CREATE TABLE memories (
  id                  TEXT PRIMARY KEY,
  kind                TEXT NOT NULL,
  topic               TEXT NOT NULL,
  content             TEXT NOT NULL,
  tags_json           TEXT NOT NULL,
  author              TEXT NOT NULL,
  source_ref          TEXT,
  content_hash        TEXT NOT NULL,
  author_conn         TEXT,
  author_conversation TEXT,
  created_at          TEXT NOT NULL,
  v3_updated_at       TEXT
);
CREATE INDEX idx_memories_created ON memories(created_at);

-- 変わり得る状態。*_origin は値の出所（v3の行・v3が暗黙に使っていた既定値・v4の操作）。
CREATE TABLE memory_state (
  id                TEXT PRIMARY KEY REFERENCES memories(id),
  status            TEXT NOT NULL,
  status_origin     TEXT NOT NULL,
  visibility        TEXT NOT NULL,
  visibility_origin TEXT NOT NULL,
  importance        TEXT NOT NULL,
  expires_at        TEXT,
  updated_at        TEXT NOT NULL,
  updated_seq       INTEGER NOT NULL
);

-- computed_by は、どの実装で計算したか（v3のtransformers.js / v4のtract）。
CREATE TABLE embeddings (
  id           TEXT NOT NULL REFERENCES memories(id),
  model_id     TEXT NOT NULL,
  content_hash TEXT NOT NULL,
  vector       BLOB NOT NULL,
  computed_by  TEXT NOT NULL,
  PRIMARY KEY (id, model_id)
);

CREATE TABLE events (
  seq             INTEGER PRIMARY KEY AUTOINCREMENT,
  kind            TEXT NOT NULL,
  subject         TEXT,
  conn_id         TEXT,
  conversation_id TEXT,
  actor           TEXT NOT NULL,
  model           TEXT,
  provider        TEXT,
  build           TEXT NOT NULL,
  payload_json    TEXT NOT NULL,
  created_at      TEXT NOT NULL
);
CREATE INDEX idx_events_subject ON events(subject);
CREATE INDEX idx_events_kind ON events(kind);

CREATE TABLE relations (
  src        TEXT NOT NULL,
  dst        TEXT NOT NULL,
  kind       TEXT NOT NULL,
  actor      TEXT NOT NULL,
  reason     TEXT,
  created_at TEXT NOT NULL,
  PRIMARY KEY (src, dst, kind)
);
CREATE INDEX idx_relations_dst ON relations(dst);

CREATE TABLE state (
  key         TEXT PRIMARY KEY,
  value_json  TEXT NOT NULL,
  updated_at  TEXT NOT NULL,
  updated_seq INTEGER NOT NULL
);

-- 接続（shim 1つ）と、clientの会話。会話は検証できる経路で識別子が届いたときだけ作る。
CREATE TABLE connections (
  conn_id         TEXT PRIMARY KEY,
  client          TEXT NOT NULL,
  cwd             TEXT,
  shim_pid        INTEGER,
  parents_json    TEXT NOT NULL,
  conversation_id TEXT,
  level           TEXT NOT NULL,
  build           TEXT NOT NULL,
  started_at      TEXT NOT NULL
);

CREATE TABLE conversations (
  conversation_id TEXT PRIMARY KEY,
  client          TEXT NOT NULL,
  session         TEXT NOT NULL,
  gen             INTEGER NOT NULL,
  source          TEXT NOT NULL,
  first_seen      TEXT NOT NULL,
  last_seen       TEXT NOT NULL
);

-- origin = 'v3_recall_log'：v3の想起ログを (query, session, 時刻) でまとめたもの。'v4'：v4の検索。
CREATE TABLE searches (
  search_id         TEXT PRIMARY KEY,
  conn_id           TEXT,
  conversation_id   TEXT,
  gen               INTEGER,
  legacy_session_id TEXT,
  query_raw         TEXT,
  query_terms_json  TEXT,
  focus_goal        TEXT,
  created_at        TEXT NOT NULL,
  origin            TEXT NOT NULL
);
CREATE INDEX idx_searches_conn ON searches(conn_id, created_at);

CREATE TABLE search_results (
  result_id    TEXT PRIMARY KEY,
  search_id    TEXT NOT NULL REFERENCES searches(search_id),
  rank         INTEGER,
  memory_id    TEXT NOT NULL,
  base_score   REAL,
  reasons_json TEXT NOT NULL
);
CREATE INDEX idx_search_results_memory ON search_results(memory_id);
CREATE INDEX idx_search_results_search ON search_results(search_id);

-- 本文を開いた観測。重みには使わず、後の uses をどの検索語へ結ぶかを決めるためだけに持つ。
CREATE TABLE reads (
  read_id         INTEGER PRIMARY KEY AUTOINCREMENT,
  conn_id         TEXT,
  conversation_id TEXT,
  gen             INTEGER,
  memory_id       TEXT NOT NULL,
  search_id       TEXT,
  link            TEXT NOT NULL,
  created_at      TEXT NOT NULL
);
CREATE INDEX idx_reads_conn ON reads(conn_id, created_at);
CREATE INDEX idx_reads_memory ON reads(memory_id);

-- 「役に立った」の観測。scope_key は数える単位（会話、無ければ接続）。
CREATE TABLE uses (
  use_id       INTEGER PRIMARY KEY AUTOINCREMENT,
  memory_id    TEXT NOT NULL,
  kind         TEXT NOT NULL,
  weight       REAL NOT NULL,
  search_id    TEXT,
  scope_key    TEXT NOT NULL,
  by_event_seq INTEGER,
  created_at   TEXT NOT NULL
);
CREATE INDEX idx_uses_memory ON uses(memory_id);

-- uses を検索語へ結んだもの。強度は読むときに減衰を掛けて作る（§6.4）。
CREATE TABLE learned_evidence (
  memory_id  TEXT NOT NULL,
  term       TEXT NOT NULL,
  scope_key  TEXT NOT NULL,
  weight     REAL NOT NULL,
  use_id     INTEGER NOT NULL,
  created_at TEXT NOT NULL
);
CREATE INDEX idx_learned_term ON learned_evidence(term);
CREATE INDEX idx_learned_memory ON learned_evidence(memory_id);

-- 札。basis_json は §4.2 の根拠。
CREATE TABLE goals (
  id                  TEXT PRIMARY KEY,
  kind                TEXT NOT NULL,
  parent              TEXT REFERENCES goals(id),
  title               TEXT NOT NULL,
  basis_json          TEXT NOT NULL,
  state               TEXT NOT NULL,
  merged_into         TEXT,
  posted_by           TEXT NOT NULL,
  posted_conn         TEXT,
  posted_conversation TEXT,
  posted_at           TEXT NOT NULL,
  closed_at           TEXT,
  closed_by           TEXT,
  close_reason        TEXT,
  close_basis_json    TEXT,
  updated_seq         INTEGER NOT NULL
);
CREATE INDEX idx_goals_parent ON goals(parent);

-- 照合できなかった全体目標の掲示・下ろしの候補。正式な札にはしない。
CREATE TABLE goal_proposals (
  id          TEXT PRIMARY KEY,
  action      TEXT NOT NULL,
  goal_id     TEXT,
  parent      TEXT,
  title       TEXT,
  quote       TEXT NOT NULL,
  reason      TEXT,
  proposed_by TEXT NOT NULL,
  conn_id     TEXT,
  created_at  TEXT NOT NULL,
  resolved_seq INTEGER
);

-- 札の焦点の間に書かれた記憶（Agentの報告）。
CREATE TABLE goal_links (
  goal_id    TEXT NOT NULL REFERENCES goals(id),
  memory_id  TEXT NOT NULL REFERENCES memories(id),
  conn_id    TEXT,
  created_at TEXT NOT NULL,
  PRIMARY KEY (goal_id, memory_id)
);

-- 照合したユーザー発言。全文は写さず、引用した範囲と会話ログへの取手だけを持つ。
CREATE TABLE utterances (
  id          TEXT PRIMARY KEY,
  client      TEXT NOT NULL,
  session     TEXT NOT NULL,
  file        TEXT NOT NULL,
  line_no     INTEGER NOT NULL,
  line_sha256 TEXT NOT NULL,
  quoted_span TEXT NOT NULL,
  said_at     TEXT,
  verified_at TEXT NOT NULL,
  verified_by TEXT NOT NULL
);

-- origin = 'agent'（Agentが指定）/ 'touched'（hookで観測した編集）/ 'v3_source_ref'。
CREATE TABLE anchors (
  memory_id   TEXT NOT NULL REFERENCES memories(id),
  ord         INTEGER NOT NULL,
  raw         TEXT NOT NULL,
  segment     TEXT NOT NULL,
  anchor_json TEXT NOT NULL,
  capture_json TEXT,
  origin      TEXT NOT NULL,
  recorded_at TEXT NOT NULL,
  PRIMARY KEY (memory_id, ord)
);

-- 照合は観測として追記する。最新の結果を事実として上書きしない。
CREATE TABLE anchor_observations (
  memory_id   TEXT NOT NULL,
  ord         INTEGER NOT NULL,
  observed_at TEXT NOT NULL,
  status      TEXT NOT NULL,
  detail_json TEXT NOT NULL,
  seq         INTEGER NOT NULL,
  FOREIGN KEY (memory_id, ord) REFERENCES anchors(memory_id, ord)
);

-- v4に専用の表が無いv3の行。落とさずにJSONで持つ。
CREATE TABLE legacy_rows (
  table_name TEXT NOT NULL,
  row_json   TEXT NOT NULL
);

CREATE VIRTUAL TABLE memories_fts USING fts5(
  id UNINDEXED, topic, tags, content, author,
  tokenize = 'trigram'
);

-- schema 4：長く使ったときの重さを抑える（会話ごとの検索・読取りの索引と、学習tagsの集計）。
CREATE INDEX idx_searches_conv ON searches(conversation_id, created_at);
CREATE INDEX idx_reads_conv ON reads(conversation_id, created_at);
CREATE INDEX idx_uses_scope ON uses(memory_id, scope_key);
-- 会話（scope）ごとの集計。正の重みは最大の1回、goal_done は足し、否定は引く。
CREATE TABLE learned_scope (
  memory_id   TEXT NOT NULL,
  term        TEXT NOT NULL,
  class       TEXT NOT NULL,
  scope_key   TEXT NOT NULL,
  max_pos     REAL NOT NULL,
  additive    REAL NOT NULL,
  neg         REAL NOT NULL,
  last_pos_at TEXT,
  PRIMARY KEY (memory_id, term, class, scope_key)
);
-- (記憶, 語, 種類) ごとの合計。強度は min(上限, raw) × 減衰（最後に強まった時刻から）。
CREATE TABLE learned_tags (
  memory_id   TEXT NOT NULL,
  term        TEXT NOT NULL,
  class       TEXT NOT NULL,
  raw         REAL NOT NULL,
  last_pos_at TEXT,
  PRIMARY KEY (memory_id, term, class)
);
CREATE INDEX idx_learned_tags_term ON learned_tags(term);
"#;

/// schema 3 → 4 で足すもの（新しいDBの SCHEMA にも同じ文が入っている）。
pub const UPGRADE_3_TO_4: &str = r#"
-- schema 4：長く使ったときの重さを抑える（会話ごとの検索・読取りの索引と、学習tagsの集計）。
CREATE INDEX IF NOT EXISTS idx_searches_conv ON searches(conversation_id, created_at);
CREATE INDEX IF NOT EXISTS idx_reads_conv ON reads(conversation_id, created_at);
CREATE INDEX IF NOT EXISTS idx_uses_scope ON uses(memory_id, scope_key);
-- 会話（scope）ごとの集計。正の重みは最大の1回、goal_done は足し、否定は引く。
CREATE TABLE IF NOT EXISTS learned_scope (
  memory_id   TEXT NOT NULL,
  term        TEXT NOT NULL,
  class       TEXT NOT NULL,
  scope_key   TEXT NOT NULL,
  max_pos     REAL NOT NULL,
  additive    REAL NOT NULL,
  neg         REAL NOT NULL,
  last_pos_at TEXT,
  PRIMARY KEY (memory_id, term, class, scope_key)
);
-- (記憶, 語, 種類) ごとの合計。強度は min(上限, raw) × 減衰（最後に強まった時刻から）。
CREATE TABLE IF NOT EXISTS learned_tags (
  memory_id   TEXT NOT NULL,
  term        TEXT NOT NULL,
  class       TEXT NOT NULL,
  raw         REAL NOT NULL,
  last_pos_at TEXT,
  PRIMARY KEY (memory_id, term, class)
);
CREATE INDEX IF NOT EXISTS idx_learned_tags_term ON learned_tags(term);
"#;
