# Using ant-memory: shared agent rules

ANTmemory is persistent memory shared by multiple AI agents such as Claude Code, Codex, Antigravity, and Hermes. Each workspace uses a physically separate SQLite database, so memories from different projects are not mixed into one store.

Database path: `<ANTMEMORY_ROOT>/stores/<workspace>/store.db`

Japanese version: [`MEMORY_USAGE.ja.md`](MEMORY_USAGE.ja.md)

---

## 1. Tools

| Tool | Purpose |
|---|---|
| `search_context` | Search before starting work. By default returns metadata + short summary for active memories. Use `full_content=true` when raw text is actually needed. |
| `write_context` | Store decisions, discoveries, preferences, and durable working knowledge. |
| `read_context` | Fetch the full raw content for one memory by `id`. |
| `list_context` | Inventory memories without returning full content. |
| `supersede_context` | Replace a memory while preserving the old version. Requires `reason`. |
| `retract_context` | Mark a memory as wrong/retracted without deleting it. Requires `reason`. |
| `change_importance` | Change importance. Requires `reason`. |
| `restore_context` | Restore a superseded/retracted memory to active. Requires `reason`. |
| `get_context_history` | Inspect creation and change history. |
| `get_successor_context` | Follow a supersede chain to the current version. |
| `promote_memory` | Change visibility (`hot/warm/cold/archive`) independently from importance. Requires `reason`. |
| `get_memory_summary` | Fetch short/detailed derived summaries. Use `read_context` for the canonical raw text. |
| `recall_deep` | Progressive deep recall including cold/archive memories. Stage 1 lists candidates; Stage 2 resolves selected IDs to raw content. |
| `begin_retrieval_session` | Start a retrieval session. Passing its `session_id` to searches lowers the rank of already-seen memories. |
| `get_recall_log` | Inspect why a memory was recalled. |
| `get_visibility_recommendations` | Get visibility-change recommendations from usage signals. Recommendations are never applied automatically. |
| `get_state` / `set_state` | Store mutable task state, progress flags, or counters. |

`delete_context` is intentionally rejected. Use `supersede_context` for updates and `retract_context` for invalid records. Both require a reason.

### Selective-recall search parameters

```text
search_context(
  query: "...",
  include_history: true,   // include superseded/retracted/expired
  include_hidden: true,    // include cold/archive
  full_content: true,      // return raw content instead of summaries
  max_results: 5,          // result-count cap
  token_budget: 500,       // approximate output-token budget
  session_id: "...",       // lower rank for already-seen memories
)
```

---

## 2. When to search

Make `search_context` a normal first step when previous decisions may matter, especially when:

- the user refers to something said or decided earlier;
- you are touching settings, specifications, naming, style, or policy that may already have been decided;
- you are returning to the same file, project, or topic;
- you do not know where the previous session stopped (`get_state` may also help).

Do not give up after one phrasing. Try two or three reasonable query variants when recall matters. The default `hybrid` mode is appropriate for most work.

`min_score` is a relative score where the best hit is normalized to `1.0`. Raise it to keep only near-best matches; lower it or increase `top_k` when you need broad recall.

### Search tips

- FTS uses a trigram index, so terms of three or more characters are more reliable for keyword retrieval.
- Natural-language questions are fine because hybrid retrieval also uses semantic similarity.
- Hyphenated terms such as `ant-memory` can be searched directly; ANTmemory patches the upstream FTS5 parsing issue.

---

## 3. When to write

Store information when the next agent would otherwise have to ask the same question or repeat the same investigation.

Good candidates:

- decisions and the reasons behind them;
- user preferences, constraints, and explicit dislikes that materially affect later work;
- failed approaches worth avoiding next time;
- durable project facts that are not already represented clearly in repository files;
- environment/tool quirks that are not obvious from source or documentation;
- checkpoints in long-running work, usually alongside `set_state`.

Do not store:

- facts that can be read directly from the repository, file tree, or Git history;
- trivial one-off conversation details;
- passwords, API keys, access tokens, or other credentials;
- speculation written as if it were confirmed fact.

---

## 4. Writing conventions

```text
write_context(
  topic:      "short, specific subject",
  content:    "self-contained context plus decision/reason",
  agent:      "claude-code" | "codex" | "antigravity" | "hermes",
  tags:       ["category", "project", "subject"],
  importance: "low" | "medium" | "high" | "critical",
  ttl_days:   30
)
```

- `topic`: be specific. Avoid generic labels such as “memo” or “work”.
- `content`: write enough context that the memory still makes sense months later. Prefer decision + reason over isolated keywords.
- `agent`: the canonical actor is the server-side `ANT_MEMORY_CLIENT`. A conflicting caller-supplied agent name is normalized to the connected client, so this field can usually be omitted.
- `tags`: use stable retrieval/filter keys such as project, chapter, domain, or subsystem.
- `importance`:
  - `critical`: violating this would break the project or a core rule;
  - `high`: an explicit durable decision;
  - `medium`: normal working memory (default);
  - `low`: reference-only information.
- `ttl_days`: use TTL for temporary information. Do not add TTL to permanent decisions unless expiration is intentional.

### When a memory changes

Do not delete the old record.

- policy/decision changed → `supersede_context`
- record was wrong → `retract_context`
- only importance changed → `change_importance`
- restore a superseded/retracted record → `restore_context`
- inspect the chain → `get_context_history` / `get_successor_context`

```text
old = search_context("writing style policy")
supersede_context(old_id, new_topic, new_content, reason: "user changed to policy B")
get_successor_context(old_id)
search_context("writing style policy", include_history=true)
```

Normal searches return active memory only. Use `include_history=true` when historical, retracted, or expired records matter.

---

## 5. Passing work between sessions or agents

Use mutable state for “where are we now?” and memory for durable history.

```text
set_state("current-work", { chapter: 3, stage: "draft-complete", next: "revision" })
get_state("current-work")
```

`state` is overwrite-oriented. Use `write_context` when the history itself matters.

---

## 6. Human/operator notes

- Shared server runtime: `<ANTMEMORY_ROOT>/server`
- Launcher: `<ANTMEMORY_ROOT>/bin/ant-memory.mjs`
- Inspect workspace resolution without starting the server:

```powershell
cd <workspace-to-check>
node <ANTMEMORY_ROOT>\bin\ant-memory.mjs --where
```

- Launcher log: `<ANTMEMORY_ROOT>/logs/launcher.log`
- Workspace registry: `<ANTMEMORY_ROOT>/runtime/workspaces.json`
- To force routing deterministically, set `ANT_MEMORY_WORKSPACE` explicitly.

Never commit databases, runtime mappings, logs, backups, or credentials. See [`../SECURITY.md`](../SECURITY.md).
