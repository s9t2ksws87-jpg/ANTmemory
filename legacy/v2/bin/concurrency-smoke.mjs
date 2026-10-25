#!/usr/bin/env node
// Isolated three-process ant-memory write-concurrency smoke test.
import { existsSync, mkdirSync, readFileSync, renameSync, writeFileSync } from "node:fs";
import { dirname, join, resolve } from "node:path";
import { fileURLToPath, pathToFileURL } from "node:url";
import { DatabaseSync } from "node:sqlite";

const BIN = dirname(fileURLToPath(import.meta.url));
const ROOT = resolve(BIN, "..");
const STORES = join(ROOT, "stores");
const { Client } = await import(pathToFileURL(join(ROOT, "server", "node_modules", "@modelcontextprotocol", "sdk", "dist", "esm", "client", "index.js")).href);
const { StdioClientTransport } = await import(pathToFileURL(join(ROOT, "server", "node_modules", "@modelcontextprotocol", "sdk", "dist", "esm", "client", "stdio.js")).href);
const sleep = (ms) => new Promise((done) => setTimeout(done, ms));
// Fresh chats and tool runners can take a few minutes to join the same run.
const WAIT_TIMEOUT_MS = 600_000;

function args() {
  const [role, ...rest] = process.argv.slice(2);
  const opt = {};
  for (let i = 0; i < rest.length; i += 2) {
    if (!rest[i]?.startsWith("--") || !rest[i + 1]) throw new Error("引数の形式が不正です");
    opt[rest[i].slice(2)] = rest[i + 1];
  }
  return { role, opt };
}
function valid(value, label) {
  if (!/^[A-Za-z0-9][A-Za-z0-9_-]{2,80}$/.test(value ?? "")) throw new Error(`${label} は英数字・_・-のみで3文字以上にしてください`);
  return value;
}
function count(value) {
  const n = Number(value ?? 20);
  if (!Number.isInteger(n) || n < 1 || n > 200) throw new Error("--count は1〜200の整数です");
  return n;
}
function locations(run) {
  const store = join(STORES, run);
  const control = join(store, ".concurrency-smoke");
  return {
    store, control, manifest: join(control, "manifest.json"), go: join(control, "GO"), report: join(control, "report.json"),
    ready: (agent) => join(control, `ready-${agent}.json`), result: (agent) => join(control, `result-${agent}.json`),
  };
}
function put(path, value) {
  const temp = `${path}.${process.pid}.tmp`;
  writeFileSync(temp, `${JSON.stringify(value, null, 2)}\n`, "utf8");
  renameSync(temp, path);
}
function get(path) { return JSON.parse(readFileSync(path, "utf8")); }
async function wait(label, predicate) {
  const deadline = Date.now() + WAIT_TIMEOUT_MS;
  while (!predicate()) {
    if (Date.now() > deadline) throw new Error(`${label} を${WAIT_TIMEOUT_MS / 1000}秒待ちましたが届きません`);
    await sleep(20);
  }
}
async function open(run, agent) {
  const client = new Client({ name: `ant-memory-concurrency-${agent}`, version: "1.0.0" });
  const transport = new StdioClientTransport({
    command: process.execPath,
    args: [join(BIN, "ant-memory.mjs")],
    cwd: ROOT,
    env: { ...process.env, ANT_MEMORY_ROOT: ROOT, ANT_MEMORY_WORKSPACE: run, ANT_MEMORY_CLIENT: agent },
  });
  await client.connect(transport);
  return client;
}
function body(result) { return (result.content ?? []).map((x) => x.text ?? "").join("\n"); }

