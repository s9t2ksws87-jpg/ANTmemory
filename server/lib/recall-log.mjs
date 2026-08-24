/**
 * ANTmemory Recall Log（ANTmemory 所有コード）
 *
 * 「なぜ思い出したか」を記録する。保存・変更の監査履歴が「なぜ保存・変更したか」を
 * 追えるのに対し、こちらはAIの想起根拠を人間が追えるようにする。
 */

import { randomUUID } from "node:crypto";
import path from "node:path";
import { fileURLToPath, pathToFileURL } from "node:url";

const HERE = path.dirname(fileURLToPath(import.meta.url));

let _db = null;
async function ensureDb() {
  if (!_db) {
    const loaded = await import(pathToFileURL(path.join(HERE, "upstream.mjs")).href);
    const u = await loaded.loadUpstream();
    _db = u.db.getDb();
  }
  return _db;
}

/**
 * 検索ヒットの理由ログを記録する。
 * reasons例: ["keyword_match","semantic_similarity","visibility_hot","recent_usage"]
 * 失敗しても検索経路を妨げない。
 */
export async function logRecall(entries, { sessionId = null, query = null } = {}) {
  try {
    const db = await ensureDb();
    const ts = new Date().toISOString();
    db.exec("BEGIN");
    try {
      const ins = db.prepare(
        `INSERT INTO memory_recall_log (recall_id, chunk_id, session_id, query, reason, score, rank, created_at)
         VALUES (?, ?, ?, ?, ?, ?, ?, ?)`,
      );
      entries.forEach((e, i) => {
        ins.run(
          randomUUID(), e.id, sessionId, query,
          JSON.stringify(e.reasons ?? []), e.score ?? null, e.rank ?? i + 1, ts,
        );
      });
      db.exec("COMMIT");
    } catch (e) {
      db.exec("ROLLBACK");
      throw e;
    }
  } catch (err) {
    process.stderr.write(`[recall-log] logRecall failed: ${err.message}\n`);
  }
}

/** 特定chunkの想起履歴（新しい順） */
export async function getRecallLog(chunkId, limit = 20) {
  const db = await ensureDb();
  return db
    .prepare(
      `SELECT reason, score, rank, created_at FROM memory_recall_log
       WHERE chunk_id = ? ORDER BY created_at DESC LIMIT ?`,
    )
    .all(chunkId, limit);
}

/** 想起統計: よく思い出される記憶トップN */
export async function topRecalled(limit = 10) {
  const db = await ensureDb();
  return db
    .prepare(
      `SELECT r.chunk_id, c.topic, COUNT(*) as recall_count, MAX(r.created_at) as last_recalled
       FROM memory_recall_log r JOIN chunks c ON c.id = r.chunk_id
       GROUP BY r.chunk_id ORDER BY recall_count DESC LIMIT ?`,
    )
    .all(limit);
}
