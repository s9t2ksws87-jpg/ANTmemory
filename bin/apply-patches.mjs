#!/usr/bin/env node
/**
 * agent-memory-store 本体への独自パッチを当てる（冪等・何度実行してもよい）。
 *
 * npm update で node_modules が入れ替わるとパッチは消えるので、
 * server/package.json の postinstall から自動で呼ばれるようにしてある。
 * 手動で当て直す場合:
 *   node <ANTMEMORY_ROOT>\bin\apply-patches.mjs
 *
 * 対象コードが見つからない場合はエラー終了する（黙って壊れないようにするため）。
 */

import { readFileSync, writeFileSync, existsSync } from "node:fs";
import path from "node:path";

const ROOT = process.env.ANT_MEMORY_ROOT
  ? path.resolve(process.env.ANT_MEMORY_ROOT)
  : path.resolve(path.join(import.meta.dirname, ".."));

const SRC = path.join(ROOT, "server", "node_modules", "agent-memory-store", "src");

// 対応バージョン。パッチは上流の具体的なソース文字列に依存するため、
// ここに無いバージョンでは黙って適用せず明示エラーにする。
const SUPPORTED_VERSIONS = ["0.0.9"];

/** @type {Array<{file: string, name: string, from: string, to: string, all?: boolean}>} */
const PATCHES = [];

// ─────────────────────────────────────────────────────────────────────────────
// [1] ハイフンを含む検索語が FTS5 の構文エラーになる不具合
//     "ant-memory" で検索すると `no such column: memory` で落ちていた。
// ─────────────────────────────────────────────────────────────────────────────
PATCHES.push({
  file: "db.js",
  name: "[1] ハイフン入り検索語の修正",
  from: `  const ftsQuery = query
    .replace(/["*^:(){}[\\]]/g, " ")
    .split(/\\s+/)
    .filter((t) => t.length > 1)
    .join(" OR ");`,
  to: `  const ftsQuery = query
    .replace(/["*^:(){}[\\]]/g, " ")
    .split(/\\s+/)
    .filter((t) => t.length > 1)
    // 各語をフレーズとしてクォートする。こうしないと "ant-memory" のような
    // ハイフン入りの語が FTS5 の演算子として解釈され構文エラーになる。
    .map((t) => \`"\${t.replace(/"/g, "")}"\`)
    .join(" OR ");`,
});

// ─────────────────────────────────────────────────────────────────────────────
// [2] FTS5 の既定トークナイザ（unicode61）は日本語を語に分割しないため、
//     「ミュウ」で本文中の「ミュウの一人称」がヒットしなかった。
// ─────────────────────────────────────────────────────────────────────────────
PATCHES.push({
  file: "db.js",
  name: "[2] trigram トークナイザ（日本語対応）",
  from: `  content='chunks',
  content_rowid=rowid
);`,
  to: `  content='chunks',
  content_rowid=rowid,
  -- 既定の unicode61 は日本語を語に分割しないため trigram を使う
  tokenize='trigram'
);`,
});

// ─────────────────────────────────────────────────────────────────────────────
// [3] 既存DBのFTS索引を trigram で作り直す移行処理
// ─────────────────────────────────────────────────────────────────────────────
PATCHES.push({
  file: "db.js",
  name: "[3] 既存DBのFTS索引の作り直し",
  from: `  // Run schema
  db.exec(SCHEMA_TABLES);
  db.exec(SCHEMA_FTS);
  db.exec(SCHEMA_TRIGGERS);`,
  to: `  // Run schema
  db.exec(SCHEMA_TABLES);

  // 既存DBが古いトークナイザ（unicode61）で作られていたら索引を作り直す。
  // 日本語がまったく分割されず検索が機能しないため。
  let ftsNeedsRebuild = false;
  const existingFts = db
    .prepare(
      "SELECT sql FROM sqlite_master WHERE type='table' AND name='chunks_fts'",
    )
    .get();
  if (existingFts && !/trigram/i.test(existingFts.sql)) {
    process.stderr.write(
      "[agent-memory-store] Rebuilding FTS index with trigram tokenizer (Japanese support)...\\n",
    );
    db.exec("DROP TABLE IF EXISTS chunks_fts");
    ftsNeedsRebuild = true;
  }

  db.exec(SCHEMA_FTS);
  db.exec(SCHEMA_TRIGGERS);

  if (ftsNeedsRebuild) {
    db.exec("INSERT INTO chunks_fts(chunks_fts) VALUES('rebuild')");
    process.stderr.write("[agent-memory-store] FTS index rebuilt.\\n");
  }`,
});

