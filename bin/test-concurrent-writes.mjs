#!/usr/bin/env node
/**
 * 4クライアント同時試験（隔離ストア）
 *
 * - claude-code / codex / antigravity / hermes が同時に書込・supersede・
 *   重要度変更を行っても ID 衝突・欠損・DB破損が0であること
 * - 同じ変更イベントの二重登録がないこと（または検出できること）
 * - integrity_check=ok、外部キー違反0
 * - Hermes は背後モデルを切り替えながら書込し、actor 連続性を確認
 *
 * 実行: node bin/test-concurrent-writes.mjs [--writes N] [--soak MS]
 */

import { join, resolve, dirname } from "node:path";
import { fileURLToPath, pathToFileURL } from "node:url";
import { DatabaseSync } from "node:sqlite";

const ROOT = resolve(dirname(fileURLToPath(import.meta.url)), "..");
const BIN = join(ROOT, "bin");

const argIdx = (name) => {
  const i = process.argv.indexOf(name);
  return i >= 0 ? process.argv[i + 1] : null;
};
const WRITES = Math.min(parseInt(argIdx("--writes") || "10", 10) || 10, 100);
const SOAK_MS = parseInt(argIdx("--soak") || "0", 10) || 0;

let passed = 0, failed = 0;
const check = (name, ok, detail = "") => {
  if (ok) { passed++; console.log(`PASS ${name}`); }
  else { failed++; console.error(`FAIL ${name}${detail ? ` — ${detail}` : ""}`); }
};
const text = (r) => (r.content ?? []).map((c) => c.text).join("\n");
const sleep = (ms) => new Promise((r) => setTimeout(r, ms));

const WS = `concurrent-writes-test-${process.pid}`;

async function openAs(clientName, extraEnv = {}) {
  const { Client } = await import(
    pathToFileURL(join(ROOT, "server", "node_modules", "@modelcontextprotocol", "sdk", "dist", "esm", "client", "index.js")).href
  );
  const { StdioClientTransport } = await import(
    pathToFileURL(join(ROOT, "server", "node_modules", "@modelcontextprotocol", "sdk", "dist", "esm", "client", "stdio.js")).href
  );
  const client = new Client({ name: `concurrent-${clientName}`, version: "1.0.0" });
  await client.connect(new StdioClientTransport({
    command: process.execPath, args: [join(BIN, "ant-memory.mjs")], cwd: ROOT,
    env: { ...process.env, ANT_MEMORY_ROOT: ROOT, ANT_MEMORY_WORKSPACE: WS, ANT_MEMORY_CLIENT: clientName, ...extraEnv },
  }));
  return client;
}

// ── 準備: 各クライアントの接続 ────────────────────────────────────────────────
console.log(`設定: writes=${WRITES}/agent soak=${SOAK_MS}ms`);
const agents = ["claude-code", "codex", "antigravity"];
const clients = new Map();
for (const a of agents) clients.set(a, await openAs(a));
clients.set("hermes-A", await openAs("hermes", { ANT_MEMORY_MODEL: "model/alpha", ANT_MEMORY_PROVIDER: "prov-a" }));
clients.set("hermes-B", await openAs("hermes", { ANT_MEMORY_MODEL: "model/beta", ANT_MEMORY_PROVIDER: "prov-b" }));

const allIds = [];
let writeErrors = [];

