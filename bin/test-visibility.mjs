#!/usr/bin/env node
/**
 * Visibility導入の試験（隔離ストア）
 *
 * - promote_memory で visibility 変更（hot/warm/cold/archive）
 * - importance と独立（critical + cold が成立する）
 * - 通常検索で cold/archive が抑制され、include_hidden=true で取得できる
 * - visibility変更は原文・importance・lifecycle状態を変えない
 * - visibility変更は監査イベントとして残る（actor + reason必須）
 *
 * 実行: node bin/test-visibility.mjs
 */

import { join, resolve, dirname } from "node:path";
import { fileURLToPath, pathToFileURL } from "node:url";
import { createHash } from "node:crypto";
import { DatabaseSync } from "node:sqlite";

const ROOT = resolve(dirname(fileURLToPath(import.meta.url)), "..");
const BIN = join(ROOT, "bin");

const WS = "visibility-test";
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

const client = new Client({ name: "visibility", version: "1.0.0" });

await client.connect(new StdioClientTransport({
  command: process.execPath,
  args: [join(BIN, "ant-memory.mjs")],
  cwd: ROOT,
  env: { ...process.env, ANT_MEMORY_ROOT: ROOT, ANT_MEMORY_WORKSPACE: WS, ANT_MEMORY_CLIENT: "hermes" },
}));

try {
  // 準備: critical の記録を2件作る
  const w1 = await client.callTool({
    name: "write_context",
    arguments: {
      topic: "visibility: 絶対変更禁止ルール",
      content: "作品世界の物理法則はいかなる場合も変更禁止。この原則は滅多に参照しないが重要。",
      tags: ["visibility"], importance: "critical",
    },
  });
  const idCritical = text(w1).match(/id=`([0-9a-f]{10})`/i)[1];
  const hashBefore = (() => {
    const db = new DatabaseSync(join(ROOT, "stores", WS, "store.db"), { readOnly: true });
    try {
      return db.prepare("SELECT content FROM chunks WHERE id = ?").get(idCritical).content;
    } finally { db.close(); }
  })();

  const w2 = await client.callTool({
    name: "write_context",
    arguments: {
      topic: "visibility: よく使う作業メモ",
      content: "頻繁に参照する現在の作業状態。ミュウの試験語を含む。",
      tags: ["visibility"], importance: "low",
    },
  });
  const idFrequent = text(w2).match(/id=`([0-9a-f]{10})`/i)?.[1];

  // ── critical + cold が成立する ────────────────────────────────────────
  const p1 = await client.callTool({
    name: "promote_memory",
    arguments: { id: idCritical, to: "cold", reason: "重要だが毎回コンテキストに入れる必要がないため" },
  });
  check("critical記憶をcoldへ変更できる（importanceと独立）", /cold/.test(text(p1)), text(p1).slice(0, 120));

  const imp = await client.callTool({ name: "read_context", arguments: { id: idCritical } });
  check("原文・importanceは変更されていない（criticalのまま）", /importance:\*\* critical/.test(text(imp)) && /物理法則/.test(text(imp)));

  // ── reason必須 ────────────────────────────────────────────────────────
  // zodバリデーションがMCPエラーとして返す（isErrorフラグ形式）
  const p2 = await client.callTool({
    name: "promote_memory",
    arguments: { id: idFrequent, to: "hot" },
  });
  check("reason無しの変更は拒否される", /reason.*undefined|Invalid arguments/.test(text(p2)), text(p2).slice(0, 120));

  // hot へ昇格
  await client.callTool({
    name: "promote_memory",
    arguments: { id: idFrequent, to: "hot", reason: "毎回の作業で参照するため" },
  });

  // ── 通常検索で cold が抑制される ───────────────────────────────────────
  const s1 = await client.callTool({
    name: "search_context",
    arguments: { query: "物理法則 変更禁止", search_mode: "bm25", min_score: 0 },
  });
  const t1 = text(s1);
  check("通常検索ではcold記憶が抑制される", !/絶対変更禁止ルール/.test(t1), t1.slice(0, 150));

  // include_hidden=true で取得できる
  const s2 = await client.callTool({
    name: "search_context",
    arguments: { query: "物理法則 変更禁止", search_mode: "bm25", min_score: 0, include_hidden: true },
  });
  check("include_hidden=true でcold記憶が取得できる", /絶対変更禁止ルール/.test(text(s2)));

  // hot記憶は通常検索で出る
  const s3 = await client.callTool({
    name: "search_context",
    arguments: { query: "よく使う作業メモ ミュウ", search_mode: "bm25", min_score: 0 },
  });
  check("hot記憶は通常検索で表示される", /よく使う作業メモ/.test(text(s3)));

  // archive も抑制される
  await client.callTool({
    name: "promote_memory",
    arguments: { id: idCritical, to: "archive", reason: "さらに深く沈める試験" },
  });
  const s4 = await client.callTool({
    name: "search_context",
    arguments: { query: "物理法則", search_mode: "bm25", min_score: 0, include_hidden: false },
  });
  check("archive記憶も抑制される", !/絶対変更禁止ルール/.test(text(s4)));

  // ── 監査イベント ──────────────────────────────────────────────────────
  const h = await client.callTool({ name: "get_context_history", arguments: { id: idCritical } });
  const th = text(h);
  check("visibility_changed イベントがactor/reason付きで記録される",
    /visibility_changed.*hermes/.test(th) && /毎回コンテキスト/.test(th), th.slice(0, 300));

  // ── 不変条件: 原文ハッシュ不変 ────────────────────────────────────────
  const db = new DatabaseSync(join(ROOT, "stores", WS, "store.db"), { readOnly: true });
  try {
    const now = db.prepare("SELECT content FROM chunks WHERE id = ?").get(idCritical).content;
    check("原文は1バイトも変更されていない", now === hashBefore);
    // lifecycle状態はactiveのまま
    const lc = db.prepare("SELECT status FROM memory_lifecycle WHERE chunk_id = ?").get(idCritical);
    check("lifecycle状態は active のまま", (lc?.status ?? "active") === "active");
  } finally { db.close(); }

} finally {
  await client.close().catch(() => {});
}

console.log(`\n結果: ${passed} passed / ${failed} failed`);
process.exit(failed ? 1 : 0);
