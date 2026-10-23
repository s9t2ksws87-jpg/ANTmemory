/**
 * ANTmemory recall Usage / Retrieval Session（ANTmemory 所有コード）
 *
 * - 参照統計（access_count / last_accessed_at / last_session_id）を記録する
 * - セッション内既読記憶は検索順位を下げる（重複投入の削減）
 * - usage は補助信号に留める（自己強化ループの防止。PLAN §5）
 */

import { randomUUID } from "node:crypto";
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

const now = () => new Date().toISOString();

/** 検索ヒット・read時の参照を記録する（軽量・失敗しても検索を妨げない） */
export async function recordAccess(chunkIds, sessionId = null) {
  try {
    const db = await ensureDb();
    const ts = now();
    db.exec("BEGIN");
    try {
      const upsert = db.prepare(
        `INSERT INTO memory_usage (chunk_id, access_count, last_accessed_at, last_session_id)
         VALUES (?, 1, ?, ?)
         ON CONFLICT(chunk_id) DO UPDATE SET
           access_count = access_count + 1,
           last_accessed_at = excluded.last_accessed_at,
           last_session_id = excluded.last_session_id`,
      );
      for (const id of chunkIds) upsert.run(id, ts, sessionId);
      db.exec("COMMIT");
    } catch (e) {
      db.exec("ROLLBACK");
      throw e;
    }
  } catch (e) {
    process.stderr.write(`[recall-usage] recordAccess failed: ${e.message}\n`);
  }
}

/** セッション開始。session_id を返す */
export async function beginSession(client, tokenBudget = null) {
  const db = await ensureDb();
  const sessionId = randomUUID();
  db.prepare(
    "INSERT INTO retrieval_sessions (session_id, client, started_at, token_budget) VALUES (?, ?, ?, ?)",
  ).run(sessionId, client, now(), tokenBudget);
  return sessionId;
}

/** セッション内で既読のchunk_id一覧 */
export async function getSeenIds(sessionId) {
  if (!sessionId) return [];
  const db = await ensureDb();
  return db
    .prepare("SELECT chunk_id FROM retrieval_session_seen WHERE session_id = ?")
    .all(sessionId)
    .map((r) => r.chunk_id);
}

/** セッションに既読を追加 */
export async function markSeen(sessionId, chunkIds) {
  if (!sessionId || !chunkIds?.length) return;
  const db = await ensureDb();
  const ts = now();
  db.exec("BEGIN");
  try {
    const ins = db.prepare(
      "INSERT OR IGNORE INTO retrieval_session_seen (session_id, chunk_id, seen_at) VALUES (?, ?, ?)",
    );
    for (const id of chunkIds) ins.run(sessionId, id, ts);
    db.exec("COMMIT");
  } catch (e) {
    db.exec("ROLLBACK");
    throw e;
  }
}

/**
 * 既読ペナルティを適用した並べ替え。
 * blocks: {id, block} 配列。既読は末尾へ回す（順位を下げる）。
 * 戻り値: {ordered: string[], seenCount: number}
 */
export function applySeenPenalty(entries, seenSet) {
  if (!seenSet?.size) return { ordered: entries.map((e) => e.block), seenCount: 0 };
  const fresh = entries.filter((e) => !seenSet.has(e.id));
  const seen = entries.filter((e) => seenSet.has(e.id));
  return {
    ordered: [...fresh.map((e) => e.block), ...seen.map((e) => e.block)],
    seenCount: seen.length,
  };
}

/** usage取得 */
export async function getUsage(chunkId) {
  const db = await ensureDb();
  return db.prepare("SELECT * FROM memory_usage WHERE chunk_id = ?").get(chunkId) ?? null;
}
