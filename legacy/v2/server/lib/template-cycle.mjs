/**
 * Phase D — Template-kun minimal observation cycle for ANTmemory.
 *
 * No new schema is introduced. The cycle is represented with the structures
 * ANTmemory already owns:
 *   chunks           = immutable observation records
 *   memory_events    = source/provenance observations
 *   memory_relations = lineage (derived_from / resolves)
 *
 * Deliberately small model:
 *   observation = something observed / known evidence
 *   unknown     = an explicitly isolated gap after observation
 *   hypothesis  = a provisional explanation for an UNKNOWN
 *
 * Mere absence of observation ("未知") is not stored. It becomes an UNKNOWN
 * only when an observer explicitly isolates the gap.
 */

import { loadCore } from "./core.mjs";
import {
  appendMemoryEvent,
  appendMemoryRelation,
  initLifecycle,
  registerNewChunk,
} from "./audit-lifecycle.mjs";

const KIND_TAG = {
  observation: "tk:observation",
  unknown: "tk:unknown",
  hypothesis: "tk:hypothesis",
};

let _core = null;

async function core() {
  if (!_core) _core = await loadCore();
  return _core;
}

function uniq(items) {
  return [...new Set(items.filter(Boolean))];
}

function parseTags(raw) {
  if (Array.isArray(raw)) return raw;
  if (!raw) return [];
  try {
    const parsed = JSON.parse(raw);
    return Array.isArray(parsed) ? parsed : [];
  } catch {
    return [];
  }
}

function dbFrom(coreObj) {
  return coreObj.db.getDb ? coreObj.db.getDb() : coreObj.db;
}

function rowById(db, id) {
  return db.prepare("SELECT id, topic, tags, importance, content, expires_at FROM chunks WHERE id = ?").get(id) ?? null;
}

function isUnknownRow(row) {
  return !!row && parseTags(row.tags).includes(KIND_TAG.unknown);
}

/**
 * Record one step of the minimal cycle.
 *
 * parentId expresses lineage only. The only special terminal relation is
 * `resolves`, created by an observation that explicitly resolves an UNKNOWN.
 * The UNKNOWN body remains untouched and queryable forever.
 */
export async function recordObservation({
  kind,
  topic,
  content,
  actor,
  tags = [],
  importance = "medium",
  ttlDays = null,
  parentId = null,
  sourceRef = null,
  resolvesUnknown = false,
  reason = null,
  model = null,
  provider = null,
}) {
  if (!Object.hasOwn(KIND_TAG, kind)) throw new Error(`不正なkind: ${kind}`);
  if (!topic || !String(topic).trim()) throw new Error("topic は必須です");
  if (!content || !String(content).trim()) throw new Error("content は必須です");
  if (kind === "hypothesis" && !parentId) {
    throw new Error("hypothesis には parent_id（対象UNKNOWN）が必須です");
  }
  if (resolvesUnknown && kind !== "observation") {
    throw new Error("resolves_unknown=true は observation でのみ指定できます");
  }

  await initLifecycle();
  const c = await core();
  const db = dbFrom(c);

  let parent = null;
  if (parentId) {
    parent = rowById(db, parentId);
    if (!parent) throw new Error(`parent_id が見つかりません: ${parentId}`);
  }
  if (kind === "hypothesis" && !isUnknownRow(parent)) {
    throw new Error(`hypothesis の parent_id は UNKNOWN である必要があります: ${parentId}`);
  }
  if (resolvesUnknown && (!parent || !isUnknownRow(parent))) {
    throw new Error(`resolves_unknown=true の parent_id は UNKNOWN である必要があります: ${parentId}`);
  }

  const mergedTags = uniq([KIND_TAG[kind], ...tags]);
  const result = await c.store.writeChunk({
    topic,
    content,
    agent: actor,
    tags: mergedTags,
    importance,
    ttlDays,
  });
  registerNewChunk(result.id, { actor, model, provider, reason: `observe_context:${kind}` });

  if (parentId) {
    appendMemoryRelation({
      sourceId: result.id,
      targetId: parentId,
      relationType: "derived_from",
      actor,
      reason: reason ?? `${kind} derived from ${parentId}`,
    });
  }
  if (resolvesUnknown) {
    appendMemoryRelation({
      sourceId: result.id,
      targetId: parentId,
      relationType: "resolves",
      actor,
      reason: reason ?? `reobservation resolves UNKNOWN ${parentId}`,
    });
    appendMemoryEvent({
      chunkId: parentId,
      type: "unknown_resolved",
      actor,
      model,
      provider,
      reason: reason ?? "再観測によってUNKNOWNを解決",
      newValue: result.id,
    });
  }
  if (sourceRef && String(sourceRef).trim()) {
    appendMemoryEvent({
      chunkId: result.id,
      type: "source_observed",
      actor,
      model,
      provider,
      reason: reason ?? `source for ${kind}`,
      newValue: String(sourceRef).trim(),
    });
  }

  return {
    id: result.id,
    kind,
    tags: mergedTags,
    parentId,
    sourceRef: sourceRef ? String(sourceRef).trim() : null,
    resolvedUnknownId: resolvesUnknown ? parentId : null,
  };
}

/** Return UNKNOWN records without destroying resolved ones. */
export async function listUnknowns({ includeResolved = false, limit = 50 } = {}) {
  await initLifecycle();
  const c = await core();
  const db = dbFrom(c);
  const rows = db.prepare(
    `SELECT c.id, c.topic, c.agent, c.tags, c.importance, c.created_at, c.updated_at,
            COALESCE(l.status, 'active') AS status,
            COALESCE(v.visibility, 'warm') AS visibility,
            (SELECT r.source_id FROM memory_relations r
               WHERE r.target_id = c.id AND r.relation_type = 'resolves'
               ORDER BY r.created_at DESC LIMIT 1) AS resolved_by,
            (SELECT COUNT(*) FROM memory_relations r
               WHERE r.target_id = c.id AND r.relation_type = 'derived_from') AS derived_count,
            (SELECT e.new_value FROM memory_events e
               WHERE e.chunk_id = c.id AND e.event_type = 'source_observed'
               ORDER BY e.created_at DESC LIMIT 1) AS source_ref
       FROM chunks c
       LEFT JOIN memory_lifecycle l ON l.chunk_id = c.id
       LEFT JOIN memory_visibility v ON v.chunk_id = c.id
      WHERE c.tags LIKE '%"tk:unknown"%'
        AND (? = 1 OR NOT EXISTS (
          SELECT 1 FROM memory_relations rr
           WHERE rr.target_id = c.id AND rr.relation_type = 'resolves'
        ))
      ORDER BY c.updated_at DESC
      LIMIT ?`,
  ).all(includeResolved ? 1 : 0, Math.max(1, Math.min(500, limit)));

  return rows.map((r) => ({ ...r, tags: parseTags(r.tags), resolved: !!r.resolved_by }));
}
