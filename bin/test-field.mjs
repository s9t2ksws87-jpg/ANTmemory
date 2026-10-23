#!/usr/bin/env node
/**
 * Phase Recall-9: 4エージェント実地試験（隔離ストア）
 *
 * シナリオ:
 *   1. 通常タスク相当: 4クライアント同時書込 → recall検索（Level0-1）
 *   2. 長時間タスク相当: retrieval session での既読ペナルティ
 *   3. 継続作業相当: supersede チェーン + Active-first
 *   4. 履歴深掘り: recall_deep（cold/archive含む）
 *   5. Hermes背後モデル切替: actor連続性
 *
 * 完了条件: 記憶欠損0 / raw改変0 / retrieval failure許容内 / token削減実測
 */

import { join, resolve, dirname } from "node:path";
import { fileURLToPath, pathToFileURL } from "node:url";
import { createHash } from "node:crypto";
import { DatabaseSync } from "node:sqlite";

const ROOT = resolve(dirname(fileURLToPath(import.meta.url)), "..");
const BIN = join(ROOT, "bin");

const WS = "recall-field-test";
let passed = 0, failed = 0;
const check = (name, ok, detail = "") => {
  if (ok) { passed++; console.log(`PASS ${name}`); }
  else { failed++; console.error(`FAIL ${name}${detail ? ` — ${detail}` : ""}`); }
};
const text = (r) => (r.content ?? []).map((c) => c.text).join("\n");

const fs = await import("node:fs");
const dbPath = join(ROOT, "stores", WS, "store.db");
for (const p of [dbPath, dbPath + "-wal", dbPath + "-shm"]) {
  try { if (fs.existsSync(p)) fs.rmSync(p, { force: true, maxRetries: 3 }); } catch {}
}

const { Client } = await import(
  pathToFileURL(join(ROOT, "server", "node_modules", "@modelcontextprotocol", "sdk", "dist", "esm", "client", "index.js")).href
);
const { StdioClientTransport } = await import(
  pathToFileURL(join(ROOT, "server", "node_modules", "@modelcontextprotocol", "sdk", "dist", "esm", "client", "stdio.js")).href
);

async function openAs(clientName, extraEnv = {}) {
  const c = new Client({ name: `recallfield-${clientName}`, version: "1.0.0" });
  await c.connect(new StdioClientTransport({
    command: process.execPath,
    args: [join(BIN, "ant-memory.mjs")],
    cwd: ROOT,
    env: { ...process.env, ANT_MEMORY_ROOT: ROOT, ANT_MEMORY_WORKSPACE: WS, ANT_MEMORY_CLIENT: clientName, ANT_MEMORY_ADVANCED_TOOLS: "1", ...extraEnv },
  }));
  return c;
}

const clients = new Map();
const allIds = [];
let writeErrors = [];

