/**
 * 選択的想起スキーマ確保（migrate-recall-schema.mjs と同じ定義・冪等）。新規DBにも対応。
 * サーバー起動時に呼ばれ、migration 未実施のDBでも 選択的想起機能が動くようにする。
 */

import path from "node:path";
import { fileURLToPath, pathToFileURL } from "node:url";

const HERE = path.dirname(fileURLToPath(import.meta.url));

export async function ensureRecallSchema() {
  const loaded = await import(pathToFileURL(path.join(HERE, "upstream.mjs")).href.replaceAll("\\", "/"));
  const db = (await loaded.loadUpstream()).db.getDb();
  db.exec(`
CREATE TABLE IF NOT EXISTS memory_visibility (
  chunk_id    TEXT PRIMARY KEY,
  visibility  TEXT NOT NULL DEFAULT 'warm',
  reason      TEXT,
  changed_at  TEXT NOT NULL,
  actor       TEXT NOT NULL DEFAULT 'system'
);

CREATE TABLE IF NOT EXISTS memory_summaries (
  chunk_id     TEXT NOT NULL,
  level        TEXT NOT NULL,
  summary      TEXT NOT NULL,
  model        TEXT,
  provider     TEXT,
  created_at   TEXT NOT NULL,
  source_hash  TEXT NOT NULL,
  PRIMARY KEY (chunk_id, level)
);

CREATE TABLE IF NOT EXISTS memory_usage (
  chunk_id          TEXT PRIMARY KEY,
  access_count      INTEGER NOT NULL DEFAULT 0,
  last_accessed_at  TEXT,
  last_session_id   TEXT
);

CREATE TABLE IF NOT EXISTS retrieval_sessions (
  session_id   TEXT PRIMARY KEY,
  client       TEXT NOT NULL,
  started_at   TEXT NOT NULL,
  token_budget INTEGER
);

CREATE TABLE IF NOT EXISTS retrieval_session_seen (
  session_id TEXT NOT NULL,
  chunk_id   TEXT NOT NULL,
  seen_at    TEXT NOT NULL,
  PRIMARY KEY (session_id, chunk_id)
);

CREATE TABLE IF NOT EXISTS memory_recall_log (
  recall_id   TEXT PRIMARY KEY,
  chunk_id    TEXT NOT NULL,
  session_id  TEXT,
  query       TEXT,
  reason      TEXT NOT NULL,
  score       REAL,
  rank        INTEGER,
  created_at  TEXT NOT NULL
);
CREATE INDEX IF NOT EXISTS idx_recall_log_chunk ON memory_recall_log(chunk_id);

INSERT OR IGNORE INTO memory_visibility (chunk_id, visibility, reason, changed_at, actor)
  SELECT id, 'warm', 'recall migration初期値(active)', updated_at, 'system'
  FROM chunks;
`);
}
