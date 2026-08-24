import http from "node:http";
import fs from "node:fs";
import path from "node:path";
import { URL, fileURLToPath } from "node:url";
import { DatabaseSync } from "node:sqlite";
import { exec } from "node:child_process";

// ─── 設定（可搬化。ANT_MEMORY_ROOT または相対配置で解決）─────────────────────
const HERE = path.dirname(fileURLToPath(import.meta.url));
const ROOT = process.env.ANT_MEMORY_ROOT
  ? path.resolve(process.env.ANT_MEMORY_ROOT)
  : path.resolve(HERE, "..");
const PORT = process.env.PORT ? parseInt(process.env.PORT, 10) : 3737;
// 既定は localhost のみ。LAN公開する場合は明示的に HOST=0.0.0.0 を指定。
const HOST = process.env.HOST || "127.0.0.1";
const STORES_DIR = path.join(ROOT, "stores");
const PUBLIC_DIR = path.join(HERE, "public");

// Helper to sanitize path & open DB safely
function getWorkspaceDb(workspaceName) {
  if (!workspaceName || typeof workspaceName !== "string") return null;
  const sanitized = path.basename(workspaceName);
  const dbPath = path.join(STORES_DIR, sanitized, "store.db");
  if (!fs.existsSync(dbPath)) return null;
  try {
    return { db: new DatabaseSync(dbPath, { readOnly: true }), path: dbPath };
  } catch (e) {
    console.error(`Failed to open DB for ${sanitized}:`, e);
    return null;
  }
}

// MIME types for static server
const MIME_TYPES = {
  ".html": "text/html; charset=utf-8",
  ".css": "text/css; charset=utf-8",
  ".js": "application/javascript; charset=utf-8",
  ".json": "application/json; charset=utf-8",
  ".png": "image/png",
  ".jpg": "image/jpeg",
  ".svg": "image/svg+xml",
  ".ico": "image/x-icon"
};

