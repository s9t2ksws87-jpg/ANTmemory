/**
 * ANTmemory audit 固有ツールの登録（ANTmemory 所有コード）
 *
 * ANTmemory が所有する McpServer インスタンスへ以下を追加する:
 *   supersede_context / retract_context / change_importance /
 *   get_context_history / restore_context
 *
 * また delete_context を無効化する（物理削除禁止・Phase 3）。
 * search_context は Phase 5 で lifecycle 意味論へ拡張する。
 */

import path from "node:path";
import { fileURLToPath, pathToFileURL } from "node:url";
import { z } from "zod/v4";

const LIB_DIR = path.dirname(fileURLToPath(import.meta.url));

export async function registerAntTools(server, { trustedActor, model = null, provider = null }) {
  const lifecyclePath = pathToFileURL(path.join(LIB_DIR, "audit-lifecycle.mjs")).href;
  const visibilityPath = pathToFileURL(path.join(LIB_DIR, "visibility.mjs")).href;
  const recallSchemaPath = pathToFileURL(path.join(LIB_DIR, "recall-schema.mjs")).href;

  // Phase C1: server ownershipをANTmemoryが持つため、schema初期化をMCP公開前に完了する。
  // 旧構成のfire-and-forget初期化は、最初のtool callとmigrationが競合し得た。
  const advancedTools = process.env.ANT_MEMORY_ADVANCED_TOOLS === "1";

  const lc = await import(lifecyclePath);
  await lc.initLifecycle();
  await lc.ensureAuditSchema();
  lc.expireDueChunks();
  const recallSchema = await import(recallSchemaPath);
  await recallSchema.ensureRecallSchema();
  await import(visibilityPath);

  // actor/model/providerは履歴へ保存する。通常応答へ毎回繰り返さず、必要ならhistoryで確認する。
  const withActorNote = (text) => text;

  // write_contextのactorはnative server側でtrustedActorへ固定済み。
  // ここでは新規chunkをlifecycleへ登録するだけにし、通常書込へ余計なactor注記を返さない。
  const writeTool = server._registeredTools?.["write_context"];
  if (writeTool) {
    writeTool.description =
      "Persist a recorded claim/decision/experience. Memory is evidence for later reasoning, not automatic truth. " +
      "Pass source_ref when a Raw/evidence handle exists so later agents can re-observe the source.";
    const originalHandler = writeTool.handler;
    writeTool.handler = async (args, ...rest) => {
      const result = await originalHandler.call(this ?? null, args, ...rest);
      try {
        const id = (result.content ?? []).map((c) => c.text).join("\n").match(/id=`([0-9a-f]{10})`/i)?.[1];
        if (id) {
          const lc = await import(lifecyclePath);
          lc.registerNewChunk(id, {
            actor: trustedActor,
            model,
            provider,
            reason: "write_context",
          });
          const sourceRef = args?.source_ref?.trim?.();
          if (sourceRef) {
            lc.appendMemoryEvent({
              chunkId: id,
              type: "source_observed",
              actor: trustedActor,
              model,
              provider,
              reason: "write_context source",
              newValue: sourceRef,
            });
          }
        }
      } catch {}
      return result;
    };
  }

  // ── read_context: lifecycle / visibility / current を常に表示 ────────────
  const readTool = server._registeredTools?.["read_context"];
  if (readTool) {
    readTool.description =
      "Retrieve the stored raw memory body plus lifecycle status, visibility, current-version state, timestamps, and source handle when available. " +
      "The memory body is authoritative as a record of what was stored, not as truth about the outside world; re-observe source_ref when correctness matters.";
    readTool.handler = async ({ id }) => {
      try {
        const retrieval = await import(pathToFileURL(path.join(LIB_DIR, "retrieval-core.mjs")).href);
        const r = await retrieval.readRecord(id);
        if (!r) return { content: [{ type: "text", text: `No chunk found with id \`${id}\`.` }] };
        const header = [
          `## ${r.topic}`,
          `**id:** \`${r.id}\` | **status:** ${r.status} | **visibility:** ${r.visibility} | **current:** ${r.current ? "yes" : "no"}`,
          `**agent:** ${r.agent} | **tags:** ${(r.tags || []).join(", ")} | **importance:** ${r.importance} | **created:** ${r.created ?? "unknown"} | **updated:** ${r.updated}`,
          ...(r.sourceRef ? [`**source:** ${r.sourceRef}`] : []),
        ].join("\n");
        return { content: [{ type: "text", text: `${header}\n\n${r.content}` }] };
      } catch (err) {
        return { content: [{ type: "text", text: `エラー: ${err.message}` }], isError: true };
      }
    };
  }

  // ── search_context の audit+recall 意味論（Phase 5 / Recall-1 / Recall-2 / Recall-4）────────
  // audit: 既定では active のみ。include_history: true で履歴状態も取得。
  // recall-1: 通常検索では visibility hot/warm のみ。include_hidden で解除。
  // recall-2: token_budget / top_k で返却量を制御する。
  // recall-4: 既定返却は Level0-1（metadata + short summary）。
  //       full_content: true で従来通り本文ごと返す（audit互換）。
  const searchTool = server._registeredTools?.["search_context"];
  if (searchTool) {
    searchTool.description =
      "Search memory. By default only active hot/warm records are returned as metadata + short summary. " +
      "Returned memories are recalled prior records, not verified truth; inspect lifecycle/timestamps/source_ref and re-observe Raw when the claim is consequential. " +
      "Displayed relative_score is normalized within the candidate set: 1.0 means best among returned candidates, not guaranteed relevance. " +
      "Use include_history for superseded/retracted/expired, include_hidden for cold/archive, full_content for raw bodies, and token_budget to cap output.";

    /** token概算: CJK≈1字/token、他≈4字/token（measure-tokens.mjs と同一方式） */
    const estimateTokens = (t) => {
      if (!t) return 0;
      const cjk = (t.match(/[\u3000-\u30ff\u3400-\u4dbf\u4e00-\u9fff\uf900-\ufaff]/g) || []).length;
      return Math.ceil(cjk + (t.length - cjk) / 4);
    };

    searchTool.inputSchema = z.object({
      ...(searchTool.inputSchema?.shape ?? {}),
      include_history: z.boolean().optional().describe("履歴(superseded/retracted/expired)も結果に含める"),
      include_hidden: z.boolean().optional().describe("cold/archiveの低可視性記憶も結果に含める"),
      token_budget: z.number().int().min(50).optional().describe("返却量の上限（概算token）。超過時は低関連から落とす"),
      full_content: z.boolean().optional().describe("本文全文を含める（既定はshort summary）"),
      ...(advancedTools ? { session_id: z.string().optional().describe("advanced retrieval session ID") } : {}),
    });
    searchTool.handler = async (args) => {
      const includeHistory = args?.include_history === true;
      const includeHidden = args?.include_hidden === true;
      const fullContent = args?.full_content === true;
      const sessionId = args?.session_id ?? null;
      const requested = args?.top_k ?? 6;
      const retrieval = await import(pathToFileURL(path.join(LIB_DIR, "retrieval-core.mjs")).href);
      const summaryMod = await import(pathToFileURL(path.join(LIB_DIR, "summaries.mjs")).href);
      const usage = await import(pathToFileURL(path.join(LIB_DIR, "usage-sessions.mjs")).href);
      const { records, meta } = await retrieval.searchRecords({
        query: args?.query ?? "",
        tags: args?.tags ?? [],
        agent: args?.agent,
        topK: requested,
        minScore: args?.min_score ?? 0.1,
        mode: args?.search_mode ?? "hybrid",
        includeHistory,
        includeHidden,
        sessionId,
      });

      if (!records.length) {
        return { content: [{ type: "text", text: "No matching chunks found." }] };
      }

      let renderedEntries = [];
      for (const r of records) {
        const metaLine = `**id:** \`${r.id}\` | **status:** ${r.status} | **visibility:** ${r.visibility} | **current:** ${r.current ? "yes" : "no"} | **created:** ${r.created ?? "unknown"}` + (r.sourceRef ? ` | **source:** ${r.sourceRef}` : "");
        if (fullContent) {
          renderedEntries.push({
            record: r,
            block: [
              `### [relative_score: ${r.score}] ${r.topic}`,
              `${metaLine} | **agent:** ${r.agent} | **tags:** ${r.tags.join(", ")} | **importance:** ${r.importance} | **updated:** ${r.updated}`,
              "",
              r.content,
            ].join("\n"),
          });
          continue;
        }
        let summary = null;
        try { summary = await summaryMod.getValidSummary(r.id, "short"); } catch {}
        if (!summary) {
          try {
            await summaryMod.generateSummaryFor(r.id);
            summary = await summaryMod.getValidSummary(r.id, "short");
          } catch {}
        }
        renderedEntries.push({
          record: r,
          block: [
            `### [relative_score: ${r.score}] ${r.topic}`,
            metaLine,
            summary ?? "(要約なし — read_context で本文を取得)",
          ].join("\n"),
        });
      }

      const budget = args?.token_budget;
      let droppedByBudget = 0;
      if (Number.isFinite(budget)) {
        const kept = [];
        let used = 0;
        for (const entry of renderedEntries) {
          const cost = estimateTokens(entry.block) + 5;
          if (used + cost > budget && kept.length > 0) {
            droppedByBudget += 1;
            continue;
          }
          kept.push(entry);
          used += cost;
        }
        renderedEntries = kept;
      }

      const returned = renderedEntries.map((e) => e.record);
      const returnedIds = returned.map((r) => r.id);
      if (returnedIds.length) {
        usage.recordAccess(returnedIds, sessionId);
        usage.markSeen(sessionId, returnedIds);
        try {
          const recall = await import(pathToFileURL(path.join(LIB_DIR, "recall-log.mjs")).href);
          await recall.logRecall(
            returned.map((r, i) => {
              const reasons = ["relevance_match"];
              if (!includeHistory) reasons.push("active_status");
              if (r.visibility === "hot") reasons.push("visibility_hot");
              else if (r.visibility === "cold" || r.visibility === "archive") reasons.push("deep_recall");
              return { id: r.id, reasons, score: r.score, rank: i + 1 };
            }),
            { sessionId, query: args?.query ?? null },
          );
        } catch {}
      }

      const notes = [];
      if (meta.filteredCandidates > 0) {
        notes.push(`${meta.filteredCandidates} 件をlifecycle/visibilityで候補確定前に除外しました`);
      }
      if (droppedByBudget > 0) notes.push(`token_budgetにより ${droppedByBudget} 件を切り詰めました`);
      if (meta.seenCount > 0 && sessionId) notes.push(`セッション内既読 ${meta.seenCount} 件の順位を下げました`);
      const suffix = notes.length ? `\n\n(${notes.join("。")}。必要なら include_history / include_hidden / top_k / token_budget を調整してください)` : "";
      const scoreNote = "relative_scoreは候補集合内の相対順位です。1.0でも『強い一致』や『正しい記憶』を保証しません。";
      const memoryNote = "memoryは過去に保存された記録です。重要な判断ではstatus/current/created/sourceを確認し、必要ならRawを再観測してください。";
      return { content: [{ type: "text", text: scoreNote + "\n" + memoryNote + "\n\n" + renderedEntries.map((e) => e.block).join("\n\n---\n\n") + suffix }] };
    };
  }
  // ── list_context: structured inventory (Phase B) ─────────────────────────
  const listTool = server._registeredTools?.["list_context"];
  if (listTool) {
    listTool.description =
      "[Phase B] List stored memories from the structured retrieval core. " +
      "Each entry includes lifecycle status, visibility, timestamps, and source handle when present. Listed memories are records, not automatic truth; no human-readable output is reparsed.";
    listTool.handler = async (args) => {
      try {
        const retrieval = await import(pathToFileURL(path.join(LIB_DIR, "retrieval-core.mjs")).href);
        const records = await retrieval.listRecords({
          agent: args?.agent,
          tags: args?.tags ?? [],
          limit: args?.limit ?? 100,
          offset: args?.offset ?? 0,
        });
        if (!records.length) return { content: [{ type: "text", text: "Memory store is empty." }] };
        const lines = records.map((r) =>
          `- \`${r.id}\` **${r.topic}** | status:${r.status} | visibility:${r.visibility} | current:${r.current ? "yes" : "no"} | agent:${r.agent} | tags:[${r.tags.join(", ")}] | ${r.importance} | created:${r.created ?? "unknown"} | updated:${r.updated}` + (r.sourceRef ? ` | source:${r.sourceRef}` : ""),
        );
        return { content: [{ type: "text", text: `${records.length} chunk(s) found:\n\n${lines.join("\n")}` }] };
      } catch (err) {
        return { content: [{ type: "text", text: `エラー: ${err.message}` }], isError: true };
      }
    };
  }
  // ── get_recall_log: 想起履歴（Recall-6）──────────────────────────────────────
  server.tool(
    "get_recall_log",
    "Show WHY a memory was recalled: reasons, score, query and rank of past retrievals.",
    {
      id: z.string(),
      limit: z.number().int().min(1).max(100).optional(),
    },
    async ({ id, limit }) => {
      try {
        const recall = await import(pathToFileURL(path.join(LIB_DIR, "recall-log.mjs")).href);
        const log = await recall.getRecallLog(id, limit ?? 20);
        if (!log.length) {
          return { content: [{ type: "text", text: `記録 \`${id}\` の想起履歴はまだありません。` }] };
        }
        const lines = [`想起履歴 ${log.length} 件:`];
        for (const e of log) {
          let reasons = [];
          try { reasons = JSON.parse(e.reason); } catch {}
          lines.push(`- ${e.created_at} [rank:${e.rank}] score:${e.score ?? "-"} query:${e.query ?? "-"} 理由: ${reasons.join(", ") || "-"}`);
        }
        return { content: [{ type: "text", text: lines.join("\n") }] };
      } catch (err) {
        return { content: [{ type: "text", text: `エラー: ${err.message}` }], isError: true };
      }
    },
  );

  // ── get_visibility_recommendations: 推奨算出（Recall-7・自動適用なし）────────
  server.tool(
    "get_visibility_recommendations",
    [
      "Get VISIBILITY recommendations (hot/cold/archive candidates) based on usage statistics.",
      "This NEVER changes anything by itself. Apply suggestions manually via set_visibility.",
    ].join(" "),
    {},
    async () => {
      try {
        const rec = await import(
          pathToFileURL(path.join(LIB_DIR, "visibility-recommendations.mjs")).href
        );
        const { thresholds, recommendations } = await rec.getRecommendations();
        if (!recommendations.length) {
          return { content: [{ type: "text", text: "現在、visibility変更の推奨はありません。" }] };
        }
        const lines = [
          `推奨 ${recommendations.length} 件（閾値: hot=累計${thresholds.hotAccess}回以上参照かつ直近${thresholds.hotDays}日以内に参照あり / cold=${thresholds.coldIdleDays}日未参照 / archive=superseded）:`,
          "",
        ];
        for (const r of recommendations) {
          lines.push(`- [${r.current} → ${r.recommended}] ${r.topic}`);
          lines.push(`  id=\`${r.id}\` | importance=${r.importance} | 根拠: ${r.basis}`);
        }
        lines.push("", "適用する場合は set_visibility(id, visibility, reason) を実行してください（このツール自体は何も変更しません）。");
        return { content: [{ type: "text", text: lines.join("\n") }] };
      } catch (err) {
        return { content: [{ type: "text", text: `エラー: ${err.message}` }], isError: true };
      }
    },
  );

  // ── begin_retrieval_session: セッション開始（Recall-5）──────────────────────
  server.tool(
    "begin_retrieval_session",
    [
      "Start a retrieval session. Pass the returned session_id to search_context.",
      "Within a session, already-seen memories are ranked lower to reduce duplicate context spend.",
    ].join(" "),
    {
      token_budget: z.number().int().min(50).optional().describe("このセッションのtoken予算（記録用）"),
    },
    async ({ token_budget }) => {
      try {
        const usage = await import(pathToFileURL(path.join(LIB_DIR, "usage-sessions.mjs")).href);
        const sessionId = await usage.beginSession(trustedActor, token_budget ?? null);
        return { content: [{ type: "text", text: `セッション開始。\nsession_id: \`${sessionId}\`\nsearch_context に session_id として渡してください。` }] };
      } catch (err) {
        return { content: [{ type: "text", text: `エラー: ${err.message}` }], isError: true };
      }
    },
  );

  // ── recall_deep: 段階的深掘り（Recall-4・cold/archive含む）────────────────────
  server.tool(
    "recall_deep",
    [
      "Progressive deep retrieval. Stage 1: lightweight candidate list (metadata only, includes cold/archived memories).",
      "Stage 2 (pass resolve=true): get the full raw bodies of the chosen ids.",
      "Use this when normal search_context results are not enough.",
    ].join(" "),
    {
      query: z.string().optional().describe("Stage 1の検索語。resolve=true + ids のStage 2では不要"),
      top_k: z.number().int().min(1).max(20).optional(),
      min_score: z.number().min(0).optional(),
      search_mode: z.enum(["hybrid", "bm25", "semantic"]).optional(),
      resolve: z.boolean().optional().describe("trueで候補の本文(raw)を返す。falseなら候補一覧のみ"),
      ids: z.array(z.string()).optional().describe("resolve=trueのときに本文を取得するID一覧"),
    },
    async ({ query, top_k, min_score, search_mode, resolve: doResolve, ids }) => {
      try {
        const retrieval = await import(pathToFileURL(path.join(LIB_DIR, "retrieval-core.mjs")).href);
        if (doResolve) {
          if (!ids?.length) throw new Error("resolve=true のときは ids を指定してください");
          const parts = [];
          for (const id of ids.slice(0, 10)) {
            const r = await retrieval.readRecord(id);
            if (!r) {
              parts.push(`No chunk found with id \`${id}\`.`);
              continue;
            }
            parts.push([
              `## ${r.topic}`,
              `**id:** \`${r.id}\` | **status:** ${r.status} | **visibility:** ${r.visibility} | **agent:** ${r.agent} | **importance:** ${r.importance}`,
              "",
              r.content,
            ].join("\n"));
          }
          return { content: [{ type: "text", text: parts.join("\n\n---\n\n") }] };
        }
        if (!query) throw new Error("候補検索では query を指定してください");
        const { records } = await retrieval.searchRecords({
          query,
          tags: [],
          agent: undefined,
          topK: top_k ?? 10,
          minScore: min_score ?? 0,
          mode: search_mode ?? "hybrid",
          includeHistory: true,
          includeHidden: true,
        });
        if (!records.length) {
          return { content: [{ type: "text", text: "No matching chunks found." }] };
        }
        const lines = records.map((r) =>
          `- [score: ${r.score}] ${r.topic} | id=\`${r.id}\` | status=${r.status} | visibility=${r.visibility}`,
        );
        return {
          content: [{
            type: "text",
            text: `候補 ${lines.length} 件（cold/archive・履歴状態も含む）。詳細は resolve=true + ids で取得:\n\n${lines.join("\n")}`,
          }],
        };
      } catch (err) {
        return { content: [{ type: "text", text: `エラー: ${err.message}` }], isError: true };
      }
    },
  );

  // ── get_successor_context: 置換関係を辿って現行版を確認する ──────────────
  server.tool(
    "get_successor_context",
    "Follow the supersedes chain from an old memory to its current successor.",
    { id: z.string() },
    async ({ id }) => {
      try {
        const lc = await import(lifecyclePath);
        const chain = [];
        let current = id;
        for (let i = 0; i < 20; i++) {
          const rel = lc.findSuccessor(current);
          if (!rel) break;
          chain.push(rel);
          current = rel.successor_id;
        }
        const retrieval = await import(pathToFileURL(path.join(LIB_DIR, "retrieval-core.mjs")).href);
        if (!chain.length) {
          const tip = await retrieval.readRecord(id);
          const label = tip?.current ? "effective current" : `chain tip / status=${tip?.status ?? "unknown"} / current=no`;
          return { content: [{ type: "text", text: `\`${id}\` には後継記録がありません（${label}）。` }] };
        }
        const lines = ["置換チェーン:"];
        let from = id;
        for (const r of chain) {
          lines.push(`- \`${from}\` → \`${r.successor_id}\` (by ${r.actor}${r.reason ? ` — ${r.reason}` : ""})`);
          from = r.successor_id;
        }
        const tip = await retrieval.readRecord(current);
        lines.push("", `chain tip: \`${current}\` | status=${tip?.status ?? "unknown"} | current=${tip?.current ? "yes" : "no"}`);
        lines.push(tip?.current ? `effective current: \`${current}\`` : "effective current: none");
        return { content: [{ type: "text", text: lines.join("\n") }] };
      } catch (err) {
        return { content: [{ type: "text", text: `エラー: ${err.message}` }], isError: true };
      }
    },
  );

  // ── supersede_context ─────────────────────────────────────────────────
  server.tool(
    "supersede_context",
    [
      "Replace an outdated memory with a new one WITHOUT deleting the old body.",
      "Creates the new chunk, links it to the old one via a 'supersedes' relation,",
      "and marks the old chunk 'superseded'. A reason is REQUIRED.",
    ].join(" "),
    {
      old_id: z.string().describe("置き換えたい旧記録のID"),
      topic: z.string().describe("新記録のタイトル"),
      content: z.string().describe("新記録の本文"),
      tags: z.array(z.string()).optional(),
      importance: z.enum(["low", "medium", "high", "critical"]).optional(),
      ttl_days: z.number().positive().optional(),
      source_ref: z.string().optional().describe("新記録のRaw/evidence handle。省略時は新記録へ直接sourceを付けない"),
      reason: z.string().describe("なぜ置き換えるのか（必須）"),
    },
    async ({ old_id, topic, content, tags, importance, ttl_days, source_ref, reason }) => {
      try {
        const lc = await import(lifecyclePath);
        const coreMod = await import(pathToFileURL(path.join(LIB_DIR, "core.mjs")).href);
        const core = await coreMod.loadCore();
        const old = core.db.getDb().prepare("SELECT tags, importance, expires_at FROM chunks WHERE id = ?").get(old_id);
        const inheritedTags = old ? (() => { try { return JSON.parse(old.tags); } catch { return []; } })() : [];
        const r = await lc.supersedeChunk({
          oldId: old_id,
          topic,
          content,
          actor: trustedActor,
          tags: tags ?? inheritedTags,
          importance: importance ?? old?.importance ?? "medium",
          ttlDays: ttl_days ?? null,
          expiresAtOverride: ttl_days === undefined ? (old?.expires_at ?? null) : undefined,
          sourceRef: source_ref ?? null,
          reason,
          model,
          provider,
        });
        return {
          content: [
            {
              type: "text",
              text: withActorNote(
                `置換完了。\n旧記録 \`${r.oldId}\` は superseded（本文は残っています）。\n新記録 id=\`${r.newId}\``,
              ),
            },
          ],
        };
      } catch (err) {
        return { content: [{ type: "text", text: `エラー: ${err.message}` }], isError: true };
      }
    },
  );

  // ── correct_context: 軽微訂正（省略metadataを自動継承）─────────────────
  server.tool(
    "correct_context",
    [
      "Correct an active memory with minimal input while preserving omitted metadata.",
      "topic/content/tags/importance/TTL are inherited unless explicitly changed.",
      "The old raw body remains available as a superseded revision. Reason is required.",
    ].join(" "),
    {
      id: z.string().describe("訂正対象の現行記録ID"),
      topic: z.string().optional(),
      content: z.string().optional(),
      tags: z.array(z.string()).optional(),
      importance: z.enum(["low", "medium", "high", "critical"]).optional(),
      ttl_days: z.number().positive().optional().describe("新しいTTL。省略時は元の絶対期限を継承"),
      clear_ttl: z.boolean().optional().describe("trueでTTLを解除して無期限化"),
      source_ref: z.string().optional().describe("Raw/evidence handle。省略時は旧source_refを継承"),
      reason: z.string().describe("訂正理由（必須）"),
    },
    async ({ id, topic, content, tags, importance, ttl_days, clear_ttl, source_ref, reason }) => {
      try {
        if (ttl_days !== undefined && clear_ttl) throw new Error("ttl_days と clear_ttl は同時指定できません");
        const coreMod = await import(pathToFileURL(path.join(LIB_DIR, "core.mjs")).href);
        const core = await coreMod.loadCore();
        const old = core.db.getDb().prepare(
          `SELECT c.topic, c.content, c.tags, c.importance, c.expires_at,
                  (SELECT e.new_value FROM memory_events e
                    WHERE e.chunk_id = c.id AND e.event_type = 'source_observed'
                    ORDER BY e.created_at DESC LIMIT 1) AS source_ref
             FROM chunks c WHERE c.id = ?`,
        ).get(id);
        if (!old) throw new Error(`訂正対象が見つかりません: ${id}`);
        if ([topic, content, tags, importance, ttl_days, source_ref].every((v) => v === undefined) && !clear_ttl) {
          throw new Error("訂正内容を1つ以上指定してください");
        }
        let oldTags = [];
        try { oldTags = JSON.parse(old.tags); } catch {}
        const lc = await import(lifecyclePath);
        const r = await lc.supersedeChunk({
          oldId: id,
          topic: topic ?? old.topic,
          content: content ?? old.content,
          actor: trustedActor,
          tags: tags ?? oldTags,
          importance: importance ?? old.importance,
          ttlDays: ttl_days ?? null,
          expiresAtOverride: clear_ttl ? null : (ttl_days === undefined ? old.expires_at : undefined),
          sourceRef: source_ref ?? old.source_ref ?? null,
          reason,
          model,
          provider,
        });
        return {
          content: [{
            type: "text",
            text: withActorNote(
              `訂正完了。旧記録 \`${r.oldId}\` は superseded のままRaw保持。\n` +
              `新記録 id=\`${r.newId}\`。省略したmetadataは旧記録から継承しました。`,
            ),
          }],
        };
      } catch (err) {
        return { content: [{ type: "text", text: `エラー: ${err.message}` }], isError: true };
      }
    },
  );

  // ── retract_context ───────────────────────────────────────────────────
  server.tool(
    "retract_context",
    "Mark a memory as RETRACTED without deleting its body. Reason is required.",
    {
      id: z.string().describe("撤回したい記録のID"),
      reason: z.string().describe("なぜ撤回するのか（必須）"),
    },
    async ({ id, reason }) => {
      try {
        const lc = await import(lifecyclePath);
        lc.retractChunk({ id, actor: trustedActor, reason, model, provider });
        return {
          content: [
            {
              type: "text",
              text: withActorNote(`記録 \`${id}\` を retracted にしました。本文は消えていません。`),
            },
          ],
        };
      } catch (err) {
        return { content: [{ type: "text", text: `エラー: ${err.message}` }], isError: true };
      }
    },
  );

  // ── change_importance ─────────────────────────────────────────────────
  server.tool(
    "change_importance",
    "Change the importance of a memory. Old/new values, actor and reason are recorded as events.",
    {
      id: z.string(),
      importance: z.enum(["low", "medium", "high", "critical"]),
      reason: z.string().describe("変更理由（必須）"),
    },
    async ({ id, importance, reason }) => {
      try {
        const lc = await import(lifecyclePath);
        const r = lc.changeImportance({ id, importance, actor: trustedActor, reason, model, provider });
        return {
          content: [
            {
              type: "text",
              text: withActorNote(`重要度を ${r.from} → ${r.to} に変更しました (\`${id}\`)。`),
            },
          ],
        };
      } catch (err) {
        return { content: [{ type: "text", text: `エラー: ${err.message}` }], isError: true };
      }
    },
  );

  // ── restore_context ───────────────────────────────────────────────────
  server.tool(
    "restore_context",
    "Bring a retracted/expired memory back to active state when it has no successor. Superseded revisions with a successor cannot become current again. Reason is required.",
    {
      id: z.string(),
      reason: z.string().describe("復活させる理由（必須）"),
    },
    async ({ id, reason }) => {
      try {
        const lc = await import(lifecyclePath);
        lc.restoreChunk({ id, actor: trustedActor, reason, model, provider });
        return {
          content: [{ type: "text", text: withActorNote(`記録 \`${id}\` を active に戻しました。`)}],
        };
      } catch (err) {
        return { content: [{ type: "text", text: `エラー: ${err.message}` }], isError: true };
      }
    },
  );

  // ── get_memory_summary: 要約取得（Recall-3・rawは read_context で取得）────────
  server.tool(
    "get_memory_summary",
    [
      "Get a SHORT or DETAILED summary of a memory without loading the raw body.",
      "Summaries are derived data; the raw content remains authoritative in read_context.",
      "If no valid summary exists, falls back to a topic-based brief.",
    ].join(" "),
    {
      id: z.string(),
      level: z.enum(["short", "detailed"]).optional().describe("要約レベル（既定: short）"),
    },
    async ({ id, level }) => {
      try {
        const lvl = level ?? "short";
        const summaryMod = await import(
          pathToFileURL(path.join(LIB_DIR, "summaries.mjs")).href
        );
        let summary = await summaryMod.getValidSummary(id, lvl);
        if (summary) {
          return { content: [{ type: "text", text: `[${lvl} summary]\n${summary}` }] };
        }
        // 無効・未生成 → 生成してから再取得（決定論的・低コスト）
        await summaryMod.generateSummaryFor(id);
        summary = await summaryMod.getValidSummary(id, lvl);
        if (!summary) {
          return { content: [{ type: "text", text: `記録 \`${id}\` の要約を生成できませんでした。read_context で本文を確認してください。` }] };
        }
        return { content: [{ type: "text", text: `[${lvl} summary]\n${summary}` }] };
      } catch (err) {
        return { content: [{ type: "text", text: `エラー: ${err.message}` }], isError: true };
      }
    },
  );

  // ── promote_memory: visibility変更（recall・reason必須）──────────────────────
  server.tool(
    "promote_memory",
    [
      "Change the VISIBILITY (hot/warm/cold/archive) of a memory without touching its body or importance.",
      "importance and visibility are independent: 'critical + cold' is valid.",
      "Reason is required and recorded as an audit event.",
    ].join(" "),
    {
      id: z.string(),
      to: z.enum(["hot", "warm", "cold", "archive"]).describe("新しいvisibility"),
      reason: z.string().describe("変更理由（必須・監査イベントとして記録）"),
    },
    async ({ id, to, reason }) => {
      try {
        const vis = await import(visibilityPath);
        const r = await vis.setVisibility({ id, to, actor: trustedActor, reason });
        if (r.unchanged) {
          return { content: [{ type: "text", text: `記録 \`${id}\` は既に ${to} です。変更はありません。` }] };
        }
        return {
          content: [
            {
              type: "text",
              text: withActorNote(`visibility を ${r.from} → ${r.to} に変更しました (\`${id}\`)。原文・重要度は変更されていません。`),
            },
          ],
        };
      } catch (err) {
        return { content: [{ type: "text", text: `エラー: ${err.message}` }], isError: true };
      }
    },
  );

  // ── set_visibility: promote_memoryの明示名alias ────────────────────────
  server.tool(
    "set_visibility",
    "Set memory visibility directly (hot/warm/cold/archive). importance and visibility are independent; reason is required.",
    {
      id: z.string(),
      visibility: z.enum(["hot", "warm", "cold", "archive"]),
      reason: z.string().describe("変更理由（必須）"),
    },
    async ({ id, visibility, reason }) => {
      try {
        const vis = await import(visibilityPath);
        const r = await vis.setVisibility({ id, to: visibility, actor: trustedActor, reason });
        if (r.unchanged) return { content: [{ type: "text", text: `記録 \`${id}\` は既に ${visibility} です。` }] };
        return { content: [{ type: "text", text: withActorNote(`visibility を ${r.from} → ${r.to} に変更しました (\`${id}\`)。`) }] };
      } catch (err) {
        return { content: [{ type: "text", text: `エラー: ${err.message}` }], isError: true };
      }
    },
  );

  // ── state discovery / CAS ─────────────────────────────────────────────
  server.tool(
    "list_state",
    "List pipeline state keys and values. Pass query to filter by key substring when the exact key is not known.",
    {
      query: z.string().optional().describe("key部分一致。省略時は一覧"),
      limit: z.number().int().min(1).max(500).optional(),
      offset: z.number().int().min(0).optional(),
    },
    async ({ query, limit, offset }) => {
      try {
        const coreMod = await import(pathToFileURL(path.join(LIB_DIR, "core.mjs")).href);
        const core = await coreMod.loadCore();
        const rows = query
          ? core.db.getDb().prepare("SELECT key, value, updated_at FROM state WHERE key LIKE ? ORDER BY updated_at DESC LIMIT ? OFFSET ?").all(`%${query}%`, limit ?? 100, offset ?? 0)
          : core.db.getDb().prepare("SELECT key, value, updated_at FROM state ORDER BY updated_at DESC LIMIT ? OFFSET ?").all(limit ?? 100, offset ?? 0);
        if (!rows.length) return { content: [{ type: "text", text: query ? "No matching state keys found." : "State store is empty." }] };
        const lines = rows.map((r) => `- ${r.key} | updated:${r.updated_at} | ${r.value}`);
        return { content: [{ type: "text", text: `${rows.length} state(s):\n\n${lines.join("\n")}` }] };
      } catch (err) {
        return { content: [{ type: "text", text: `エラー: ${err.message}` }], isError: true };
      }
    },
  );

  server.tool(
    "search_state",
    "Search pipeline state by key substring. Values are returned for discovery, not ranked semantic retrieval.",
    { query: z.string(), limit: z.number().int().min(1).max(200).optional() },
    async ({ query, limit }) => {
      try {
        const coreMod = await import(pathToFileURL(path.join(LIB_DIR, "core.mjs")).href);
        const core = await coreMod.loadCore();
        const rows = core.db.getDb().prepare("SELECT key, value, updated_at FROM state WHERE key LIKE ? ORDER BY updated_at DESC LIMIT ?").all(`%${query}%`, limit ?? 50);
        if (!rows.length) return { content: [{ type: "text", text: "No matching state keys found." }] };
        return { content: [{ type: "text", text: rows.map((r) => `- ${r.key} | updated:${r.updated_at} | ${r.value}`).join("\n") }] };
      } catch (err) {
        return { content: [{ type: "text", text: `エラー: ${err.message}` }], isError: true };
      }
    },
  );

  server.tool(
    "compare_and_set_state",
    "Atomically update a state key only if its current JSON value equals expected_value. Prevents lost updates between agents.",
    { key: z.string(), expected_value: z.any(), value: z.any() },
    async ({ key, expected_value, value }) => {
      const coreMod = await import(pathToFileURL(path.join(LIB_DIR, "core.mjs")).href);
      const core = await coreMod.loadCore();
      const db = core.db.getDb();
      db.exec("BEGIN IMMEDIATE");
      try {
        const row = db.prepare("SELECT value FROM state WHERE key = ?").get(key);
        const current = row ? JSON.parse(row.value) : null;
        if (JSON.stringify(current) !== JSON.stringify(expected_value)) {
          db.exec("ROLLBACK");
          return {
            content: [{ type: "text", text: `CAS_CONFLICT key=${key}\ncurrent=${JSON.stringify(current)}\nexpected=${JSON.stringify(expected_value)}` }],
            isError: true,
          };
        }
        const ts = new Date().toISOString();
        db.prepare("INSERT INTO state (key, value, updated_at) VALUES (?, ?, ?) ON CONFLICT(key) DO UPDATE SET value=excluded.value, updated_at=excluded.updated_at").run(key, JSON.stringify(value), ts);
        db.exec("COMMIT");
        return { content: [{ type: "text", text: `State \"${key}\" atomically updated at ${ts}.` }] };
      } catch (err) {
        try { db.exec("ROLLBACK"); } catch {}
        return { content: [{ type: "text", text: `エラー: ${err.message}` }], isError: true };
      }
    },
  );

  // ── Phase D: Template-kun minimal observation cycle ──────────────────
  // 新しいGraph/UNKNOWN/Hypothesis schemaは作らない。
  // chunks + events + relationsだけを使い、観測→UNKNOWN→仮説→再観測を追記する。
  server.tool(
    "observe_context",
    [
      "Record one step of the minimal observation cycle without overwriting prior records.",
      "kind=unknown isolates an explicit UNKNOWN; kind=hypothesis requires parent_id pointing to that UNKNOWN;",
      "kind=observation records evidence/reobservation. Set resolves_unknown=true only when an observation resolves its parent UNKNOWN.",
      "source_ref is an optional handle back to a file/commit/experiment/conversation/raw source.",
    ].join(" "),
    {
      kind: z.enum(["observation", "unknown", "hypothesis"]),
      topic: z.string(),
      content: z.string(),
      parent_id: z.string().optional().describe("系譜元。hypothesisでは対象UNKNOWN、再観測ではUNKNOWN/仮説等"),
      source_ref: z.string().optional().describe("Rawへ戻るためのfile/commit/experiment/conversation等のhandle"),
      resolves_unknown: z.boolean().optional().describe("observationがparent UNKNOWNを解決するときのみtrue"),
      tags: z.array(z.string()).optional(),
      importance: z.enum(["low", "medium", "high", "critical"]).optional(),
      ttl_days: z.number().positive().optional(),
      reason: z.string().optional(),
    },
    async ({ kind, topic, content, parent_id, source_ref, resolves_unknown, tags, importance, ttl_days, reason }) => {
      try {
        const cycle = await import(pathToFileURL(path.join(LIB_DIR, "template-cycle.mjs")).href);
        const r = await cycle.recordObservation({
          kind,
          topic,
          content,
          actor: trustedActor,
          tags: tags ?? [],
          importance: importance ?? (kind === "unknown" ? "high" : "medium"),
          ttlDays: ttl_days ?? null,
          parentId: parent_id ?? null,
          sourceRef: source_ref ?? null,
          resolvesUnknown: resolves_unknown === true,
          reason: reason ?? null,
          model,
          provider,
        });
        const lines = [
          `観測記録 id=\`${r.id}\` kind=${r.kind}`,
          `tags=${r.tags.join(", ")}`,
        ];
        if (r.parentId) lines.push(`parent=\`${r.parentId}\``);
        if (r.sourceRef) lines.push(`source_ref=${r.sourceRef}`);
        if (r.resolvedUnknownId) lines.push(`UNKNOWN \`${r.resolvedUnknownId}\` はこの再観測でresolved。旧UNKNOWN本文は保持されています。`);
        return { content: [{ type: "text", text: withActorNote(lines.join("\n")) }] };
      } catch (err) {
        return { content: [{ type: "text", text: `エラー: ${err.message}` }], isError: true };
      }
    },
  );

  server.tool(
    "list_unknowns",
    "List explicitly isolated UNKNOWN records. Resolved UNKNOWNs are preserved but hidden by default; include_resolved=true shows them too.",
    {
      include_resolved: z.boolean().optional(),
      limit: z.number().int().min(1).max(500).optional(),
    },
    async ({ include_resolved, limit }) => {
      try {
        const cycle = await import(pathToFileURL(path.join(LIB_DIR, "template-cycle.mjs")).href);
        const rows = await cycle.listUnknowns({ includeResolved: include_resolved === true, limit: limit ?? 50 });
        if (!rows.length) return { content: [{ type: "text", text: "未解決UNKNOWNはありません。" }] };
        const lines = rows.map((r) =>
          `- \`${r.id}\` ${r.topic} | ${r.resolved ? `resolved_by:${r.resolved_by}` : "UNRESOLVED"} | status:${r.status} | visibility:${r.visibility} | derived:${r.derived_count}` +
          (r.source_ref ? ` | source:${r.source_ref}` : ""),
        );
        return { content: [{ type: "text", text: `${rows.length} UNKNOWN(s):\n\n${lines.join("\n")}` }] };
      } catch (err) {
        return { content: [{ type: "text", text: `エラー: ${err.message}` }], isError: true };
      }
    },
  );

  // ── get_context_history ───────────────────────────────────────────────
  server.tool(
    "get_context_history",
    "Show the full event timeline of a memory: creation, corrections, importance changes, supersedes.",
    {
      id: z.string(),
    },
    async ({ id }) => {
      try {
        const lc = await import(lifecyclePath);
        const h = lc.getContextHistory(id);
        if (!h.events.length && !h.relations.length) {
          return {
            content: [
              {
                type: "text",
                text: `履歴イベントはまだありません。現在状態: ${h.status}`,
              },
            ],
          };
        }
        const lines = [`状態: ${h.status}`, "", "イベント:"];
        for (const e of h.events) {
          lines.push(
            `- ${e.created_at} [${e.event_type}] by ${e.actor}` +
              (e.model ? ` (${e.model})` : "") +
              (e.reason ? ` — 理由: ${e.reason}` : "") +
              (e.old_value || e.new_value ? ` (${e.old_value ?? "-"} → ${e.new_value ?? "-"})` : ""),
          );
        }
        if (h.relations.length) {
          lines.push("", "関係:");
          for (const r of h.relations) {
            lines.push(
              `- ${r.direction === "out" ? "この記録が" : "この記録を"} ${r.relation_type}: \`${r.other_id}\`` +
                (r.reason ? ` — 理由: ${r.reason}` : ""),
            );
          }
        }
        return { content: [{ type: "text", text: lines.join("\n") }] };
      } catch (err) {
        return { content: [{ type: "text", text: `エラー: ${err.message}` }], isError: true };
      }
    },
  );

  // ── Agent-facing surface simplification ───────────────────────────────
  // 旧互換・保守・観測専用toolは実装を残すが、通常Agentには見せない。
  // ANT_MEMORY_ADVANCED_TOOLS=1 の明示時だけ再公開する。
  if (!advancedTools) {
    const hiddenByDefault = [
      "get_recall_log",
      "get_visibility_recommendations",
      "begin_retrieval_session",
      "recall_deep",
      "get_successor_context",
      "get_memory_summary",
      "promote_memory",
      "search_state",
      "compare_and_set_state",
    ];
    for (const name of hiddenByDefault) {
      if (server._registeredTools?.[name]) delete server._registeredTools[name];
    }
  }
}
