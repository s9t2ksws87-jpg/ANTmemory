#!/usr/bin/env node
/**
 * 選択的想起スキーマのmigration検証
 *
 * - 最新の基準値(backups/LATEST_BASELINE.txt)と現在の全DBを照合:
 *   chunk件数・ID・本文ハッシュが無欠損・無改変であること
 *   （選択的想起スキーマ移行後に追加されたchunkは許容、欠損・改変は不可）
 * - 全DBで integrity_check=ok / fk違反0 / 監査・選択的想起スキーマ完備を確認
 *
 * 実行: node bin/verify-recall-migration.mjs
 */

import { readFileSync, existsSync } from "node:fs";
import { join, resolve, dirname } from "node:path";
import { createHash } from "node:crypto";
import { DatabaseSync } from "node:sqlite";
import { fileURLToPath } from "node:url";

const ROOT = resolve(dirname(fileURLToPath(import.meta.url)), "..");
const STORES_DIR = join(ROOT, "stores");

const pointer = join(ROOT, "backups", "LATEST_BASELINE.txt");
if (!existsSync(pointer)) {
  console.error("基準値ポインタがない: backups/LATEST_BASELINE.txt（capture-baseline.mjs を先に実行）");
  process.exit(1);
}
const baseline = JSON.parse(readFileSync(readFileSync(pointer, "utf8").trim(), "utf8"));

function sha256(s) {
  return createHash("sha256").update(s, "utf8").digest("hex");
}

let passed = 0, failed = 0;
const check = (name, ok, detail = "") => {
  if (ok) { passed++; console.log(`PASS ${name}`); }
  else { failed++; console.error(`FAIL ${name}${detail ? ` — ${detail}` : ""}`); }
};

for (const entry of baseline.stores) {
  const dbPath = join(STORES_DIR, entry.workspace, "store.db");
  if (!existsSync(dbPath)) continue; // 基準値以降に削除済みのテスト用WSはスキップ

  const db = new DatabaseSync(dbPath, { readOnly: true });
  try {
    const integrity = db.prepare("PRAGMA integrity_check").get().integrity_check;
    const fk = db.prepare("PRAGMA foreign_key_check").all().length;
    check(`${entry.workspace}: integrity=ok`, integrity === "ok", integrity);
    check(`${entry.workspace}: fk違反0`, fk === 0, `fk=${fk}`);

    const rows = db.prepare("SELECT id, content FROM chunks ORDER BY id").all();
    const currentHashes = new Map(rows.map((r) => [r.id, sha256(r.content)]));

    let bad = [];
    for (const b of entry.chunkHashes || []) {
      const cur = currentHashes.get(b.id);
      if (cur === undefined) bad.push(`${b.id} 欠損`);
      else if (cur !== b.contentSha256) bad.push(`${b.id} 本文変化`);
    }
    check(`${entry.workspace}: 既存chunk無欠損・無改変`, bad.length === 0, bad.slice(0, 3).join(", "));

    // 監査スキーマ
    const auditTableCount = db.prepare(
      "SELECT COUNT(*) c FROM sqlite_master WHERE type='table' AND name IN ('memory_events','memory_relations','memory_lifecycle')",
    ).get().c;
    check(`${entry.workspace}: 監査スキーマ完備`, auditTableCount === 3);

    // 選択的想起スキーマ
    const recallTableCount = db.prepare(
      "SELECT COUNT(*) c FROM sqlite_master WHERE type='table' AND name IN ('memory_visibility','memory_summaries','memory_usage','retrieval_sessions','retrieval_session_seen','memory_recall_log')",
    ).get().c;
    check(`${entry.workspace}: 選択的想起スキーマ完備`, recallTableCount === 6, `tables=${recallTableCount}`);

    // visibility初期付与の確認（chunkがある場合）
    if (rows.length > 0 && recallTableCount === 6) {
      const visCount = db.prepare("SELECT COUNT(*) c FROM memory_visibility").get().c;
      check(`${entry.workspace}: 全chunkにvisibility初期値あり`, visCount >= rows.length,
        `vis=${visCount} chunks=${rows.length}`);
    }
  } finally { db.close(); }
}

console.log(`\n結果: ${passed} passed / ${failed} failed`);
process.exit(failed ? 1 : 0);