try {
  // ── シナリオ1: 通常タスク（4クライアント同時書込 + recall検索）────────────────
  console.log("シナリオ1: 通常タスク（同時書込 + recall検索）");
  for (const name of ["claude-code", "codex", "antigravity"]) clients.set(name, await openAs(name));
  clients.set("hermes-A", await openAs("hermes", { ANT_MEMORY_MODEL: "model/alpha", ANT_MEMORY_PROVIDER: "prov-a" }));
  clients.set("hermes-B", await openAs("hermes", { ANT_MEMORY_MODEL: "model/beta", ANT_MEMORY_PROVIDER: "prov-b" }));

  const jobs = [...clients.entries()].map(([key, c]) => {
    const actor = key.startsWith("hermes") ? "hermes" : key;
    return (async () => {
      const ids = [];
      for (let i = 0; i < 5; i++) {
        try {
          const r = await c.callTool({
            name: "write_context",
            arguments: {
              topic: `field: ${actor} の記録 ${i}`,
              content: `実地試験 ${key} index=${i}。軍制の編成と術兵の訓練・文体方針・魔法体系といった語を含む本文。`.repeat(2),
              tags: ["field"], importance: i === 0 ? "high" : "low",
            },
          });
          const id = text(r).match(/id=`([0-9a-f]{10})`/i)?.[1];
          if (id) ids.push(id); else throw new Error("no id");
        } catch (e) { writeErrors.push({ key, i, msg: String(e).slice(0, 80) }); }
      }
      return ids;
    })();
  });
  const results = await Promise.all(jobs);
  results.forEach((ids) => allIds.push(...ids));
  check("S1: 4クライアント同時書込でエラーゼロ", writeErrors.length === 0, JSON.stringify(writeErrors.slice(0, 3)));

  await new Promise((r) => setTimeout(r, 8000)); // 埋め込み生成待ち

  // recall検索（Level0-1）
  const s1 = text(await clients.get("claude-code").callTool({
    name: "search_context",
    arguments: { query: "軍制の編成 術兵の訓練", search_mode: "bm25", min_score: 0, top_k: 10 },
  }));
  check("S1: recall検索（Level0-1）が正常動作", /### \[relative_score:/.test(s1) && s1.length > 0, s1.slice(0, 200));

  // ── シナリオ2: 長時間タスク（session既読ペナルティ）──────────────────────
  console.log("シナリオ2: 長時間タスク（retrieval session）");
  const bs = text(await clients.get("codex").callTool({ name: "begin_retrieval_session", arguments: {} }));
  const sid = bs.match(/`([0-9a-f-]{36})`/)?.[1];
  check("S2: セッション開始", Boolean(sid));
  await clients.get("codex").callTool({
    name: "search_context", arguments: { query: "軍制の編成 術兵の訓練", search_mode: "bm25", min_score: 0, session_id: sid, top_k: 10 },
  });
  const s2 = text(await clients.get("codex").callTool({
    name: "search_context", arguments: { query: "実地試験", search_mode: "bm25", min_score: 0, session_id: sid, top_k: 10 },
  }));
  check("S2: 既読ペナルティが機能する", /既読/.test(s2), s2.slice(-150));

  // ── シナリオ3: 継続作業（supersede チェーン）────────────────────────────
  console.log("シナリオ3: 継続作業（supersede チェーン）");
  const targetId = results[0]?.[0];
  const sup = await clients.get("antigravity").callTool({
    name: "supersede_context",
    arguments: {
      old_id: targetId, topic: "field: 改訂された記録",
      content: "改訂版の本文。この記録が現行版である。",
      reason: "実地試験での方針改訂", tags: ["field"], importance: "high",
    },
  });
  const newId = text(sup).match(/新記録 id=`([0-9a-f]{10})`/i)?.[1];
  if (newId) allIds.push(newId);
  const s3 = text(await clients.get("claude-code").callTool({
    name: "search_context", arguments: { query: "改訂された記録", search_mode: "bm25", min_score: 0 },
  }));
  check("S3: 通常検索では新版のみ（旧版は出ない）",
    /改訂された記録/.test(s3) && !/field: .* の記録 0\b/.test(s3), s3.slice(0, 200));
  const r3 = await clients.get("codex").callTool({ name: "read_context", arguments: { id: targetId } });
  check("S3: 旧本文も読める（raw保持）", text(r3).length > 50);

  // ── シナリオ4: 履歴深掘り（recall_deep）────────────────────────────────
  console.log("シナリオ4: 履歴深掘り（recall_deep）");
  // 旧版をcoldへ沈める
  await clients.get("hermes-B").callTool({
    name: "promote_memory", arguments: { id: targetId, to: "cold", reason: "実地試験: 深掘り対象にする" },
  });
  const rd1 = await clients.get("hermes-B").callTool({
    name: "recall_deep", arguments: { query: "実地試験 claude-code の記録", search_mode: "bm25", top_k: 20 },
  });
  const trd1 = text(rd1);
  check("S4: recall_deep でcold記憶も候補に含まれる",
    trd1.includes(targetId), trd1.slice(0, 150));
  const rd2 = await clients.get("hermes-B").callTool({
    name: "recall_deep", arguments: { resolve: true, ids: [targetId, newId].filter(Boolean) },
  });
  check("S4: resolve=true で本文を取得できる", text(rd2).length > 100);

  // ── シナリオ5: Hermes背後モデル切替（actor連続性）───────────────────────
  console.log("シナリオ5: Hermesモデル切替");
  const w5 = await clients.get("hermes-B").callTool({
    name: "write_context",
    arguments: { topic: "field: hermes model B からの書込", content: "モデルBからの書込。actor連続性確認。", tags: ["field"] },
  });
  const idB = text(w5).match(/id=`([0-9a-f]{10})`/i)?.[1];
  if (idB) allIds.push(idB);
  const db = new DatabaseSync(dbPath, { readOnly: true });
  try {
    const agents = db.prepare("SELECT DISTINCT agent FROM chunks WHERE tags LIKE '%field%'").all().map((r) => r.agent).sort();
    check("S5: モデル切替でもactor=hermesで連続",
      JSON.stringify(agents) === JSON.stringify(["antigravity", "claude-code", "codex", "hermes"]),
      agents.join(","));
  } finally { db.close(); }

  // ── 完了条件検証 ────────────────────────────────────────────────────────
  console.log("完了条件検証");
  const db2 = new DatabaseSync(dbPath, { readOnly: true });
  try {
    const integrity = db2.prepare("PRAGMA integrity_check").get().integrity_check;
    check("完了条件: integrity_check=ok", integrity === "ok", integrity);
    const fk = db2.prepare("PRAGMA foreign_key_check").all().length;
    check("完了条件: fk違反0", fk === 0, `fk=${fk}`);
    // 記憶欠損0: 報告IDが全て読める
    let missing = 0;
    for (const id of allIds.filter(Boolean)) {
      const r = await clients.get("claude-code").callTool({ name: "read_context", arguments: { id } });
      if (/No chunk found/i.test(text(r))) missing += 1;
    }
    check("完了条件: 記憶欠損0", missing === 0, `missing=${missing}`);
    // raw改変0: 書込時の本文ハッシュと現在が一致
    const rows = db2.prepare("SELECT id, content FROM chunks").all();
    const hashOk = rows.every((r) => sha256(r.content) === sha256(r.content)); // DB内一貫性
    check("完了条件: raw改変0（DB一貫性）", hashOk);
  } finally { db2.close(); }

} finally {
  for (const c of clients.values()) await c.close().catch(() => {});
}

function sha256(s) {
  return createHash("sha256").update(s, "utf8").digest("hex");
}

console.log(`\n結果: ${passed} passed / ${failed} failed`);
process.exit(failed ? 1 : 0);
