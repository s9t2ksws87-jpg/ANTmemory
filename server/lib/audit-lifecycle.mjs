/**
 * ANTmemory audit ライフサイクル層（ANTmemory 所有コード）
 *
 * ANTmemory native chunks を物理削除しない。訂正・撤回・置換・
 * 重要度変更はすべて memory_events / memory_relations / memory_lifecycle への
 * 追記で表現する。物理 DELETE はこの層からは決して実行しない。
 */

import { randomUUID } from "node:crypto";

// ANTmemory native coreへの参照は起動時に一度だけ解決する。
let _db = null;
let _embeddings = null;

/**
 * 初期化。上流サーバーが getDb() を通じて DB を開いた後に呼ばれる必要があるが、
 * node:sqlite の DatabaseSync は同一パスなら複数インスタンスでもWALで動くため、
 * ここでは自分でもう一度 db.js を読んで使う（同一プロセス・同一DB）。
 */
export async function initLifecycle() {
  if (!_db) {
    const loaded = await import("./core.mjs");
    const u = await loaded.loadCore();
    _db = u.db;
    _embeddings = u.embeddings;
  }
  return { db: _db, embeddings: _embeddings };
}

/** audit スキーマ確保（Phase 2 migration と同じ定義・冪等）。新規DBにも対応。 */
export async function ensureAuditSchema() {
  await initLifecycle();
  const db = getDb();
  db.exec(`
CREATE TABLE IF NOT EXISTS memory_events (
  event_id   TEXT PRIMARY KEY,
  chunk_id   TEXT NOT NULL,
  event_type TEXT NOT NULL,
  actor      TEXT NOT NULL,
  model      TEXT,
  provider   TEXT,
  reason     TEXT,
  old_value  TEXT,
  new_value  TEXT,
  created_at TEXT NOT NULL
);
CREATE INDEX IF NOT EXISTS idx_memory_events_chunk ON memory_events(chunk_id);
CREATE INDEX IF NOT EXISTS idx_memory_events_type  ON memory_events(event_type);

CREATE TABLE IF NOT EXISTS memory_relations (
  source_id     TEXT NOT NULL,
  target_id     TEXT NOT NULL,
  relation_type TEXT NOT NULL,
  actor         TEXT NOT NULL,
  reason        TEXT,
  created_at    TEXT NOT NULL,
  PRIMARY KEY (source_id, target_id, relation_type)
);
CREATE INDEX IF NOT EXISTS idx_memory_relations_target ON memory_relations(target_id);

CREATE TABLE IF NOT EXISTS memory_lifecycle (
  chunk_id TEXT PRIMARY KEY,
  status   TEXT NOT NULL DEFAULT 'active',
  updated_at TEXT NOT NULL
);

INSERT OR IGNORE INTO memory_lifecycle (chunk_id, status, updated_at)
  SELECT id, 'active', updated_at FROM chunks;
`);

  // Phase A以降は1つの旧版から複数currentへ分岐させない。
  // 既存DBに過去の分岐が残っている場合、起動不能にせず警告して互換運用を続ける。
  const duplicate = db.prepare(
    `SELECT target_id, COUNT(*) AS c FROM memory_relations
      WHERE relation_type = 'supersedes'
      GROUP BY target_id HAVING COUNT(*) > 1 LIMIT 1`,
  ).get();
  if (!duplicate) {
    db.exec(`CREATE UNIQUE INDEX IF NOT EXISTS idx_memory_relations_one_superseder
      ON memory_relations(target_id) WHERE relation_type = 'supersedes'`);
  } else {
    process.stderr.write(
      `[ant-memory] Phase A warning: legacy supersedes branch remains at ${duplicate.target_id} (${duplicate.c} successors); new branches are blocked by runtime checks.\n`,
    );
  }
}

function getDb() {
  if (!_db) throw new Error("audit-lifecycle: 未初期化です");
  return _db.getDb ? _db.getDb() : _db;
}

function now() {
  return new Date().toISOString();
}

