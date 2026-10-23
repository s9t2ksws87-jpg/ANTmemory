#!/usr/bin/env node
/**
 * Phase Recall-4: Progressive Disclosure + Active-first 強化 の試験
 *
 * - search_context 既定返却は Level0-1（metadata + short summary）で raw を含まない
 * - full_content=true で audit 互換（本文ごと）に戻る
 * - read_context(id) 互換性維持・recall_deep で段階的深掘り
 * - 置換チェーンがあっても通常検索で旧版がノイズにならない
 * - include_history=true で旧版と置換理由を取得できる
 *
 * 実行: node bin/test-progressive-disclosure.mjs
 */

import { rmSync } from "node:fs";
import { join, resolve, dirname } from "node:path";
import { fileURLToPath, pathToFileURL } from "node:url";

const ROOT = resolve(dirname(fileURLToPath(import.meta.url)), "..");
const BIN = join(ROOT, "bin");

const WS = "recall-progressive-test";
const dbPath = join(ROOT, "stores", WS, "store.db");
for (const p of [dbPath, `${dbPath}-wal`, `${dbPath}-shm`]) {
  try { rmSync(p, { force: true }); } catch {}
}
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

const client = new Client({ name: "recallprog", version: "1.0.0" });
await client.connect(new StdioClientTransport({
  command: process.execPath,
  args: [join(BIN, "ant-memory.mjs")],
  cwd: ROOT,
  env: { ...process.env, ANT_MEMORY_ROOT: ROOT, ANT_MEMORY_WORKSPACE: WS, ANT_MEMORY_CLIENT: "hermes", ANT_MEMORY_ADVANCED_TOOLS: "1" },
}));

try {
  // 準備: 長文記憶を2件作り、片方をsupersedeして置換チェーンを作る
  const longBody1 = [
    "文体方針の初版決定。丁寧な敬体で統一する方針を採用した。",
    "理由は読者層に若年層が多いため。会話文はキャラクター性を優先する。",
    "この方針は第一話から第三話まで適用済み。",
  ].join("");
  const w1 = await client.callTool({
    name: "write_context",
    arguments: { topic: "recallprog: 文体方針 初版", content: longBody1, tags: ["recallprog"], importance: "high" },
  });
  const oldId = text(w1).match(/id=`([0-9a-f]{10})`/i)[1];

  const s = await client.callTool({
    name: "supersede_context",
    arguments: {
      old_id: oldId,
      topic: "recallprog: 文体方針 改訂版",
      content: "文体方針を改訂。常体（だ・である）へ変更する。新しい理由として、作品トーンの重厚感を優先する判断があった。",
      reason: "作品トーン変更のため",
      tags: ["recallprog"], importance: "high",
    },
  });
  const newId = text(s).match(/新記録 id=`([0-9a-f]{10})`/i)?.[1];
  check("supersede成功", Boolean(newId));

  // ── 既定返却は Level0-1 ──────────────────────────────────────────────
  const s1 = await client.callTool({
    name: "search_context",
    arguments: { query: "文体方針", search_mode: "bm25", min_score: 0 },
  });
  const t1 = text(s1);
  check("既定返却にshort summaryが含まれる", /文体方針/.test(t1));
  check("既定返却はraw全文を含まない（初版の末尾文がない）",
    !/第三話から第三話まで適用済み|第一話から第三話まで適用済み/.test(t1), t1.slice(0, 200));

  // 新版のみ表示（旧版は履歴状態で非表示）
  check("通常検索では新版のみ（旧版は出ない）", /改訂版/.test(t1) && !/初版/.test(t1));

  // ── full_content=true で audit 互換 ─────────────────────────────────────
  const s2 = await client.callTool({
    name: "search_context",
    arguments: { query: "文体方針", search_mode: "bm25", min_score: 0, full_content: true },
  });
  const t2 = text(s2);
  check("full_content=true で本文が返る", /常体（だ・である）へ変更する/.test(t2));

  // ── read_context 互換 ────────────────────────────────────────────────
  const r1 = await client.callTool({ name: "read_context", arguments: { id: oldId } });
  check("read_context で旧版全文も取得できる", /敬体で統一する方針/.test(text(r1)));

  // ── recall_deep Stage 1 → Stage 2 ───────────────────────────────────
  const rd1 = await client.callTool({
    name: "recall_deep",
    arguments: { query: "文体方針", search_mode: "bm25", top_k: 10 },
  });
  const trd1 = text(rd1);
  check("recall_deep Stage1 は候補一覧を返す", /候補 .* 件/.test(trd1), trd1.slice(0, 150));
  check("recall_deep 候補にはIDが含まれる", /id=`[0-9a-f]{10}`/.test(trd1));

  const ids = [...trd1.matchAll(/id=`([0-9a-f]{10})`/g)].map((m) => m[1]).slice(0, 2);
  const rd2 = await client.callTool({
    name: "recall_deep",
    arguments: { resolve: true, ids },
  });
  check("recall_deep resolve=true で本文(raw)を取得できる", ids.length > 0 && text(rd2).length > 100);

  // ── include_history で旧版＋理由 ─────────────────────────────────────
  const s3 = await client.callTool({
    name: "search_context",
    arguments: { query: "文体方針", search_mode: "bm25", min_score: 0, include_history: true },
  });
  const t3 = text(s3);
  check("include_history=true で旧版も取得できる", /初版/.test(t3));
  const h = await client.callTool({ name: "get_context_history", arguments: { id: oldId } });
  check("履歴から置換理由を取得できる", /作品トーン変更のため/.test(text(h)));

} finally {
  await client.close().catch(() => {});
}

console.log(`\n結果: ${passed} passed / ${failed} failed`);
process.exit(failed ? 1 : 0);
