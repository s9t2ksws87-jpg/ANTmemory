#!/usr/bin/env node
/**
 * Phase E: shared Goal pointer + non-authoritative memory + general provenance.
 *
 * Invariants:
 * - No new tool/schema is needed for Goal sharing: goal.current is ordinary state.
 * - State is the current shared blackboard; durable history stays in memory chunks.
 * - write_context accepts source_ref and retrieval exposes source + created time.
 * - Memory retrieval tells agents that recalled memory is not automatically truth.
 * - source_ref can be retrofitted through correct_context without deleting old Raw.
 */

import { rmSync } from "node:fs";
import { join, resolve, dirname } from "node:path";
import { fileURLToPath, pathToFileURL } from "node:url";
import { DatabaseSync } from "node:sqlite";

const ROOT = resolve(dirname(fileURLToPath(import.meta.url)), "..");
const WS = "phase-e-test";
const dbPath = join(ROOT, "stores", WS, "store.db");
for (const p of [dbPath, `${dbPath}-wal`, `${dbPath}-shm`]) {
  try { rmSync(p, { force: true }); } catch {}
}

const { Client } = await import(pathToFileURL(join(ROOT, "server", "node_modules", "@modelcontextprotocol", "sdk", "dist", "esm", "client", "index.js")).href);
const { StdioClientTransport } = await import(pathToFileURL(join(ROOT, "server", "node_modules", "@modelcontextprotocol", "sdk", "dist", "esm", "client", "stdio.js")).href);

let pass = 0;
let fail = 0;
const check = (name, ok, detail = "") => {
  if (ok) { pass += 1; console.log(`PASS ${name}`); }
  else { fail += 1; console.error(`FAIL ${name}${detail ? ` — ${detail}` : ""}`); }
};
const text = (r) => (r.content ?? []).map((x) => x.text).join("\n");
const idOf = (s) => s.match(/id=`([0-9a-f]{10})`/i)?.[1] ?? null;

const client = new Client({ name: "phase-e", version: "1" });
await client.connect(new StdioClientTransport({
  command: process.execPath,
  args: [join(ROOT, "bin", "ant-memory.mjs")],
  cwd: ROOT,
  env: {
    ...process.env,
    ANT_MEMORY_ROOT: ROOT,
    ANT_MEMORY_WORKSPACE: WS,
    ANT_MEMORY_CLIENT: "phase-e-audit",
  },
}));

try {
  const listed = await client.listTools();
  const tools = listed.tools;
  check("default surface remains 16 tools", tools.length === 16, tools.map((t) => t.name).sort().join(","));

  const write = tools.find((t) => t.name === "write_context");
  const search = tools.find((t) => t.name === "search_context");
  const read = tools.find((t) => t.name === "read_context");
  const setState = tools.find((t) => t.name === "set_state");
  check("write_context exposes source_ref", "source_ref" in (write?.inputSchema?.properties ?? {}), JSON.stringify(write?.inputSchema));
  check("tool contract says memory is not automatic truth", /not automatic|not verified truth|not authority/i.test(`${write?.description}\n${search?.description}\n${read?.description}`), `${write?.description}\n${search?.description}\n${read?.description}`);
  check("set_state describes shared current blackboard", /shared blackboard|current shared state/i.test(setState?.description ?? ""), setState?.description ?? "");

  const first = text(await client.callTool({
    name: "write_context",
    arguments: {
      topic: "Phase E historical claim",
      content: "PHASE_E_RECORDED_CLAIM original",
      tags: ["phase-e"],
      source_ref: "file://history/phase-e-original.md",
    },
  }));
  const firstId = idOf(first);
  check("write_context with source_ref succeeds", !!firstId, first);

  const firstRead = text(await client.callTool({ name: "read_context", arguments: { id: firstId } }));
  check("read exposes created timestamp", /created:\*\*/.test(firstRead), firstRead);
  check("read exposes Raw handle", /source:\*\* file:\/\/history\/phase-e-original\.md/.test(firstRead), firstRead);

  const recalled = text(await client.callTool({
    name: "search_context",
    arguments: { query: "PHASE_E_RECORDED_CLAIM", search_mode: "bm25", min_score: 0, full_content: true },
  }));
  check("search reminds agent to re-observe Raw", /memoryは過去に保存された記録/.test(recalled) && /Rawを再観測/.test(recalled), recalled);

  const corrected = text(await client.callTool({
    name: "correct_context",
    arguments: {
      id: firstId,
      source_ref: "conversation://phase-e/source-correction",
      reason: "Raw handleを後からより正確な会話sourceへ訂正",
    },
  }));
  const correctedId = idOf(corrected);
  check("source_ref-only correction creates revision", !!correctedId && correctedId !== firstId, corrected);
  const correctedRead = text(await client.callTool({ name: "read_context", arguments: { id: correctedId } }));
  check("corrected revision exposes new Raw handle", /source:\*\* conversation:\/\/phase-e\/source-correction/.test(correctedRead), correctedRead);
  const oldRead = text(await client.callTool({ name: "read_context", arguments: { id: firstId } }));
  check("old Raw remains historical superseded record", /status:\*\* superseded/.test(oldRead) && /PHASE_E_RECORDED_CLAIM original/.test(oldRead), oldRead);

  const goalV1 = {
    goal_id: "phase-e-goal",
    final_goal: "複数Agentが同じ最終目標と現在地を共有する",
    completion_criteria: ["current pointer共有", "履歴はmemoryへ分離"],
    current_goal: "Goal共有契約を固定",
    blocking_unknown_ids: [],
    next: "CAS更新を確認",
  };
  await client.callTool({ name: "set_state", arguments: { key: "goal.current", value: goalV1 } });
  const goalRead = text(await client.callTool({ name: "get_state", arguments: { key: "goal.current" } }));
  check("goal.current can hold shared Goal pointer", /phase-e-goal/.test(goalRead) && /Goal共有契約を固定/.test(goalRead), goalRead);

  const goalAudit = { ...goalV1, current_goal: "CAS更新済み", next: "履歴memoryを参照" };
  const casOk = text(await client.callTool({ name: "set_state", arguments: { key: "goal.current", expected_value: goalV1, value: goalAudit } }));
  check("goal.current CAS update succeeds", /atomically updated/.test(casOk), casOk);
  const casBad = await client.callTool({ name: "set_state", arguments: { key: "goal.current", expected_value: goalV1, value: { ...goalV1, current_goal: "stale writer" } } });
  check("stale Goal writer is rejected", /CAS_CONFLICT/.test(text(casBad)), text(casBad));
} finally {
  await client.close();
}

const db = new DatabaseSync(dbPath, { readOnly: true });
try {
  check("source_ref uses existing memory_events", db.prepare("SELECT COUNT(*) c FROM memory_events WHERE event_type='source_observed'").get().c >= 2);
  check("no Phase E schema added", !db.prepare("SELECT name FROM sqlite_master WHERE type='table'").all().some((r) => /goal|evidence|claim/i.test(r.name)), "unexpected table");
  check("DB integrity", db.prepare("PRAGMA integrity_check").get().integrity_check === "ok");
} finally {
  db.close();
}

console.log(`\n結果: ${pass} passed / ${fail} failed`);
process.exit(fail ? 1 : 0);
