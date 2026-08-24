#!/usr/bin/env node
/**
 * Token Budgetの試験（隔離ストア）
 *
 * - max_results で返却件数を制限できる
 * - token_budget で返却量（概算token）を制限できる
 * - budget超過時は低関連（末尾側）が先に落ちる
 * - 何件切り詰めたかが返却末尾に明示される
 * - 高関連記憶が低関連記憶より優先される
 *
 * 実行: node bin/test-token-budget.mjs
 */

import { join, resolve, dirname } from "node:path";
import { fileURLToPath, pathToFileURL } from "node:url";

const ROOT = resolve(dirname(fileURLToPath(import.meta.url)), "..");
const BIN = join(ROOT, "bin");

const WS = "token-budget-test";
let passed = 0, failed = 0;
const check = (name, ok, detail = "") => {
  if (ok) { passed++; console.log(`PASS ${name}`); }
  else { failed++; console.error(`FAIL ${name}${detail ? ` — ${detail}` : ""}`); }
};
const text = (r) => (r.content ?? []).map((c) => c.text).join("\n");
const estimateTokens = (t) => {
  if (!t) return 0;
  const cjk = (t.match(/[\u3000-\u30ff\u3400-\u4dbf\u4e00-\u9fff\uf900-\ufaff]/g) || []).length;
  return Math.ceil(cjk + (t.length - cjk) / 4);
};

const { Client } = await import(
  pathToFileURL(join(ROOT, "server", "node_modules", "@modelcontextprotocol", "sdk", "dist", "esm", "client", "index.js")).href
);
const { StdioClientTransport } = await import(
  pathToFileURL(join(ROOT, "server", "node_modules", "@modelcontextprotocol", "sdk", "dist", "esm", "client", "stdio.js")).href
);

const client = new Client({ name: "budget", version: "1.0.0" });
await client.connect(new StdioClientTransport({
  command: process.execPath,
  args: [join(BIN, "ant-memory.mjs")],
  cwd: ROOT,
  env: { ...process.env, ANT_MEMORY_ROOT: ROOT, ANT_MEMORY_WORKSPACE: WS, ANT_MEMORY_CLIENT: "hermes" },
}));

try {
  // 準備: 同一テーマで長短さまざまな記憶を8件投入（関連度に勾配を作る）
  const topics = [
    "軍制の編成と術兵の運用",       // クエリ語に最も一致
    "軍制の歴史的変遷と改革",
    "軍制の階級制度と昇進",
    "軍制と兵站管理の関係",
    "編成の単位と部隊規模",
    "術兵の訓練制度と資格",
    "階級章の意匠と規定",
    "全く別の話題：料理のレシピ",
  ];
  for (let i = 0; i < topics.length; i++) {
    await client.callTool({
      name: "write_context",
      arguments: {
        topic: `budget[${i}]: ${topics[i]}`,
        content: `${topics[i]}に関する記録です。軍制・編成・術兵・兵站・階級といった語を含む本文その${i + 1}。`.repeat(i === 7 ? 1 : 3),
        tags: ["budget"], importance: "low",
      },
    });
  }

  // 埋め込み生成を待機
  await new Promise((r) => setTimeout(r, 6000));

  const baseQuery = { query: "軍制の編成 術兵の訓練 兵站管理 階級制度", search_mode: "bm25", min_score: 0 };
  // ウォームアップ＋全件数確認
  const full = text(await client.callTool({ name: "search_context", arguments: { ...baseQuery, top_k: 20 } }));
  const fullCount = (full.match(/###/g) || []).length;
  check("母体: 4件以上ヒットしている", fullCount >= 4, `hits=${fullCount}`);

  // ── max_results ──────────────────────────────────────────────────────
  const r3 = text(await client.callTool({
    name: "search_context", arguments: { ...baseQuery, top_k: 20, max_results: 3 },
  }));
  check("max_results=3 で件数が3件以下になる", (r3.match(/###/g) || []).length <= 3, `hits=${(r3.match(/###/g) || []).length}`);
  check("切り詰め通知が返却末尾に含まれる", /切り詰めました/.test(r3), r3.slice(-150));

  // ── token_budget ─────────────────────────────────────────────────────
  // Level0-1 の要約が短い環境でも必ず複数件の合計を下回るよう、十分小さい予算を使う。
  // 切り詰め通知部分を除いた実体で判定する。
  const budget = 120;
  const rb = text(await client.callTool({
    name: "search_context", arguments: { ...baseQuery, top_k: 20, token_budget: budget },
  }));
  const bodyOnly = rb.replace(/\n\n\([^)]*\)\s*$/, "");
  const usedTokens = estimateTokens(bodyOnly);
  check(`token_budget=${budget} を大幅超過しない`, usedTokens <= budget + 200,
    `used≈${usedTokens}tok`);
  check("budget超過時の切り詰め通知がある", /token_budget\/max_results により/.test(rb), rb.slice(-180));

  // ── 高関連が優先される ────────────────────────────────────────────────
  // budget=250まで絞っても、最一致の「軍制の編成と術兵」[0] が残ること
  const rSmall = text(await client.callTool({
    name: "search_context",
    arguments: { query: "軍制の編成と術兵", search_mode: "bm25", min_score: 0, top_k: 20, token_budget: 250 },
  }));
  check("高関連記憶が優先的に残る", /budget\[0\]: 軍制の編成と術兵の運用/.test(rSmall),
    rSmall.slice(0, 200));

  // ── budget無し時は従来通り（回帰）─────────────────────────────────────
  const rNone = text(await client.callTool({
    name: "search_context", arguments: { ...baseQuery, top_k: 10 },
  }));
  check("budget未指定時は切り詰められない", !/切り詰めました/.test(rNone));

} finally {
  await client.close().catch(() => {});
}

console.log(`\n結果: ${passed} passed / ${failed} failed`);
process.exit(failed ? 1 : 0);
