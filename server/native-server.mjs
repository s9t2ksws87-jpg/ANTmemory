#!/usr/bin/env node
/**
 * ANTmemory native MCP server entry (Phase C1).
 *
 * Phase C native server entry. Storage/search/embedding/state primitives are
 * ANTmemory-owned and exposed through lib/core.mjs.
 */

import { McpServer } from "@modelcontextprotocol/sdk/server/mcp.js";
import { StdioServerTransport } from "@modelcontextprotocol/sdk/server/stdio.js";
import { z } from "zod/v4";
import { loadCore } from "./lib/core.mjs";
import { registerAntTools } from "./lib/register-ant-tools.mjs";

const upstream = await loadCore();
await upstream.store.initStore();

const trustedActor = process.env.ANT_MEMORY_CLIENT || "unknown";
const model = process.env.ANT_MEMORY_MODEL || null;
const provider = process.env.ANT_MEMORY_PROVIDER || null;

const server = new McpServer({ name: "ant-memory", version: "2.0.0" });

server.tool(
  "search_context",
  [
    "Search stored memory chunks using hybrid ranking (BM25 + semantic similarity).",
    "Call this at the start of any task to retrieve relevant prior knowledge, decisions, and outputs before generating a response.",
    "Supports 'hybrid', 'bm25', and 'semantic' modes.",
  ].join(" "),
  {
    query: z.string().describe("Search query."),
    tags: z.array(z.string()).optional().describe("Narrow results to chunks matching any of these tags."),
    agent: z.string().optional().describe("Narrow results to chunks written by a specific agent ID."),
    top_k: z.number().int().min(1).max(20).optional().describe("Maximum results (default 6)."),
    min_score: z.number().min(0).optional().describe("Minimum candidate-relative score. The best returned candidate is normalized to 1.0, so 1.0 does not mean guaranteed relevance."),
    search_mode: z.enum(["hybrid", "bm25", "semantic"]).optional(),
  },
  async ({ query, tags, agent, top_k, min_score, search_mode }) => {
    const results = await upstream.store.searchChunks({
      query,
      tags: tags ?? [],
      agent,
      topK: top_k ?? 6,
      minScore: min_score ?? 0.1,
      mode: search_mode ?? "hybrid",
    });
    if (!results.length) return { content: [{ type: "text", text: "No matching chunks found." }] };
    const body = results.map((r) => [
      `### [score: ${r.score}] ${r.topic}`,
      `**id:** \`${r.id}\` | **agent:** ${r.agent} | **tags:** ${r.tags.join(", ")} | **importance:** ${r.importance} | **updated:** ${r.updated}`,
      "",
      r.content,
    ].join("\n")).join("\n\n---\n\n");
    return { content: [{ type: "text", text: body }] };
  },
);

server.tool(
  "write_context",
  [
    "Persist a memory chunk to the current workspace store.",
    "A memory is a recorded claim/decision/experience, not automatically verified truth.",
    "When there is a Raw source that should be re-observable later, pass source_ref.",
    "The writer identity is taken from the trusted MCP connection; no agent field is needed.",
  ].join(" "),
  {
    topic: z.string(),
    content: z.string(),
    tags: z.array(z.string()).optional(),
    importance: z.enum(["low", "medium", "high", "critical"]).optional(),
    ttl_days: z.number().positive().optional(),
    source_ref: z.string().optional().describe("Optional handle back to Raw/evidence such as file/commit/experiment/conversation/URL."),
  },
  async ({ topic, content, tags, importance, ttl_days, source_ref }) => {
    const result = await upstream.store.writeChunk({
      topic,
      content,
      agent: trustedActor,
      tags: tags ?? [],
      importance: importance ?? "medium",
      ttlDays: ttl_days,
    });
    const sourceNote = source_ref?.trim() ? ` | source=${source_ref.trim()}` : "";
    return { content: [{ type: "text", text: `Chunk saved: id=\`${result.id}\` | topic="${result.topic}" | tags=[${result.tags.join(", ")}] | importance=${result.importance}${sourceNote}` }] };
  },
);

