#!/usr/bin/env node
/**
 * Phase 1: 基準試験（非破壊）
 *
 * 隔離ストア上で現在の正常機能を固定する:
 *   - native core所有とupstream package非依存
 *   - 保存則を担うnative schema/search契約
 *   - ワークスペース解決の主要ケース
 *   - 検索・書込・読込・state・ワークスペース切替
 *
 * 実行: node bin/test-baseline.mjs
 */

import { spawnSync } from "node:child_process";
import { mkdirSync, rmSync, existsSync, readFileSync } from "node:fs";
import { tmpdir } from "node:os";
import { join, resolve, dirname } from "node:path";
import { fileURLToPath, pathToFileURL } from "node:url";

const BIN = dirname(fileURLToPath(import.meta.url));
const ROOT = resolve(BIN, "..");
const { Client } = await import(
  pathToFileURL(
    join(ROOT, "server", "node_modules", "@modelcontextprotocol", "sdk", "dist", "esm", "client", "index.js"),
  ).href
);
const { StdioClientTransport } = await import(
  pathToFileURL(
    join(ROOT, "server", "node_modules", "@modelcontextprotocol", "sdk", "dist", "esm", "client", "stdio.js"),
  ).href
);

let passed = 0;
let failed = 0;
const failures = [];

function check(name, ok, detail = "") {
  if (ok) {
    passed += 1;
    console.log(`PASS ${name}`);
  } else {
    failed += 1;
    failures.push(name);
    console.error(`FAIL ${name}${detail ? ` — ${detail}` : ""}`);
  }
}

// ─── 1. Native core ownership ────────────────────────────────────────────────
{
  const pkg = JSON.parse(readFileSync(join(ROOT, "server", "package.json"), "utf8"));
  check("agent-memory-store packageへ依存しない", !pkg.dependencies?.["agent-memory-store"]);
  check("apply-patches scriptへ依存しない", !pkg.scripts?.postinstall && !pkg.scripts?.patch);
  check("native core filesが存在する", ["native-db.mjs","native-store.mjs","native-search.mjs","native-embeddings.mjs"].every((f)=>existsSync(join(ROOT,"server","lib",f))));
  const dbSrc = readFileSync(join(ROOT,"server","lib","native-db.mjs"),"utf8");
  const searchSrc = readFileSync(join(ROOT,"server","lib","native-search.mjs"),"utf8");
  check("native DBは日本語trigram FTSを所有", /tokenize='trigram'/.test(dbSrc));
  check("native DBにTTL物理DELETEがない", !/DELETE FROM chunks WHERE expires_at/.test(dbSrc));
  check("native searchはstructured raw APIを所有", /export async function hybridSearchRaw/.test(searchSrc));
}

// ─── 2. ワークスペース解決 ────────────────────────────────────────────────────
{
  function where(env) {
    const r = spawnSync(process.execPath, [join(BIN, "ant-memory.mjs"), "--where"], {
      encoding: "utf8",
      cwd: ROOT,
      env: { ...process.env, ANT_MEMORY_ROOT: ROOT, ANT_MEMORY_REGISTRY: "", ...env },
    });
    return JSON.parse(r.stdout);
  }

  const w1 = where({ ANT_MEMORY_WORKSPACE: "phase1-test-ws" });
  check("ANT_MEMORY_WORKSPACE(名前指定)がそのまま採用される",
    w1.slug === "phase1-test-ws" && w1.source === "env:ANT_MEMORY_WORKSPACE(name)");

  const w2 = where({ ANT_MEMORY_WORKSPACE: ROOT });
  check("ANT_MEMORY_WORKSPACE(絶対パス指定)が解決される",
    w2.slug.length > 0 && w2.source === "env:ANT_MEMORY_WORKSPACE(path)", JSON.stringify(w2));

  const w3 = where({});
  // ROOT 自体は正当なワークスペースとして許可するようになったため（2026-08-24 の
  // Antigravity 誤接続修正）、cwd=ROOT では _shared ではなく ANTmemory ストアに解決する。
  check("判定不能時は共有ストアへフォールバック / cwd=ROOT は ANTmemory に解決",
    (w3.slug === "_shared" && w3.source.startsWith("fallback")) ||
      (w3.workspaceRoot === ROOT && w3.source.startsWith("cwd:")),
    JSON.stringify(w3));
}

// ─── 4. 検索・書込・読込・state（隔離ストア） ────────────────────────────────
{
  const ws = "phase1-baseline-test";
  const client = new Client({ name: "phase1-test", version: "1.0.0" });
  const transport = new StdioClientTransport({
    command: process.execPath,
    args: [join(BIN, "ant-memory.mjs")],
    cwd: ROOT,
    env: {
      ...process.env,
      ANT_MEMORY_ROOT: ROOT,
      ANT_MEMORY_WORKSPACE: ws,
      ANT_MEMORY_CLIENT: "hermes",
    },
  });
  try {
    await client.connect(transport);

    const w = await client.callTool({
      name: "write_context",
      arguments: {
        topic: "phase1: ハイフン入りの語 ant-memory の検索試験",
        content: "これはPhase1基準試験の本文です。ant-memory や ミュウ のような語を含む。",
        agent: "hermes",
        tags: ["phase1", "baseline"],
        importance: "low",
      },
    });
    const idMatch = (w.content ?? []).map((c) => c.text).join("\n").match(/id=`([0-9a-f]{10})`/i);
    check("write_context が成功しIDを返す", Boolean(idMatch));

    const id = idMatch?.[1];
    const r = await client.callTool({ name: "read_context", arguments: { id } });
    check("read_context で本文が読める", /Phase1基準試験/.test((r.content ?? []).map((c) => c.text).join("\n")));

    const s1 = await client.callTool({ name: "search_context", arguments: { query: "ant-memory", search_mode: "bm25", min_score: 0 } });
    check("ハイフン入り語の検索がエラーにならない（パッチ[1]の回帰）",
      !/no such column/i.test((s1.content ?? []).map((c) => c.text).join("\n")) &&
      /phase1: ハイフン入り/.test((s1.content ?? []).map((c) => c.text).join("\n")));

    const s2 = await client.callTool({ name: "search_context", arguments: { query: "ミュウ", search_mode: "bm25", min_score: 0 } });
    check("日本語 trigram 検索が効く（パッチ[2]の回帰）",
      /phase1: ハイフン入り/.test((s2.content ?? []).map((c) => c.text).join("\n")));

    const st = await client.callTool({ name: "set_state", arguments: { key: "phase1", value: { step: 1 } } });
    const gs = await client.callTool({ name: "get_state", arguments: { key: "phase1" } });
    check("set_state → get_state の往復",
      /"step":\s*1/.test((gs.content ?? []).map((c) => c.text).join("\n")) && Boolean(st));

    const l = await client.callTool({ name: "list_context", arguments: { agent: "hermes" } });
    check("list_context が書込を返す", /phase1: ハイフン入り/.test((l.content ?? []).map((c) => c.text).join("\n")));
  } finally {
    await client.close();
  }
}

console.log(`\n結果: ${passed} passed / ${failed} failed`);
process.exit(failed ? 1 : 0);
