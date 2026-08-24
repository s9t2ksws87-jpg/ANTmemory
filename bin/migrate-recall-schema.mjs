#!/usr/bin/env node
/**
 * 選択的想起スキーマ migration（ANTmemory 所有コード）
 *
 * 既存 chunks を一切変更せず、選択的想起の器を追加する:
 *   - memory_visibility         hot/warm/cold/archive（importanceと独立）
 *   - memory_summaries          short/detailed要約（派生データ・source_hash管理）
 *   - memory_usage              access_count / last_accessed_at / last_session_id
 *   - retrieval_sessions        検索セッション
 *   - retrieval_session_seen    セッション内既読
 *   - memory_recall_log         「なぜ思い出したか」の記録
 *
 * 特性（migrate-audit-schema.mjs と同一）:
 *   - 冪等: 何度実行しても二重生成が起きない
 *   - 非破壊: chunks の件数・ID・本文ハッシュは一切変わらない
 *   - 移行前自動バックアップ（VACUUM INTO → backups/pre-migration-recall/）
 *
 * 使い方:
 *   node bin/migrate-recall-schema.mjs --all            # stores/ 下の全DBへ適用
 *   node bin/migrate-recall-schema.mjs --workspace 名   # 1つのDBへ適用
 */

import { mkdirSync, existsSync, readdirSync } from "node:fs";
import { join, resolve } from "node:path";
import { createHash } from "node:crypto";
import { DatabaseSync } from "node:sqlite";
import { fileURLToPath } from "node:url";

const ROOT = resolve(fileURLToPath(import.meta.url), "..", "..");
const STORES_DIR = join(ROOT, "stores");

const args = process.argv.slice(2);
const ALL = args.includes("--all") || args.length <= 2;
const wsIdx = args.indexOf("--workspace");
const WORKSPACE = wsIdx >= 0 ? args[wsIdx + 1] : null;

const SCHEMA = `
-- ── visibility: importanceと独立した露出制御 ──────────────────────────────
CREATE TABLE IF NOT EXISTS memory_visibility (
  chunk_id    TEXT PRIMARY KEY,
  visibility  TEXT NOT NULL DEFAULT 'warm',  -- hot | warm | cold | archive
  reason      TEXT,
  changed_at  TEXT NOT NULL,
  actor       TEXT NOT NULL DEFAULT 'system'
);

-- ── summaries: 原文はchunks正本。summaryは再生成可能な派生データ ──────────
CREATE TABLE IF NOT EXISTS memory_summaries (
  chunk_id     TEXT NOT NULL,
  level        TEXT NOT NULL,                -- 'short' | 'detailed'
  summary      TEXT NOT NULL,
  model        TEXT,
  provider     TEXT,
  created_at   TEXT NOT NULL,
  source_hash  TEXT NOT NULL,                -- 元本文のSHA256
  PRIMARY KEY (chunk_id, level)
);

-- ── usage: 参照統計（補助信号に留める）──────────────────────────────────────
CREATE TABLE IF NOT EXISTS memory_usage (
  chunk_id          TEXT PRIMARY KEY,
  access_count      INTEGER NOT NULL DEFAULT 0,
  last_accessed_at  TEXT,
  last_session_id   TEXT
);

-- ── retrieval sessions ────────────────────────────────────────────────────
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

-- ── recall log: 「なぜ思い出したか」の監査ログ ─────────────────────────────
CREATE TABLE IF NOT EXISTS memory_recall_log (
  recall_id   TEXT PRIMARY KEY,
  chunk_id    TEXT NOT NULL,
  session_id  TEXT,
  query       TEXT,
  reason      TEXT NOT NULL,   -- JSON配列
  score       REAL,
  rank        INTEGER,
  created_at  TEXT NOT NULL
);
CREATE INDEX IF NOT EXISTS idx_recall_log_chunk ON memory_recall_log(chunk_id);

-- 既存activeなchunkに warm を初期付与（冪等）
INSERT OR IGNORE INTO memory_visibility (chunk_id, visibility, reason, changed_at, actor)
  SELECT id, 'warm', 'recall migration初期値(active)', updated_at, 'system'
  FROM chunks;
`;

