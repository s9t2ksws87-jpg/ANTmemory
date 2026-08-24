#!/usr/bin/env node
/**
 * ant-memory — agent-memory-store のワークスペース別ランチャー
 *
 * サーバー本体（agent-memory-store）は <ANTMEMORY_ROOT>\server に
 * 1 つだけインストールして全エージェントで使い回す。
 * DB ファイルだけを「起動時の作業ディレクトリ（＝ワークスペース）」ごとに
 * 物理的に分離する。
 *
 *   <ANTMEMORY_ROOT>\stores\<ワークスペース名>\store.db
 *
 * 解決の優先順位:
 *   1. 環境変数 ANT_MEMORY_WORKSPACE（絶対パス or 任意の名前）
 *   2. cwd から上へ辿ってワークスペースの目印（.git / CLAUDE.md など）を探す
 *   3. 見つからない・共有領域だった場合は _shared にフォールバック
 *
 * デバッグ:
 *   node ant-memory.mjs --where   … サーバーを起動せず解決結果だけ表示
 */

import { createHash } from "node:crypto";
import {
  mkdirSync,
  appendFileSync,
  readFileSync,
  writeFileSync,
  existsSync,
  readdirSync,
  statSync,
} from "node:fs";
import path from "node:path";
import os from "node:os";
import { pathToFileURL, fileURLToPath } from "node:url";
import { execFileSync } from "node:child_process";

// ─── 配置 ────────────────────────────────────────────────────────────────────

const ROOT = process.env.ANT_MEMORY_ROOT
  ? path.resolve(process.env.ANT_MEMORY_ROOT)
  : path.resolve(path.join(import.meta.dirname, ".."));

const STORES_DIR = path.join(ROOT, "stores");
const LOG_FILE = path.join(ROOT, "logs", "launcher.log");
// 実行時レジストリはGit管理外の runtime/ へ置き、リポジトリ設定と分離する。
const REGISTRY_FILE = process.env.ANT_MEMORY_REGISTRY
  ? path.resolve(process.env.ANT_MEMORY_REGISTRY)
  : path.join(ROOT, "runtime", "workspaces.json");
const SERVER_ENTRY = path.join(
  ROOT,
  "server",
  "node_modules",
  "agent-memory-store",
  "src",
  "index.js",
);

// ワークスペースの目印。上位ディレクトリへ辿りながら最初に見つかった所を根とする。
const MARKERS = [
  ".git",
  "CLAUDE.md",
  "AGENTS.md",
  "GEMINI.md",
  ".agents",
  ".claude",
  "package.json",
  "pyproject.toml",
  "Cargo.toml",
  "go.mod",
];

// ここが「ワークスペース」と判定されたら信用しない（共有領域・IDE の作業場所など）。
// 注意: ROOT 自体は拒否しない。ANTmemory リポジトリをワークスペースとして開いて
// 開発するユースケースが実在し（Antigravity 誤接続事故 2026-08-24）、拒否すると
// 正しい候補が弾かれて別ウィンドウの DB へ誤接続する原因になった。
// 代わりにリポジトリ内部のディレクトリ（テスト用ストアや生成物）を拒否する。
const DENY_ROOTS = [
  os.homedir(),
  STORES_DIR,
  path.join(ROOT, "server"),
  path.join(ROOT, "runtime"),
  path.join(ROOT, "logs"),
  process.env.APPDATA,
  process.env.LOCALAPPDATA,
  process.env.ProgramFiles,
  process.env["ProgramFiles(x86)"],
  process.env.ProgramData,
  process.env.windir,
].filter(Boolean);

// ─── ユーティリティ ──────────────────────────────────────────────────────────

function log(line) {
  try {
    mkdirSync(path.dirname(LOG_FILE), { recursive: true });
    const ts = new Date().toISOString();
    appendFileSync(LOG_FILE, `${ts} ${line}\n`, "utf8");
  } catch {
    // ログが書けなくても起動は妨げない
  }
}

/** パス比較用に正規化（Windows は大文字小文字を区別しない） */
function norm(p) {
  return path.resolve(p).replace(/[\\/]+$/, "").toLowerCase();
}

function isUnder(child, parent) {
  const c = norm(child);
  const p = norm(parent);
  return c === p || c.startsWith(p + path.sep.toLowerCase()) || c.startsWith(p + "\\") || c.startsWith(p + "/");
}

