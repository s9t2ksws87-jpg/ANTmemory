#!/usr/bin/env node
/**
 * Phase D: Template-kun minimal observation cycle.
 *
 * Invariants:
 * - No UNKNOWN/Hypothesis/Graph tables are introduced.
 * - Explicit UNKNOWN is an immutable normal chunk tagged tk:unknown.
 * - Hypothesis never overwrites UNKNOWN; it is a derived child.
 * - Reobservation/resolution is another chunk; UNKNOWN raw body stays intact.
 * - Resolved UNKNOWN remains inspectable but is omitted from default open list.
 * - source_ref is append-only provenance in memory_events.
 */

import { rmSync } from "node:fs";
import { join, resolve, dirname } from "node:path";
import { fileURLToPath, pathToFileURL } from "node:url";
import { DatabaseSync } from "node:sqlite";

const ROOT = resolve(dirname(fileURLToPath(import.meta.url)), "..");
const WS = "phase-d-test";
const dbPath = join(ROOT, "stores", WS, "store.db");
for (const p of [dbPath, dbPath + "-wal", dbPath + "-shm"]) try { rmSync(p, { force: true }); } catch {}

const { Client } = await import(pathToFileURL(join(ROOT,"server","node_modules","@modelcontextprotocol","sdk","dist","esm","client","index.js")).href);
const { StdioClientTransport } = await import(pathToFileURL(join(ROOT,"server","node_modules","@modelcontextprotocol","sdk","dist","esm","client","stdio.js")).href);

let pass = 0, fail = 0;
const check = (name, ok, detail = "") => {
  if (ok) { pass++; console.log(`PASS ${name}`); }
  else { fail++; console.error(`FAIL ${name}${detail ? ` — ${detail}` : ""}`); }
};
const text = (r) => (r.content ?? []).map((x) => x.text).join("\n");
const idOf = (r) => text(r).match(/id=`([0-9a-f]{10})`/i)?.[1] ?? null;

const client = new Client({ name: "phase-d", version: "1" });
await client.connect(new StdioClientTransport({
  command: process.execPath,
  args: [join(ROOT,"bin","ant-memory.mjs")],
  cwd: ROOT,
  env: { ...process.env, ANT_MEMORY_ROOT: ROOT, ANT_MEMORY_WORKSPACE: WS, ANT_MEMORY_CLIENT: "phase-d-audit" },
}));