function sha256(s) {
  return createHash("sha256").update(s, "utf8").digest("hex");
}

function fingerprint(db) {
  const rows = db.prepare("SELECT id, topic, content FROM chunks ORDER BY id").all();
  return {
    count: rows.length,
    hash: sha256(rows.map((r) => `${r.id}\u0000${r.topic}\u0000${r.content}`).join("\u0001")),
  };
}

const RECALL_TABLES = [
  "memory_visibility",
  "memory_summaries",
  "memory_usage",
  "retrieval_sessions",
  "retrieval_session_seen",
  "memory_recall_log",
];

function migrateOne(wsName, dbPath) {
  const result = { workspace: wsName };
  let db;
  try {
    db = new DatabaseSync(dbPath);
  } catch (e) {
    return { workspace: wsName, error: String(e) };
  }

  try {
    db.exec("PRAGMA busy_timeout = 5000");
    const before = fingerprint(db);

    const existingTables = db
      .prepare(
        `SELECT name FROM sqlite_master WHERE type='table' AND name IN (${RECALL_TABLES.map(() => "?").join(",")})`,
      )
      .all(...RECALL_TABLES).length;
    const hasRecallSchema = existingTables === RECALL_TABLES.length;

    if (!hasRecallSchema) {
      // 移行前バックアップ（空でないDBのみ・既存なら上書きしない）
      if (before.count > 0) {
        const backupDir = join(ROOT, "backups", "pre-migration-recall");
        mkdirSync(backupDir, { recursive: true });
        const backupPath = join(backupDir, `${wsName}.db`);
        if (!existsSync(backupPath)) {
          const ro = new DatabaseSync(dbPath, { readOnly: true });
          try {
            ro.exec(`VACUUM INTO '${backupPath.replaceAll("'", "''")}'`);
          } finally {
            ro.close();
          }
          result.backup = backupPath;
        }
      }

      db.exec("BEGIN");
      try {
        db.exec(SCHEMA);
        db.exec("COMMIT");
        result.migrated = true;
      } catch (e) {
        db.exec("ROLLBACK");
        throw e;
      }
    }

    const after = fingerprint(db);
    result.chunkCount = after.count;
    result.nonDestructive = before.count === after.count && before.hash === after.hash;

    // 冪等性検証: 再度SCHEMAを流しても変化しない
    if (!CHECK_ONLY_MODE) {
      const visBefore = db.prepare("SELECT COUNT(*) c FROM memory_visibility").get().c;
      db.exec(SCHEMA);
      const visAfter = db.prepare("SELECT COUNT(*) c FROM memory_visibility").get().c;
      result.idempotent = visBefore === visAfter;
    } else {
      result.idempotent = true; // check時は検証しない
    }

    result.ok = true;
  } catch (e) {
    result.error = String(e);
    result.ok = false;
  } finally {
    db.close();
  }
  return result;
}

const CHECK_ONLY_MODE = false;

// ─── 対象収集 ────────────────────────────────────────────────────────────────

let targets = [];
if (WORKSPACE) {
  targets.push([WORKSPACE, join(STORES_DIR, WORKSPACE, "store.db")]);
} else if (ALL) {
  for (const e of readdirSync(STORES_DIR, { withFileTypes: true })) {
    if (!e.isDirectory()) continue;
    const p = join(STORES_DIR, e.name, "store.db");
    if (existsSync(p)) targets.push([e.name, p]);
  }
} else {
  console.log("使い方: node bin/migrate-recall-schema.mjs [--all | --workspace <名前>]");
  process.exit(1);
}

let failures = 0;
for (const [name, path] of targets) {
  if (!existsSync(path)) continue;
  const r = migrateOne(name, path);
  if (r.error) failures += 1;
  console.log(
    `${r.ok ? "OK  " : "FAIL"} ${name}` +
      (r.migrated ? " migrated" : "") +
      ` chunks=${r.chunkCount}` +
      ` nonDestructive=${r.nonDestructive}` +
      ` idempotent=${r.idempotent}` +
      (r.backup ? ` backup=${r.backup}` : "") +
      (r.error ? ` error=${r.error}` : ""),
  );
}

process.exit(failures ? 1 : 0);