// ─────────────────────────────────────────────────────────────────────────────
// [4] TTL の失効判定が効かない不具合
//     expires_at は ISO 形式（2026-08-02T21:44:01.014Z）で保存されるのに、
//     SQLite の datetime('now') は "2026-08-02 21:44:01" を返す。
//     文字列比較なので 'T'(0x54) > ' '(0x20) となり、同日中の失効を見逃す。
// ─────────────────────────────────────────────────────────────────────────────
PATCHES.push({
  file: "db.js",
  name: "[4] TTL 失効判定の書式ずれ修正",
  from: `  db.exec(
    \`DELETE FROM chunks WHERE expires_at IS NOT NULL AND expires_at < datetime('now')\`,
  );`,
  to: `  // expires_at は ISO 形式で保存されるため、比較相手も ISO 形式で作る。
  // datetime('now') は "YYYY-MM-DD HH:MM:SS" を返すので、文字列比較では
  // 同日中に失効したデータを取りこぼす。
  db.exec(
    \`DELETE FROM chunks WHERE expires_at IS NOT NULL
       AND expires_at < strftime('%Y-%m-%dT%H:%M:%fZ', 'now')\`,
  );`,
});

// ─────────────────────────────────────────────────────────────────────────────
// [5] 旧ファイル形式からの移行が絶対に実行されない不具合
//     initStore() が getDb()（store.db を作る）を先に呼ぶため、
//     直後の migrateIfNeeded() が「store.db があるので移行済み」と誤判定する。
// ─────────────────────────────────────────────────────────────────────────────
PATCHES.push({
  file: "store.js",
  name: "[5] 旧形式からの移行が走らない不具合の修正",
  from: `export async function initStore() {
  // Ensure DB is ready (also runs schema + expiry purge)
  getDb();

  // Migrate from filesystem if needed
  await migrateIfNeeded();`,
  to: `export async function initStore() {
  // 移行は store.db を作る前に走らせないといけない。
  // migrateIfNeeded() は「store.db が既にあれば移行済み」と判定するので、
  // getDb() を先に呼ぶと移行が永久に実行されない。
  await migrateIfNeeded();

  // Ensure DB is ready (also runs schema + expiry purge)
  getDb();`,
});