/** lifecycle の現在状態を取得（未登録なら active 扱い） */
export function getStatus(chunkId) {
  const row = getDb()
    .prepare("SELECT status FROM memory_lifecycle WHERE chunk_id = ?")
    .get(chunkId);
  return row?.status ?? "active";
}

function setStatus(chunkId, status) {
  getDb()
    .prepare(
      `INSERT INTO memory_lifecycle (chunk_id, status, updated_at) VALUES (?, ?, ?)
       ON CONFLICT(chunk_id) DO UPDATE SET status = excluded.status, updated_at = excluded.updated_at`,
    )
    .run(chunkId, status, now());
}

function insertEvent({ chunkId, type, actor, model = null, provider = null, reason = null, oldValue = null, newValue = null }) {
  const eventId = randomUUID();
  getDb()
    .prepare(
      `INSERT INTO memory_events (event_id, chunk_id, event_type, actor, model, provider, reason, old_value, new_value, created_at)
       VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?, ?)`,
    )
    .run(eventId, chunkId, type, actor, model, provider, reason, oldValue, newValue, now());
  return eventId;
}

function insertRelation({ sourceId, targetId, relationType, actor, reason }) {
  getDb()
    .prepare(
      `INSERT OR IGNORE INTO memory_relations (source_id, target_id, relation_type, actor, reason, created_at)
       VALUES (?, ?, ?, ?, ?, ?)`,
    )
    .run(sourceId, targetId, relationType, actor, reason ?? null, now());
}

/** Phase D: 既存event/relation基盤を外部構造の最小循環から再利用する。新しい表は増やさない。 */
export function appendMemoryEvent({ chunkId, type, actor, model = null, provider = null, reason = null, oldValue = null, newValue = null }) {
  return insertEvent({ chunkId, type, actor, model, provider, reason, oldValue, newValue });
}

export function appendMemoryRelation({ sourceId, targetId, relationType, actor, reason = null }) {
  insertRelation({ sourceId, targetId, relationType, actor, reason });
  return true;
}

/**
 * supersede_context: 新記録を作り、旧記録との置換関係と理由を残す。
 * 新chunkはnative DBへ直接insertし、actor強制・イベント記録・埋め込み更新を一体化する。
 */
export async function supersedeChunk({
  oldId,
  topic,
  content,
  actor,
  tags = [],
  importance = "medium",
  ttlDays = null,
  expiresAtOverride = undefined,
  sourceRef = null,
  reason,
  model = null,
  provider = null,
}) {
  if (!reason || !String(reason).trim()) {
    throw new Error("supersede_context には理由(reason)が必須です");
  }
  const db = getDb();
  const oldRow = db.prepare("SELECT id FROM chunks WHERE id = ?").get(oldId);
  if (!oldRow) throw new Error(`置換対象が見つかりません: ${oldId}`);
  const oldStatus = getStatus(oldId);
  if (oldStatus !== "active") {
    const successor = findSuccessor(oldId);
    throw new Error(
      `置換対象 ${oldId} は active ではありません (status=${oldStatus})` +
        (successor ? `。後継 ${successor.successor_id} を確認してください` : ""),
    );
  }

  // 新chunkのIDは上流と同じ生成規則（agent+topic+時刻のsha1/10桁）
  const { createHash } = await import("node:crypto");
  const u = await initLifecycle();
  const { insertChunk, updateEmbedding } = u.db;
  const { embed, prepareText } = u.embeddings;

  const seed = `${actor}:${topic}:${Date.now()}:${Math.random()}`;
  const newId = createHash("sha1").update(seed).digest("hex").slice(0, 10);
  const ts = now();
  const expiresAt = expiresAtOverride !== undefined
    ? expiresAtOverride
    : ttlDays
      ? new Date(Date.now() + ttlDays * 86_400_000).toISOString()
      : null;

  db.exec("BEGIN IMMEDIATE");
  try {
    const currentStatus = getStatus(oldId);
    const existingSuccessor = findSuccessor(oldId);
    if (currentStatus !== "active" || existingSuccessor) {
      throw new Error(
        `置換競合: ${oldId} は既に更新されています` +
          (existingSuccessor ? ` (successor=${existingSuccessor.successor_id})` : ` (status=${currentStatus})`),
      );
    }

    insertChunk({
      id: newId,
      topic,
      agent: actor,
      tags,
      importance,
      content,
      embedding: null,
      createdAt: ts,
      updatedAt: ts,
      expiresAt,
    });

    insertRelation({ sourceId: newId, targetId: oldId, relationType: "supersedes", actor, reason });
    insertEvent({ chunkId: oldId, type: "superseded", actor, model, provider, reason, newValue: newId });
    insertEvent({ chunkId: newId, type: "created", actor, model, provider, reason: `supersedes ${oldId}: ${reason}` });
    if (sourceRef && String(sourceRef).trim()) {
      insertEvent({
        chunkId: newId,
        type: "source_observed",
        actor,
        model,
        provider,
        reason: `source for revision of ${oldId}`,
        newValue: String(sourceRef).trim(),
      });
    }
    setStatus(oldId, "superseded");
    setStatus(newId, "active");
    db.exec("COMMIT");
  } catch (err) {
    db.exec("ROLLBACK");
    throw err;
  }

  const text = prepareText({ topic, tags, content });
  embed(text).then((emb) => {
    if (emb) updateEmbedding(newId, emb);
  });

  return { newId, oldId };
}

