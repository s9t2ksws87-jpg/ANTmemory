/**
 * SQLite database layer powered by node:sqlite (built-in).
 *
 * Single-file database at <STORE_PATH>/store.db with WAL mode.
 * FTS5 for full-text BM25 search, BLOB columns for vector embeddings.
 * Zero external dependencies — uses Node.js native SQLite (>=22.5).
 */

import { DatabaseSync } from "node:sqlite";
import { mkdirSync } from "fs";
import path from "path";

const STORE_PATH = process.env.AGENT_STORE_PATH
  ? path.resolve(process.env.AGENT_STORE_PATH)
  : path.join(process.cwd(), ".ant-memory");

const DB_PATH = path.join(STORE_PATH, "store.db");

let db = null;
const stmtCache = new Map();

/**
 * Returns a cached prepared statement for static SQL.
 * Avoids re-preparing the same SQL on every call.
 */
function stmt(sql) {
  let s = stmtCache.get(sql);
  if (!s) {
    s = getDb().prepare(sql);
    stmtCache.set(sql, s);
  }
  return s;
}

// ─── Schema ─────────────────────────────────────────────────────────────────

const SCHEMA_TABLES = `
CREATE TABLE IF NOT EXISTS chunks (
  id         TEXT PRIMARY KEY,
  topic      TEXT NOT NULL,
  agent      TEXT NOT NULL DEFAULT 'global',
  tags       TEXT NOT NULL DEFAULT '[]',
  importance TEXT NOT NULL DEFAULT 'medium',
  content    TEXT NOT NULL,
  embedding  BLOB,
  created_at TEXT NOT NULL,
  updated_at TEXT NOT NULL,
  expires_at TEXT
);

CREATE INDEX IF NOT EXISTS idx_chunks_agent   ON chunks(agent);
CREATE INDEX IF NOT EXISTS idx_chunks_updated ON chunks(updated_at);
CREATE INDEX IF NOT EXISTS idx_chunks_expires ON chunks(expires_at);

CREATE TABLE IF NOT EXISTS state (
  key        TEXT PRIMARY KEY,
  value      TEXT NOT NULL,
  updated_at TEXT NOT NULL
);
`;

const SCHEMA_FTS = `
CREATE VIRTUAL TABLE IF NOT EXISTS chunks_fts USING fts5(
  id UNINDEXED,
  topic,
  tags,
  agent,
  content,
  content='chunks',
  content_rowid=rowid,
  -- 既定の unicode61 は日本語を語に分割しないため trigram を使う
  tokenize='trigram'
);
`;

const SCHEMA_TRIGGERS = `
CREATE TRIGGER IF NOT EXISTS chunks_ai AFTER INSERT ON chunks BEGIN
  INSERT INTO chunks_fts(rowid, id, topic, tags, agent, content)
  VALUES (new.rowid, new.id, new.topic, new.tags, new.agent, new.content);
END;

CREATE TRIGGER IF NOT EXISTS chunks_ad AFTER DELETE ON chunks BEGIN
  INSERT INTO chunks_fts(chunks_fts, rowid, id, topic, tags, agent, content)
  VALUES ('delete', old.rowid, old.id, old.topic, old.tags, old.agent, old.content);
END;

CREATE TRIGGER IF NOT EXISTS chunks_au AFTER UPDATE ON chunks BEGIN
  INSERT INTO chunks_fts(chunks_fts, rowid, id, topic, tags, agent, content)
  VALUES ('delete', old.rowid, old.id, old.topic, old.tags, old.agent, old.content);
  INSERT INTO chunks_fts(rowid, id, topic, tags, agent, content)
  VALUES (new.rowid, new.id, new.topic, new.tags, new.agent, new.content);
END;
`;

// ─── Initialization ─────────────────────────────────────────────────────────

/**
 * Returns the database instance. Creates it on first call.
 * Synchronous — node:sqlite DatabaseSync is synchronous by design.
 */
