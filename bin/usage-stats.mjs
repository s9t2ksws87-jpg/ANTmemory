#!/usr/bin/env node
/**
 * 監査履歴の運用統計の取得
 *
 * 実DB（stores/ 下の全ワークスペース）を読み取り専用で開き、選択的想起設計に必要な
 * 運用統計を取得する。書き込みは一切行わない。
 *
 * 取得項目（選択的想起の基準計測）:
 * - 平均検索結果件数の母体となる chunk 件数分布
 * - superseded / retracted / expired 比率
 * - 本文長分布（Level分割の効果見積り用）
 * - agent別利用傾向
 * - 同一テーマ置換チェーンの実在数
 */

import { existsSync, readdirSync, writeFileSync } from "node:fs";
import { join, resolve, dirname } from "node:path";
import { DatabaseSync } from "node:sqlite";
import { fileURLToPath } from "node:url";

const ROOT = resolve(dirname(fileURLToPath(import.meta.url)), "..");
const STORES_DIR = join(ROOT, "stores");

const stats = {
  measuredAt: new Date().toISOString(),
  profile: "audit-history",
  workspaces: [],
};

function pct(n, d) { return d ? Math.round((n / d) * 1000) / 10 : 0; }

for (const e of readdirSync(STORES_DIR, { withFileTypes: true })) {
  if (!e.isDirectory()) continue;
  const dbPath = join(STORES_DIR, e.name, "store.db");
  if (!existsSync(dbPath)) continue;

  const ws = { workspace: e.name };
  const db = new DatabaseSync(dbPath, { readOnly: true });
  try {
    // chunks / state 件数
    ws.chunkCount = db.prepare("SELECT COUNT(*) c FROM chunks").get().c;
    ws.stateCount = db.prepare("SELECT COUNT(*) c FROM state").get().c;
    if (ws.chunkCount === 0) { stats.workspaces.push(ws); continue; }

    // lifecycle 状態比率
    const hasLc = db.prepare(
      "SELECT COUNT(*) c FROM sqlite_master WHERE type='table' AND name='memory_lifecycle'",
    ).get().c > 0;
    if (hasLc) {
      const rows = db.prepare("SELECT status, COUNT(*) c FROM memory_lifecycle GROUP BY status").all();
      ws.lifecycle = Object.fromEntries(rows.map((r) => [r.status, r.c]));
      ws.historyRatioPct = pct((ws.chunkCount || 0) - (ws.lifecycle.active ?? 0), ws.chunkCount);
    } else {
      ws.lifecycle = null;
    }

    // 本文長分布（Level分割の効果見積り用）
    const lens = db.prepare("SELECT LENGTH(content) l FROM chunks ORDER BY l").all().map((r) => r.l);
    if (lens.length) {
      const at = (p) => lens[Math.min(Math.floor(lens.length * p), lens.length - 1)];
      ws.contentLength = {
        min: lens[0], p25: at(0.25), median: at(0.5), p75: at(0.75), max: lens[lens.length - 1],
        avg: Math.round(lens.reduce((s, x) => s + x, 0) / lens.length),
      };
      // 検索1件あたりの平均返却字数 ≈ avg本文長 + メタデータ約120字
      ws.estReturnCharsPerHit = ws.contentLength.avg + 120;
    }

    // agent別傾向
    ws.agents = db.prepare("SELECT agent, COUNT(*) c FROM chunks GROUP BY agent ORDER BY c DESC").all();

    // 置換チェーン（supersedes関係）の実在数
    if (db.prepare("SELECT COUNT(*) c FROM sqlite_master WHERE type='table' AND name='memory_relations'").get().c > 0) {
      ws.supersedeChains = db.prepare(
        "SELECT COUNT(DISTINCT target_id) c FROM memory_relations WHERE relation_type='supersedes'",
      ).get().c;
    } else { ws.supersedeChains = 0; }

    // イベント総数
    if (db.prepare("SELECT COUNT(*) c FROM sqlite_master WHERE type='table' AND name='memory_events'").get().c > 0) {
      ws.eventCount = db.prepare("SELECT COUNT(*) c FROM memory_events").get().c;
    }
  } finally { db.close(); }

  stats.workspaces.push(ws);
}

// 集計
const active = stats.workspaces.filter((w) => w.chunkCount > 0);
const totals = {
  workspacesWithMemory: active.length,
  totalChunks: active.reduce((s, w) => s + w.chunkCount, 0),
};
if (active.some((w) => w.historyRatioPct !== undefined)) {
  const tc = totals.totalChunks;
  totals.avgHistoryRatioPct =
    Math.round(active.reduce((s, w) => s + (w.historyRatioPct ?? 0) * w.chunkCount, 0) / tc * 10) / 10;
}
const withLen = active.filter((w) => w.contentLength);
if (withLen.length) {
  totals.avgContentCharsAcrossWorkspaces =
    Math.round(withLen.reduce((s, w) => s + w.contentLength.avg * w.chunkCount, 0) /
      withLen.reduce((s, w) => s + w.chunkCount, 0));
}
stats.totals = totals;

// 出力
mkdirSyncSafe();
function mkdirSyncSafe() {}
const outPath = join(ROOT, "backups", `usage-stats-${stats.measuredAt.replace(/[:.]/g, "-").slice(0, 19)}.json`);
writeFileSync(outPath, JSON.stringify(stats, null, 2) + "\n", "utf8");

console.log("=== 監査履歴の運用統計 ===");
console.log(`記憶を持つワークスペース: ${totals.workspacesWithMemory}`);
console.log(`総chunk数: ${totals.totalChunks}`);
if (totals.avgHistoryRatioPct !== undefined)
  console.log(`履歴状態(superseded等)比率: ${totals.avgHistoryRatioPct}%`);
if (totals.avgContentCharsAcrossWorkspaces !== undefined)
  console.log(`平均本文長: ${totals.avgContentCharsAcrossWorkspaces} 字`);
console.log("");
for (const w of active.sort((a, b) => b.chunkCount - a.chunkCount)) {
  console.log(
    `${w.workspace}: ${w.chunkCount}chunk` +
    (w.contentLength ? ` 平均${w.contentLength.avg}字` : "") +
    (w.historyRatioPct !== undefined ? ` 履歴${w.historyRatioPct}%` : "") +
    (w.eventCount !== undefined ? ` events=${w.eventCount}` : ""),
  );
}
console.log(`\n保存先: ${outPath}`);

// 上位ワークスペースでの検索返却量見積り（選択的想起の効果検証の対象特定）
const top = active.sort((a, b) => b.chunkCount - a.chunkCount)[0];
if (top?.estReturnCharsPerHit) {
  console.log(`\n最大ワークスペース「${top.workspace}」での1クエリ(top10)返却見積り:`);
  console.log(`  約 ${top.estReturnCharsPerHit * 10} 字 ≈ ${Math.ceil((top.estReturnCharsPerHit * 10))} 字`);
}
