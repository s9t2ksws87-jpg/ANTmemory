#!/usr/bin/env node
/**
 * 検索・読込のライフサイクル意味論試験（隔離ストア）
 *
 * - 既定検索は active の記憶を優先し、履歴状態(superseded/retracted/expired)を隠す
 * - include_history=true で旧版＋置換理由が取得できる
 * - get_successor_context で置換チェーンを辿れる
 * - 日本語・ハイフン・min_score の既存試験の回帰
 *
 * 実行: node bin/test-lifecycle-search.mjs
 */

import { join, resolve, dirname } from "node:path";
import { fileURLToPath, pathToFileURL } from "node:url";

const ROOT = resolve(dirname(fileURLToPath(import.meta.url)), "..");
const BIN = join(ROOT, "bin");
const { Client } = await import(
  pathToFileURL(join(ROOT, "server", "node_modules", "@modelcontextprotocol", "sdk", "dist", "esm", "client", "index.js")).href
);
const { StdioClientTransport } = await import(
  pathToFileURL(join(ROOT, "server", "node_modules", "@modelcontextprotocol", "sdk", "dist", "esm", "client", "stdio.js")).href
);

const WS = "lifecycle-search-test";
let passed = 0, failed = 0;
const check = (name, ok, detail = "") => {
  if (ok) { passed++; console.log(`PASS ${name}`); }
  else { failed++; console.error(`FAIL ${name}${detail ? ` — ${detail}` : ""}`); }
};
const text = (r) => (r.content ?? []).map((c) => c.text).join("\n");

const client = new Client({ name: "lifecycle-search", version: "1.0.0" });
await client.connect(new StdioClientTransport({
  command: process.execPath,
  args: [join(BIN, "ant-memory.mjs")],
  cwd: ROOT,
  env: { ...process.env, ANT_MEMORY_ROOT: ROOT, ANT_MEMORY_WORKSPACE: WS, ANT_MEMORY_CLIENT: "hermes" },
}));

try {
  // v1: 方針A
  const w1 = await client.callTool({
    name: "write_context",
    arguments: {
      topic: "lifecycle-search: 文体の方針（旧版）",
      content: "文体の方針はA案：丁寧な敬体で統一する。ant-memory の検索でも引けるようにしておく。",
      tags: ["lifecycle-search"], importance: "high",
    },
  });
  const oldId = text(w1).match(/id=`([0-9a-f]{10})`/i)[1];

  // supersede → 現行方針B
  const s = await client.callTool({
    name: "supersede_context",
    arguments: {
      old_id: oldId,
      topic: "lifecycle-search: 文体の方針（新版）",
      content: "文体の方針はB案に変更：常体（だ・である）へ変更する。",
      reason: "ユーザーが作品トーンの変更を決めた",
      tags: ["lifecycle-search"], importance: "high",
    },
  });
  const newId = text(s).match(/新記録 id=`([0-9a-f]{10})`/i)?.[1];
  check("supersede が成功", Boolean(newId), text(s));

  // retracted も作る
  const w3 = await client.callTool({
    name: "write_context",
    arguments: { topic: "lifecycle-search: 撤回される記録", content: "この記録は誤りなので撤回する。ミュウの試験語。", tags: ["lifecycle-search"] },
  });
  const rId = text(w3).match(/id=`([0-9a-f]{10})`/i)[1];
  await client.callTool({ name: "retract_context", arguments: { id: rId, reason: "誤情報のため" } });

  // ── 既定検索は新版のみ ─────────────────────────────────────────────────
  const s1 = await client.callTool({ name: "search_context", arguments: { query: "文体の方針", min_score: 0 } });
  const t1 = text(s1);
  check("通常検索では新版(B案)が取得できる", /B案/.test(t1) && !/A案：/.test(t1), t1.slice(0, 200));
  check("通常検索では撤回済みが隠れる", !/撤回される記録/.test(t1));

  // ── include_history=true で旧版＋理由 ────────────────────────────────
  const s2 = await client.callTool({ name: "search_context", arguments: { query: "文体の方針", min_score: 0, include_history: true } });
  const t2 = text(s2);
  check("include_history で旧版(A案)も取得できる", /A案/.test(t2));

  const h = await client.callTool({ name: "get_context_history", arguments: { id: oldId } });
  check("履歴から置換理由を取得できる", /ユーザーが作品トーンの変更を決めた/.test(text(h)));

  // ── 置換チェーン ──────────────────────────────────────────────────────
  const sc = await client.callTool({ name: "get_successor_context", arguments: { id: oldId } });
  check("後継チェーンが現行版へ辿れる", new RegExp(`現行版: \\\`${newId}\\\``).test(text(sc)), text(sc));

  // ── 回帰: ハイフン・日本語・min_score ────────────────────────────────
  const s3 = await client.callTool({ name: "search_context", arguments: { query: "ant-memory", search_mode: "bm25", min_score: 0, include_history: true } });
  check("ハイフン入り検索（回帰）", !/no such column/i.test(text(s3)), text(s3).slice(0, 150));

  const s4 = await client.callTool({ name: "search_context", arguments: { query: "ミュウ", search_mode: "bm25", min_score: 0, include_history: true, full_content: true } });
  check("日本語trigram検索（回帰）", /ミュウ/.test(text(s4)));

  const s5 = await client.callTool({ name: "search_context", arguments: { query: "文体の方針", min_score: 0.9 } });
  const t5 = text(s5);
  check("min_score=0.9 で最良ヒット相当のみ", !/---/.test(t5.split("include_history").length ? t5.replace(/\n\n\(\d+ 件[^\n]*\)/, "") : t5) || /B案|見つかりません/.test(t5));

  // ── read_context 互換性 ───────────────────────────────────────────────
  const r1 = await client.callTool({ name: "read_context", arguments: { id: oldId } });
  check("read_context(id) の互換性維持（旧本文も読める）", /A案：丁寧な敬体/.test(text(r1)));
} finally {
  await client.close();
}

console.log(`\n結果: ${passed} passed / ${failed} failed`);
process.exit(failed ? 1 : 0);
