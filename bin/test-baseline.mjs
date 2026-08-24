#!/usr/bin/env node
/**
 * 基準試験（非破壊）
 *
 * 隔離ストア上で現在の正常機能を固定する:
 *   - パッチの冪等性（2回適用しても結果が変わらない）
 *   - 対応外バージョンでの明示エラー
 *   - ワークスペース解決の主要ケース
 *   - 検索・書込・読込・state・ワークスペース切替
 *
 * 実行: node bin/test-baseline.mjs
 */

import { spawnSync } from "node:child_process";
import { mkdirSync, rmSync, existsSync, readFileSync } from "node:fs";
import { tmpdir } from "node:os";
import { basename, join, resolve, dirname } from "node:path";
import { fileURLToPath, pathToFileURL } from "node:url";

const BIN = dirname(fileURLToPath(import.meta.url));
const ROOT = resolve(BIN, "..");
const SERVER_ENTRY = join(
  ROOT,
  "server",
  "node_modules",
  "agent-memory-store",
  "src",
  "index.js",
);
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

// ─── 1. パッチの冪等性 ────────────────────────────────────────────────────────
{
  const r1 = spawnSync(process.execPath, [join(BIN, "apply-patches.mjs")], { encoding: "utf8" });
  check("パッチ適用(1回目)が成功", r1.status === 0, r1.stderr?.slice(0, 200));

  const before = new Map();
  for (const f of ["db.js", "store.js", "search.js", "migrate.js"]) {
    before.set(f, readFileSync(join(SERVER_ENTRY, "..", f), "utf8"));
  }

  const r2 = spawnSync(process.execPath, [join(BIN, "apply-patches.mjs")], { encoding: "utf8" });
  check("パッチ適用(2回目)が成功", r2.status === 0, r2.stderr?.slice(0, 200));
  check("パッチ適用(2回目)は何も適用しない（冪等）", /0 件適用/.test(r2.stdout ?? ""), r2.stdout?.slice(-120));

  let identical = true;
  for (const f of ["db.js", "store.js", "search.js", "migrate.js"]) {
    if (readFileSync(join(SERVER_ENTRY, "..", f), "utf8") !== before.get(f)) identical = false;
  }
  check("2回適用でファイル内容が不変", identical);
}

// ─── 2. 対応外バージョンの明示エラー ──────────────────────────────────────────
{
  // 実DBへ触れず、SUPPORTED_VERSIONS を一時的に空にして検証する。
  const src = readFileSync(join(BIN, "apply-patches.mjs"), "utf8");
  const patched = src.replace(
    'const SUPPORTED_VERSIONS = ["0.0.9"];',
    'const SUPPORTED_VERSIONS = [];',
  );
  const r = spawnSync(process.execPath, ["--input-type=module", "-e", patched], {
    encoding: "utf8",
    env: { ...process.env, ANT_MEMORY_ROOT: ROOT },
  });
  check("対応外バージョンでは黙って進まずエラー終了", r.status === 3 && /対応外/.test(r.stderr ?? ""));
}

// ─── 3. ワークスペース解決 ────────────────────────────────────────────────────
{
  function where(env) {
    const r = spawnSync(process.execPath, [join(BIN, "ant-memory.mjs"), "--where"], {
      encoding: "utf8",
      cwd: ROOT,
      env: { ...process.env, ANT_MEMORY_ROOT: ROOT, ANT_MEMORY_REGISTRY: "", ...env },
    });
    return JSON.parse(r.stdout);
  }

  const w1 = where({ ANT_MEMORY_WORKSPACE: "baseline-test-ws" });
  check("ANT_MEMORY_WORKSPACE(名前指定)がそのまま採用される",
    w1.slug === "baseline-test-ws" && w1.source === "env:ANT_MEMORY_WORKSPACE(name)");

  const w2 = where({ ANT_MEMORY_WORKSPACE: ROOT });
  check("ANT_MEMORY_WORKSPACE(絶対パス指定)が解決される",
    w2.slug.length > 0 && w2.source === "env:ANT_MEMORY_WORKSPACE(path)", JSON.stringify(w2));

  const w3 = where({});
  // ROOT 自体は正当なワークスペースなので、cwd=ROOT では _shared ではなく
  // 現在のリポジトリフォルダー名のストアに解決する。
  check("判定不能時は共有ストアへフォールバック / cwd=ROOT はリポジトリ名に解決",
    (w3.slug === "_shared" && w3.source.startsWith("fallback")) ||
      (w3.slug === basename(ROOT) && w3.source.startsWith("cwd:")),
    JSON.stringify(w3));
}

// ─── 4. 検索・書込・読込・state（隔離ストア） ────────────────────────────────
{
  const ws = "baseline-regression-test";
  const client = new Client({ name: "baseline-test", version: "1.0.0" });
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
        topic: "baseline: ハイフン入りの語 ant-memory の検索試験",
        content: "これは基準試験の本文です。ant-memory や ミュウ のような語を含む。",
        agent: "hermes",
        tags: ["baseline"],
        importance: "low",
      },
    });
    const idMatch = (w.content ?? []).map((c) => c.text).join("\n").match(/id=`([0-9a-f]{10})`/i);
    check("write_context が成功しIDを返す", Boolean(idMatch));

    const id = idMatch?.[1];
    const r = await client.callTool({ name: "read_context", arguments: { id } });
    check("read_context で本文が読める", /基準試験/.test((r.content ?? []).map((c) => c.text).join("\n")));

    const s1 = await client.callTool({ name: "search_context", arguments: { query: "ant-memory", search_mode: "bm25", min_score: 0 } });
    check("ハイフン入り語の検索がエラーにならない（パッチ[1]の回帰）",
      !/no such column/i.test((s1.content ?? []).map((c) => c.text).join("\n")) &&
      /baseline: ハイフン入り/.test((s1.content ?? []).map((c) => c.text).join("\n")));

    const s2 = await client.callTool({ name: "search_context", arguments: { query: "ミュウ", search_mode: "bm25", min_score: 0 } });
    check("日本語 trigram 検索が効く（パッチ[2]の回帰）",
      /baseline: ハイフン入り/.test((s2.content ?? []).map((c) => c.text).join("\n")));

    const st = await client.callTool({ name: "set_state", arguments: { key: "baseline", value: { step: 1 } } });
    const gs = await client.callTool({ name: "get_state", arguments: { key: "baseline" } });
    check("set_state → get_state の往復",
      /"step":\s*1/.test((gs.content ?? []).map((c) => c.text).join("\n")) && Boolean(st));

    const l = await client.callTool({ name: "list_context", arguments: { agent: "hermes" } });
    check("list_context が書込を返す", /baseline: ハイフン入り/.test((l.content ?? []).map((c) => c.text).join("\n")));
  } finally {
    await client.close();
  }
}

console.log(`\n結果: ${passed} passed / ${failed} failed`);
process.exit(failed ? 1 : 0);
