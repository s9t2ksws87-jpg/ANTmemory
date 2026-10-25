#!/usr/bin/env node
/**
 * Phase 9: 本番移行の検証
 *
 * - Phase 0 で採取した基準値(baseline.json)と現在の全DBを照合:
 *   chunk件数・ID・本文ハッシュが完全一致すること（非破壊性の最終確認）
 * - 全DBで integrity_check=ok / fk違反0 / auditスキーマ存在を確認
 *
 * 実行: node bin/verify-audit-migration.mjs
 */

import { readFileSync, existsSync } from "node:fs";
import { join, resolve, dirname } from "node:path";
import { createHash } from "node:crypto";
import { DatabaseSync } from "node:sqlite";
import { fileURLToPath } from "node:url";

const ROOT = resolve(dirname(fileURLToPath(import.meta.url)), "..");
const STORES_DIR = join(ROOT, "stores");

// 最新の基準値を読む
const baselinePointer = join(ROOT, "backups", "LATEST_BASELINE.txt");
if (!existsSync(baselinePointer)) {
  console.error("基準値ポインタがない: backups/LATEST_BASELINE.txt（Phase 0 を先に実行）");
  process.exit(1);
}
const baselinePath = readFileSync(baselinePointer, "utf8").trim();
const baseline = JSON.parse(readFileSync(baselinePath, "utf8"));

function sha256(s) {
  return createHash("sha256").update(s, "utf8").digest("hex");
}

let passed = 0, failed = 0;
const check = (name, ok, detail = "") => {
  if (ok) { passed++; console.log(`PASS ${name}`); }
  else { failed++; console.error(`FAIL ${name}${detail ? ` — ${detail}` : ""}`); }
};

let totalChunks = 0;
const baselineMap = new Map(baseline.stores.map((s) => [s.workspace, s]));

for (const entry of baseline.stores) {
  const dbPath = join(STORES_DIR, entry.workspace, "store.db");
  if (!existsSync(dbPath)) {
    check(`${entry.workspace}: DB存在`, false, "store.db が消えている");
    continue;
  }
  const db = new DatabaseSync(dbPath, { readOnly: true });
  try {
    const integrity = db.prepare("PRAGMA integrity_check").get().integrity_check;
    const fk = db.prepare("PRAGMA foreign_key_check").all().length;
    check(`${entry.workspace}: integrity=ok`, integrity === "ok", integrity);
    check(`${entry.workspace}: fk違反0`, fk === 0, `fk=${fk}`);

    // Phase 0 のchunkハッシュ一覧と照合
    const rows = db
      .prepare("SELECT id, topic, agent, content FROM chunks ORDER BY id")
      .all();
    const currentHashes = new Map(
      rows.map((r) => [r.id, sha256(r.content)]),
    );
    const baseRows = entry.chunkHashes || [];

    let mismatches = [];
    for (const b of baseRows) {
      const cur = currentHashes.get(b.id);
      if (cur === undefined) mismatches.push(`${b.id} 欠損`);
      else if (cur !== b.contentSha256) mismatches.push(`${b.id} 本文変化`);
    }
    // 追加されたchunkは許容（audit移行後のテスト書込など）だが欠損・改変は不可
    check(`${entry.workspace}: 既存chunk無欠損・無改変`, mismatches.length === 0,
      mismatches.slice(0, 3).join(", "));

    // audit スキーマ存在
    const auditTables = db.prepare(
      "SELECT COUNT(*) AS c FROM sqlite_master WHERE type='table' AND name IN ('memory_events','memory_relations','memory_lifecycle')",
    ).get().c;
    check(`${entry.workspace}: auditスキーマ完備`, auditTables === 3, `tables=${auditTables}`);

    totalChunks += baseRows.length;
  } finally { db.close(); }
}

console.log(`\n基準値: ${baselinePath}`);
console.log(`検証対象chunk総数: ${totalChunks}`);
console.log(`結果: ${passed} passed / ${failed} failed`);
process.exit(failed ? 1 : 0);