export function getDb() {
  if (db) return db;

  mkdirSync(STORE_PATH, { recursive: true });

  db = new DatabaseSync(DB_PATH);

  // WAL mode for better concurrent read performance
  db.exec("PRAGMA journal_mode = WAL");

  // 複数プロセスが同一DBへ同時書込するため、ロック競合時に待つ。
  db.exec("PRAGMA busy_timeout = 5000");

  // Run schema
  db.exec(SCHEMA_TABLES);

  // 既存DBが古いトークナイザ（unicode61）で作られていたら索引を作り直す。
  // 日本語がまったく分割されず検索が機能しないため。
  let ftsNeedsRebuild = false;
  const existingFts = db
    .prepare(
      "SELECT sql FROM sqlite_master WHERE type='table' AND name='chunks_fts'",
    )
    .get();
  if (existingFts && !/trigram/i.test(existingFts.sql)) {
    process.stderr.write(
      "[ant-memory] Rebuilding FTS index with trigram tokenizer (Japanese support)...\n",
    );
    db.exec("DROP TABLE IF EXISTS chunks_fts");
    ftsNeedsRebuild = true;
  }

  db.exec(SCHEMA_FTS);
  db.exec(SCHEMA_TRIGGERS);

  if (ftsNeedsRebuild) {
    db.exec("INSERT INTO chunks_fts(chunks_fts) VALUES('rebuild')");
    process.stderr.write("[ant-memory] FTS index rebuilt.\n");
  }

  // Purge expired chunks
  // ANTmemory Phase A: TTLはRaw削除ではなくlifecycleのexpired状態で表現する。
  // 上流の物理DELETEは無効化し、ANTmemory側 expireDueChunks() に委ねる。

  // Graceful shutdown
  const shutdown = () => {
    if (db) db.close();
    process.exit(0);
  };
  process.on("SIGINT", shutdown);
  process.on("SIGTERM", shutdown);

  return db;
}

// ─── CRUD Operations ────────────────────────────────────────────────────────

/**
 * Inserts or replaces a chunk in the database.
 */
export function insertChunk({
  id,
  topic,
  agent,
  tags,
  importance,
  content,
  embedding,
  createdAt,
  updatedAt,
  expiresAt,
}) {
  stmt(
    `INSERT OR REPLACE INTO chunks (id, topic, agent, tags, importance, content, embedding, created_at, updated_at, expires_at)
     VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?, ?)`,
  ).run(
    id,
    topic,
    agent,
    JSON.stringify(tags),
    importance,
    content,
    embedding ? Buffer.from(embedding.buffer) : null,
    createdAt,
    updatedAt,
    expiresAt,
  );
}

/**
 * Retrieves a single chunk by ID.
 * @returns {object|null}
 */
export function getChunk(id) {
  const row = stmt(`SELECT * FROM chunks WHERE id = ?`).get(id);
  if (!row) return null;
  return parseChunkRow(row);
}

/**
 * Deletes a chunk by ID.
 * @returns {boolean} true if a row was deleted
 */
export function deleteChunkById(id) {
  const result = stmt(`DELETE FROM chunks WHERE id = ?`).run(id);
  return result.changes > 0;
}

/**
 * Lists chunk metadata, with optional agent/tags filters.
 * Sorted by updated_at descending.
 */
export function listChunksDb({ agent, tags = [], limit = 100, offset = 0 } = {}) {
  const d = getDb();
  let sql = `SELECT id, topic, agent, tags, importance, updated_at FROM chunks`;
  const conditions = [];
  const params = [];

  if (agent) {
    conditions.push(`agent = ?`);
    params.push(agent);
  }

  if (tags.length > 0) {
    const tagConditions = tags.map(() => `tags LIKE ?`);
    conditions.push(`(${tagConditions.join(" OR ")})`);
    params.push(...tags.map((t) => `%"${t}"%`));
  }

  if (conditions.length) sql += ` WHERE ${conditions.join(" AND ")}`;
  sql += ` ORDER BY updated_at DESC LIMIT ? OFFSET ?`;
  params.push(limit, offset);

  const rows = d.prepare(sql).all(...params);
  return rows.map((r) => ({
    id: r.id,
    topic: r.topic,
    agent: r.agent,
    tags: JSON.parse(r.tags),
    importance: r.importance,
    updated: r.updated_at,
  }));
}

/**
 * Full-text search via FTS5 (BM25).
 * Returns ranked results with full chunk data (avoids separate lookups).
 */
