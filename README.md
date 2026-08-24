# ANTmemory

Auditable persistent memory shared across AI agents such as Claude Code, Codex, Antigravity, and Hermes.

**Current release: ANTmemory v1.0.0.** This is the first public stable release.

ANTmemory was originally named **Agent Nest Tool**, shortened to **ANT**. The name also describes its ant-nest-like structure: multiple agents bring memories into a shared, compartmentalized store and retrieve the context their workspace needs.

> 日本語版: [README.ja.md](README.ja.md)

**Requirements:** Node.js 24 or newer (`node:sqlite`; Node 24.x LTS recommended).

ANTmemory builds on [agent-memory-store](https://github.com/vbfs/agent-memory-store) (MIT). A single MCP server implementation is shared by all agents, while each workspace gets its own physical SQLite database file. Workspace isolation is therefore enforced by separate files rather than by a scope field inside one database.

On top of the upstream server, ANTmemory adds two complementary capabilities:

- **Auditable history:** keep history, prohibit destructive deletion, and make changes explainable.
- **Selective recall:** keep the source memory intact, but retrieve only what is useful until deeper recall is requested.

The repository is relocatable. Scripts resolve the project root from `ANT_MEMORY_ROOT` or from their own location. Runtime workspace mappings are stored in `runtime/workspaces.json`, which is intentionally excluded from Git.

---

## What it provides

### Upstream memory features

- MCP tools for storing, searching, and reading memory through `write_context`, `search_context`, and `read_context`.
- Hybrid retrieval using BM25/FTS5 and semantic search with local MiniLM embeddings.
- Full database-file isolation between workspaces.
- FTS5 `trigram` tokenization for Japanese search.

### Keep history and audit changes

- **No physical deletion:** `delete_context` is always rejected and points callers to non-destructive alternatives.
- **Update with `supersede_context`:** creates a replacement while keeping the old record and its replacement chain. A reason is required.
- **Retract with `retract_context`:** invalidates a wrong record without removing it. A reason is required.
- **Audit events:** actor, timestamp, action, and reason are recorded in `memory_events`.
- **Actor trust boundary:** the canonical actor comes from `ANT_MEMORY_CLIENT` when the server starts. Agent-supplied actor names are normalized to the connected client, so a model cannot impersonate another client. Model/provider changes behind Hermes do not change the actor identity.
- **Active-first retrieval:** normal searches return the current active version rather than superseded/retracted history.

### Never forget, but do not recall until needed

- **Progressive Disclosure:** normal `search_context` output is metadata plus a short summary (Level 0–1). Full text is fetched explicitly through `read_context`, `recall_deep`, or `full_content=true`.
- **Measured token reduction:** on the fixed release corpus, average normal-search output fell from **792 to 277 estimated tokens (−65.0%)**, with zero raw-memory modification.
- **Visibility Layer:** `hot`, `warm`, `cold`, and `archive` are independent from importance. A memory may be `critical + cold`. Normal search exposes hot/warm; deep recall can include cold/archive.
- **Summary Layer:** deterministic local extraction with no LLM call. `chunks.content` remains canonical. A summary whose `source_hash` no longer matches the raw text is invalidated and regenerated.
- **Token Budget / `max_results`:** callers can cap returned context; lower-ranked material is truncated first and the truncation is reported.
- **Retrieval Sessions:** passing a `session_id` lowers the rank of memories already shown in the same task, reducing repeated context injection.
- **Recall reason log:** records why a memory was surfaced so the Viewer can show retrieval explanations.
- **Visibility recommendations:** usage signals can suggest hot/cold/archive changes, but ANTmemory does not apply them automatically. A human or calling workflow decides whether to run `promote_memory`.

See [`docs/ARCHITECTURE_VALIDATION.md`](docs/ARCHITECTURE_VALIDATION.md) for release evidence and [`docs/ARCHITECTURE_RESEARCH.md`](docs/ARCHITECTURE_RESEARCH.md) for the design comparison with other memory systems.

---

## Quick start

```powershell
git clone https://github.com/s9t2ksws87-jpg/ANTmemory.git
cd ANTmemory\server
npm ci
```

`npm ci` runs `bin/apply-patches.mjs` through `postinstall` using the release lockfile.

Add the MCP server to your agent configuration. Example for Claude Code:

```json
{
  "mcpServers": {
    "ant-memory": {
      "command": "node",
      "args": ["<ABSOLUTE_PATH_TO_ANTMEMORY>\\bin\\ant-memory.mjs"],
      "env": {
        "ANT_MEMORY_CLIENT": "claude-code"
      }
    }
  }
}
```

The server automatically resolves the workspace, creates the database, and ensures the audit, lifecycle, visibility, summary, usage, and recall schemas on first use.

For guidance on when an agent should store, search, supersede, retract, or deep-recall memories, see [docs/MEMORY_USAGE.md](docs/MEMORY_USAGE.md) or the Japanese guide [docs/MEMORY_USAGE.ja.md](docs/MEMORY_USAGE.ja.md).

---

## Viewer

ANTmemory includes a read-only localhost Viewer for inspecting workspace memories, lifecycle history, visibility, usage, and recall reasons.

```powershell
node viewer\server.mjs
```

On Windows you can also run `viewer\start-viewer.bat`. The Viewer listens on `127.0.0.1:3737` by default and opens workspace databases read-only. See [viewer/README.md](viewer/README.md).

---

## Repository layout

```text
ANTmemory/
├── bin/ant-memory.mjs       launcher: workspace resolution and DB switching
├── bin/apply-patches.mjs    idempotent upstream patches; run from postinstall
├── bin/migrate-*.mjs        non-destructive, idempotent migrations
├── bin/test-*.mjs           isolated test suites
├── server/                  agent-memory-store dependency + ANTmemory extensions
│   └── lib/                 audit, lifecycle, visibility, summary, usage, recall, schema
├── viewer/                  localhost-only read-only Viewer
├── docs/
│   ├── MEMORY_USAGE.md      agent operating guide (English)
│   ├── MEMORY_USAGE.ja.md   agent operating guide (Japanese)
│   ├── ARCHITECTURE_VALIDATION.md release evidence for v1.0.0
│   └── ARCHITECTURE_RESEARCH.md       design comparison/research
└── stores/                  per-workspace databases (Git-ignored)
```

This release tree contains only product source, tests, release evidence, and maintained operational documentation. Documentation lives under `docs/`.

---

## Workspace resolution

`bin/ant-memory.mjs` resolves a workspace in this order:

1. `ANT_MEMORY_WORKSPACE`
   - absolute path: use the folder name as the DB slug; same-name collisions receive a short hash and are recorded in `runtime/workspaces.json`.
   - plain name: use `stores/<name>/store.db`. Plain-name selection is not automatically registered in `workspaces.json`.
2. Walk upward from the current working directory looking for project markers:
   `.git`, `CLAUDE.md`, `AGENTS.md`, `GEMINI.md`, `.agents`, `.claude`, `package.json`, `pyproject.toml`, `Cargo.toml`, or `go.mod`.
3. If no safe workspace can be identified, fall back to `_shared`.

A new workspace receives its audit and selective-recall schemas automatically. `capture-baseline` remains an explicit human-run operation so test stores are not accidentally captured into a production baseline.

To inspect resolution without starting the server:

```powershell
node <ABSOLUTE_PATH_TO_ANTMEMORY>\bin\ant-memory.mjs --where
```

Launcher decisions are written to `logs/launcher.log`.

---

## Client behavior

| Client | Workspace detection |
|---|---|
| Claude Code | current working directory |
| Codex CLI | current working directory |
| Antigravity | IDE workspace heuristics described below |
| Hermes | current working directory or `ANT_MEMORY_WORKSPACE` |

All clients launch the same `bin/ant-memory.mjs` and identify themselves with `ANT_MEMORY_CLIENT`.

### Antigravity

Antigravity may launch MCP servers from its own installation directory without passing the project directory. ANTmemory therefore correlates Antigravity's internal `workspaceStorage` metadata with the language-server `--workspace_id` (SHA-256 of the folder URI). A single active workspace can be resolved deterministically; ambiguous multi-window cases fall back to the most recently used workspace.

This integration depends on undocumented Antigravity internals and may require adjustment after IDE updates. Use an explicit `ANT_MEMORY_WORKSPACE` whenever deterministic routing matters.

### Known Codex limitation

When Codex runs as a VS Code extension, ANTmemory currently has no reliable workspace hint and may fall back to `_shared`. Running Codex from a terminal inside the workspace resolves correctly.

### Hermes

The following Hermes-specific behaviors were observed while validating ANTmemory. They may change in future Hermes builds.

1. **Do not assume `hermes mcp add --env` writes an environment variable.** In the Hermes build tested for ANTmemory, `--env KEY=VALUE` was written into the server `args` as literal command-line arguments instead of into the `env` mapping. The MCP server therefore started without `ANT_MEMORY_CLIENT`, and ANTmemory recorded the client/actor as `unknown`. After `hermes mcp add`, inspect Hermes `config.yaml` and correct it manually if needed. A typical Windows path is `$HOME\AppData\Local\hermes\config.yaml`.
2. **`env` in `config.yaml` must be a mapping, not a list.** A list such as `- ANT_MEMORY_CLIENT=hermes` failed at startup with a `dictionary update sequence element ...; 2 is required` error.

```yaml
ant-memory:
  command: node
  args:
    - <ABSOLUTE_PATH_TO_ANTMEMORY>/bin/ant-memory.mjs
  env:
    ANT_MEMORY_CLIENT: hermes
```

3. **Treat an actor-mismatch warning as a configuration diagnostic.** Even when a tool call supplies `agent="hermes"`, ANTmemory intentionally trusts the server-side connection actor instead. If the connection actor is `unknown`, the supplied name is rejected/normalized and a warning is emitted. If you see this warning unexpectedly, verify `ANT_MEMORY_CLIENT` first.
4. **Hermes' patch/edit tool may refuse to modify `config.yaml`.** In the tested environment, Hermes' security guard rejected that write. Edit the file from a terminal or ordinary text editor instead.
5. **Reload MCP after changing the config.** Existing sessions do not pick up the change. Run `hermes mcp test <name>` to verify the connection, then open a new Hermes session.
6. **Tool names use the `mcp__ant_memory__*` prefix.** The MCP name `ant-memory` is normalized by replacing the hyphen with an underscore in the tool prefix.
7. **cwd-based workspace detection depends on where Hermes is launched.** Starting Hermes from a home directory or another location without project markers causes ANTmemory to fall back to `_shared`. For project work, launch Hermes with the project directory as the current working directory, or set `ANT_MEMORY_WORKSPACE` explicitly.

---

## Patches applied to agent-memory-store 0.0.9

`bin/apply-patches.mjs` applies the following measured fixes idempotently:

1. Quote hyphenated FTS5 search terms to avoid query-syntax errors.
2. Use the FTS5 `trigram` tokenizer for Japanese substring search.
3. Rebuild existing FTS indexes when migrating from the previous tokenizer.
4. Compare TTL timestamps using a consistent ISO timestamp format.
5. Run legacy-file migration before `store.db` creation can suppress it.
6. Implement the previously exposed but unused `min_score` behavior with score normalization.
7. Preserve raw ranking precision until after normalization instead of rounding RRF scores too early.
8. Normalize `Date` values during legacy migration so `node:sqlite` can bind them.
9. Set `busy_timeout=5000` to handle measured concurrent writes from multiple clients.

The patcher is intentionally version-sensitive. If an upstream update changes the target source strings or uses an unsupported version, installation fails instead of silently applying a partial patch.

---

## Tests

Tests use isolated stores and must not modify production workspace databases.

```powershell
node bin\test-baseline.mjs
node bin\test-actors.mjs
node bin\test-lifecycle-search.mjs
node bin\test-viewer.mjs
node bin\test-visibility.mjs
node bin\test-summaries.mjs
node bin\test-token-budget.mjs
node bin\test-progressive-disclosure.mjs
node bin\test-usage-sessions.mjs
node bin\test-visibility-recommendations.mjs
node bin\test-field.mjs
```

The architecture validation report records **12 suites / 201 cases passing**, including a five-process field test with Claude Code, Codex, Antigravity, and Hermes clients. See [docs/ARCHITECTURE_VALIDATION.md](docs/ARCHITECTURE_VALIDATION.md) for the exact scenarios and KPI methodology.

GitHub Actions runs a locked clean-install baseline on Windows and Linux, the architecture regression suite on Windows, a production dependency audit, a release-layout/privacy check, and a Git-history credential/runtime-artifact audit.

---

## Security and privacy

Memory databases can contain conversation content, project details, or other private material. `stores/`, `runtime/`, `logs/`, `backups/`, and local configuration are intentionally excluded from Git.

Before publishing forks or bug reports, verify that no database, launcher log, absolute personal path, credential, or exported memory content is attached. See [SECURITY.md](SECURITY.md).

The Viewer is intended for localhost use. ANTmemory does not provide authentication or a hardened remote multi-user deployment model.

---

## License

ANTmemory-owned code and documentation are released under the MIT License. Third-party software retains its original copyright and license. See [LICENSE](LICENSE) and [THIRD_PARTY_NOTICES.md](THIRD_PARTY_NOTICES.md).

---

## Maintenance

- **Upstream updates:** treat `agent-memory-store` updates as compatibility work. The patcher rejects unsupported versions rather than guessing.
- **Backups:** stop MCP writers and copy `stores/`, or use the migration scripts' pre-migration backups when appropriate.
- **Runtime state:** never commit `stores/`, `runtime/`, `logs/`, `backups/`, or generated test artifacts.
