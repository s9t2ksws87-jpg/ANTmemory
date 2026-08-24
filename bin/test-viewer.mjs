#!/usr/bin/env node
/**
 * ビューアの検証
 *
 * - 127.0.0.1 のみでリッスン（LANの他端末からアクセス不可）
 * - CORS ヘッダーが外部オリジンを許可しない
 * - /api/memories が実FTS5検索（ハイフン・日本語がヒット）
 * - /api/agents が動的エージェント一覧
 * - /api/history がイベント履歴を返す
 * - HTML/JSを含む記憶を表示してもUIが壊れない（サーバー側はエスケープ不要のJSON配信）
 *
 * 実行: node bin/test-viewer.mjs
 */

import { spawn } from "node:child_process";
import { join, resolve, dirname } from "node:path";
import { fileURLToPath, pathToFileURL } from "node:url";

const ROOT = resolve(dirname(fileURLToPath(import.meta.url)), "..");
const BIN = join(ROOT, "bin");
const VIEWER = join(ROOT, "viewer", "server.mjs");

let passed = 0, failed = 0;
const check = (name, ok, detail = "") => {
  if (ok) { passed++; console.log(`PASS ${name}`); }
  else { failed++; console.error(`FAIL ${name}${detail ? ` — ${detail}` : ""}`); }
};

// 準備: 隔離ストアへXSSテスト記憶とハイフン語記憶を書く
const { Client } = await import(
  pathToFileURL(join(ROOT, "server", "node_modules", "@modelcontextprotocol", "sdk", "dist", "esm", "client", "index.js")).href
);
const { StdioClientTransport } = await import(
  pathToFileURL(join(ROOT, "server", "node_modules", "@modelcontextprotocol", "sdk", "dist", "esm", "client", "stdio.js")).href
);
const WS = "viewer-test";
{
  const client = new Client({ name: "viewer-setup", version: "1.0.0" });
  await client.connect(new StdioClientTransport({
    command: process.execPath, args: [join(BIN, "ant-memory.mjs")], cwd: ROOT,
    env: { ...process.env, ANT_MEMORY_ROOT: ROOT, ANT_MEMORY_WORKSPACE: WS, ANT_MEMORY_CLIENT: "hermes" },
  }));
  await client.callTool({
    name: "write_context",
    arguments: {
      topic: "viewer: XSS試験",
      content: '<script>window.__xss=true</script><img src=x onerror="window.__xss2=true">',
      tags: ["viewer"],
    },
  });
  await client.callTool({
    name: "write_context",
    arguments: {
      topic: "viewer: ハイフン語の記憶",
      content: "ant-memory プロジェクトの説明。ミュウの試験も含む。",
      tags: ["viewer"],
    },
  });
  await client.close();
}

// ビューア起動
const proc = spawn(process.execPath, [VIEWER], {
  cwd: ROOT,
  env: { ...process.env, ANT_MEMORY_ROOT: ROOT, PORT: "3777", NO_OPEN: "1" },
  stdio: ["ignore", "pipe", "pipe"],
});
let out = "";
proc.stdout.on("data", (d) => (out += d));
proc.stderr.on("data", (d) => (out += d));

const sleep = (ms) => new Promise((r) => setTimeout(r, ms));
let port = 3777;
for (let i = 0; i < 40; i++) {
  await sleep(250);
  if (out.includes(`http://localhost:${port}`)) break;
  const m = out.match(/http:\/\/localhost:(\d+)/);
  if (m) { port = Number(m[1]); break; }
}

try {
  // 1. localhost バインド確認
  const local = await fetch(`http://127.0.0.1:${port}/api/workspaces`);
  check("localhost でアクセス可能", local.ok);

  // 2. LAN インターフェースのIPへは繋がらないはず（127.0.0.1 バインド）
  const os = await import("node:os");
  const nets = os.networkInterfaces();
  const lanIp = Object.values(nets).flat().find((n) => n && n.family === "IPv4" && !n.internal)?.address;
  if (lanIp) {
    let lanFail = false;
    try {
      await fetch(`http://${lanIp}:${port}/api/workspaces`, { signal: AbortSignal.timeout(1500) });
    } catch { lanFail = true; }
    check("LAN の他端末から直接アクセスできない", lanFail, `lanIp=${lanIp}`);
  } else {
    check("LAN の他端末から直接アクセスできない", true, "(LAN IPが見つからないためスキップ)");
  }

  // 3. CORS ヘッダー
  await (await fetch(`http://127.0.0.1:${port}/api/workspaces`)).json();
  const corsHeader = (await fetch(`http://127.0.0.1:${port}/api/workspaces`)).headers.get("access-control-allow-origin");
  check("CORS が * を許可しない", corsHeader !== "*", `header=${corsHeader}`);

  // 4. FTS5 検索: ハイフン語
  const memUrl = `http://127.0.0.1:${port}/api/memories?workspace=${encodeURIComponent(WS)}&q=${encodeURIComponent("ant-memory")}&limit=50`;
  const memData = (await (await fetch(memUrl)).json());
  check("FTS5検索でハイフン入り語がヒット", memData.memories?.some((m) => /ハイフン語/.test(m.topic)), `total=${memData.total}`);

  // 5. FTS5 検索: 日本語
  const jpData = (await (await fetch(`http://127.0.0.1:${port}/api/memories?workspace=${encodeURIComponent(WS)}&q=${encodeURIComponent("ミュウ")}&limit=50`)).json());
  check("FTS5検索で日本語がヒット", jpData.memories?.some((m) => /ハイフン語/.test(m.topic)), `total=${jpData.total}`);

  // 6. XSS 記憶がそのままJSONで返る（UIはtextContentで表示するため実行されない）
  const xssData = (await (await fetch(`http://127.0.0.1:${port}/api/memories?workspace=${encodeURIComponent(WS)}&q=${encodeURIComponent("XSS")}&limit=50`)).json());
  check("HTML/JSを含む記憶が取得できる", xssData.memories?.some((m) => m.content.includes("<script>")));

  // 7. 動的エージェント一覧
  const agentsData = (await (await fetch(`http://127.0.0.1:${port}/api/agents?workspace=${encodeURIComponent(WS)}`)).json());
  check("エージェント一覧がDBから動的取得される", agentsData.agents?.some((a) => a.agent === "hermes"), JSON.stringify(agentsData));

  // 8. 履歴API
  const xssId = xssData.memories?.[0]?.id;
  const histData = (await (await fetch(`http://127.0.0.1:${port}/api/history?workspace=${encodeURIComponent(WS)}&id=${xssId}`)).json());
  check("履歴APIが状態とイベントを返す", histData.status === "active" && Array.isArray(histData.events));

  // 9. index.html が外部CDNに依存しない
  const html = await (await fetch(`http://127.0.0.1:${port}/index.html`)).text();
  check("index.html に外部CDN依存がない", !/https?:\/\/(fonts|cdn)\./.test(html));
} finally {
  // Windows/libuv では child.kill() の直後に process.exit() すると、stdio の非同期
  // handle が closing 中のまま強制終了され assertion になることがある。
  // 子プロセスの exit を待ってからテストプロセスを自然終了させる。
  if (proc.exitCode === null && proc.signalCode === null) {
    const exited = new Promise((resolveExit) => proc.once("exit", resolveExit));
    proc.kill();
    await Promise.race([exited, sleep(3000)]);
  }
  proc.stdout.destroy();
  proc.stderr.destroy();
}

console.log(`\n結果: ${passed} passed / ${failed} failed`);
process.exitCode = failed ? 1 : 0;
