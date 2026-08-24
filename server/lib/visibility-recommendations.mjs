/**
 * ANTmemory Visibility Recommendations（ANTmemory 所有コード）
 *
 * ANTmemoryでは自動昇降格を行わない（「参照されない＝不要」ではないため）。
 * システムは推奨を算出するだけで、適用は人間が promote_memory で行う。
 * 推奨の根拠（usage統計）を必ず添付し、説明可能にする。
 */

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

// 閾値（検証コーパスでの実測後に調整）
const THRESHOLDS = {
  hotDays: 30,       // 直近N日以内に参照があること
  hotAccess: 5,      // 累計M回以上参照 かつ 直近N日以内に参照 → hot候補
  coldIdleDays: 90,  // N日未参照のwarm → cold候補
};

function daysAgoIso(days) {
  return new Date(Date.now() - days * 86_400_000).toISOString();
}
function daysSince(iso) {
  if (!iso) return null;
  return Math.floor((Date.now() - new Date(iso).getTime()) / 86_400_000);
}

/**
 * visibility推奨一覧を算出する。
 * 戻り値: [{id, topic, current, recommended, basis}] のみ（DBは変更しない）
 */
export async function getRecommendations() {
  const db = await ensureDb();
  const recs = [];

  // hot候補: active + warm + 累計5回以上参照 かつ 直近30日以内にも参照
  // 注: memory_usage.access_count は累積値であり「直近N日の回数」は保持しない。
  //     直近N日の回数を正しく数えるには参照時刻のイベント集計（memory_recall_log 等）が要るため
  //     ANTmemoryでは「累計回数 + 直近アクセスの新しさ」を条件とする（仕様と実装の一致を優先）。
  const hotCands = db
    .prepare(
      `SELECT c.id, c.topic, c.importance,
              mu.access_count, mu.last_accessed_at,
              mv.visibility AS current
       FROM chunks c
       JOIN memory_lifecycle ml ON ml.chunk_id = c.id AND ml.status = 'active'
       LEFT JOIN memory_visibility mv ON mv.chunk_id = c.id
       JOIN memory_usage mu ON mu.chunk_id = c.id
       WHERE COALESCE(mv.visibility, 'warm') = 'warm'
         AND mu.access_count >= ?
         AND mu.last_accessed_at >= ?`,
    )
    .all(THRESHOLDS.hotAccess, daysAgoIso(THRESHOLDS.hotDays));
  for (const r of hotCands) {
    recs.push({
      id: r.id, topic: r.topic, importance: r.importance,
      current: r.current ?? "warm", recommended: "hot",
      basis: `累計${r.access_count}回参照（最新参照は${daysSince(r.last_accessed_at)}日前）`,
    });
  }

  // cold候補: active + warm + 90日以上未参照
  const coldCands = db
    .prepare(
      `SELECT c.id, c.topic, c.importance,
              COALESCE(mu.last_accessed_at, c.updated_at) AS last_ref,
              mv.visibility AS current
       FROM chunks c
       JOIN memory_lifecycle ml ON ml.chunk_id = c.id AND ml.status = 'active'
       LEFT JOIN memory_visibility mv ON mv.chunk_id = c.id
       LEFT JOIN memory_usage mu ON mu.chunk_id = c.id
       WHERE COALESCE(mv.visibility, 'warm') = 'warm'`,
    )
    .all();
  for (const r of coldCands) {
    const idle = daysSince(r.last_ref);
    if (idle !== null && idle >= THRESHOLDS.coldIdleDays) {
      recs.push({
        id: r.id, topic: r.topic, importance: r.importance,
        current: r.current ?? "warm", recommended: "cold",
        basis: `${idle}日間未参照`,
      });
    }
  }

  // archive候補: superseded
  const archCands = db
    .prepare(
      `SELECT c.id, c.topic, c.importance, mv.visibility AS current
       FROM chunks c
       JOIN memory_lifecycle ml ON ml.chunk_id = c.id AND ml.status = 'superseded'
       LEFT JOIN memory_visibility mv ON mv.chunk_id = c.id`,
    )
    .all();
  for (const r of archCands) {
    if ((r.current ?? "warm") === "archive") continue;
    recs.push({
      id: r.id, topic: r.topic, importance: r.importance,
      current: r.current ?? "warm", recommended: "archive",
      basis: "superseded（後継記録が存在）",
    });
  }

  return { thresholds: THRESHOLDS, recommendations: recs };
}