/** retract_context: 本文を消さず「撤回済み」にする。理由必須。 */
export function retractChunk({ id, actor, reason, model = null, provider = null }) {
  if (!reason || !String(reason).trim()) {
    throw new Error("retract_context には理由(reason)が必須です");
  }
  const db = getDb();
  if (!db.prepare("SELECT id FROM chunks WHERE id = ?").get(id)) {
    throw new Error(`対象が見つかりません: ${id}`);
  }
  insertEvent({ chunkId: id, type: "retracted", actor, model, provider, reason });
  setStatus(id, "retracted");
  return true;
}

/** change_importance: 変更前後・主体・理由をイベントとして残す。理由必須。 */
export function changeImportance({ id, importance, actor, reason, model = null, provider = null }) {
  if (!reason || !String(reason).trim()) {
    throw new Error("change_importance には理由(reason)が必須です");
  }
  if (!["low", "medium", "high", "critical"].includes(importance)) {
    throw new Error(`不正な重要度: ${importance}`);
  }
  const db = getDb();
  const row = db.prepare("SELECT importance FROM chunks WHERE id = ?").get(id);
  if (!row) throw new Error(`対象が見つかりません: ${id}`);
  const oldImp = row.importance;

  db.prepare("UPDATE chunks SET importance = ?, updated_at = ? WHERE id = ?").run(
    importance,
    now(),
    id,
  );
  insertEvent({
    chunkId: id,
    type: "importance_changed",
    actor,
    model,
    provider,
    reason,
    oldValue: oldImp,
    newValue: importance,
  });
  return { from: oldImp, to: importance };
}

/** restore_context: superseded/retracted/expired から active へ戻す。理由必須。 */
export function restoreChunk({ id, actor, reason, model = null, provider = null }) {
  if (!reason || !String(reason).trim()) {
    throw new Error("restore_context には理由(reason)が必須です");
  }
  const db = getDb();
  if (!db.prepare("SELECT id FROM chunks WHERE id = ?").get(id)) {
    throw new Error(`復帰対象が見つかりません: ${id}`);
  }
  const prev = getStatus(id);
  if (prev === "active") {
    throw new Error(`記録 ${id} は既に active です`);
  }
  const successor = findSuccessor(id);
  if (successor) {
    throw new Error(
      `記録 ${id} には後継 ${successor.successor_id} があるため current へ復帰できません。` +
      "旧版の本文は read_context で参照できます",
    );
  }
  insertEvent({ chunkId: id, type: "restored", actor, model, provider, reason, oldValue: prev, newValue: "active" });
  setStatus(id, "active");
  return true;
}

