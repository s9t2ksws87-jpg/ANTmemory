/**
 * ANTmemory Phase B — Structured Retrieval Core
 *
 * Human-readable MCP text must be the final presentation layer, never an input to
 * lifecycle/visibility/ranking decisions. This module returns structured records.
 */

import path from "node:path";
import { fileURLToPath, pathToFileURL } from "node:url";

const HERE = path.dirname(fileURLToPath(import.meta.url));

let _upstream = null;
async function core() {
  if (!_upstream) {
    const mod = await import(pathToFileURL(path.join(HERE, "core.mjs")).href);
    _upstream = await mod.loadCore();
  }
  return _upstream;
}

function parseTags(value) {
  if (Array.isArray(value)) return value;
  try { return JSON.parse(value ?? "[]"); } catch { return []; }
}

function normalizeScores(results, minScore = 0.1) {
  if (!results.length) return [];
  const raw = results.map((r) =>
    typeof r.score === "number" && Number.isFinite(r.score) ? r.score : 0,
  );
  const flip = raw.every((v) => v <= 0) && raw.some((v) => v < 0);
  const rel = raw.map((v) => (flip ? -v : v));
  const max = Math.max(...rel);
  if (!(max > 0)) return minScore > 0 ? [] : results.map((r) => ({ ...r, score: 0 }));
  return results
    .map((r, i) => ({ ...r, score: Math.round((rel[i] / max) * 100) / 100 }))
    .filter((r) => r.score >= minScore);
}

async function enrichState(records) {
  if (!records.length) return [];
  const u = await core();
  const db = u.db.getDb();
  const statusStmt = db.prepare("SELECT status FROM memory_lifecycle WHERE chunk_id = ?");
  const visibilityStmt = db.prepare("SELECT visibility FROM memory_visibility WHERE chunk_id = ?");
  const successorStmt = db.prepare("SELECT 1 FROM memory_relations WHERE target_id = ? AND relation_type = 'supersedes' LIMIT 1");
  const sourceStmt = db.prepare("SELECT new_value FROM memory_events WHERE chunk_id = ? AND event_type = 'source_observed' ORDER BY created_at DESC LIMIT 1");
  return records.map((r) => {
    const status = statusStmt.get(r.id)?.status ?? "active";
    return {
      ...r,
      tags: parseTags(r.tags),
      status,
      visibility: visibilityStmt.get(r.id)?.visibility ?? "warm",
      current: status === "active" && !successorStmt.get(r.id),
      sourceRef: sourceStmt.get(r.id)?.new_value ?? null,
    };
  });
}

export function isNormallyVisibleRecord(record) {
  return record.visibility === "hot" || record.visibility === "warm" || record.visibility == null;
}

/**
 * Structured ranked search.
 * Filtering is applied to structured candidates BEFORE final score normalization,
 * result truncation, seen penalty, or presentation.
 */
export async function searchRecords({
  query,
  tags = [],
  agent,
  topK = 6,
  minScore = 0.1,
  mode = "hybrid",
  includeHistory = false,
  includeHidden = false,
  sessionId = null,
  candidateLimit = null,
} = {}) {
  const u = await core();
  const desired = Math.max(1, topK ?? 6);
  // Phase B correctness rule: hidden/history candidates must never starve a lower-ranked
  // eligible record merely because an upstream topK was exhausted. Use the filtered
  // workspace population as a safe upper bound for the raw ranked pool.
  const db = u.db.getDb();
  let countSql = "SELECT COUNT(*) AS c FROM chunks";
  const countWhere = [];
  const countParams = [];
  if (agent) { countWhere.push("agent = ?"); countParams.push(agent); }
  if (tags.length) {
    countWhere.push(`(${tags.map(() => "tags LIKE ?").join(" OR ")})`);
    countParams.push(...tags.map((t) => `%\"${t}\"%`));
  }
  if (countWhere.length) countSql += ` WHERE ${countWhere.join(" AND ")}`;
  const population = Number(db.prepare(countSql).get(...countParams)?.c ?? desired);
  const poolSize = candidateLimit ?? Math.max(desired, population);
  const raw = await u.search.hybridSearchRaw({
    query,
    tags,
    agent,
    topK: poolSize,
    minScore: 0,
    mode,
  });
  const enriched = await enrichState(raw);
  const filtered = enriched.filter((r) =>
    (includeHistory || r.status === "active") &&
    (includeHidden || isNormallyVisibleRecord(r)),
  );
  let ranked = normalizeScores(filtered, minScore);

  let seenCount = 0;
  if (sessionId && ranked.length) {
    const usage = await import(pathToFileURL(path.join(HERE, "usage-sessions.mjs")).href);
    const seenSet = new Set(await usage.getSeenIds(sessionId));
    if (seenSet.size) {
      const fresh = ranked.filter((r) => !seenSet.has(r.id));
      const seen = ranked.filter((r) => seenSet.has(r.id));
      seenCount = seen.length;
      ranked = [...fresh, ...seen];
    }
  }

  return {
    records: ranked.slice(0, desired),
    meta: {
      rawCandidates: raw.length,
      eligibleCandidates: filtered.length,
      filteredCandidates: enriched.length - filtered.length,
      seenCount,
      candidateLimit: poolSize,
    },
  };
}

/** Inventory is structured from SQL directly; no parsing of list_context text. */
export async function listRecords({ agent, tags = [], limit = 100, offset = 0 } = {}) {
  const u = await core();
  const db = u.db.getDb();
  let sql = `
    SELECT c.id, c.topic, c.agent, c.tags, c.importance, c.created_at AS created, c.updated_at AS updated,
           COALESCE(l.status, 'active') AS status,
           COALESCE(v.visibility, 'warm') AS visibility,
           (SELECT e.new_value FROM memory_events e WHERE e.chunk_id = c.id AND e.event_type = 'source_observed' ORDER BY e.created_at DESC LIMIT 1) AS sourceRef,
           CASE WHEN COALESCE(l.status, 'active') = 'active' AND NOT EXISTS (
             SELECT 1 FROM memory_relations mr
              WHERE mr.target_id = c.id AND mr.relation_type = 'supersedes'
           ) THEN 1 ELSE 0 END AS current
      FROM chunks c
      LEFT JOIN memory_lifecycle l ON l.chunk_id = c.id
      LEFT JOIN memory_visibility v ON v.chunk_id = c.id`;
  const where = [];
  const params = [];
  if (agent) { where.push("c.agent = ?"); params.push(agent); }
  if (tags.length) {
    where.push(`(${tags.map(() => "c.tags LIKE ?").join(" OR ")})`);
    params.push(...tags.map((t) => `%\"${t}\"%`));
  }
  if (where.length) sql += ` WHERE ${where.join(" AND ")}`;
  sql += " ORDER BY c.updated_at DESC LIMIT ? OFFSET ?";
  params.push(limit, offset);
  return db.prepare(sql).all(...params).map((r) => ({ ...r, tags: parseTags(r.tags) }));
}

export async function readRecord(id) {
  const u = await core();
  const chunk = u.db.getChunk(id);
  if (!chunk) return null;
  const [record] = await enrichState([{
    id: chunk.id,
    topic: chunk.topic,
    agent: chunk.agent,
    tags: chunk.tags,
    importance: chunk.importance,
    content: chunk.content,
    created: chunk.createdAt,
    updated: chunk.updatedAt,
    expires: chunk.expiresAt,
  }]);
  return record;
}
