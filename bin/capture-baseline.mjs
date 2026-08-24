#!/usr/bin/env node
/**
 * 現状凍結・バックアップ・基準値採取
 *
 * - 全既存 store.db を backups/<timestamp>/ へ整合的なスナップショットとして退避
 *   （SQLite の VACUUM INTO を使うので WAL 中のデータも含めて一貫性が取れる）
 * - 各DBで PRAGMA integrity_check / foreign_key_check を実行
 * - 記憶件数・state件数・全chunkのID/本文ハッシュ一覧を基準値ファイルとして保存
 *
 * 実行: node bin/capture-baseline.mjs
 * 本番DBは読み取り専用で開くだけで、書き込まない。
 */

import { mkdirSync, writeFileSync, existsSync, readdirSync } from "node:fs";
import { createHash } from "node:crypto";
import { join, resolve } from "node:path";
import { DatabaseSync } from "node:sqlite";
import { fileURLToPath } from "node:url";

const ROOT = resolve(fileURLToPath(import.meta.url), "..", "..");
const STORES_DIR = join(ROOT, "stores");
const BACKUP_ROOT = join(ROOT, "backups");

const timestamp = new Date().toISOString().replace(/[:.]/g, "-").slice(0, 19);
const backupDir = join(BACKUP_ROOT, `baseline-${timestamp}`);
mkdirSync(backupDir, { recursive: true });

function sha256(s) {
  return createHash("sha256").update(s, "utf8").digest("hex");
}

if (!existsSync(STORES_DIR)) {
  console.error(`stores ディレクトリがない: ${STORES_DIR}`);
  process.exit(1);
}

const workspaces = readdirSync(STORES_DIR, { withFileTypes: true })
  .filter((e) => e.isDirectory())
  .map((e) => e.name)
  .sort();

const report = { timestamp: new Date().toISOString(), stores: [] };
let failures = 0;

for (const ws of workspaces) {
  const dbPath = join(STORES_DIR, ws, "store.db");
  if (!existsSync(dbPath)) {
    console.log(`SKIP ${ws} (store.db なし)`);
    continue;
  }

  const entry = { workspace: ws, source: dbPath };
  let db = null;
  try {
    // 読み取り専用で開いて整合性を確認
    db = new DatabaseSync(dbPath, { readOnly: true });
    entry.integrity = db.prepare("PRAGMA integrity_check").get().integrity_check;
    entry.foreignKeyViolations = db.prepare("PRAGMA foreign_key_check").all().length;
    entry.chunkCount = db.prepare("SELECT COUNT(*) AS c FROM chunks").get().c;
    entry.stateCount = db.prepare("SELECT COUNT(*) AS c FROM state").get().c;

    // 基準値: 全chunkのID・本文ハッシュ・主要メタ
    entry.chunkHashes = db
      .prepare(
        "SELECT id, topic, agent, content, created_at, updated_at, expires_at FROM chunks ORDER BY id",
      )
      .all()
      .map((r) => ({
        id: r.id,
        topic: r.topic,
        agent: r.agent,
        contentSha256: sha256(r.content),
        createdAt: r.created_at,
        updatedAt: r.updated_at,
        expiresAt: r.expires_at,
      }));
    db.close();
    db = null;

    // 整合スナップショットとして退避（VACUUM INTO は読み取り専用接続でも可）
    const snapshotPath = join(backupDir, `${ws}.db`);
    const ro = new DatabaseSync(dbPath, { readOnly: true });
    try {
      ro.exec(`VACUUM INTO '${snapshotPath.replaceAll("'", "''")}'`);
    } finally {
      ro.close();
    }
    entry.backup = snapshotPath;

    // スナップショット自体の検証: 件数とハッシュが元と一致するか
    const snap = new DatabaseSync(snapshotPath, { readOnly: true });
    try {
      const snapCount = snap.prepare("SELECT COUNT(*) AS c FROM chunks").get().c;
      entry.snapshotChunkCount = snapCount;
      entry.snapshotVerified = snapCount === entry.chunkCount;
    } finally {
      snap.close();
    }

    if (entry.integrity !== "ok" || entry.foreignKeyViolations > 0 || !entry.snapshotVerified) {
      failures += 1;
    }
    console.log(
      `OK   ${ws}: chunks=${entry.chunkCount} state=${entry.stateCount} ` +
        `integrity=${entry.integrity} fk=${entry.foreignKeyViolations} snapshot=${entry.snapshotVerified}`,
    );
  } catch (err) {
    failures += 1;
    entry.error = String(err);
    console.error(`FAIL ${ws}: ${err.message}`);
    if (db) try { db.close(); } catch {}
  }
  report.stores.push(entry);
}

const baselinePath = join(backupDir, "baseline.json");
writeFileSync(baselinePath, JSON.stringify(report, null, 2) + "\n", "utf8");

// 「最新の基準値」への固定ポインタ（migration検証で使う）
writeFileSync(
  join(BACKUP_ROOT, "LATEST_BASELINE.txt"),
  `${baselinePath}\n`,
  "utf8",
);

console.log(`\n基準値: ${baselinePath}`);
console.log(`スナップショット: ${backupDir}`);
console.log(
  `対象 ${report.stores.length} 件 / 異常 ${failures} 件`,
);
process.exit(failures > 0 ? 1 : 0);