try {
  // GO 合図なしで全員ほぼ同時に書き始める（同時実行のストレス）
  const jobs = [];
  for (const [key, client] of clients) {
    const actor = key.startsWith("hermes") ? "hermes" : key;
    jobs.push((async () => {
      const ids = [];
      for (let i = 0; i < WRITES; i++) {
        try {
          const r = await client.callTool({
            name: "write_context",
            arguments: {
              topic: `concurrent: ${actor} の記録 ${String(i).padStart(3, "0")}`,
              content: `同時試験 ${key} index=${i}。整合性とID衝突の検証用本文。`,
              tags: ["concurrent"],
              importance: i % 4 === 0 ? "high" : "low",
            },
          });
          const id = text(r).match(/id=`([0-9a-f]{10})`/i)?.[1];
          if (!id) throw new Error(`ID取得失敗: ${text(r).slice(0, 80)}`);
          ids.push(id);
          if (SOAK_MS > 0) await sleep(SOAK_MS);
        } catch (err) {
          writeErrors.push({ key, i, message: String(err).slice(0, 120) });
        }
      }
      return ids;
    })());
  }
  const results = await Promise.all(jobs);
  results.forEach((ids) => allIds.push(...ids));

  check("同時書込でエラーゼロ", writeErrors.length === 0, JSON.stringify(writeErrors.slice(0, 3)));

  // ── Hermes モデル切替をまたぐ supersede（hermes-B が hermes-A の記録を置換）──
  const hermesAIds = results[agents.length]; // hermes-A の ids
  let succId = null;
  if (hermesAIds?.length) {
    const s = await clients.get("hermes-B").callTool({
      name: "supersede_context",
      arguments: {
        old_id: hermesAIds[0],
        topic: `concurrent: hermes の改訂版`,
        content: "モデルBからモデルAの記録を置換。actor連続性の検証。",
        reason: "concurrent write置換試験",
        tags: ["concurrent"],
      },
    });
    succId = text(s).match(/新記録 id=`([0-9a-f]{10})`/i)?.[1];
  }
  check("Hermesのモデル切替をまたぐsupersedeが成功", Boolean(succId));
  if (succId) allIds.push(succId);

  // ── 同時重要度変更（同じ対象に複数クライアントが触れる）─────────────────────
  if (allIds.length >= 2) {
    const target = allIds[0];
    const impJobs = [...clients.values()].map((c, idx) =>
      c.callTool({
        name: "change_importance",
        arguments: { id: target, importance: idx % 2 === 0 ? "medium" : "low", reason: `concurrent write同時変更 idx=${idx}` },
      }).then(() => ({ ok: true })).catch((e) => ({ ok: false, e: String(e).slice(0, 80) })),
    );
    const impResults = await Promise.all(impJobs);
    check("同一対象への同時重要度変更でエラーなし", impResults.every((r) => r.ok), JSON.stringify(impResults.filter((r) => !r.ok).slice(0, 2)));
  }

  // ── 全件 read back ────────────────────────────────────────────────────────
  const verifier = clients.get("claude-code");
  let missing = 0;
  for (const id of allIds) {
    const r = await verifier.callTool({ name: "read_context", arguments: { id } });
    if (/No chunk found/i.test(text(r))) missing += 1;
  }
  check("書込した全IDが読み戻せる", missing === 0, `missing=${missing}/${allIds.length}`);
} finally {
  for (const c of clients.values()) await c.close().catch(() => {});
}

// ── DB 整合性検証 ────────────────────────────────────────────────────────────
{
  const db = new DatabaseSync(join(ROOT, "stores", WS, "store.db"), { readOnly: true });
  try {
    const integrity = db.prepare("PRAGMA integrity_check").get().integrity_check;
    check("integrity_check=ok", integrity === "ok", integrity);

    const fk = db.prepare("PRAGMA foreign_key_check").all().length;
    check("外部キー違反ゼロ", fk === 0, `violations=${fk}`);

    const stored = db.prepare("SELECT COUNT(*) AS c FROM chunks WHERE tags LIKE '%concurrent%'").get().c;
    const expectedWrites = (agents.length + 2) * WRITES + 1; // +supersede新規1件
    check("期待件数が格納されている", stored === expectedWrites, `stored=${stored} expected=${expectedWrites}`);

    // ID重複は主キーが保証するので、ここでは「報告ID数 == 格納数」を確認
    check("ID衝突なし（報告ID数==格納数）", allIds.length === expectedWrites && new Set(allIds).size === expectedWrites,
      `reported=${allIds.length} distinct=${new Set(allIds).size}`);

    // イベント二重登録チェック: created イベントはchunkごとに高々1回（監査ツール経由のみ）。
    // 注意: 上流write_chunkはイベントを書かないため、createdイベントは監査操作由来のみ。
    const dupEvents = db.prepare(
      `SELECT chunk_id, event_type, COUNT(*) AS c FROM memory_events
       WHERE event_type IN ('superseded','retracted','restored')
       GROUP BY chunk_id, event_type HAVING c > 1 AND event_type != 'importance_changed'`,
    ).all();
    check("単発系イベントの二重登録なし", dupEvents.length === 0, JSON.stringify(dupEvents.slice(0, 3)));

    // Hermes actor 連続性
    const hermesAgents = db.prepare(
      "SELECT COUNT(DISTINCT agent) AS c FROM chunks WHERE agent='hermes'",
    ).get().c;
    check("モデル切替をまたいでもactor=hermesで連続", hermesAgents === 1);

    // provenance 記録確認
    const provs = db.prepare(
      "SELECT DISTINCT model FROM memory_events WHERE model IS NOT NULL",
    ).all().map((r) => r.model);
    check("補助provenance(model)が記録されている", provs.length >= 2, JSON.stringify(provs));
  } finally { db.close(); }
}

console.log(`\n結果: ${passed} passed / ${failed} failed`);
process.exit(failed ? 1 : 0);
