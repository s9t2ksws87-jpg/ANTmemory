#!/usr/bin/env node
/**
 * Phase 4: 記録主体の信頼性・4クライアント対応の試験
 *
 * - 4クライアント(claude-code/codex/antigravity/hermes)が隔離ストアへ
 *   正しいagent名で書込できる
 * - 別agent名を偽装した書込は接続元へ正規化される
 * - Hermes: 背後モデル(ANT_MEMORY_MODEL)を切り替えても actor は hermes のまま、
 *   model/provider は補助provenanceとしてイベントに記録される
 *
 * 実行: node bin/test-actors.mjs
 */

import { join, resolve, dirname } from "node:path";
import { fileURLToPath, pathToFileURL } from "node:url";
import { DatabaseSync } from "node:sqlite";

const ROOT = resolve(dirname(fileURLToPath(import.meta.url)), "..");
const BIN = join(ROOT, "bin");
const { Client } = await import(
  pathToFileURL(join(ROOT, "server", "node_modules", "@modelcontextprotocol", "sdk", "dist", "esm", "client", "index.js")).href
);
const { StdioClientTransport } = await import(
  pathToFileURL(join(ROOT, "server", "node_modules", "@modelcontextprotocol", "sdk", "dist", "esm", "client", "stdio.js")).href
);

const WS = "phase4-actors-test";
let passed = 0, failed = 0;
const check = (name, ok, detail = "") => {
  if (ok) { passed++; console.log(`PASS ${name}`); }
  else { failed++; console.error(`FAIL ${name}${detail ? ` — ${detail}` : ""}`); }
};
const text = (r) => (r.content ?? []).map((c) => c.text).join("\n");

async function openAs(clientName, extra = {}) {
  const client = new Client({ name: `phase4-${clientName}`, version: "1.0.0" });
  await client.connect(new StdioClientTransport({
    command: process.execPath,
    args: [join(BIN, "ant-memory.mjs")],
    cwd: ROOT,
    env: {
      ...process.env, ANT_MEMORY_ROOT: ROOT,
      ANT_MEMORY_WORKSPACE: WS, ANT_MEMORY_CLIENT: clientName, ...extra,
    },
  }));
  return client;
}

// ── 1. 4クライアントが正しいagent名で書込できる ─────────────────────────────
const ids = {};
for (const name of ["claude-code", "codex", "antigravity", "hermes"]) {
  const c = await openAs(name);
  try {
    const r = await c.callTool({
      name: "write_context",
      arguments: { topic: `phase4: ${name} の書込`, content: `${name} からの正規書込試験。`, tags: ["phase4"] },
    });
    const id = text(r).match(/id=`([0-9a-f]{10})`/i)?.[1];
    check(`${name} が書込できる`, Boolean(id));
    ids[name] = id;
  } finally { await c.close(); }
}

// ── 2. 偽装入力は公開schemaに存在せず、送られても接続元actorが正本 ───────────────
{
  const c = await openAs("codex");
  try {
    const r = await c.callTool({
      name: "write_context",
      arguments: { topic: "phase4: 偽装試験", content: "codexがclaude-codeを名乗る試験。", agent: "claude-code", tags: ["phase4"] },
    });
    check("偽装入力を応答へ反映しない", !/claude-code.*として記録|警告/.test(text(r)), text(r).slice(0, 120));
  } finally { await c.close(); }
}

// ── 3. Hermes: モデル切替でも actor 連続性 ─────────────────────────────────
{
  const c1 = await openAs("hermes", { ANT_MEMORY_MODEL: "stealth/ox-alpha", ANT_MEMORY_PROVIDER: "openrouter" });
  try {
    const r = await c1.callTool({
      name: "write_context",
      arguments: { topic: "phase4: hermes model A", content: "モデルAでの書込。", tags: ["phase4"] },
    });
    ids["hermes-A"] = text(r).match(/id=`([0-9a-f]{10})`/i)?.[1];
  } finally { await c1.close(); }

  const c2 = await openAs("hermes", { ANT_MEMORY_MODEL: "other/model-b", ANT_MEMORY_PROVIDER: "other" });
  try {
    const r = await c2.callTool({
      name: "write_context",
      arguments: { topic: "phase4: hermes model B", content: "モデルBでの書込。", tags: ["phase4"] },
    });
    ids["hermes-B"] = text(r).match(/id=`([0-9a-f]{10})`/i)?.[1];
    // model B で重要度変更し、provenanceが記録されるか確認
    await c2.callTool({
      name: "change_importance",
      arguments: { id: ids["hermes-A"], importance: "high", reason: "phase4 provenance試験" },
    });
  } finally { await c2.close(); }
}

// ── DB検証 ──────────────────────────────────────────────────────────────────
{
  const db = new DatabaseSync(join(ROOT, "stores", WS, "store.db"), { readOnly: true });
  try {
    const agents = db.prepare("SELECT DISTINCT agent FROM chunks WHERE tags LIKE '%phase4%'").all().map((r) => r.agent).sort();
    check("書込主体は4クライアント名のみ", JSON.stringify(agents) === JSON.stringify(["antigravity", "claude-code", "codex", "hermes"]), agents.join(","));

    const fake = db.prepare("SELECT COUNT(*) AS c FROM chunks WHERE topic LIKE '%偽装%' AND agent != 'codex'").get().c;
    check("偽装書込は接続元(codex)として記録される", fake === 0);

    const hermesModels = db.prepare(
      "SELECT COUNT(DISTINCT agent) AS c FROM chunks WHERE agent='hermes' AND topic LIKE '%hermes model%'",
    ).get().c;
    check("モデル切替でも actor は hermes のまま", hermesModels === 1);

    const ev = db.prepare(
      "SELECT actor, model, provider FROM memory_events WHERE event_type='importance_changed' ORDER BY created_at DESC LIMIT 1",
    ).get();
    check("イベントに actor=hermes と補助provenance(model/provider)が記録される",
      ev?.actor === "hermes" && ev?.model === "other/model-b" && ev?.provider === "other", JSON.stringify(ev));
  } finally { db.close(); }
}

console.log(`\n結果: ${passed} passed / ${failed} failed`);
process.exit(failed ? 1 : 0);