export function searchFTS({ query, agent, tags = [], topK = 18 }) {
  const d = getDb();

  // Escape FTS5 special chars and build query
  const ftsQuery = query
    .replace(/["*^:(){}[\]]/g, " ")
    .split(/\s+/)
    .filter((t) => t.length > 1)
    // 各語をフレーズとしてクォートする。こうしないと "ant-memory" のような
    // ハイフン入りの語が FTS5 の演算子として解釈され構文エラーになる。
    .map((t) => `"${t.replace(/"/g, "")}"`)
    .join(" OR ");

  if (!ftsQuery) return [];

  let sql = `
    SELECT c.id, c.topic, c.agent, c.tags, c.importance, c.content, c.created_at, c.updated_at, rank
    FROM chunks_fts
    JOIN chunks c ON c.id = chunks_fts.id
    WHERE chunks_fts MATCH ?`;
  const params = [ftsQuery];

  if (agent) {
    sql += ` AND c.agent = ?`;
    params.push(agent);
  }

  if (tags.length > 0) {
    const tagConditions = tags.map(() => `c.tags LIKE ?`);
    sql += ` AND (${tagConditions.join(" OR ")})`;
    params.push(...tags.map((t) => `%"${t}"%`));
  }

  sql += ` ORDER BY rank LIMIT ?`;
  params.push(topK);

  const rows = d.prepare(sql).all(...params);
  return rows.map((r) => ({
    id: r.id,
    topic: r.topic,
    agent: r.agent,
    tags: JSON.parse(r.tags),
    importance: r.importance,
    content: r.content,
    created: r.created_at,
    updated: r.updated_at,
    score: -r.rank,
  }));
}

/**
 * Retrieves all embeddings for vector search.
 * @returns {Array<{ id: string, embedding: Float32Array }>}
 */
export function getAllEmbeddings({ agent, tags = [] } = {}) {
  const d = getDb();
  let sql = `SELECT id, embedding FROM chunks WHERE embedding IS NOT NULL`;
  const params = [];

  if (agent) {
    sql += ` AND agent = ?`;
    params.push(agent);
  }

  if (tags.length > 0) {
    const tagConditions = tags.map(() => `tags LIKE ?`);
    sql += ` AND (${tagConditions.join(" OR ")})`;
    params.push(...tags.map((t) => `%"${t}"%`));
  }

  const rows = d.prepare(sql).all(...params);
  return rows
    .filter((r) => r.embedding !== null)
    .map((r) => ({
      id: r.id,
      embedding: new Float32Array(
        r.embedding.buffer,
        r.embedding.byteOffset,
        r.embedding.byteLength / 4,
      ),
    }));
}

/**
 * Updates only the embedding for a chunk.
 */
export function updateEmbedding(id, embedding) {
  stmt(`UPDATE chunks SET embedding = ? WHERE id = ?`).run(
    Buffer.from(embedding.buffer),
    id,
  );
}

/**
 * Returns chunks that have no embedding yet.
 */
export function getChunksWithoutEmbedding() {
  return stmt(
    `SELECT id, topic, tags, content FROM chunks WHERE embedding IS NULL`,
  )
    .all()
    .map((r) => ({
      id: r.id,
      topic: r.topic,
      tags: r.tags,
      content: r.content,
    }));
}

// ─── State Operations ───────────────────────────────────────────────────────

export function getStateDb(key) {
  const row = stmt(`SELECT value FROM state WHERE key = ?`).get(key);
  if (!row) return null;
  return JSON.parse(row.value);
}

export function setStateDb(key, value) {
  const updatedAt = new Date().toISOString();
  stmt(
    `INSERT OR REPLACE INTO state (key, value, updated_at) VALUES (?, ?, ?)`,
  ).run(key, JSON.stringify(value), updatedAt);
  return { key, updated: updatedAt };
}

// ─── Helpers ────────────────────────────────────────────────────────────────

function parseChunkRow(row) {
  return {
    id: row.id,
    topic: row.topic,
    agent: row.agent,
    tags: JSON.parse(row.tags),
    importance: row.importance,
    content: row.content,
    embedding: row.embedding
      ? new Float32Array(
          row.embedding.buffer,
          row.embedding.byteOffset,
          row.embedding.byteLength / 4,
        )
      : null,
    createdAt: row.created_at,
    updatedAt: row.updated_at,
    expiresAt: row.expires_at,
  };
}

export { STORE_PATH };
