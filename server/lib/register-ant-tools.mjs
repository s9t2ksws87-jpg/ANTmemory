/**
 * ANTmemory 固有ツールの登録（ANTmemory 所有コード）
 *
 * 上流 McpServer インスタンスへ以下を追加する:
 *   supersede_context / retract_context / change_importance /
 *   get_context_history / restore_context
 *
 * また delete_context を無効化する（物理削除禁止）。
 * search_context は lifecycle 意味論へ拡張する。
 */

import path from "node:path";
import { fileURLToPath, pathToFileURL } from "node:url";
import { z } from "zod/v4";

const LIB_DIR = path.dirname(fileURLToPath(import.meta.url));

export function registerAntTools(server, { trustedActor, model = null, provider = null }) {
  const lifecyclePath = pathToFileURL(path.join(LIB_DIR, "audit-lifecycle.mjs")).href;
  const visibilityPath = pathToFileURL(path.join(LIB_DIR, "visibility.mjs")).href;

  // recall_deep 用: search_context の生ハンドラを保持する参照（後で設定される）
  let originalSearchHandlerForRecall = null;
  const setOriginalSearchHandlerForRecall = (h) => { originalSearchHandlerForRecall = h; };

  // ライフサイクル層の初期化（上流DB参照の解決＋監査スキーマ確保）
  import(lifecyclePath).then((lc) => lc.initLifecycle());

  // ── 選択的想起スキーマ確保（migrationと同じ定義・冪等）────────────────────
  // 新規ワークスペースでも選択的想起テーブルが必要。migration 済みDBでは no-op。
  const recallSchemaPath = pathToFileURL(path.join(LIB_DIR, "recall-schema.mjs")).href;
  import(recallSchemaPath)
    .then((m) => m.ensureRecallSchema())
    .catch((err) =>
      process.stderr.write(`[ant-memory] recall schema ensure failed: ${err.message}\n`),
  );
  import(visibilityPath).catch((err) =>
    process.stderr.write(`[ant-memory] visibility module load failed: ${err.message}\n`),
  );

  // ── 監査スキーマ確保（migrationと同じ定義・冪等）──────────────────────────
  // 新規ワークスペースで最初にサーバーが起動されたときにも監査テーブルが必要。
  import(lifecyclePath)
    .then((lc) => lc.ensureAuditSchema())
    .catch((err) =>
      process.stderr.write(`[ant-memory] audit schema ensure failed: ${err.message}\n`),
    );

  // ── delete_context の無効化（物理削除禁止）──────────────────────────────
  // 上流で登録済みのツールを disable し、説明を差し替える。呼ばれたら拒否する。
  const registered = server._registeredTools?.["delete_context"];
  if (registered) {
    registered.update({
      description:
        "DISABLED (always refuses) — physical deletion is forbidden. Use retract_context or supersede_context instead.",
    });
    // disable() は使わない: SDKのdisable()はルーティング層でhandler呼出をスキップするため
    // 案内文ではなく "MCP error -32602: Tool disabled" だけが返り、
    // エージェントへの代替手段の誘導（UX）が死ぬ。公開状態のままhandler内で必ず拒否する。
    registered.handler = async () => ({
      content: [
        {
          type: "text",
          text:
            "物理削除は禁止されています（2026-08-02 の誤削除事故を受けた設計）。\n" +
            "訂正は supersede_context、撤回は retract_context を使ってください。",
        },
      ],
      isError: true,
    });
  }

  const withActorNote = (text) =>
    `actor=${trustedActor} として記録されます${model ? ` (model=${model})` : ""}\n\n${text}`;

  // ── write_context の agent 強制正規化（記録主体の信頼性）──────────────────
  // ANT_MEMORY_CLIENT（サーバー起動時の接続元）を記録主体の正本とし、
  // AI側が自由入力した agent 値によるなりすましを防ぐ。
  // 指定がない場合も接続元で満たす。空文字・"global" も接続元へ正規化する。
  const writeTool = server._registeredTools?.["write_context"];
  if (writeTool) {
    const originalHandler = writeTool.handler;
    writeTool.handler = async (args, ...rest) => {
      const requested = args?.agent;
      let note = "";
      if (!requested || requested === "global") {
        args.agent = trustedActor;
        note = requested === "global"
          ? `(agent="global" は接続元 "${trustedActor}" へ正規化されました)`
          : `(agent 未指定のため接続元 "${trustedActor}" を使用)`;
      } else if (requested !== trustedActor) {
        args.agent = trustedActor;
        note = `(警告: agent="${requested}" の申告は拒否され、接続元 "${trustedActor}" として記録されました)`;
      }
      const result = await originalHandler.call(this ?? null, args, ...rest);
      if (note) {
        return {
          ...result,
          content: [{ type: "text", text: `${note}\n${(result.content ?? []).map((c) => c.text).join("\n")}` }],
        };
      }
      return result;
    };
    // 新規書込chunkをlifecycleへ登録（migration後に書かれたchunkの漏れを防ぐ）
    const withActorRegistration = writeTool.handler;
    writeTool.handler = async (args, ...rest) => {
      const result = await withActorRegistration(args, ...rest);
      try {
        const id = (result.content ?? []).map((c) => c.text).join("\n").match(/id=`([0-9a-f]{10})`/i)?.[1];
        if (id) {
          const lc = await import(lifecyclePath);
          lc.registerNewChunk(id);
        }
      } catch {}
      return result;
    };
  }

  // ── search_context の監査・選択的想起の意味論 ─────────────────────────────
  // 既定では active のみ。include_history: true で履歴状態も取得。
  // 通常検索では visibility hot/warm のみ。include_hidden で解除。
  // token_budget / max_results で返却量を制御する。
  // 既定返却は Level0-1（metadata + short summary）。
  // full_content: true で従来通り本文ごと返す。
  const searchTool = server._registeredTools?.["search_context"];
  if (searchTool) {
    const originalSearch = searchTool.handler;
    // recall_deep が生ハンドラ（監査・visibilityフィルタなし）を使えるよう共有する
    setOriginalSearchHandlerForRecall(originalSearch);
    searchTool.description =
      (searchTool.description ?? "") +
      " By default only ACTIVE memories are returned. Pass include_history=true to also see superseded/retracted/expired ones." +
      " By default only hot/warm VISIBILITY memories are returned. Pass include_hidden=true to also see cold/archived ones." +
      " token_budget (approx tokens) and max_results cap the response size; low-relevance entries are trimmed first." +
      " By default results contain METADATA + SHORT SUMMARY only (not the raw body). Pass full_content=true for the legacy full-body format, or call read_context / recall_deep for details.";

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
      max_results: z.number().int().min(1).max(20).optional().describe("返却件数の上限"),
      token_budget: z.number().int().min(50).optional().describe("返却量の上限（概算token）。超過時は低関連から落とす"),
      full_content: z.boolean().optional().describe("本文全文を含む従来形式で返す（token消費大）"),
      session_id: z.string().optional().describe("検索セッションID。指定すると既読記憶の順位を下げる"),
    });
    searchTool.handler = async (args, ...rest) => {
      const result = await originalSearch.call(null, args, ...rest);
      const includeHistory = args?.include_history === true;
      const includeHidden = args?.include_hidden === true;
      const fullContent = args?.full_content === true;
      const sessionId = args?.session_id ?? null;

      // フィルタ（検索結果テキストからIDを抽出して判定）
      const lc = await import(lifecyclePath);
      const vis = await import(visibilityPath);
      const summaryMod = await import(pathToFileURL(path.join(LIB_DIR, "summaries.mjs")).href);
      const usage = await import(pathToFileURL(path.join(LIB_DIR, "usage-sessions.mjs")).href);
      let bodyText = (result.content ?? []).map((c) => c.text).join("\n");
      if (!bodyText || /No matching chunks found/.test(bodyText)) return result;

      const blocks = bodyText.split("\n\n---\n\n");
      const kept = [];
      const keptMeta = []; // (id, score) 対。Level変換用
      for (const block of blocks) {
        const id = block.match(/\*\*id:\*\* `([0-9a-f]{10})`/i)?.[1];
        if (!id) { kept.push(block); continue; }
        if (!includeHistory) {
          let status = "active";
          try { status = lc.getStatus(id); } catch {}
          if (status !== "active") { kept.push(null); continue; }
        }
        if (!includeHidden) {
          let v = "warm";
          try { v = await vis.getVisibility(id); } catch {}
          if (!vis.isNormallyVisible(v)) { kept.push(null); continue; }
        }
        kept.push(block);
        const score = block.match(/### \[score: ([^\]]+)\]/)?.[1] ?? "?";
        keptMeta.push({ id, score });
      }

      // ── セッション既読ペナルティ（順位を下げる）──────────────────────────
      const seenSet = new Set(await usage.getSeenIds(sessionId));
      const entries = [];
      for (let i = 0; i < kept.length; i++) {
        if (kept[i] === null) continue;
        entries.push({ id: keptMeta[entries.length]?.id ?? `__noblock${i}__`, block: kept[i] });
      }
      const { ordered: penalizedBlocks, seenCount } = usage.applySeenPenalty(entries, seenSet);

      // ── Level 0-1 変換（full_content=false のとき）──────────────────────
      let visibleBlocks = penalizedBlocks;
      if (!fullContent) {
        const levelBlocks = [];
        for (const block of visibleBlocks) {
          const id = block.match(/\*\*id:\*\* `([0-9a-f]{10})`/i)?.[1] ?? "?";
          let summary = null;
          try { summary = await summaryMod.getValidSummary(id, "short"); } catch {}
          if (!summary) {
            try {
              await summaryMod.generateSummaryFor(id);
              summary = await summaryMod.getValidSummary(id, "short");
            } catch {}
          }
          // フォールバック: summaryが得られないときはtopicベースの簡潔表示に留める
          const topicLine = block.split("\n")[0] ?? `### [score: ?]`;
          const body = summary ? `\n${summary}` : "\n(要約なし — read_context で本文を取得)";
          levelBlocks.push(`${topicLine}\n**id:** \`${id}\`${body}`);
        }
        visibleBlocks = levelBlocks;
      }

      // ── Token Budget ────────────────────────────────────────────────────
      // 切り詰め順序: 結果はスコア順に並んでいるため「末尾側」が低関連。
      // 1. max_results: 件数上限（末尾から落とす）
      // 2. token_budget: 概算tokenの累積が予算を超えたら、それ以降を落とす
      const budget = args?.token_budget;
      const maxResults = args?.max_results;
      let droppedByBudget = 0;

      if (Number.isFinite(maxResults) && visibleBlocks.length > maxResults) {
        droppedByBudget += visibleBlocks.length - maxResults;
        visibleBlocks = visibleBlocks.slice(0, maxResults);
      }
      if (Number.isFinite(budget)) {
        const keptInBudget = [];
        let used = 0;
        for (const block of visibleBlocks) {
          const cost = estimateTokens(block) + 5; // 区切り等のオーバーヘッド
          if (used + cost > budget && keptInBudget.length > 0) {
            droppedByBudget += 1;
            continue;
          }
          // 1件目が予算超過でも最低1件は返す（空応答より有用）
          keptInBudget.push(block);
          used += cost;
        }
        visibleBlocks = keptInBudget;
      }

      if (!visibleBlocks.length) {
        return { content: [{ type: "text", text: "No matching chunks found." }] };
      }
      const notes = [];
      const filteredByDefault = blocks.length - kept.filter(Boolean).length;
      if (filteredByDefault > 0 && (!includeHistory || !includeHidden)) {
        notes.push(`${filteredByDefault} 件の記憶を既定フィルタ(履歴状態・低可視性)で非表示にしました`);
      }
      if (droppedByBudget > 0) {
        notes.push(`token_budget/max_results により ${droppedByBudget} 件を切り詰めました`);
      }
      if (seenCount > 0 && sessionId) {
        notes.push(`セッション内既読 ${seenCount} 件の順位を下げました`);
      }
      // ── 参照統計の記録（検索を妨げない・非同期扱い）────────────────────────
      const returnedIds = visibleBlocks
        .map((b) => b.match(/\*\*id:\*\* `([0-9a-f]{10})`/i)?.[1] ?? b.match(/`([0-9a-f]{10})`/i)?.[1])
        .filter(Boolean);
      if (returnedIds.length) {
        usage.recordAccess(returnedIds, sessionId);
        usage.markSeen(sessionId, returnedIds);
        // ── 想起理由ログ ───────────────────────────────────────────────────
        try {
          const visMod = await import(pathToFileURL(path.join(LIB_DIR, "visibility.mjs")).href);
          const recall = await import(pathToFileURL(path.join(LIB_DIR, "recall-log.mjs")).href);
          await recall.logRecall(
            await Promise.all(visibleBlocks.map(async (b, i) => {
              const id = b.match(/\*\*id:\*\* `([0-9a-f]{10})`/i)?.[1]
                ?? b.match(/`([0-9a-f]{10})`/i)?.[1];
              const reasons = ["relevance_match"];
              if (!includeHistory) reasons.push("active_status");
              try {
                const v = await visMod.getVisibility(id);
                if (v === "hot") reasons.push("visibility_hot");
                else if (v === "cold" || v === "archive") reasons.push("deep_recall");
              } catch {}
              return { id, reasons, score: null, rank: i + 1 };
            })),
            { sessionId, query: args?.query ?? null },
          );
        } catch {}
      }
      const suffix = notes.length ? `\n\n(${notes.join("。")}。必要なら include_history / include_hidden / max_results / token_budget を調整してください)` : "";
      return { content: [{ type: "text", text: visibleBlocks.join("\n\n---\n\n") + suffix }] };
    };
  }

  // ── get_recall_log: 想起履歴 ─────────────────────────────────────────────
  server.tool(
    "get_recall_log",
    "Show WHY a memory was recalled: the reasons, scores and queries of past retrievals.",
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
          lines.push(`- ${e.created_at} [rank:${e.rank}] 理由: ${reasons.join(", ") || "-"}`);
        }
        return { content: [{ type: "text", text: lines.join("\n") }] };
      } catch (err) {
        return { content: [{ type: "text", text: `エラー: ${err.message}` }], isError: true };
      }
    },
  );

  // ── get_visibility_recommendations: 推奨算出（自動適用なし）──────────────
  server.tool(
    "get_visibility_recommendations",
    [
      "Get VISIBILITY recommendations (hot/cold/archive candidates) based on usage statistics.",
      "This NEVER changes anything by itself. Apply suggestions manually via promote_memory.",
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
        lines.push("", "適用する場合は promote_memory(id, to, reason) を実行してください（このツール自体は何も変更しません）。");
        return { content: [{ type: "text", text: lines.join("\n") }] };
      } catch (err) {
        return { content: [{ type: "text", text: `エラー: ${err.message}` }], isError: true };
      }
    },
  );

  // ── begin_retrieval_session: セッション開始 ─────────────────────────────
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

  // ── recall_deep: 段階的深掘り（cold/archive含む）─────────────────────────
  server.tool(
    "recall_deep",
    [
      "Progressive deep retrieval. Stage 1: lightweight candidate list (metadata only, includes cold/archived memories).",
      "Stage 2 (pass resolve=true): get the full raw bodies of the chosen ids.",
      "Use this when normal search_context results are not enough.",
    ].join(" "),
    {
      query: z.string(),
      top_k: z.number().int().min(1).max(20).optional(),
      min_score: z.number().min(0).optional(),
      search_mode: z.enum(["hybrid", "bm25", "semantic"]).optional(),
      resolve: z.boolean().optional().describe("trueで候補の本文(raw)を返す。falseなら候補一覧のみ"),
      ids: z.array(z.string()).optional().describe("resolve=trueのときに本文を取得するID一覧"),
    },
    async ({ query, top_k, min_score, search_mode, resolve: doResolve, ids }) => {
      try {
        const text = (r) => (r.content ?? []).map((c) => c.text).join("\n");
        if (doResolve) {
          if (!ids?.length) throw new Error("resolve=true のときは ids を指定してください");
          const readTool = server._registeredTools?.["read_context"];
          if (!readTool) throw new Error("read_context が利用できません");
          const parts = [];
          for (const id of ids.slice(0, 10)) {
            const r = await readTool.handler.call(null, { id });
            parts.push(text(r));
          }
          return { content: [{ type: "text", text: parts.join("\n\n---\n\n") }] };
        }
        // Stage 1: 候補一覧（visibility/history制限なし・軽量）
        if (!originalSearchHandlerForRecall) throw new Error("search handler 未初期化");
        const r = await originalSearchHandlerForRecall.call(null, {
          query, tags: [], agent: undefined,
          topK: top_k ?? 10, minScore: min_score ?? 0, mode: search_mode ?? "hybrid",
        });
        const body = text(r);
        if (!body || /No matching chunks found/.test(body)) {
          return { content: [{ type: "text", text: "No matching chunks found." }] };
        }
        const blocks = body.split("\n\n---\n\n").map((b) => {
          const id = b.match(/\*\*id:\*\* `([0-9a-f]{10})`/i)?.[1] ?? "?";
          const topic = b.match(/### \[score:[^\]]+\] (.+)/)?.[1] ?? "?";
          const score = b.match(/### \[score: ([^\]]+)\]/)?.[1] ?? "?";
          return `- [score: ${score}] ${topic} | id=\`${id}\` | status=${"?"}`;
        });
        return {
          content: [
            {
              type: "text",
              text:
                `候補 ${blocks.length} 件（cold/archive も含む）。詳細は resolve=true + ids で取得:\n\n` +
                blocks.join("\n"),
            },
          ],
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
        if (!chain.length) {
          return { content: [{ type: "text", text: `\`${id}\` には後継記録がありません（これが現行版です）。` }] };
        }
        const lines = ["置換チェーン:"];
        let from = id;
        for (const r of chain) {
          lines.push(`- \`${from}\` → \`${r.successor_id}\` (by ${r.actor}${r.reason ? ` — ${r.reason}` : ""})`);
          from = r.successor_id;
        }
        lines.push("", `現行版: \`${current}\``);
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
      reason: z.string().describe("なぜ置き換えるのか（必須）"),
    },
    async ({ old_id, topic, content, tags, importance, ttl_days, reason }) => {
      try {
        const lc = await import(lifecyclePath);
        const r = await lc.supersedeChunk({
          oldId: old_id,
          topic,
          content,
          actor: trustedActor,
          tags: tags ?? [],
          importance: importance ?? "medium",
          ttlDays: ttl_days ?? null,
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
    "Bring a superseded/retracted/expired memory back to active state. Reason is required.",
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

  // ── get_memory_summary: 要約取得（rawは read_context で取得）─────────────
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

  // ── promote_memory: visibility変更（reason必須）─────────────────────────
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
}