try {
  const tools = (await client.listTools()).tools.map((t) => t.name);
  check("observe_context公開", tools.includes("observe_context"));
  check("list_unknowns公開", tools.includes("list_unknowns"));
  check("簡素化後default surfaceは16 tools", tools.length === 16, tools.sort().join(","));

  const u = await client.callTool({ name: "observe_context", arguments: {
    kind: "unknown",
    topic: "UNKNOWN: FTS順位差の原因",
    content: "観測したが、順位差がBM25由来かvector由来か現時点では判別できない。",
    source_ref: "experiment://phase-d/raw/run-001",
    tags: ["phase-d"],
    reason: "観測後に分離できなかった点をUNKNOWNとして保持",
  }});
  const unknownId = idOf(u);
  check("UNKNOWN作成", !!unknownId, text(u));

  const open1 = await client.callTool({ name: "list_unknowns", arguments: {} });
  check("未解決UNKNOWNが発見できる", new RegExp(unknownId).test(text(open1)) && /UNRESOLVED/.test(text(open1)), text(open1));
  check("source_refが一覧からRawへ戻る取手になる", /experiment:\/\/phase-d\/raw\/run-001/.test(text(open1)), text(open1));

  const h = await client.callTool({ name: "observe_context", arguments: {
    kind: "hypothesis",
    topic: "仮説: vector側が順位差を作る",
    content: "埋め込み類似度の差が主因だと仮定する。まだ確定しない。",
    parent_id: unknownId,
    source_ref: "note://phase-d/hypothesis-001",
    reason: "UNKNOWNを消さず仮説として分離",
  }});
  const hypothesisId = idOf(h);
  check("仮説をUNKNOWNの別chunkとして作成", !!hypothesisId && hypothesisId !== unknownId, text(h));

  const beforeResolve = await client.callTool({ name: "read_context", arguments: { id: unknownId } });
  check("仮説作成後もUNKNOWN Rawが不変", /順位差がBM25由来かvector由来か/.test(text(beforeResolve)), text(beforeResolve));

  const badResolve = await client.callTool({ name: "observe_context", arguments: {
    kind: "hypothesis",
    topic: "不正解決",
    content: "仮説だけでUNKNOWNを解決してはいけない。",
    parent_id: unknownId,
    resolves_unknown: true,
  }});
  check("仮説だけでUNKNOWN解決を拒否", /resolves_unknown=true は observation でのみ/.test(text(badResolve)), text(badResolve));

  const r = await client.callTool({ name: "observe_context", arguments: {
    kind: "observation",
    topic: "再観測: BM25固定で順位差が消えた",
    content: "BM25-only対照では順位差が消えた。今回のUNKNOWNについてはvector寄与が原因と判断できる。",
    parent_id: unknownId,
    source_ref: "experiment://phase-d/raw/run-002",
    resolves_unknown: true,
    reason: "対照実験による再観測",
  }});
  const resolutionId = idOf(r);
  check("再観測を別chunkとして作成", !!resolutionId && resolutionId !== unknownId, text(r));
  check("解決しても旧UNKNOWN保持を明示", /旧UNKNOWN本文は保持/.test(text(r)), text(r));

  const open2 = await client.callTool({ name: "list_unknowns", arguments: {} });
  check("resolved UNKNOWNは通常の未解決一覧から外れる", !new RegExp(unknownId).test(text(open2)), text(open2));
  const all = await client.callTool({ name: "list_unknowns", arguments: { include_resolved: true } });
  check("resolved UNKNOWNも明示指定で再発見可能", new RegExp(unknownId).test(text(all)) && new RegExp(resolutionId).test(text(all)), text(all));

  const hist = await client.callTool({ name: "get_context_history", arguments: { id: unknownId } });
  const histText = text(hist);
  check("UNKNOWN履歴にhypothesis/reobservation系譜が残る", new RegExp(hypothesisId).test(histText) && new RegExp(resolutionId).test(histText) && /derived_from/.test(histText) && /resolves/.test(histText), histText);
  check("UNKNOWN解決eventが残る", /unknown_resolved/.test(histText), histText);

  const afterResolve = await client.callTool({ name: "read_context", arguments: { id: unknownId } });
  check("解決後もUNKNOWN Raw本文は読める", /順位差がBM25由来かvector由来か/.test(text(afterResolve)), text(afterResolve));

  // reopenは旧UNKNOWNのresolved履歴を消さず、新しいUNKNOWNを派生させる。
  const reopened = await client.callTool({ name: "observe_context", arguments: {
    kind: "unknown",
    topic: "UNKNOWN再発: 別データでは順位差が残る",
    content: "別コーパスではBM25固定後も差が残ったため、新しいUNKNOWNとして再観測が必要。",
    parent_id: unknownId,
    source_ref: "experiment://phase-d/raw/run-003",
    reason: "解決済みUNKNOWNを巻き戻さず新しい疑問として派生",
  }});
  const reopenedId = idOf(reopened);
  const open3 = await client.callTool({ name: "list_unknowns", arguments: {} });
  check("reopenは旧解決を消さず新UNKNOWNとして表現", !!reopenedId && reopenedId !== unknownId && new RegExp(reopenedId).test(text(open3)) && !new RegExp(unknownId).test(text(open3)), text(open3));

  const known = await client.callTool({ name: "observe_context", arguments: {
    kind: "observation",
    topic: "観測済み事実",
    content: "既知になった観測は通常のobservationとして残す。",
    source_ref: "file://docs/known.txt",
  }});
  check("既知観測はUNKNOWN化せず記録できる", !!idOf(known), text(known));
} finally {
  await client.close();
}

const db = new DatabaseSync(dbPath, { readOnly: true });
try {
  const tables = db.prepare("SELECT name FROM sqlite_master WHERE type='table'").all().map((r) => r.name);
  check("UNKNOWN専用tableを作らない", !tables.some((n) => /unknown|hypothesis|observation/i.test(n)), tables.join(","));
  check("既存memory_relationsを再利用", db.prepare("SELECT COUNT(*) c FROM memory_relations WHERE relation_type IN ('derived_from','resolves')").get().c >= 3);
  check("source_refは既存memory_eventsへ追記", db.prepare("SELECT COUNT(*) c FROM memory_events WHERE event_type='source_observed'").get().c >= 3);
  check("DB integrity", db.prepare("PRAGMA integrity_check").get().integrity_check === "ok");
  check("FK violation 0", db.prepare("PRAGMA foreign_key_check").all().length === 0);
} finally {
  db.close();
}

console.log(`\n結果: ${pass} passed / ${fail} failed`);
process.exit(fail ? 1 : 0);