// ─────────────────────────────────────────────────────────────────────────────
// [6] min_score が公開されているのに未実装だった
//     hybridSearch() は minScore を受け取るだけで一度も使っていない。
//     ただしモードごとにスコアの尺度が違うため、素の値で足切りすると壊れる:
//       bm25     … FTS5 の rank（負値。小さいほど良い）
//       semantic … コサイン類似度（0〜1）
//       hybrid   … RRF スコア（最大でも 1/61 ≒ 0.016）
//     そこで「最良ヒットを 1.0 とした相対値」に正規化してから比較する。
//     min_score は「最良ヒットに対してどれだけ近いか」という一貫した意味になる。
// ─────────────────────────────────────────────────────────────────────────────
PATCHES.push({
  file: "search.js",
  name: "[6] min_score の実装",
  from: `export async function hybridSearch({`,
  to: `/**
 * スコアを 0〜1 に正規化して minScore で足切りする。
 *
 * モードごとに素のスコアの尺度が違う（BM25 の rank は負値、RRF は最大でも
 * 0.016 程度、コサイン類似度は 0〜1）ため、そのまま閾値と比べても意味がない。
 * 最良ヒットを 1.0 とした相対値に揃えることで、min_score を
 * 「最良ヒットに対する相対的な近さ」として一貫して扱えるようにする。
 */
function normalizeAndFilter(results, minScore) {
  if (!results.length) return results;

  const raw = results.map((r) =>
    typeof r.score === "number" && Number.isFinite(r.score) ? r.score : 0,
  );

  // FTS5 の rank は「小さいほど良い」負値なので、符号を反転して向きを揃える
  const flip = raw.every((v) => v <= 0) && raw.some((v) => v < 0);
  const rel = raw.map((v) => (flip ? -v : v));
  const max = Math.max(...rel);

  // スコアが全て 0（順位情報なし）のときは足切りの根拠がないのでそのまま返す
  if (!(max > 0)) return minScore > 0 ? [] : results;

  return results
    .map((r, i) => ({ ...r, score: Math.round((rel[i] / max) * 100) / 100 }))
    .filter((r) => r.score >= minScore);
}

/**
 * 公開エントリ。実処理は hybridSearchRaw に委譲し、結果に minScore を適用する。
 */
export async function hybridSearch(opts) {
  const results = await hybridSearchRaw(opts);
  return normalizeAndFilter(results, opts?.minScore ?? 0.1);
}

async function hybridSearchRaw({`,
});

// ─────────────────────────────────────────────────────────────────────────────
// [7] min_score を正しく効かせるための前処理
//     結果を組み立てる時点でスコアを小数2桁に丸めているため、RRF のように
//     0.0098 対 0.0097 といった僅差のスコアが両方 0.01 に潰れてしまい、
//     正規化しても差が消えて足切りできない。丸めは normalizeAndFilter に任せ、
//     ここでは生のスコアを保持する。
// ─────────────────────────────────────────────────────────────────────────────
PATCHES.push({
  file: "search.js",
  name: "[7] スコアの早すぎる丸めを除去",
  from: `    score: Math.round(r.score * 100) / 100,`,
  to: `    score: r.score, // 丸めは normalizeAndFilter で行う`,
  all: true,
});

PATCHES.push({
  file: "search.js",
  name: "[7] スコアの早すぎる丸めを除去（融合結果）",
  from: `        score: Math.round(score * 100) / 100,`,
  to: `        score, // 丸めは normalizeAndFilter で行う`,
  all: true,
});

PATCHES.push({
  file: "search.js",
  name: "[7] スコアの早すぎる丸めを除去（ベクトル結果）",
  from: `      score: Math.round(score * 100) / 100,`,
  to: `      score, // 丸めは normalizeAndFilter で行う`,
  all: true,
});

// ─────────────────────────────────────────────────────────────────────────────
// [8] 旧形式からの移行が、日付のバインド失敗で1件も入らない不具合
//     gray-matter(YAML) は `updated: 2026-07-01T00:00:00.000Z` を Date オブジェクト
//     として解釈するが、node:sqlite は Date をバインドできず TypeError になる。
//     しかも per-file の catch が握り潰すため、外からは「0件移行」に見えていた。
// ─────────────────────────────────────────────────────────────────────────────
PATCHES.push({
  file: "migrate.js",
  name: "[8] 移行時の日付バインド失敗の修正",
  from: `        const now = new Date().toISOString();
        await insertChunk({
          id: meta.id || file.replace(".md", ""),
          topic: meta.topic || "Untitled",
          agent: meta.agent || "global",
          tags: meta.tags || [],
          importance: meta.importance || "medium",
          content: content.trim(),
          embedding: null, // Will be computed in background
          createdAt: meta.updated || now,
          updatedAt: meta.updated || now,
          expiresAt: meta.expires || null,
        });
        chunkCount++;
      } catch {
        // Skip unreadable files
      }`,
  to: `        const now = new Date().toISOString();

        // gray-matter(YAML) は日付らしき値を Date オブジェクトに変換するが、
        // node:sqlite は Date をバインドできない。ISO 文字列に揃える。
        const toIso = (v) =>
          v instanceof Date
            ? v.toISOString()
            : typeof v === "string" && v
              ? v
              : null;
        const updatedAt = toIso(meta.updated) || now;

        await insertChunk({
          id: meta.id || file.replace(".md", ""),
          topic: meta.topic || "Untitled",
          agent: meta.agent || "global",
          tags: meta.tags || [],
          importance: meta.importance || "medium",
          content: content.trim(),
          embedding: null, // Will be computed in background
          createdAt: updatedAt,
          updatedAt: updatedAt,
          expiresAt: toIso(meta.expires),
        });
        chunkCount++;
      } catch (err) {
        // 握り潰すと移行漏れに気づけないので、何が落ちたかは必ず出す
        process.stderr.write(
          \`[agent-memory-store] Migration skipped \${file}: \${err.message}\\n\`,
        );
      }`,
});