async function worker(run, agent, n) {
  const p = locations(run);
  if (!existsSync(p.manifest)) throw new Error("coordinator が未起動です");
  const manifest = get(p.manifest);
  if (!manifest.agents.includes(agent) || manifest.count !== n) throw new Error("agent または count が coordinator と一致しません");
  const client = await open(run, agent);
  try {
    await client.callTool({ name: "list_context", arguments: { limit: 1 } });
    put(p.ready(agent), { agent, pid: process.pid, readyAt: new Date().toISOString() });
    console.log(`READY ${agent}`);
    await wait("GO合図", () => existsSync(p.go));
    const writes = [], errors = [];
    for (let i = 0; i < n; i += 1) {
      const topic = `concurrency:${run}:${agent}:${String(i).padStart(3, "0")}`;
      try {
        const result = await client.callTool({ name: "write_context", arguments: {
          topic, content: `同時書き込み試験 ${run}; agent=${agent}; index=${i}.`, agent,
          tags: ["concurrency-smoke", run, agent], importance: "low",
        }});
        const id = body(result).match(/id=\`([0-9a-f]{10})\`/i)?.[1];
        if (!id) throw new Error(`IDを取得できません: ${body(result)}`);
        writes.push({ id, topic });
      } catch (error) { errors.push({ index: i, message: String(error) }); }
    }
    put(p.result(agent), { agent, writes, errors, finishedAt: new Date().toISOString() });
    console.log(`DONE ${agent}: success=${writes.length} errors=${errors.length}`);
    if (errors.length) process.exitCode = 1;
  } finally { await client.close(); }
}

async function coordinator(run, agents, n) {
  const p = locations(run);
  if (existsSync(p.store)) throw new Error(`既存の隔離ストアがあります: ${p.store}; 新しいrun名を使ってください`);
  mkdirSync(p.control, { recursive: true });
  put(p.manifest, { run, agents, count: n, createdAt: new Date().toISOString() });
  // Initialize before workers launch so this tests concurrent writes to an
  // existing store, rather than concurrent first-time schema creation.
  const primer = await open(run, "coordinator");
  try {
    await primer.callTool({ name: "list_context", arguments: { limit: 1 } });
  } finally {
    await primer.close();
  }
  console.log(`READY待機: ${agents.join(", ")}`);
  console.log(`隔離ストア: ${p.store}`);
  await wait("全workerのREADY", () => agents.every((agent) => existsSync(p.ready(agent))));
  writeFileSync(p.go, `${new Date().toISOString()}\n`, "utf8");
  console.log("GO: 3 workerが同時に書き込み開始");
  await wait("全workerの結果", () => agents.every((agent) => existsSync(p.result(agent))));
  const results = agents.map((agent) => get(p.result(agent)));
  const ids = results.flatMap((r) => r.writes.map((w) => w.id));
  const workerErrors = results.flatMap((r) => r.errors.map((e) => ({ agent: r.agent, ...e })));
  const verifier = await open(run, "verifier");
  const missingReadIds = [];
  try {
    for (const id of ids) {
      const result = await verifier.callTool({ name: "read_context", arguments: { id } });
      if (/No chunk found/i.test(body(result))) missingReadIds.push(id);
    }
  } finally { await verifier.close(); }
  const db = new DatabaseSync(join(p.store, "store.db"), { readOnly: true });
  let integrity, foreignKeyViolations, storedCount;
  try {
    integrity = db.prepare("PRAGMA integrity_check").get().integrity_check;
    foreignKeyViolations = db.prepare("PRAGMA foreign_key_check").all().length;
    storedCount = db.prepare("SELECT COUNT(*) AS count FROM chunks WHERE topic LIKE ?").get(`concurrency:${run}:%`).count;
  } finally { db.close(); }
  const expected = agents.length * n;
  const report = {
    run, store: p.store, expected, writesReported: ids.length, distinctIds: new Set(ids).size, storedCount,
    workerErrors, missingReadIds, integrity, foreignKeyViolations,
    passed: workerErrors.length === 0 && ids.length === expected && new Set(ids).size === expected && storedCount === expected && missingReadIds.length === 0 && integrity === "ok" && foreignKeyViolations === 0,
    finishedAt: new Date().toISOString(),
  };
  put(p.report, report);
  console.log(JSON.stringify(report, null, 2));
  if (!report.passed) process.exitCode = 1;
}

try {
  const { role, opt } = args();
  if (!role || role === "--help" || role === "help") {
    console.log("coordinator: node bin/concurrency-smoke.mjs coordinator --run <run> --agents claude-code,antigravity,codex --count 20");
    console.log("worker:      node bin/concurrency-smoke.mjs worker --run <run> --agent <agent> --count 20");
  } else {
    const run = valid(opt.run, "--run"), n = count(opt.count);
    if (role === "worker") await worker(run, valid(opt.agent, "--agent"), n);
    else if (role === "coordinator") {
      const agents = String(opt.agents ?? "").split(",").map((x) => valid(x.trim(), "--agents"));
      if (agents.length < 2 || new Set(agents).size !== agents.length) throw new Error("--agents は重複しない2人以上です");
      await coordinator(run, agents, n);
    } else throw new Error("role は coordinator または worker です");
  }
} catch (error) {
  console.error(`ERROR: ${error instanceof Error ? error.message : String(error)}`);
  process.exitCode = 2;
}