// ─── audit: FTS5 検索（UIの「FTS5」表記と実装を一致させる）──────────────────────
// trigram トークナイザの FTS 索引が無いDBでは LIKE へ自動フォールバックする。
function searchMemories(db, q, params) {
  const { agent, importance, tag, sort, limit } = params;

  let orderSql = " ORDER BY updated_at DESC";
  if (sort === "created_desc") orderSql = " ORDER BY created_at DESC";
  else if (sort === "created_asc") orderSql = " ORDER BY created_at ASC";
  else if (sort === "updated_asc") orderSql = " ORDER BY updated_at ASC";
  else if (sort === "importance") {
    orderSql = " ORDER BY CASE importance WHEN 'critical' THEN 1 WHEN 'high' THEN 2 WHEN 'medium' THEN 3 WHEN 'low' THEN 4 ELSE 5 END, updated_at DESC";
  }

  const baseSelect = "SELECT id, topic, agent, tags, importance, content, created_at, updated_at, expires_at FROM chunks";
  const filters = [];
  const args = [];
  if (agent) { filters.push("agent = ?"); args.push(agent); }
  if (importance) { filters.push("importance = ?"); args.push(importance); }
  if (tag) {
    filters.push("(tags LIKE ? OR tags LIKE ? OR tags LIKE ? OR tags LIKE ?)");
    args.push(`["${tag}"]`, `["${tag}",%`, `%,"${tag}"]`, `%,"${tag}",%`);
  }
  const where = filters.length ? " WHERE " + filters.join(" AND ") : "";

  const qTrim = (q || "").trim();
  if (!qTrim) {
    const rows = db.prepare(`${baseSelect}${where}${orderSql} LIMIT ?`).all(...args, limit);
    return rows;
  }

  // FTS5 が使えるか（索引の存在確認）
  const hasFts = db.prepare(
    "SELECT COUNT(*) AS c FROM sqlite_master WHERE type='table' AND name='chunks_fts'",
  ).get().c > 0;

  if (hasFts) {
    // 各語をフレーズとしてクォート（ハイフン入り語の構文エラー対策・上流パッチと同じ方式）
    const ftsQuery = qTrim
      .replace(/["*^:(){}[\]]/g, " ")
      .split(/\s+/)
      .filter((t) => t.length > 1)
      .map((t) => `"${t.replaceAll('"', "")}"`)
      .join(" OR ");
    if (ftsQuery) {
      try {
        const ftsWhere = filters.map((f) => `c.${f.startsWith("(") ? "tags LIKE" : f}`);
        // filters 内の tags 条件は c. を付け直す
        const conds = [];
        if (agent) conds.push("c.agent = ?");
        if (importance) conds.push("c.importance = ?");
        if (tag) { conds.push("(c.tags LIKE ? OR c.tags LIKE ? OR c.tags LIKE ? OR c.tags LIKE ?)"); }
        const condSql = conds.length ? " AND " + conds.join(" AND ") : "";
        const sql = `
          SELECT c.id, c.topic, c.agent, c.tags, c.importance, c.content, c.created_at, c.updated_at, c.expires_at, rank
          FROM chunks_fts
          JOIN chunks c ON c.id = chunks_fts.id
          WHERE chunks_fts MATCH ?${condSql}
          ORDER BY rank
          LIMIT ?`;
        return db.prepare(sql).all(ftsQuery, ...args, limit);
      } catch (e) {
        console.error("FTS search failed, falling back to LIKE:", e.message);
      }
    }
  }

  // フォールバック: LIKE 検索
  const likeConds = [...filters, "(topic LIKE ? OR content LIKE ? OR tags LIKE ? OR agent LIKE ?)"];
  const likeArgs = [...args, `%${qTrim}%`, `%${qTrim}%`, `%${qTrim}%`, `%${qTrim}%`];
  return db.prepare(`${baseSelect} WHERE ${likeConds.join(" AND ")}${orderSql} LIMIT ?`).all(...likeArgs, limit);
}

const server = http.createServer((req, res) => {
  const parsedUrl = new URL(req.url, `http://${req.headers.host || "localhost"}`);
  const pathname = parsedUrl.pathname;

  const sendJson = (data, statusCode = 200) => {
    res.writeHead(statusCode, {
      "Content-Type": "application/json; charset=utf-8",
      // 同一生成元からのアクセス専用。外部オリジンからの読み取りを許さない。
      "Access-Control-Allow-Origin": "null",
      "Cache-Control": "no-cache"
    });
    res.end(JSON.stringify(data));
  };

  const sendError = (message, statusCode = 400) => {
    sendJson({ error: message }, statusCode);
  };

  // API Routes
  if (pathname.startsWith("/api/")) {
    try {
      // 1. Get all workspaces
      if (pathname === "/api/workspaces") {
        if (!fs.existsSync(STORES_DIR)) {
          return sendJson({ workspaces: [] });
        }
        const entries = fs.readdirSync(STORES_DIR, { withFileTypes: true });
        const list = [];
        for (const entry of entries) {
          if (entry.isDirectory()) {
            const dbPath = path.join(STORES_DIR, entry.name, "store.db");
            if (fs.existsSync(dbPath)) {
              try {
                const stat = fs.statSync(dbPath);
                const dbObj = getWorkspaceDb(entry.name);
                let chunkCount = 0;
                let stateCount = 0;
                let lastUpdated = null;
                if (dbObj) {
                  const cRes = dbObj.db.prepare("SELECT COUNT(*) as cnt FROM chunks").get();
                  const sRes = dbObj.db.prepare("SELECT COUNT(*) as cnt FROM state").get();
                  chunkCount = cRes ? cRes.cnt : 0;
                  stateCount = sRes ? sRes.cnt : 0;

                  const recent = dbObj.db.prepare("SELECT MAX(updated_at) as max_up FROM chunks").get();
                  if (recent && recent.max_up) {
                    lastUpdated = recent.max_up;
                  }
                  dbObj.db.close();
                }
                list.push({
                  name: entry.name,
                  sizeBytes: stat.size,
                  chunkCount,
                  stateCount,
                  lastUpdated: lastUpdated || stat.mtime.toISOString()
                });
              } catch (e) {
                console.error(`Error reading stat for ${entry.name}:`, e);
              }
            }
          }
        }
        list.sort((a, b) => b.chunkCount - a.chunkCount || new Date(b.lastUpdated) - new Date(a.lastUpdated));
        return sendJson({ workspaces: list });
      }

      // Workspace parameter is required for remaining APIs
      const wsName = parsedUrl.searchParams.get("workspace");
      if (!wsName) {
        return sendError("Workspace parameter is required");
      }

      const wsDb = getWorkspaceDb(wsName);
      if (!wsDb) {
        return sendError(`Workspace DB not found: ${wsName}`, 404);
      }

      const { db } = wsDb;

      try {
        // 2. Search & List Memories（audit: FTS5 + lifecycle フィルタ）
        if (pathname === "/api/memories") {
          const q = parsedUrl.searchParams.get("q") || "";
          const agent = parsedUrl.searchParams.get("agent") || "";
          const importance = parsedUrl.searchParams.get("importance") || "";
          const tag = parsedUrl.searchParams.get("tag") || "";
          const sort = parsedUrl.searchParams.get("sort") || "updated_desc";
          const limit = parseInt(parsedUrl.searchParams.get("limit") || "500", 10);
          const includeHistory = parsedUrl.searchParams.get("include_history") === "true";

          let rows = searchMemories(db, q, { agent, importance, tag, sort, limit });

          // audit: lifecycle フィルタ（既定は active のみ）
          if (!includeHistory) {
            const hasLifecycle = db.prepare(
              "SELECT COUNT(*) AS c FROM sqlite_master WHERE type='table' AND name='memory_lifecycle'",
            ).get().c > 0;
            if (hasLifecycle) {
              const statusMap = new Map(
                db.prepare("SELECT chunk_id, status FROM memory_lifecycle").all().map((r) => [r.chunk_id, r.status]),
              );
              rows = rows.filter((r) => (statusMap.get(r.id) ?? "active") === "active");
            }
          }

          const memories = rows.map(r => {
            let tagsArr = [];
            try {
              tagsArr = JSON.parse(r.tags);
            } catch (e) {
              tagsArr = [];
            }
            const { rank, ...rest } = r;
            return { ...rest, tags: tagsArr };
          });

          return sendJson({ memories, total: memories.length });
        }

        // audit/recall: Get memory history (events + relations + status + visibility + recall log)
        if (pathname === "/api/history") {
          const id = parsedUrl.searchParams.get("id");
          if (!id) return sendError("id parameter is required");

          const hasAuditSchema = db.prepare(
            "SELECT COUNT(*) AS c FROM sqlite_master WHERE type='table' AND name='memory_events'",
          ).get().c > 0;
          const hasRecallSchema = db.prepare(
            "SELECT COUNT(*) AS c FROM sqlite_master WHERE type='table' AND name='memory_recall_log'",
          ).get().c > 0;

          const lcRow = hasAuditSchema
            ? db.prepare("SELECT status FROM memory_lifecycle WHERE chunk_id = ?").get(id)
            : null;
          const events = hasAuditSchema ? db.prepare(
            `SELECT event_type, actor, model, provider, reason, old_value, new_value, created_at
             FROM memory_events WHERE chunk_id = ? ORDER BY created_at ASC`,
          ).all(id) : [];
          const relations = hasAuditSchema ? [
            ...db.prepare(
              `SELECT relation_type, target_id AS other_id, 'out' AS direction, actor, reason, created_at
               FROM memory_relations WHERE source_id = ?`,
            ).all(id),
            ...db.prepare(
              `SELECT relation_type, source_id AS other_id, 'in' AS direction, actor, reason, created_at
               FROM memory_relations WHERE target_id = ?`,
            ).all(id),
          ] : [];

          // recall: visibility / usage / recall log
          let visibility = null, usageRow = null, recalls = [];
          if (hasRecallSchema) {
            const visRow = db.prepare("SELECT visibility, reason, changed_at, actor FROM memory_visibility WHERE chunk_id = ?").get(id);
            visibility = visRow?.visibility ?? "warm";
            usageRow = db.prepare("SELECT access_count, last_accessed_at, last_session_id FROM memory_usage WHERE chunk_id = ?").get(id) ?? null;
            recalls = db.prepare(
              "SELECT query, reason, score, rank, created_at FROM memory_recall_log WHERE chunk_id = ? ORDER BY created_at DESC LIMIT 20",
            ).all(id);
          }

          return sendJson({
            id,
            status: lcRow?.status ?? "active",
            visibility,
            usage: usageRow,
            events,
            relations,
            recalls: recalls.map((r) => {
              let reasons = [];
              try { reasons = JSON.parse(r.reason); } catch {}
              return { ...r, reasons };
            }),
          });
        }

        // 3. Get State Table
        if (pathname === "/api/state") {
          const rows = db.prepare("SELECT key, value, updated_at FROM state ORDER BY updated_at DESC").all();
          return sendJson({ state: rows });
        }

        // audit: Get agents list dynamically
        if (pathname === "/api/agents") {
          const rows = db.prepare("SELECT agent, COUNT(*) as count FROM chunks GROUP BY agent ORDER BY count DESC").all();
          return sendJson({ agents: rows });
        }

        // 4. Get Tags list for filter
        if (pathname === "/api/tags") {
          const rows = db.prepare("SELECT tags FROM chunks").all();
          const tagMap = {};
          for (const r of rows) {
            try {
              const parsed = JSON.parse(r.tags);
              if (Array.isArray(parsed)) {
                for (const t of parsed) {
                  if (t) tagMap[t] = (tagMap[t] || 0) + 1;
                }
              }
            } catch (e) {}
          }
          const sortedTags = Object.entries(tagMap)
            .map(([name, count]) => ({ name, count }))
            .sort((a, b) => b.count - a.count);
          return sendJson({ tags: sortedTags });
        }

        // 5. Get Statistics
        if (pathname === "/api/stats") {
          const totalChunks = db.prepare("SELECT COUNT(*) as cnt FROM chunks").get().cnt;
          const totalStates = db.prepare("SELECT COUNT(*) as cnt FROM state").get().cnt;

          const agentRows = db.prepare("SELECT agent, COUNT(*) as count FROM chunks GROUP BY agent ORDER BY count DESC").all();
          const importanceRows = db.prepare("SELECT importance, COUNT(*) as count FROM chunks GROUP BY importance ORDER BY count DESC").all();

          const recentChunks = db.prepare("SELECT id, topic, agent, updated_at FROM chunks ORDER BY updated_at DESC LIMIT 5").all();

          return sendJson({
            stats: {
              totalChunks,
              totalStates,
              agents: agentRows,
              importance: importanceRows,
              recentChunks
            }
          });
        }

        // 6. Export Workspace Data
        if (pathname === "/api/export") {
          const format = parsedUrl.searchParams.get("format") || "json";
          const rows = db.prepare("SELECT * FROM chunks ORDER BY updated_at DESC").all();

          if (format === "markdown") {
            let mdContent = `# ANTmemory Export - Workspace: ${wsName}\n\n`;
            mdContent += `Exported at: ${new Date().toISOString()}\nTotal memories: ${rows.length}\n\n---\n\n`;
            for (const r of rows) {
              mdContent += `## [${(r.importance || 'medium').toUpperCase()}] ${r.topic}\n`;
              mdContent += `- **ID**: \`${r.id}\` | **Agent**: \`${r.agent}\` | **Updated**: ${r.updated_at}\n`;
              mdContent += `- **Tags**: ${r.tags}\n\n`;
              mdContent += `${r.content}\n\n---\n\n`;
            }
            res.writeHead(200, {
              "Content-Type": "text/markdown; charset=utf-8",
              "Content-Disposition": `attachment; filename="antmemory_${encodeURIComponent(wsName)}.md"`
            });
            return res.end(mdContent);
          } else {
            res.writeHead(200, {
              "Content-Type": "application/json; charset=utf-8",
              "Content-Disposition": `attachment; filename="antmemory_${encodeURIComponent(wsName)}.json"`
            });
            return res.end(JSON.stringify(rows, null, 2));
          }
        }

        return sendError("Unknown API Endpoint", 404);
      } finally {
        db.close();
      }
    } catch (err) {
      console.error("API Processing Error:", err);
      return sendError(`Internal Server Error: ${err.message}`, 500);
    }
  }

  // Serve static files
  let safePath = path.normalize(pathname).replace(/^(\.\.[\/\\])+/, "");
  if (safePath === "/" || safePath === "\\") {
    safePath = "/index.html";
  }

  const filePath = path.join(PUBLIC_DIR, safePath);

  if (!filePath.startsWith(PUBLIC_DIR)) {
    res.writeHead(403);
    return res.end("Forbidden");
  }

  fs.stat(filePath, (err, stats) => {
    if (err || !stats.isFile()) {
      res.writeHead(404, { "Content-Type": "text/plain; charset=utf-8" });
      return res.end("404 Not Found");
    }

    const ext = path.extname(filePath).toLowerCase();
    const contentType = MIME_TYPES[ext] || "application/octet-stream";

    res.writeHead(200, { "Content-Type": contentType });
    const stream = fs.createReadStream(filePath);
    stream.pipe(res);
  });
});

function startServer(p) {
  server.listen(p, HOST, () => {
    const displayUrl = HOST === "127.0.0.1" ? `http://localhost:${p}` : `http://${HOST}:${p}`;
    console.log(`\n==================================================`);
    console.log(`  🚀 ANTmemory Database Viewer is running!`);
    console.log(`  URL: ${displayUrl}`);
    console.log(`  Bind: ${HOST} (localhost限定・LAN公開は HOST=0.0.0.0 で明示起動)`);
    console.log(`  Stores: ${STORES_DIR}`);
    console.log(`==================================================\n`);

    if (process.platform === "win32" && !process.env.NO_OPEN) {
      exec(`start ${displayUrl}`);
    }
  });

  server.on("error", (err) => {
    if (err.code === "EADDRINUSE") {
      console.log(`Port ${p} is in use, trying ${p + 1}...`);
      server.removeAllListeners("error");
      startServer(p + 1);
    } else {
      console.error("Server Error:", err);
    }
  });
}

startServer(PORT);
