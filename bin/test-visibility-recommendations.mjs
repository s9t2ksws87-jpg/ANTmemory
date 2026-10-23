#!/usr/bin/env node
/**
 * Phase Recall-7: Visibility推奨（Promotion/Decay推奨ベース）の試験
 *
 * - get_visibility_recommendations が usage統計に基づく推奨を返す
 * - ツール自体は何も変更しない（自動適用なし）
 * - superseded記憶が archive候補になる
 * - 推奨に根拠（basis）が含まれる
 *
 * 実行: node bin/test-visibility-recommendations.mjs
 */

import { join, resolve, dirname } from "node:path";
import { fileURLToPath, pathToFileURL } from "node:url";
import { DatabaseSync } from "node:sqlite";

const ROOT = resolve(dirname(fileURLToPath(import.meta.url)), "..");
const BIN = join(ROOT, "bin");

const WS = "recall-recommend-test";
let passed = 0, failed = 0;
const check = (name, ok, detail = "") => {
  if (ok) { passed++; console.log(`PASS ${name}`); }
  else { failed++; console.error(`FAIL ${name}${detail ? ` — ${detail}` : ""}`); }
};
const text = (r) => (r.content ?? []).map((c) => c.text).join("\n");

const { Client } = await import(
  pathToFileURL(join(ROOT, "server", "node_modules", "@modelcontextprotocol", "sdk", "dist", "esm", "client", "index.js")).href
);
const { StdioClientTransport } = await import(
  pathToFileURL(join(ROOT, "server", "node_modules", "@modelcontextprotocol", "sdk", "dist", "esm", "client", "stdio.js")).href
);

// クリーンスタート（db + wal + shm を確実に削除）
{
  const fs = await import("node:fs");
  const dbPath = join(ROOT, "stores", WS, "store.db");
  for (const p of [dbPath, dbPath + "-wal", dbPath + "-shm"]) {
    try { if (fs.existsSync(p)) fs.rmSync(p, { force: true, maxRetries: 3 }); } catch {}
  }
}

const client = new Client({ name: "recallrec", version: "1.0.0" });
await client.connect(new StdioClientTransport({
  command: process.execPath,
  args: [join(BIN, "ant-memory.mjs")],
  cwd: ROOT,
  env: { ...process.env, ANT_MEMORY_ROOT: ROOT, ANT_MEMORY_WORKSPACE: WS, ANT_MEMORY_CLIENT: "hermes", ANT_MEMORY_ADVANCED_TOOLS: "1" },
}));

try {
  // 準備1: よく参照される記録 → hot候補になる
  const w1 = await client.callTool({
    name: "write_context",
    arguments: {
      topic: "rec試験: 頻繁参照される原則",
      content: "軍制の編成に関する頻繁参照の記録。この原則は毎回の作業基準になる。",
      tags: ["recallrec"], importance: "high",
    },
  });
  const hotId = text(w1).match(/id=`([0-9a-f]{10})`/i)[1];

  // 準備2: supersede される記録 → archive候補になる
  const w2 = await client.callTool({
    name: "write_context",
    arguments: {
      topic: "rec試験: 古い方針",
      content: "古い方針の記録。後で改訂される。",
      tags: ["recallrec"], importance: "low",
    },
  });
  const oldId = text(w2).match(/id=`([0-9a-f]{10})`/i)[1];
  await client.callTool({
    name: "supersede_context",
    arguments: {
      old_id: oldId,
      topic: "rec試験: 新しい方針",
      content: "改訂版の方針。軍制の編成とは無関係の内容。",
      reason: "方針改訂のため",
      tags: ["recallrec"],
    },
  });

  // hot候補を作るため6回検索（関連記録のみヒットさせる）
  await new Promise((r) => setTimeout(r, 5000));
  for (let i = 0; i < 6; i++) {
    await client.callTool({
      name: "search_context",
      arguments: { query: "軍制の編成 原則 作業基準", search_mode: "bm25", min_score: 0.5, top_k: 5 },
    });
  }

  // ── 推奨取得 ──────────────────────────────────────────────────────────
  const rec = text(await client.callTool({ name: "get_visibility_recommendations", arguments: {} }));
  check("get_visibility_recommendations が動作する", /推奨|推奨はありません/.test(rec), rec.slice(0, 150));
  check("hot候補が提示される（usage根拠付き）",
    /→ hot/.test(rec) && /回参照/.test(rec) && rec.includes(hotId),
    rec.slice(0, 400));
  check("archive候補が提示される（superseded根拠）",
    /→ archive/.test(rec) && /superseded/.test(rec));

  // ── ツール自体は何も変更しない ────────────────────────────────────────
  const db = new DatabaseSync(join(ROOT, "stores", WS, "store.db"), { readOnly: true });
  try {
    const visRow = db.prepare("SELECT visibility FROM memory_visibility WHERE chunk_id = ?").get(hotId);
    check("推奨取得後にvisibilityは不変（自動適用なし）",
      (visRow?.visibility ?? "warm") === "warm", `vis=${visRow?.visibility}`);
    // importanceも不変
    const imp = db.prepare("SELECT importance FROM chunks WHERE id = ?").get(hotId).importance;
    check("importanceは不変", imp === "high");
  } finally { db.close(); }

  // ── 推奨を人間が適用すると監査イベントが残る ──────────────────────────
  await client.callTool({
    name: "promote_memory",
    arguments: { id: hotId, to: "hot", reason: "推奨(hot候補)を承認して適用" },
  });
  const h = await client.callTool({ name: "get_context_history", arguments: { id: hotId } });
  check("推奨適用後も監査イベントで説明可能", /visibility_changed.*hot/s.test(text(h)));

} finally {
  await client.close().catch(() => {});
}

console.log(`\n結果: ${passed} passed / ${failed} failed`);
process.exit(failed ? 1 : 0);