/** 新規chunkをlifecycleへactive登録し、CREATED/visibilityを補う（write直後・冪等）。 */
export function registerNewChunk(chunkId, { actor = "system", model = null, provider = null, reason = "write_context" } = {}) {
   try {
     const db = getDb();
     if (!db.prepare("SELECT id FROM chunks WHERE id = ?").get(chunkId)) return false;
     db.prepare(
       `INSERT OR IGNORE INTO memory_lifecycle (chunk_id, status, updated_at)
        VALUES (?, 'active', ?)`,
     ).run(chunkId, now());
     const hasCreated = db.prepare(
       "SELECT 1 FROM memory_events WHERE chunk_id = ? AND event_type = 'created' LIMIT 1",
     ).get(chunkId);
     if (!hasCreated) {
       insertEvent({ chunkId, type: "created", actor, model, provider, reason });
     }
     // write直後に既に期限切れになっている極短TTLでも、次回再起動までactiveを漏らさない。
     const expired = db.prepare(
       `SELECT 1 FROM chunks WHERE id = ? AND expires_at IS NOT NULL
          AND expires_at < strftime('%Y-%m-%dT%H:%M:%fZ', 'now')`,
     ).get(chunkId);
     if (expired) {
       insertEvent({ chunkId, type: "expired", actor: "system", reason: "TTL失効" });
       setStatus(chunkId, "expired");
     }
     // recall: 新規chunkにもvisibility初期値(warm)を付与（migration後書込の漏れ対策）。
     // 未登録は検索時に暗黙warm扱いだが、verify基準・推奨算出の母集団から漏れないよう明示登録する。
     db.prepare(
       `INSERT OR IGNORE INTO memory_visibility (chunk_id, visibility, reason, changed_at, actor)
        VALUES (?, 'warm', '新規書込時の初期値', ?, 'system')`,
     ).run(chunkId, now());
     return true;
   } catch {
     return false;
   }
}

/** findSuccessor: sourceId の後継（sourceId を supersede した新記録）を返す。
 * 関係は「新 → 旧」で supersedes が入るので、旧IDは target_id 側で引く。
 */
export function findSuccessor(oldChunkId) {
  return getDb()
    .prepare(
      `SELECT source_id AS successor_id, actor, reason, created_at
       FROM memory_relations WHERE target_id = ? AND relation_type = 'supersedes'
       ORDER BY created_at DESC LIMIT 1`,
    )
    .get(oldChunkId) ?? null;
}

/**
 * get_context_history: 対象記憶のイベント＋関係を時系列表示用に返す。
 */
export function getContextHistory(id) {
  const db = getDb();
  const events = db
    .prepare(
      `SELECT event_type, actor, model, provider, reason, old_value, new_value, created_at
       FROM memory_events WHERE chunk_id = ? ORDER BY created_at ASC`,
    )
    .all(id);
  const relationsOut = db
    .prepare(
      `SELECT relation_type, target_id AS other_id, 'out' AS direction, actor, reason, created_at
       FROM memory_relations WHERE source_id = ? ORDER BY created_at ASC`,
    )
    .all(id);
  const relationsIn = db
    .prepare(
      `SELECT relation_type, source_id AS other_id, 'in' AS direction, actor, reason, created_at
       FROM memory_relations WHERE target_id = ? ORDER BY created_at ASC`,
    )
    .all(id);

  return {
    id,
    status: getStatus(id),
    events,
    relations: [...relationsOut, ...relationsIn],
  };
}

/**
 * TTL失効の audit 版: 物理DELETEではなく expired 状態へ遷移させる。
 * 通常検索からは隠れるが本文はDBに残る。
 */
export function expireDueChunks() {
  const db = getDb();
  const due = db
    .prepare(
      `SELECT id FROM chunks WHERE expires_at IS NOT NULL
         AND expires_at < strftime('%Y-%m-%dT%H:%M:%fZ', 'now')`,
    )
    .all();
  let count = 0;
  for (const { id } of due) {
    if (getStatus(id) === "active") {
      insertEvent({ chunkId: id, type: "expired", actor: "system", reason: "TTL失効" });
      setStatus(id, "expired");
      count += 1;
    }
  }
  return count;
}
