#!/usr/bin/env node
/**
 * Phase Recall-5: Usage Tracking / Retrieval Session の試験（隔離ストア）
 *
 * - begin_retrieval_session で session_id が発行される
 * - search_context(session_id) で既読記憶の順位が下がる
 * - access_count / last_accessed_at が記録される
 * - usage無しでも検索精度は変わらない（回帰）
 *
 * 実行: node bin/test-usage-sessions.mjs
 */

import { join, resolve, dirname } from "node:path";
import { fileURLToPath, pathToFileURL } from "node:url";
import { DatabaseSync } from "node:sqlite";
import fs from "node:fs";

const ROOT = resolve(dirname(fileURLToPath(import.meta.url)), "..");
const BIN = join(ROOT, "bin");

const WS = "recall-usage-test";
const dbPath = join(ROOT, "stores", WS, "store.db");
for (const p of [dbPath, dbPath + "-wal", dbPath + "-shm"]) try { fs.rmSync(p, { force: true }); } catch {}
let passed = 0, failed = 0;
const check = (name, ok, detail = "") => {
  if (ok) { passed++; console.log(`PASS ${name}`); }
  else { failed++; console.error(`FAIL ${name}${detail ? ` — ${detail}` : ""}`); }
};
const text = (r) => (r.content ?? []).map((c) => c.text).join("\n");
const topicsOf = (t) => [...t.matchAll(/### \[relative_score:[^\]]+\] ([^\n]+)/g)].map((m) => m[1]);

const { Client } = await import(
  pathToFileURL(join(ROOT, "server", "node_modules", "@modelcontextprotocol", "sdk", "dist", "esm", "client", "index.js")).href
);
const { StdioClientTransport } = await import(
  pathToFileURL(join(ROOT, "server", "node_modules", "@modelcontextprotocol", "sdk", "dist", "esm", "client", "stdio.js")).href
);

const client2 = new Client({ name: "recallusage2", version: "1.0.0" });
await client2.connect(new StdioClientTransport({
  command: process.execPath,
  args: [join(BIN, "ant-memory.mjs")],
  cwd: ROOT,
  env: { ...process.env, ANT_MEMORY_ROOT: ROOT, ANT_MEMORY_WORKSPACE: WS, ANT_MEMORY_CLIENT: "hermes", ANT_MEMORY_ADVANCED_TOOLS: "1" },
}));
try {
  for (let i = 0; i < 5; i++) {
    await client2.callTool({
      name: "write_context",
      arguments: {
        topic: `usage試験[${i}]: 軍制の編成と運用${i}`,
        content: `軍制に関する記録その${i}。編成・術兵・兵站管理を含む本文。`,
        tags: ["u35"], importance: "low",
      },
    });
  }
  await new Promise((r) => setTimeout(r, 6000));

  const baseQuery = { query: "軍制の編成 術兵の訓練 兵站管理", search_mode: "bm25", min_score: 0 };

  // セッション開始
  const bs = text(await client2.callTool({ name: "begin_retrieval_session", arguments: {} }));
  const sid = bs.match(/`([0-9a-f-]{36})`/)?.[1];
  check("begin_retrieval_session がsession_idを発行する", Boolean(sid));

  // 1回目: 全件ヒット
  const s1 = topicsOf(text(await client2.callTool({
    name: "search_context", arguments: { ...baseQuery, session_id: sid, top_k: 10 },
  })));
  check("1回目は全候補が返る", s1.length >= 4, `${s1.length}件`);

  // 1回目の上位3件だけ既読にする（top_kで部分閲覧をシミュレート）
  const firstSeen = s1.slice(0, 3);
  const sid2 = text(await client2.callTool({ name: "begin_retrieval_session", arguments: {} }))
    .match(/`([0-9a-f-]{36})`/)[1];
  await client2.callTool({
    name: "search_context", arguments: { ...baseQuery, session_id: sid2, top_k: 3 },
  });

  // 2回目: 既読(上位3)が末尾へ回り、未読が先頭に来る
  const s2 = topicsOf(text(await client2.callTool({
    name: "search_context", arguments: { ...baseQuery, session_id: sid2, top_k: 10 },
  })));
  check("2回目も全件返る（消えない）", s2.length === s1.length);
  const head2 = s2.slice(0, s2.length - firstSeen.length);
  const tail2 = s2.slice(-firstSeen.length);
  check("既読記憶の順位が下がる（既読→末尾、未読→先頭）",
    firstSeen.every((t) => tail2.includes(t)) && !head2.some((t) => firstSeen.includes(t)),
    JSON.stringify({ firstSeen, order: s2 }));

  // top_kと併用: 新しいセッションで既読を下げた上で未読だけ見られる
  const sid3 = text(await client2.callTool({ name: "begin_retrieval_session", arguments: {} }))
    .match(/`([0-9a-f-]{36})`/)[1];
  await client2.callTool({
    name: "search_context", arguments: { ...baseQuery, session_id: sid3, top_k: 3 },
  });
  const s3 = topicsOf(text(await client2.callTool({
    name: "search_context", arguments: { ...baseQuery, session_id: sid3, top_k: 2 },
  })));
  check("既読ペナルティ+top_k で未読が優先表示される",
    s3.length <= 2 && !firstSeen.includes(s3[0]), JSON.stringify(s3));

  // usage記録の確認
  const db = new DatabaseSync(join(ROOT, "stores", WS, "store.db"), { readOnly: true });
  try {
    const used = db.prepare("SELECT COUNT(*) c FROM memory_usage WHERE access_count > 0").get().c;
    check("access_count が記録されている", used >= 4, `used=${used}`);
    const withSession = db.prepare(
      "SELECT COUNT(*) c FROM memory_usage WHERE last_session_id IS NOT NULL",
    ).get().c;
    check("last_session_id が記録されている", withSession >= 4, `with=${withSession}`);
  } finally { db.close(); }

  // usage無しでも検索精度は同じ（回帰）
  const noSess = topicsOf(text(await client2.callTool({
    name: "search_context", arguments: { ...baseQuery, top_k: 10 },
  })));
  check("usage/session無しでも検索結果の精度は維持される",
    noSess.length >= 4 && new Set(noSess).size === new Set(s1).size);

} finally {
  await client2.close().catch(() => {});
}

console.log(`\n結果: ${passed} passed / ${failed} failed`);
process.exit(failed ? 1 : 0);
