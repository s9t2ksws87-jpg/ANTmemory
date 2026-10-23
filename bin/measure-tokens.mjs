#!/usr/bin/env node
/**
 * Phase Recall-0: トークン基準値計測
 *
 * 固定コーパス（日本語・英語・長文を含む）を隔離ストアへ投入し、代表クエリ群で
 * search_context の返却量を計測する。audit/recall を同じコーパス・クエリで比較するための
 * 基準値を作る。recallの各フェーズ完了時にもこのスクリプトで再計測し差分を記録する。
 *
 * 原則（docs/ARCHITECTURE_RESEARCH.md 冒頭）:
 *   機能追加より「本当にtoken削減できたか」の計測が最優先。
 *
 * 使い方:
 *   node bin/measure-tokens.mjs                 # 計測実行 → backups/ へ記録
 *   node bin/measure-tokens.mjs --compare <旧JSON>  # 過去基準値との比較表示
 *
 * token概算: 日本語≈1字/token、英語≈4字/token の簡易推定（文字数も併記）。
 */

import { mkdirSync, writeFileSync, existsSync, readFileSync } from "node:fs";
import { join, resolve, dirname } from "node:path";
import { fileURLToPath, pathToFileURL } from "node:url";

const ROOT = resolve(dirname(fileURLToPath(import.meta.url)), "..");
const BIN = join(ROOT, "bin");

const argIdx = (name) => {
  const i = process.argv.indexOf(name);
  return i >= 0 ? process.argv[i + 1] : null;
};
const COMPARE_WITH = argIdx("--compare");

// ─── 固定コーパス（日本語・英語・長文・ハイフン語を含む。変更禁止＝計測の正本）───
const CORPUS = [
  { topic: "コーパス: 文体の方針", content: "作品の文体は常体（だ・である）で統一する。会話文のみ敬体を許可する。理由: ユーザーが作品トーンの重厚感を優先したため、丁寧な敬体案は却下された。", tags: ["corpus", "style"], importance: "high" },
  { topic: "コーパス: ミュウの一人称", content: "ミュウは一人称「ミュウ」を使用する。幼さと神秘性を同時に出すための意図的な設定であり、「私」や「あたし」への変更は許可しない。", tags: ["corpus", "character"], importance: "high" },
  { topic: "Corpus: ant-memory architecture", content: "The ant-memory launcher resolves the workspace from the current working directory and switches the SQLite database per workspace. All agents share one server binary.", tags: ["corpus", "architecture"], importance: "medium" },
  { topic: "コーパス: スドラヴィア軍制", content: "スドラヴィア軍制はカール大王の陸軍改革を起点とし、班10〜15人、小隊100人以上、大隊1,000人以上、旅団3,000人以上、師団10,000人以上、軍団30,000〜60,000人で編成する。三等将校は百人隊長と呼ばれる昇進の壁であり、下将以上は戦場にほとんど出ず、元帥は国王と同じ権限を持つ。この構造がスドラヴィアを三頭国と呼ぶ理由の一つである。術兵はほぼ女性で、術の残量を弾薬同様に兵站管理する。大術には三十人以上の術兵を半日拘束して集結させる必要がある。", tags: ["corpus", "worldbuilding"], importance: "medium" },
  { topic: "コーパス: 長文テスト用 設定積み上げ1", content: "異世界シミュレータの魔法体系は五大属性（火水風土地）と二つの特殊系（時空・精神）から構成される。各属性は相性表に従い、火は風に強く水に弱い。術式の発動には詠唱とイメージの固定が必須で、無詠唱発動は上級者でも制御成功率が五割を超えない。魔力回路の疲労は睡眠以外で回復せず、連続使用による回路焼灼は回復不能な障害として残る。国家資格としての術師等級は九級から一級まで存在し、三級以上は宮廷術師への登用資格を得る。", tags: ["corpus", "worldbuilding"], importance: "low" },
  { topic: "Corpus: long-form test entry 2", content: "Retrieval quality depends on the fusion of lexical and semantic signals. BM25 handles exact keyword overlap while embedding similarity covers paraphrases. Reciprocal rank fusion merges both ranked lists without score normalization. The budget allocator then trims low-relevance entries first.", tags: ["corpus", "retrieval"], importance: "low" },
  { topic: "コーパス: 作業履歴系の記録", content: "第三話の初稿は2026年8月に完了。推敲段階で視点人物の混乱が指摘され、章冒頭に時系列の明示を追加することで解決した。次回作業は第四話プロット。", tags: ["corpus", "progress"], importance: "low" },
];

// ─── 代表クエリ群（変更禁止。日本語・英語・ハイフン・曖昧を含む）──────────────
const QUERIES = [
  { q: "文体の方針", mode: "hybrid" },
  { q: "ミュウ 一人称", mode: "hybrid" },
  { q: "ant-memory", mode: "bm25" },
  { q: "軍制 編成 術兵", mode: "hybrid" },
  { q: "魔法体系 相性 詠唱", mode: "hybrid" },
  { q: "retrieval quality fusion", mode: "bm25" },
  { q: "第三話 初稿 完了", mode: "hybrid" },
  { q: "物語の世界観について教えて", mode: "semantic" }, // 意味検索（曖昧クエリ）
];

/** token概算: CJKは1字≈1token、それ以外は4字≈1token */
function estimateTokens(text) {
  if (!text) return 0;
  const cjk = (text.match(/[\u3000-\u30ff\u3400-\u4dbf\u4e00-\u9fff\uf900-\ufaff]/g) || []).length;
  const other = text.length - cjk;
  return Math.ceil(cjk + other / 4);
}

