/**
 * ANTmemory recall Summary Layer（ANTmemory 所有コード）
 *
 * 原文は chunks.content が正本。ここで生成する summary は常に派生データであり、
 * source_hash（元本文のSHA256）が一致しないものは無効として扱う。
 *
 * 要約生成方針（docs/ARCHITECTURE_RESEARCH.md §4: Mem0の書込時LLM呼び出しは不採用）:
 * - 外部LLMを使わない決定論的抽出（先頭文 + キー文選別）
 * - コストゼロ・ローカル完結・同一本文なら必ず同一 summary
 * - 将来LLM要約に差し替え可能な構造（model/provider欄を保持）
 */

import { createHash } from "node:crypto";
import path from "node:path";
import { fileURLToPath, pathToFileURL } from "node:url";

const HERE = path.dirname(fileURLToPath(import.meta.url));

let _db = null;
async function ensureDb() {
  if (!_db) {
    const loaded = await import(pathToFileURL(path.join(HERE, "core.mjs")).href);
    const u = await loaded.loadCore();
    _db = u.db.getDb();
  }
  return _db;
}

export function sha256(s) {
  return createHash("sha256").update(s, "utf8").digest("hex");
}

/** 文分割（日本語・英語の句読点に対応） */
function splitSentences(text) {
  return text
    .replace(/\r\n/g, "\n")
    .split(/(?<=[。．！？!?\n])/)
    .map((s) => s.trim())
    .filter((s) => s.length > 0);
}

/**
 * short summary: 先頭文をそのまま使う（最大120字）。
 * 決定論的・高速・原文の冒頭を歪めない。
 */
export function buildShortSummary(content) {
  const sentences = splitSentences(content);
  if (!sentences.length) return content.slice(0, 120);
  let s = sentences[0];
  if (s.length > 120) s = s.slice(0, 117) + "…";
  return s;
}

/**
 * detailed summary: 重要文を選別して結合（最大600字）。
 * スコア: 冒頭近さ + キーワード密度（決定・理由・禁止などを示す語）+ 長さ適度性。
 */
export function buildDetailedSummary(content) {
  const KEY_HINTS = [
    "決定", "理由", "却下", "採用", "禁止", "変更", "方針", "原則",
    "ため", "により", "重要", "必須", "注意", "失敗", "解決", "結論",
    "because", "decision", "important", "must", "reason",
  ];
  // 意味変化に関わる強い語。これらを含む文は優先的に残す。
  const STRONG_HINTS = ["禁止", "却下", "撤回", "絶対", "変更禁止", "never", "forbidden"];
  const sentences = splitSentences(content);
  if (sentences.length <= 3) {
    return content.length <= 600 ? content : content.slice(0, 597) + "…";
  }

  const scored = sentences.map((s, idx) => {
    let score = 0;
    score += Math.max(0, 5 - idx); // 冒頭ほど重要
    let hintHit = 0;
    for (const hint of KEY_HINTS) {
      if (s.toLowerCase().includes(hint.toLowerCase())) hintHit += 1;
    }
    score += Math.min(hintHit, 3) * 2; // キーワードヒット（最大+6）
    for (const strong of STRONG_HINTS) {
      if (s.toLowerCase().includes(strong.toLowerCase())) { score += 5; break; } // 強い語は優先
    }
    // 長すぎる文は減点、短すぎる文も減点
    if (s.length > 200) score -= 2;
    if (s.length < 15) score -= 1;
    return { s, idx, score };
  });

  const picked = scored
    .slice()
    .sort((a, b) => b.score - a.score)
    .slice(0, Math.max(3, Math.ceil(sentences.length / 3)))
    .sort((a, b) => a.idx - b.idx) // 原文順に復元
    .map((x) => x.s);

  let out = picked.join(" ");
  if (out.length > 600) out = out.slice(0, 597) + "…";
  return out;
}

/** 単一chunkのsummary生成（既存の有効なものがあれば再利用しない・上書き更新） */
export async function generateSummaryFor(chunkId) {
  const db = await ensureDb();
  const row = db.prepare("SELECT content FROM chunks WHERE id = ?").get(chunkId);
  if (!row) throw new Error(`対象が見つかりません: ${chunkId}`);
  const content = row.content;
  const hash = sha256(content);

  db.exec("BEGIN");
  try {
    for (const [level, summary] of [
      ["short", buildShortSummary(content)],
      ["detailed", buildDetailedSummary(content)],
    ]) {
      db.prepare(
        `INSERT INTO memory_summaries (chunk_id, level, summary, model, provider, created_at, source_hash)
         VALUES (?, ?, ?, ?, ?, ?, ?)
         ON CONFLICT(chunk_id, level) DO UPDATE SET
           summary = excluded.summary,
           model = excluded.model,
           provider = excluded.provider,
           created_at = excluded.created_at,
           source_hash = excluded.source_hash`,
      ).run(chunkId, level, summary, "rule-extract-v1", null, new Date().toISOString(), hash);
    }
    db.exec("COMMIT");
  } catch (e) {
    db.exec("ROLLBACK");
    throw e;
  }
  return { chunkId, short: buildShortSummary(content), detailed: buildDetailedSummary(content) };
}

/**
 * source_hash が現本文と一致する summary を取得。
 * 一致しない（原文が更新された等）場合は null を返す＝無効扱い。
 */
export async function getValidSummary(chunkId, level) {
  const db = await ensureDb();
  const chunk = db.prepare("SELECT content FROM chunks WHERE id = ?").get(chunkId);
  if (!chunk) return null;
  const row = db
    .prepare("SELECT summary, source_hash FROM memory_summaries WHERE chunk_id = ? AND level = ?")
    .get(chunkId, level);
  if (!row) return null;
  if (row.source_hash !== sha256(chunk.content)) return null; // 無効
  return row.summary;
}

/** summary未生成・無効のchunk一覧（バックフィル用） */
export async function getChunksNeedingSummary(limit = 50) {
  const db = await ensureDb();
  const rows = db
    .prepare(
      `SELECT c.id, c.content FROM chunks c
       LEFT JOIN memory_summaries ms ON ms.chunk_id = c.id AND ms.level = 'short'
       WHERE ms.chunk_id IS NULL
       LIMIT ?`,
    )
    .all(limit);
  // source_hash 不一致（原文更新後の無効summary）も対象に含める
  const withStale = [];
  for (const r of rows) void r;
  const stale = db
    .prepare("SELECT chunk_id, source_hash FROM memory_summaries WHERE level='short'")
    .all();
  const chunkContents = db.prepare("SELECT id, content FROM chunks").all();
  const hashById = new Map(chunkContents.map((c) => [c.id, sha256(c.content)]));
  for (const s of stale) {
    if (hashById.get(s.chunk_id) !== s.source_hash && !rows.some((r) => r.id === s.chunk_id)) {
      withStale.push({ id: s.chunk_id });
    }
  }
  return [...rows.map((r) => ({ id: r.id })), ...withStale].slice(0, limit);
}

/** バックフィル: 未生成chunkのsummaryを生成（サーバー起動時のバックグラウンド用） */
export async function backfillSummaries(limit = 100) {
  const targets = await getChunksNeedingSummary(limit);
  let done = 0;
  for (const { id } of targets) {
    try {
      await generateSummaryFor(id);
      done += 1;
    } catch (e) {
      process.stderr.write(`[recall-summary] skip ${id}: ${e.message}\n`);
    }
  }
  return done;
}
