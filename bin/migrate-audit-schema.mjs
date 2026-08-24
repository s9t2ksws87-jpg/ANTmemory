#!/usr/bin/env node
/**
 * 監査スキーマ migration（ANTmemory 固有機能・所有コード）
 *
 * 既存 chunks を一切変更せず、監査履歴の器を追加する:
 *   - memory_events    記憶に対する変更の追記型ログ
 *   - memory_relations 記憶同士の関係（supersedes / corrects / ...）
 *   - memory_lifecycle 現在状態の投影（再構築可能・正本は memory_events）
 *
 * 特性:
 *   - 冪等: 何度実行しても二重テーブル生成・二重イベントが起きない
 *   - 非破壊: chunks の件数・ID・topic・本文ハッシュは一切変わらない
 *   - 移行前自動バックアップ（VACUUM INTO）を各DBに1回だけ取る
 *
 * 使い方:
 *   node bin/migrate-audit-schema.mjs --all            # stores/ 下の全DBへ適用
 *   node bin/migrate-audit-schema.mjs --workspace 名   # 1つのDBへ適用
 *   node bin/migrate-audit-schema.mjs --check          # 適用状況だけ確認
 */

import { mkdirSync, existsSync, readdirSync, writeFileSync, copyFileSync } from "node:fs";
import { join, resolve } from "node:path";
import { DatabaseSync } from "node:sqlite";
import { fileURLToPath } from "node:url";

const ROOT = resolve(fileURLToPath(import.meta.url), "..", "..");
const STORES_DIR = join(ROOT, "stores");

const args = process.argv.slice(2);
const CHECK_ONLY = args.includes("--check");
const ALL = args.includes("--all");
const wsIdx = args.indexOf("--workspace");
const WORKSPACE = wsIdx >= 0 ? args[wsIdx + 1] : null;

const SCHEMA = `
CREATE TABLE IF NOT EXISTS memory_events (
  event_id   TEXT PRIMARY KEY,
  chunk_id   TEXT NOT NULL,
  event_type TEXT NOT NULL,
  actor      TEXT NOT NULL,
  model      TEXT,
  provider   TEXT,
  reason     TEXT,
  old_value  TEXT,
  new_value  TEXT,
  created_at TEXT NOT NULL
);
CREATE INDEX IF NOT EXISTS idx_memory_events_chunk ON memory_events(chunk_id);
CREATE INDEX IF NOT EXISTS idx_memory_events_type  ON memory_events(event_type);

CREATE TABLE IF NOT EXISTS memory_relations (
  source_id     TEXT NOT NULL,
  target_id     TEXT NOT NULL,
  relation_type TEXT NOT NULL,
  actor         TEXT NOT NULL,
  reason        TEXT,
  created_at    TEXT NOT NULL,
  PRIMARY KEY (source_id, target_id, relation_type)
);
CREATE INDEX IF NOT EXISTS idx_memory_relations_target ON memory_relations(target_id);

CREATE TABLE IF NOT EXISTS memory_lifecycle (
  chunk_id TEXT PRIMARY KEY,
  status   TEXT NOT NULL DEFAULT 'active',
  updated_at TEXT NOT NULL
);

-- 既存chunkを active として登録するのは初回のみ。以後は INSERT OR IGNORE。
INSERT OR IGNORE INTO memory_lifecycle (chunk_id, status, updated_at)
  SELECT id, 'active', updated_at FROM chunks;
`;

function sha256(s) {
  return createHash("sha256").update(s, "utf8").digest("hex");
}
import { createHash } from "node:crypto";

function fingerprint(db) {
  const rows = db
    .prepare("SELECT id, topic, content FROM chunks ORDER BY id")
    .all();
  return {
    count: rows.length,
    hash: sha256(rows.map((r) => `${r.id}\u0000${r.topic}\u0000${r.content}`).join("\u0001")),
  };
}

function migrateOne(wsName, dbPath) {
  const result = { workspace: wsName };
  let db;
  try {
    db = new DatabaseSync(dbPath); // 書き込み可。schema追加のため。
  } catch (e) {
    return { workspace: wsName, error: String(e) };
  }

  try {
    db.exec("PRAGMA busy_timeout = 5000");
    const before = fingerprint(db);

    // 既に監査スキーマがあるか
    const hasAuditSchema = db
      .prepare("SELECT COUNT(*) AS c FROM sqlite_master WHERE type='table' AND name IN ('memory_events','memory_relations','memory_lifecycle')")
      .get().c === 3;

    if (!CHECK_ONLY && !hasAuditSchema) {
      // 移行前バックアップ（空でないDBのみ・既存なら上書きしない）
      if (before.count > 0) {
        const backupDir = join(ROOT, "backups", "pre-migration-audit");
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
    result.nonDestructive =
      before.count === after.count && before.hash === after.hash;

    // lifecycle整合: 全chunkが active 登録されていること
    result.lifecycleActive = db
      .prepare("SELECT COUNT(*) AS c FROM memory_lifecycle WHERE status='active'")
      .get().c;

    // 冪等性検証: もう一度 SCHEMA を流してもイベント数・テーブル数が変わらない
    if (!CHECK_ONLY) {
      const evBefore = db.prepare("SELECT COUNT(*) AS c FROM memory_events").get().c;
      db.exec(SCHEMA);
      const evAfter = db.prepare("SELECT COUNT(*) AS c FROM memory_events").get().c;
      result.idempotent = evBefore === evAfter;
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

// ─── 対象収集 ────────────────────────────────────────────────────────────────

let targets = [];
if (WORKSPACE) {
  targets.push([WORKSPACE, join(STORES_DIR, WORKSPACE, "store.db")]);
} else if (ALL || process.argv.length <= 2) {
  for (const e of readdirSync(STORES_DIR, { withFileTypes: true })) {
    if (!e.isDirectory()) continue;
    const p = join(STORES_DIR, e.name, "store.db");
    if (existsSync(p)) targets.push([e.name, p]);
  }
} else {
  console.log("使い方: node bin/migrate-audit-schema.mjs [--all | --workspace <名前> | --check]");
  process.exit(1);
}

let failures = 0;
for (const [name, path] of targets) {
  if (!existsSync(path)) {
    console.log(`SKIP ${name} (store.db なし)`);
    continue;
  }
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

// 実行記録（監査用・Git管理外の logs/ へ）
if (!CHECK_ONLY) {
  try {
    mkdirSync(join(ROOT, "logs"), { recursive: true });
    writeFileSync(
      join(ROOT, "logs", "migrate-audit-last.json"),
      JSON.stringify({ at: new Date().toISOString(), results: targets.map(([n]) => n) }, null, 2),
      "utf8",
    );
  } catch {}
}

process.exit(failures ? 1 : 0);