/** ディレクトリ名として安全な文字列にする */
function sanitize(name) {
  return (
    name
      // Windows で使えない文字と制御文字を落とす
      .replace(/[<>:"/\\|?*\u0000-\u001f]/g, "_")
      .replace(/^\.+/, "_")
      .replace(/[. ]+$/, "")
      .trim() || "workspace"
  );
}

function shortHash(s) {
  return createHash("sha1").update(norm(s), "utf8").digest("hex").slice(0, 8);
}

function exists(p) {
  return existsSync(p);
}

// ─── ワークスペース解決 ──────────────────────────────────────────────────────

/** cwd から上へ辿って、目印のあるディレクトリを探す */
function findWorkspaceRoot(startDir) {
  let dir = path.resolve(startDir);
  const seen = new Set();

  while (!seen.has(dir)) {
    seen.add(dir);

    for (const marker of MARKERS) {
      if (exists(path.join(dir, marker))) return { root: dir, marker };
    }

    const parent = path.dirname(dir);
    if (parent === dir) break; // ドライブ直下まで来た
    dir = parent;
  }
  return null;
}

/**
 * IDE から起動された場合に作業空間の手がかりになりうる環境変数。
 * どれも「候補」であって信用はしない。cwd と同じ検証（目印の有無・共有領域でないこと）
 * を通ったものだけ採用する。
 */
const ENV_HINTS = [
  "VSCODE_WORKSPACE_FOLDER",
  "WORKSPACE_FOLDER",
  "WORKSPACE_ROOT",
  "PROJECT_ROOT",
  "ANTIGRAVITY_WORKSPACE",
  "GEMINI_WORKSPACE",
  "INIT_CWD",
  "PWD",
];

/**
 * 候補ディレクトリを検証して、正当ならワークスペース情報を返す。
 * @returns {{slug: string, root: string, source: string}|null}
 */
function tryCandidate(dir, label) {
  if (!dir || !dir.trim()) return null;
  let resolved;
  try {
    resolved = path.resolve(dir.trim());
  } catch {
    return null;
  }
  if (!exists(resolved)) return null;

  const found = findWorkspaceRoot(resolved);
  if (!found) return null;
  if (DENY_ROOTS.some((d) => norm(found.root) === norm(d))) return null;

  return {
    slug: slugFor(found.root),
    root: found.root,
    source: `${label}:${found.marker}`,
  };
}

/**
 * Antigravity IDE が現在開いているワークスペースを突き止める。
 *
 * Antigravity IDE は MCP サーバーを自身のインストールフォルダから起動し、
 * 作業空間を示す環境変数も渡してこない（VSCODE_CWD も IDE 自身の場所を指す）。
 * ワークスペース側の .agents\mcp_config.json も IDE では読まれない
 * （IDE のコード上 mcp_config.json はユーザーホーム配下のみ、.agents はルール用）。
 *
 * そこで IDE 自身が管理している workspaceStorage を読む。
 * 各ウィンドウの workspace.json に開いているフォルダの URI が入っている。
 *
 * どのウィンドウかの特定は 2 段構え。
 *
 * (a) 決定的な特定 — 親プロセスの --workspace_id と突き合わせる
 *     language_server の起動引数 --workspace_id は sha256(フォルダURI) である。
 *     例: sha256("file:///f%3A/ANTIGRABTY/%E3%83%9F...") =
 *         0923ce8c0d9ca0c41af5cdd7a4c3969b0d4c91148d637f0b3a2eec64e4635543
 *         が実際の --workspace_id と完全一致することを実測で確認した。
 *     ハッシュ元の文字列は workspace.json の folder そのままなので、
 *     候補をハッシュして突き合わせれば推測なしで一意に決まる。
 *
 *     注意: language_server は 2 種類動いている。実測した親子関係:
 *         node(ant-memory) ← language_server(--enable_lsp 付き・workspace_id あり) ✔
 *         node(ant-memory) ← language_server(管理用・workspace_id なし)
 *     IDE は両方から MCP サーバーを起こすため、(a) が効く場合と効かない場合がある。
 *     効かない場合は下の (b) に落ちるだけなので、(a) を試すこと自体に害はない。
 *
 * (b) 次善 — 最後に使われたウィンドウを採用する
 *     （workspace.json のあるフォルダと state.vscdb の更新時刻で判断）。
 *     複数ウィンドウを開いていると外す可能性がある。
 *
 * ログの source= が antigravity:workspace_id なら (a)、antigravity:recent なら (b)。
 */
function antigravityOpenWorkspace() {
  // Antigravity IDE から起動された場合だけ使う（他クライアントの挙動は変えない）
  if (!process.env.ANTIGRAVITY_EDITOR_APP_ROOT) return null;

  const bases = [];
  // VSCODE_CODE_CACHE_PATH は <Roaming>\<製品名>\CachedData\<hash> なので 2 つ上が製品フォルダ
  const cache = process.env.VSCODE_CODE_CACHE_PATH;
  if (cache) {
    bases.push(path.join(path.dirname(path.dirname(cache)), "User", "workspaceStorage"));
  }
  if (process.env.APPDATA) {
    bases.push(
      path.join(process.env.APPDATA, "Antigravity IDE", "User", "workspaceStorage"),
    );
  }

  // 開いているウィンドウの候補を集める
  const candidates = [];
  for (const base of bases) {
    if (!exists(base)) continue;

    let entries;
    try {
      entries = readdirSync(base);
    } catch {
      continue;
    }

    for (const entry of entries) {
      const wsFile = path.join(base, entry, "workspace.json");
      if (!exists(wsFile)) continue;

      let folderUri;
      try {
        folderUri = JSON.parse(readFileSync(wsFile, "utf8")).folder;
      } catch {
        continue;
      }
      if (!folderUri) continue;

      let dir;
      try {
        dir = fileURLToPath(folderUri);
      } catch {
        continue;
      }
      if (!exists(dir)) continue;

      // ウィンドウが使われた時刻。state.vscdb はウィンドウ操作のたびに更新される。
      let mtime = 0;
      for (const p of [path.join(base, entry), path.join(base, entry, "state.vscdb")]) {
        try {
          mtime = Math.max(mtime, statSync(p).mtimeMs);
        } catch {
          // 触れないものは無視
        }
      }

      candidates.push({ dir, folderUri, mtime });
    }
  }

  if (!candidates.length) return null;

  // (a) 決定的な特定: 実行中プロセスの --workspace_id と突き合わせる。
  //     workspace_id は 2 種類の形式が実測されている:
  //       - sha256(workspace.json の folder 文字列)（旧バージョン）
  //       - folder 文字列の非英数字連続を "_" に置換したスラグ
  //         例: file:///f%3A/ANTIGRABTY/ANTmemory → file_f_3A_ANTIGRABTY_ANTmemory
  //     候補を総当たりで両形式に変換すれば推測なしに一致する。
  //     候補が 1 つしか無いなら、そもそも曖昧さがないので照会自体を省く（約0.6秒の節約）。
  if (candidates.length > 1) {
    const found = antigravityWorkspaceIdFromProcessTree();
    if (found) {
      const fid = found.id.toLowerCase();
      for (const c of candidates) {
        const h = createHash("sha256").update(c.folderUri, "utf8").digest("hex");
        const slug = c.folderUri.replace(/[^A-Za-z0-9]+/g, "_").toLowerCase();
        if (h.toLowerCase() === fid || slug === fid) {
          return { dir: c.dir, how: found.how };
        }
      }
    }
  }

  // (b) 次善: 最後に使われたウィンドウ
  const best = candidates.reduce((a, b) => (b.mtime > a.mtime ? b : a));
  return { dir: best.dir, how: "recent" };
}

/**
 * 実行中のプロセスの起動引数から --workspace_id を拾う。
 * 形式はバージョンで異なる（両方実測）:
 *   - sha256(フォルダURI): 64桁の16進
 *   - URLスラグ: file:///f%3A/ANTIGRABTY/ANTmemory → file_f_3A_ANTIGRABTY_ANTmemory
 *
 * 探し方は 2 通り。実測に基づいて両方見る。
 *
 *   (1) 祖先プロセス
 *       素直に考えれば ant-memory を起こした親にあるはずだが、実際には無い。
 *       Antigravity の実際の親子関係はこうなっている:
 *           node(ant-memory) ← language_server(管理用) ← Antigravity IDE.exe ← explorer
 *       管理用 language_server の引数は csrf_token / extension_server_port /
 *       app_data_dir などで、workspace_id は含まれない。
 *       将来 IDE 側の実装が変わって親に載る可能性はあるので一応見る。
 *
 *   (2) 実行中の language_server プロセス全体
 *       --workspace_id を持つのはウィンドウごとの LSP プロセス（--enable_lsp 付き）で、
 *       これは MCP サーバーの祖先ではなく兄弟にあたる。
 *       そのため「どのウィンドウのものか」は本来ひも付けられないが、
 *       **workspace_id が 1 種類しか無い＝ウィンドウが 1 つしか開いていない**
 *       場合に限り、曖昧さなく確定できる。複数あるときは諦めて次善に回す。
 *
 * Windows でプロセスの起動引数を得るには OS の助けが要るため PowerShell を 1 回だけ呼ぶ
 * （実測で約 0.6 秒）。失敗しても null を返すだけで、呼び出し側は次善の判定に進む。
 */
function antigravityWorkspaceIdFromProcessTree() {
  if (process.platform !== "win32") return null;

  const script = `
$ErrorActionPreference = 'SilentlyContinue'
$id = ${process.ppid}
for ($i = 0; $i -lt 6; $i++) {
  $p = Get-CimInstance Win32_Process -Filter "ProcessId=$id"
  if (-not $p) { break }
  if ($p.CommandLine -match '--workspace_id[\\s=]+([0-9a-zA-Z_]+)') { Write-Output "CHAIN $($matches[1])" }
  if ($p.ParentProcessId -eq $id) { break }
  $id = $p.ParentProcessId
}
Get-CimInstance Win32_Process -Filter "Name LIKE 'language_server%'" | ForEach-Object {
  if ($_.CommandLine -match '--workspace_id[\\s=]+([0-9a-zA-Z_]+)') { Write-Output "LSP $($matches[1])" }
}`.trim();

  let out;
  try {
    out = execFileSync(
      "powershell.exe",
      ["-NoProfile", "-NonInteractive", "-ExecutionPolicy", "Bypass", "-Command", script],
      { encoding: "utf8", timeout: 8000, windowsHide: true, stdio: ["ignore", "pipe", "ignore"] },
    );
  } catch {
    return null;
  }

  const chain = [];
  const lsp = new Set();
  for (const line of out.split(/\r?\n/)) {
    const m = line.match(/^(CHAIN|LSP) ([0-9a-zA-Z_]+)$/);
    if (!m) continue;
    if (m[1] === "CHAIN") chain.push(m[2]);
    else lsp.add(m[2].toLowerCase());
  }

  // (1) 祖先にあれば文句なし
  if (chain.length) return { id: chain[0], how: "workspace_id" };

  // (2) ウィンドウが 1 つだけなら曖昧さがない
  if (lsp.size === 1) return { id: [...lsp][0], how: "workspace_id(単一ウィンドウ)" };

  return null;
}

/**
 * 解決結果を返す。
 * @returns {{slug: string, root: string|null, source: string}}
 */
function resolveWorkspace() {
  const cwd = process.cwd();
  const override = process.env.ANT_MEMORY_WORKSPACE;

  // 1) 明示指定が最優先
  if (override && override.trim()) {
    const v = override.trim();
    if (path.isAbsolute(v)) {
      return {
        slug: slugFor(v),
        root: path.resolve(v),
        source: "env:ANT_MEMORY_WORKSPACE(path)",
      };
    }
    // 単なる名前指定はそのまま採用（ハッシュを付けない）
    return { slug: sanitize(v), root: null, source: "env:ANT_MEMORY_WORKSPACE(name)" };
  }

  // 2) cwd から探索（Claude Code / Codex はここで決まる）
  const byCwd = tryCandidate(cwd, "cwd");
  if (byCwd) return byCwd;

  // 3) cwd が使えないとき（Antigravity IDE のように自分のインストール先から
  //    サーバーを起動する実装）だけ、環境変数のヒントを順に試す
  for (const key of ENV_HINTS) {
    const byEnv = tryCandidate(process.env[key], `env:${key}`);
    if (byEnv) return byEnv;
  }

  // 4) Antigravity IDE 専用の最終手段。IDE が持つ workspaceStorage を読む
  const ide = antigravityOpenWorkspace();
  if (ide) {
    const byIde = tryCandidate(ide.dir, `antigravity:${ide.how}`);
    if (byIde) return byIde;
  }

  // 5) どれも当たらなければ共有ストアへ退避
  return { slug: "_shared", root: null, source: `fallback(cwd=${cwd})` };
}

/**
 * どの環境変数が作業空間の手がかりになりうるか調べるための診断ダンプ。
 * 値が「実在するディレクトリのパス」に見えるものだけを書き出す（秘密情報を残さないため）。
 */
function dumpEnvDiagnostics(client) {
  try {
    const safeName = /^(VSCODE|WORKSPACE|PROJECT|ANTIGRAVITY|GEMINI|CODEX|CLAUDE|INIT_CWD|PWD|OLDPWD)/i;
    const lines = [];
    for (const [k, v] of Object.entries(process.env)) {
      if (!safeName.test(k)) continue;
      if (!v || v.length > 500) continue;
      if (!/[\\/]/.test(v)) continue;
      if (!exists(v)) continue;
      lines.push(`${k}=${v}`);
    }
    const out = path.join(ROOT, "logs", `env-${sanitize(client)}.txt`);
    mkdirSync(path.dirname(out), { recursive: true });
    writeFileSync(
      out,
      `# ${new Date().toISOString()}\n# cwd=${process.cwd()}\n` +
        `# 実在するディレクトリを指す環境変数のみ記録\n` +
        (lines.length ? lines.join("\n") : "(該当なし)") +
        "\n",
      "utf8",
    );
  } catch {
    // 診断が失敗しても起動は妨げない
  }
}

/**
 * ワークスペースの絶対パスからスラッグを決める。
 * 基本はフォルダ名そのまま。別パスで同名が既に登録済みならハッシュを付けて衝突回避。
 * 決まった対応は workspaces.json に記録して以後は固定する。
 */
function slugFor(absPath) {
  const full = path.resolve(absPath);
  const base = sanitize(path.basename(full)) || "workspace";

  let registry = {};
  try {
    registry = JSON.parse(readFileSync(REGISTRY_FILE, "utf8"));
  } catch {
    registry = {};
  }

  // 既に登録済みならそれを使う
  for (const [slug, recorded] of Object.entries(registry)) {
    if (norm(recorded) === norm(full)) return slug;
  }

  // 同名が別パスで使われていればハッシュ付きに
  const taken = Object.prototype.hasOwnProperty.call(registry, base);
  const slug = taken ? `${base}-${shortHash(full)}` : base;

  registry[slug] = full;
  try {
    mkdirSync(path.dirname(REGISTRY_FILE), { recursive: true });
    writeFileSync(REGISTRY_FILE, JSON.stringify(registry, null, 2) + "\n", "utf8");
  } catch {
    // 記録に失敗しても動作には影響しない（スラッグは決まっている）
  }
  return slug;
}

// ─── 起動 ────────────────────────────────────────────────────────────────────

const resolved = resolveWorkspace();
const storePath = path.join(STORES_DIR, resolved.slug);

mkdirSync(storePath, { recursive: true });

// agent-memory-store は読み込み時に AGENT_STORE_PATH を見る。import より前に設定する。
process.env.AGENT_STORE_PATH = storePath;

const client = process.env.ANT_MEMORY_CLIENT || "unknown";

log(
  `start client=${client} slug=${resolved.slug} source=${resolved.source} ` +
    `cwd=${process.cwd()} store=${storePath}`,
);

// _shared に落ちたときだけ、原因究明用に環境変数の手がかりを書き出す
if (resolved.slug === "_shared") dumpEnvDiagnostics(client);

if (process.argv.includes("--where")) {
  // 検証用。MCP としては起動しない。
  console.log(
    JSON.stringify(
      {
        client,
        cwd: process.cwd(),
        workspaceRoot: resolved.root,
        source: resolved.source,
        slug: resolved.slug,
        storePath,
        dbFile: path.join(storePath, "store.db"),
        serverEntry: SERVER_ENTRY,
      },
      null,
      2,
    ),
  );
  process.exit(0);
}

// stdout は MCP のプロトコル用。ここから先は絶対に stdout へ書かない。
process.stderr.write(
  `[ant-memory] workspace="${resolved.slug}" store="${storePath}" (${resolved.source})\n`,
);

// ─── 上流サーバーをロードし、ANTmemory 固有ツールを追加する ──────────────
// 上流 index.js は自身で McpServer を生成して connect するため、
// McpServer.prototype.connect を一時的にラップして、生成されたインスタンスへ
// ANTmemory 固有ツールを後登録する。上流ソースは一切変更しない。
const { McpServer } = await import(
  pathToFileURL(
    path.join(ROOT, "server", "node_modules", "@modelcontextprotocol", "sdk", "dist", "esm", "server", "mcp.js"),
  ).href
);

const originalConnect = McpServer.prototype.connect;
McpServer.prototype.connect = async function (transport) {
  const result = await originalConnect.call(this, transport);
  try {
    const { registerAntTools } = await import(
      pathToFileURL(path.join(ROOT, "server", "lib", "register-ant-tools.mjs")).href
    );
    registerAntTools(this, {
      trustedActor: client,
      model: process.env.ANT_MEMORY_MODEL || null,
      provider: process.env.ANT_MEMORY_PROVIDER || null,
    });
    process.stderr.write(`[ant-memory] ANTmemory tools registered (actor=${client})\n`);
  } catch (err) {
    process.stderr.write(`[ant-memory] ANTmemory tool registration failed: ${err.message}\n`);
  }
  return result;
};

await import(pathToFileURL(SERVER_ENTRY).href);