server.tool(
  "read_context",
  "Retrieve the full content of a specific chunk by ID.",
  { id: z.string() },
  async ({ id }) => {
    const chunk = await upstream.store.readChunk(id);
    if (!chunk) return { content: [{ type: "text", text: `No chunk found with id \`${id}\`.` }] };
    const { meta, content } = chunk;
    return { content: [{ type: "text", text: `## ${meta.topic}\n**id:** \`${meta.id}\` | **agent:** ${meta.agent} | **tags:** ${(meta.tags || []).join(", ")} | **importance:** ${meta.importance} | **updated:** ${meta.updated}\n\n${content}` }] };
  },
);

server.tool(
  "list_context",
  "List stored chunks (metadata only).",
  {
    agent: z.string().optional(),
    tags: z.array(z.string()).optional(),
    limit: z.number().int().min(1).max(500).optional(),
    offset: z.number().int().min(0).optional(),
  },
  async ({ agent, tags, limit, offset }) => {
    const chunks = await upstream.store.listChunks({ agent, tags: tags ?? [], limit: limit ?? 100, offset: offset ?? 0 });
    if (!chunks.length) return { content: [{ type: "text", text: "Memory store is empty." }] };
    const lines = chunks.map((c) => `- \`${c.id}\` **${c.topic}** | agent:${c.agent} | tags:[${c.tags.join(", ")}] | ${c.importance} | ${c.updated}`);
    return { content: [{ type: "text", text: `${chunks.length} chunk(s) found:\n\n${lines.join("\n")}` }] };
  },
);

server.tool(
  "get_state",
  "Read a current shared state variable by key. State is for the latest operational/current value (for example goal.current), not durable history; keep reasons and history in memory records.",
  { key: z.string() },
  async ({ key }) => {
    const value = await upstream.store.getState(key);
    if (value === null) return { content: [{ type: "text", text: `State key "${key}" not found.` }] };
    return { content: [{ type: "text", text: typeof value === "string" ? value : JSON.stringify(value, null, 2) }] };
  },
);

server.tool(
  "set_state",
  "Write a current shared state variable. State is a shared blackboard/current pointer, not durable history. Pass expected_value to make multi-agent updates atomic and reject stale writes.",
  { key: z.string(), value: z.any(), expected_value: z.any().optional() },
  async ({ key, value, expected_value }) => {
    if (expected_value === undefined) {
      const result = await upstream.store.setState(key, value);
      return { content: [{ type: "text", text: `State "${key}" written at ${result.updated}.` }] };
    }
    const db = upstream.db.getDb();
    db.exec("BEGIN IMMEDIATE");
    try {
      const row = db.prepare("SELECT value FROM state WHERE key = ?").get(key);
      const current = row ? JSON.parse(row.value) : null;
      if (JSON.stringify(current) !== JSON.stringify(expected_value)) {
        db.exec("ROLLBACK");
        return { content: [{ type: "text", text: `CAS_CONFLICT key=${key}\ncurrent=${JSON.stringify(current)}\nexpected=${JSON.stringify(expected_value)}` }], isError: true };
      }
      const ts = new Date().toISOString();
      db.prepare("INSERT INTO state (key, value, updated_at) VALUES (?, ?, ?) ON CONFLICT(key) DO UPDATE SET value=excluded.value, updated_at=excluded.updated_at").run(key, JSON.stringify(value), ts);
      db.exec("COMMIT");
      return { content: [{ type: "text", text: `State "${key}" atomically updated at ${ts}.` }] };
    } catch (err) {
      try { db.exec("ROLLBACK"); } catch {}
      return { content: [{ type: "text", text: `エラー: ${err.message}` }], isError: true };
    }
  },
);

await registerAntTools(server, { trustedActor, model, provider });

const transport = new StdioServerTransport();
await server.connect(transport);
process.stderr.write(`[ant-memory] native server connected (actor=${trustedActor})\n`);
