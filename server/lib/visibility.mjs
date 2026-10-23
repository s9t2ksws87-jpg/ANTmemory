/**
 * ANTmemory recall Visibility層（ANTmemory 所有コード）
 *
 * visibility（hot/warm/cold/archive）を importance と完全に独立して管理する。
 * 変更はすべて監査イベント（memory_events, actor+reason必須）として残す。
 * 原文・importance・lifecycle状態は決して変更しない。
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

export const VISIBILITIES = ["hot", "warm", "cold", "archive"];

/** visibility取得。未登録なら warm 扱い（migration済みDBでは通常登録済み） */
export async function getVisibility(chunkId) {
  const db = await ensureDb();
  const row = db
    .prepare("SELECT visibility FROM memory_visibility WHERE chunk_id = ?")
    .get(chunkId);
  return row?.visibility ?? "warm";
}

/**
 * visibility変更（手動・reason必須）。
 * 監査イベントとして memory_events へ記録する。
 */
export async function setVisibility({ id, to, actor, reason }) {
  if (!VISIBILITIES.includes(to)) {
    throw new Error(`不正なvisibility: ${to}（有効値: ${VISIBILITIES.join(", ")}）`);
  }
  if (!reason || !String(reason).trim()) {
    throw new Error("visibility変更には理由(reason)が必須です");
  }
  const db = await ensureDb();
  if (!db.prepare("SELECT id FROM chunks WHERE id = ?").get(id)) {
    throw new Error(`対象が見つかりません: ${id}`);
  }

  const from = await getVisibility(id);
  if (from === to) {
    return { from, to, unchanged: true };
  }

  db.exec("BEGIN");
  try {
    db.prepare(
      `INSERT INTO memory_visibility (chunk_id, visibility, reason, changed_at, actor)
       VALUES (?, ?, ?, ?, ?)
       ON CONFLICT(chunk_id) DO UPDATE SET
         visibility = excluded.visibility,
         reason = excluded.reason,
         changed_at = excluded.changed_at,
         actor = excluded.actor`,
    ).run(id, to, String(reason).trim(), new Date().toISOString(), actor);

    // 監査イベント（importance_changed と同型）
    db.prepare(
      `INSERT INTO memory_events (event_id, chunk_id, event_type, actor, reason, old_value, new_value, created_at)
       VALUES (?, ?, 'visibility_changed', ?, ?, ?, ?, ?)`,
    ).run(randomUUID(), id, actor, String(reason).trim(), from, to, new Date().toISOString());

    db.exec("COMMIT");
  } catch (e) {
    db.exec("ROLLBACK");
    throw e;
  }
  return { from, to };
}

/**
 * 検索結果に適用するvisibilityフィルタの既定設定。
 * 通常検索では hot/warm のみ許可し cold/archive を抑制する。
 */
export function isNormallyVisible(visibility) {
  return visibility === "hot" || visibility === "warm" || visibility === undefined || visibility === null;
}