// ─── 隔離ストア準備 ──────────────────────────────────────────────────────────
const WS = "recall-token-measure";

const { Client } = await import(
  pathToFileURL(join(ROOT, "server", "node_modules", "@modelcontextprotocol", "sdk", "dist", "esm", "client", "index.js")).href
);
const { StdioClientTransport } = await import(
  pathToFileURL(join(ROOT, "server", "node_modules", "@modelcontextprotocol", "sdk", "dist", "esm", "client", "stdio.js")).href
);

async function open() {
  const client = new Client({ name: "recall-measure", version: "1.0.0" });
  await client.connect(new StdioClientTransport({
    command: process.execPath,
    args: [join(BIN, "ant-memory.mjs")],
    cwd: ROOT,
    env: { ...process.env, ANT_MEMORY_ROOT: ROOT, ANT_MEMORY_WORKSPACE: WS, ANT_MEMORY_CLIENT: "hermes" },
  }));
  return client;
}
const text = (r) => (r.content ?? []).map((c) => c.text).join("\n");
const sleep = (ms) => new Promise((r) => setTimeout(r, ms));

// コーパス投入（既存があればスキップ判定のため topic 一覧を先に取得）
console.log("隔離ストアを準備中...");
let client = await open();

// 既存コーパスの確認
const existing = text(await client.callTool({
  name: "search_context",
  arguments: { query: "コーパス Corpus corpus", search_mode: "bm25", min_score: 0, top_k: 20 },
}));
const alreadySeeded = (existing.match(/###/g) || []).length >= CORPUS.length;

if (!alreadySeeded) {
  for (const item of CORPUS) {
    const w = await client.callTool({
      name: "write_context",
      arguments: { topic: item.topic, content: item.content, agent: "hermes", tags: item.tags, importance: item.importance },
    });
    const id = text(w).match(/id=`([0-9a-f]{10})`/i)?.[1];
    if (!id) throw new Error(`seed失敗: ${item.topic}`);
  }
  // 埋め込み生成待ち（意味検索の精度安定化）
  console.log("埋め込み生成を待機中...");
  await sleep(8000);
}

// ── 計測 ──────────────────────────────────────────────────────────────────────
// ウォームアップ（モデルロード等を除外するため最初の1クエリは捨てる）
await client.callTool({ name: "search_context", arguments: { query: QUERIES[0].q, min_score: 0 } });

const measurements = [];
for (const { q, mode } of QUERIES) {
  const r = await client.callTool({
    name: "search_context",
    arguments: { query: q, mode: mode === undefined ? undefined : undefined, search_mode: mode, min_score: 0, top_k: 10 },
  });
  const body = text(r);
  const hits = (body.match(/###/g) || []).length;
  measurements.push({
    query: q,
    mode,
    resultCount: hits,
    chars: body.length,
    estTokens: estimateTokens(body),
  });
}

await client.close();
client = null;

// ── 統計集計 ────────────────────────────────────────────────────────────────
const totalChars = measurements.reduce((s, m) => s + m.chars, 0);
const totalTokens = measurements.reduce((s, m) => s + m.estTokens, 0);
const summary = {
  measuredAt: new Date().toISOString(),
  version: "audit",
  workspace: WS,
  corpusSize: CORPUS.length,
  queryCount: QUERIES.length,
  perQuery: measurements,
  avgCharsPerQuery: Math.round(totalChars / measurements.length),
  avgEstTokensPerQuery: Math.round(totalTokens / measurements.length),
  note: "token値は簡易推定(CJK=1token, 他=4chars/token)。audit/recall比較は同一推定方式なので有効。",
};

// ── 出力・保存 ───────────────────────────────────────────────────────────────
mkdirSync(join(ROOT, "backups"), { recursive: true });
const outPath = join(ROOT, "backups", `recall-token-${summary.measuredAt.replace(/[:.]/g, "-").slice(0, 19)}.json`);
writeFileSync(outPath, JSON.stringify(summary, null, 2) + "\n", "utf8");
writeFileSync(join(ROOT, "backups", "Recall_TOKEN_LATEST.txt"), `${outPath}\n`, "utf8");

console.log("\n=== 計測結果 (audit基準値) ===");
for (const m of measurements) {
  console.log(`  ${m.mode.padEnd(8)} "${m.query}" → ${m.resultCount}件 ${String(m.chars).padStart(5)}字 ≈${m.estTokens}tok`);
}
console.log(`\n平均返却量/クエリ: ${summary.avgCharsPerQuery} 字 ≈ ${summary.avgEstTokensPerQuery} tokens`);
console.log(`保存先: ${outPath}`);

// ── 比較モード ───────────────────────────────────────────────────────────────
if (COMPARE_WITH && existsSync(COMPARE_WITH)) {
  const base = JSON.parse(readFileSync(COMPARE_WITH, "utf8"));
  const delta = summary.avgEstTokensPerQuery - base.avgEstTokensPerQuery;
  const pct = ((delta / base.avgEstTokensPerQuery) * 100).toFixed(1);
  console.log(`\n=== 比較 ===`);
  console.log(`基準: ${base.avgEstTokensPerQuery} tok (${base.measuredAt}, ${base.version})`);
  console.log(`現在: ${summary.avgEstTokensPerQuery} tok`);
  console.log(`差分: ${delta > 0 ? "+" : ""}${delta} tok (${pct}%)`);
  console.log(pct <= -60 ? "✓ KPI達成（60%以上削減）" : "✗ KPI未達（60%削減が必要）");
}

process.exit(0);
