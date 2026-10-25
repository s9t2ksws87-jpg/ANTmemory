/**
 * Hybrid search engine combining FTS5 BM25 (native SQLite) and vector similarity.
 *
 * Search modes:
 *   - "hybrid"   — FTS5 BM25 + vector cosine similarity merged via Reciprocal Rank Fusion
 *   - "bm25"     — FTS5 only (no embeddings needed)
 *   - "semantic"  — Vector similarity only
 *
 * Falls back to BM25-only if embeddings are not available.
 */

import { searchFTS, getAllEmbeddings, getChunk } from "./native-db.mjs";
import { embed, isEmbeddingAvailable } from "./native-embeddings.mjs";

/**
 * Converts a full FTS result into the enriched output format.
 */
function ftsResultToEnriched(r) {
  return {
    id: r.id,
    topic: r.topic,
    agent: r.agent,
    tags: r.tags,
    importance: r.importance,
    score: r.score, // 丸めは normalizeAndFilter で行う
    content: r.content,
    created: r.created,
    updated: r.updated,
  };
}

// ─── Vector Search ──────────────────────────────────────────────────────────

/**
 * Computes cosine similarity between two Float32Arrays.
 * Assumes both vectors are already L2-normalized (dot product = cosine sim).
 */
function cosineSimilarity(a, b) {
  let dot = 0;
  for (let i = 0; i < a.length; i++) dot += a[i] * b[i];
  return dot;
}

/**
 * Brute-force vector search over all chunk embeddings.
 */
function vectorSearch(queryEmbedding, { agent, tags = [], topK = 18 }) {
  const embeddings = getAllEmbeddings({ agent, tags });
  if (!embeddings.length) return [];

  return embeddings
    .map(({ id, embedding }) => ({
      id,
      score: cosineSimilarity(queryEmbedding, embedding),
    }))
    .filter((r) => r.score > 0)
    .sort((a, b) => b.score - a.score)
    .slice(0, topK);
}

// ─── Fusion ─────────────────────────────────────────────────────────────────

/**
 * Reciprocal Rank Fusion — merges two ranked lists into one.
 */
function reciprocalRankFusion(bm25Hits, vecHits, wBM25 = 0.4, wVec = 0.6) {
  const K = 60;
  const scores = new Map();

  bm25Hits.forEach(({ id }, rank) => {
    scores.set(id, (scores.get(id) || 0) + wBM25 / (K + rank + 1));
  });

  vecHits.forEach(({ id }, rank) => {
    scores.set(id, (scores.get(id) || 0) + wVec / (K + rank + 1));
  });

  return [...scores.entries()]
    .map(([id, score]) => ({ id, score }))
    .sort((a, b) => b.score - a.score);
}

// ─── Main Search ────────────────────────────────────────────────────────────

/**
 * Main search function — performs hybrid, BM25, or semantic search.
 *
 * @param {object} opts
 * @param {string}   opts.query
 * @param {string[]} [opts.tags]
 * @param {string}   [opts.agent]
 * @param {number}   [opts.topK]
 * @param {number}   [opts.minScore]
 * @param {string}   [opts.mode]  - "hybrid" | "bm25" | "semantic"
 * @returns {Promise<Array>}
 */
/**
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

export async function hybridSearchRaw({
  query,
  tags = [],
  agent,
  topK = 6,
  minScore = 0.1,
  mode = "hybrid",
}) {
  const candidateK = topK * 3;
  const embeddingsReady = isEmbeddingAvailable();

  // Determine effective mode
  let effectiveMode = mode;
  if ((mode === "hybrid" || mode === "semantic") && !embeddingsReady) {
    effectiveMode = "bm25";
  }

  // BM25-only: searchFTS already returns full chunk data, no enrichment needed
  if (effectiveMode === "bm25") {
    const results = searchFTS({ query, agent, tags, topK: candidateK });
    return results.slice(0, topK).map(ftsResultToEnriched);
  }

  // Semantic-only
  if (effectiveMode === "semantic") {
    const queryEmbedding = await embed(query);
    if (!queryEmbedding) {
      const results = searchFTS({ query, agent, tags, topK: candidateK });
      return results.slice(0, topK).map(ftsResultToEnriched);
    }
    const vecHits = vectorSearch(queryEmbedding, { agent, tags, topK: candidateK });
    return enrichVectorResults(vecHits.slice(0, topK));
  }

  // Hybrid: run FTS5 (sync) and embed query (async) in parallel
  const queryEmbeddingPromise = embed(query);
  const bm25Hits = searchFTS({ query, agent, tags, topK: candidateK });
  const queryEmbedding = await queryEmbeddingPromise;

  if (!queryEmbedding) {
    return bm25Hits.slice(0, topK).map(ftsResultToEnriched);
  }

  const vecHits = vectorSearch(queryEmbedding, { agent, tags, topK: candidateK });
  const fused = reciprocalRankFusion(bm25Hits, vecHits);

  // Enrich fused results: build lookup from BM25 data, only fetch missing from DB
  const bm25Map = new Map(bm25Hits.map((r) => [r.id, r]));
  const topResults = fused.slice(0, topK);
  const enriched = [];

  for (const { id, score } of topResults) {
    const cached = bm25Map.get(id);
    if (cached) {
      enriched.push({
        id: cached.id,
        topic: cached.topic,
        agent: cached.agent,
        tags: cached.tags,
        importance: cached.importance,
        score, // 丸めは normalizeAndFilter で行う
        content: cached.content,
        created: cached.created,
        updated: cached.updated,
      });
    } else {
      const chunk = getChunk(id);
      if (!chunk) continue;
      enriched.push({
        id: chunk.id,
        topic: chunk.topic,
        agent: chunk.agent,
        tags: chunk.tags,
        importance: chunk.importance,
        score, // 丸めは normalizeAndFilter で行う
        content: chunk.content,
        created: chunk.createdAt,
        updated: chunk.updatedAt,
      });
    }
  }

  return enriched;
}

/**
 * Enriches vector-only results by fetching full chunk data from DB.
 */
function enrichVectorResults(vecHits) {
  const enriched = [];
  for (const { id, score } of vecHits) {
    const chunk = getChunk(id);
    if (!chunk) continue;
    enriched.push({
      id: chunk.id,
      topic: chunk.topic,
      agent: chunk.agent,
      tags: chunk.tags,
      importance: chunk.importance,
      score, // 丸めは normalizeAndFilter で行う
      content: chunk.content,
      created: chunk.createdAt,
      updated: chunk.updatedAt,
    });
  }
  return enriched;
}
