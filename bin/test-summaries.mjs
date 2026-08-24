#!/usr/bin/env node
/**
 * Summary Layerの試験（隔離ストア）
 *
 * - short/detailed summary が生成される
 * - raw content は一切変更されない（ハッシュ照合）
 * - summary 削除・再生成が raw へ影響しない
 * - source_hash が一致しない summary は無効扱い（原文更新後に古い要約を出さない）
 * - summary がない記憶でも read_context が正常動作する（フォールバック）
 *
 * 実行: node bin/test-summaries.mjs
 */

import { join, resolve, dirname } from "node:path";
import { fileURLToPath, pathToFileURL } from "node:url";
import { createHash } from "node:crypto";
import { DatabaseSync } from "node:sqlite";

const ROOT = resolve(dirname(fileURLToPath(import.meta.url)), "..");
const BIN = join(ROOT, "bin");

const WS = "summary-test";
let passed = 0, failed = 0;
const check = (name, ok, detail = "") => {
  if (ok) { passed++; console.log(`PASS ${name}`); }
  else { failed++; console.error(`FAIL ${name}${detail ? ` — ${detail}` : ""}`); }
};
const text = (r) => (r.content ?? []).map((c) => c.text).join("\n");
const sha256 = (s) => createHash("sha256").update(s, "utf8").digest("hex");

const { Client } = await import(
  pathToFileURL(join(ROOT, "server", "node_modules", "@modelcontextprotocol", "sdk", "dist", "esm", "client", "index.js")).href
);
const { StdioClientTransport } = await import(
  pathToFileURL(join(ROOT, "server", "node_modules", "@modelcontextprotocol", "sdk", "dist", "esm", "client", "stdio.js")).href
);

const client = new Client({ name: "summary", version: "1.0.0" });
await client.connect(new StdioClientTransport({
  command: process.execPath,
  args: [join(BIN, "ant-memory.mjs")],
  cwd: ROOT,
  env: { ...process.env, ANT_MEMORY_ROOT: ROOT, ANT_MEMORY_WORKSPACE: WS, ANT_MEMORY_CLIENT: "hermes" },
}));

try {
  // 複数文の長めの記録を投入
  const longContent = [
    "作品世界の魔法体系について方針を決定した。",
    "五大属性と特殊系二種の構成を採用。理由は物語規模に対して体系が複雑すぎないため。",
    "当初検討した八属性案は却下。読者の追跡負荷が高いことが理由。",
    "無詠唱発動は上級者限定とし、制御成功率は五割以下に設定する。",
    "この原則は全話を通じて変更禁止である。",
    "次回改稿時に属性相性表を設定ファイルへ反映する。",
  ].join("");
  const w = await client.callTool({
    name: "write_context",
    arguments: { topic: "summary: 魔法体系の方針", content: longContent, tags: ["summary"], importance: "high" },
  });
  const id = text(w).match(/id=`([0-9a-f]{10})`/i)[1];

  // 原文ハッシュ保存
  const dbRO = new DatabaseSync(join(ROOT, "stores", WS, "store.db"), { readOnly: true });
  let hashBefore;
  try {
    hashBefore = sha256(dbRO.prepare("SELECT content FROM chunks WHERE id=?").get(id).content);
  } finally { dbRO.close(); }

  // ── short summary ─────────────────────────────────────────────────────
  const s1 = await client.callTool({ name: "get_memory_summary", arguments: { id } });
  check("short summary が取得できる", /\[short summary\]/.test(text(s1)), text(s1).slice(0, 120));
  check("short summary は先頭文ベース", /魔法体系について方針を決定した/.test(text(s1)));
  check("short summary は原文より短い", text(s1).length < longContent.length);

  // ── detailed summary ──────────────────────────────────────────────────
  const s2 = await client.callTool({ name: "get_memory_summary", arguments: { id, level: "detailed" } });
  const td2 = text(s2);
  check("detailed summary が取得できる", /\[detailed summary\]/.test(td2));
  // キー文（却下理由・禁止原則）が含まれること
  check("detailed summary に重要文が選別される", /却下/.test(td2) || /変更禁止/.test(td2), td2.slice(0, 300));

  // ── raw 不変 ──────────────────────────────────────────────────────────
  const dbRO2 = new DatabaseSync(join(ROOT, "stores", WS, "store.db"), { readOnly: true });
  let hashAfter;
  try {
    hashAfter = sha256(dbRO2.prepare("SELECT content FROM chunks WHERE id=?").get(id).content);
  } finally { dbRO2.close(); }
  check("summary生成後もraw contentは不変", hashAfter === hashBefore);

  // ── source_hash 無効化：原文更新をシミュレート ────────────────────────
  {
    const db = new DatabaseSync(join(ROOT, "stores", WS, "store.db"));
    try {
      // 直接DB更新ではなく、テスト用にsource_hashだけ壊す（原文は触らない）
      db.prepare("UPDATE memory_summaries SET source_hash = 'deadbeef' WHERE chunk_id = ?").run(id);
    } finally { db.close(); }
  }
  const s3 = await client.callTool({ name: "get_memory_summary", arguments: { id } });
  // 無効→自動再生成されるため、再び有効なsummaryが返る（古い無効値ではない）
  check("source_hash不一致のsummaryは無効扱いされ、自動再生成される",
    /\[short summary\]/.test(text(s3)) && !/deadbeef/.test(text(s3)));

  // 再生成後もraw不変
  const dbRO3 = new DatabaseSync(join(ROOT, "stores", WS, "store.db"), { readOnly: true });
  try {
    const h = sha256(dbRO3.prepare("SELECT content FROM chunks WHERE id=?").get(id).content);
    check("summary再生成後もraw不変", h === hashBefore);
  } finally { dbRO3.close(); }

  // ── summary削除がrawへ影響しない ──────────────────────────────────────
  {
    const db = new DatabaseSync(join(ROOT, "stores", WS, "store.db"));
    try { db.prepare("DELETE FROM memory_summaries WHERE chunk_id = ?").run(id); } finally { db.close(); }
    const s4 = await client.callTool({ name: "get_memory_summary", arguments: { id, level: "detailed" } });
    check("summary削除後も再取得（再生成）できる", /detailed summary/.test(text(s4)));
    const dbRO4 = new DatabaseSync(join(ROOT, "stores", WS, "store.db"), { readOnly: true });
    try {
      const h = sha256(dbRO4.prepare("SELECT content FROM chunks WHERE id=?").get(id).content);
      check("summary削除・再生成を通じてraw不変", h === hashBefore);
    } finally { dbRO4.close(); }
  }

  // ── read_context 互換性 ───────────────────────────────────────────────
  const r = await client.callTool({ name: "read_context", arguments: { id } });
  check("read_context で全文が取得できる（フォールバック正常）", /無詠唱発動/.test(text(r)) && /次回改稿/.test(text(r)));

} finally {
  await client.close().catch(() => {});
}

console.log(`\n結果: ${passed} passed / ${failed} failed`);
process.exit(failed ? 1 : 0);