// ─────────────────────────────────────────────────────────────────────────────
// [9] 同時書込時の "database is locked" 対策
//     複数のMCPサーバープロセスが同一DBへ同時書込するANTmemory構成では、
//     busy_timeout が無いと即座に SQLITE_BUSY で書込が失敗する。
// ─────────────────────────────────────────────────────────────────────────────
PATCHES.push({
  file: "db.js",
  name: "[9] busy_timeout の設定",
  from: `  // WAL mode for better concurrent read performance
  db.exec("PRAGMA journal_mode = WAL");`,
  to: `  // WAL mode for better concurrent read performance
  db.exec("PRAGMA journal_mode = WAL");

  // 複数プロセスが同一DBへ同時書込するため、ロック競合時に待つ。
  db.exec("PRAGMA busy_timeout = 5000");`,
});

// ─── 適用 ────────────────────────────────────────────────────────────────────

// 対象バージョン検証。対応外なら何も適用せず明示エラーで終了する。
const pkgPath = path.join(SRC, "..", "package.json");
if (existsSync(pkgPath)) {
  const installed = JSON.parse(readFileSync(pkgPath, "utf8")).version;
  if (!SUPPORTED_VERSIONS.includes(installed)) {
    console.error(
      `[apply-patches] agent-memory-store ${installed} は対応外です ` +
        `(対応: ${SUPPORTED_VERSIONS.join(", ")})。\n` +
        `  パッチは上流のソース文字列に依存するため、黙って適用はしない。\n` +
        `  対応版をインストールするか、apply-patches.mjs の SUPPORTED_VERSIONS を更新して\n` +
        `  各パッチの対象文字列を実測の上で追加すること。`,
    );
    process.exit(3);
  }
}

const files = new Map();
const applied = [];
const skipped = [];

for (const p of PATCHES) {
  const full = path.join(SRC, p.file);
  if (!existsSync(full)) {
    console.error(`[apply-patches] 見つからない: ${full}`);
    process.exit(1);
  }
  if (!files.has(p.file)) files.set(p.file, readFileSync(full, "utf8"));

  let src = files.get(p.file);

  // 全置換パッチは「置換対象が残っていない」ことをもって適用済みと判定する
  const done = p.all ? !src.includes(p.from) : src.includes(p.to);
  if (done) {
    skipped.push(`${p.name}（適用済み）`);
    continue;
  }
  if (!src.includes(p.from)) {
    console.error(
      `[apply-patches] ${p.name}: 対象コードが ${p.file} に見つからない。\n` +
        `  本体が更新されてパッチが古くなった可能性がある。手当てが必要。`,
    );
    process.exit(2);
  }

  files.set(p.file, p.all ? src.replaceAll(p.from, p.to) : src.replace(p.from, p.to));
  applied.push(p.name);
}

for (const [file, content] of files) {
  const full = path.join(SRC, file);
  if (readFileSync(full, "utf8") !== content) writeFileSync(full, content, "utf8");
}

for (const a of applied) console.log(`[apply-patches] 適用: ${a}`);
for (const s of skipped) console.log(`[apply-patches] skip: ${s}`);
console.log(`[apply-patches] 完了（${applied.length} 件適用 / ${skipped.length} 件は適用済み）`);
