# ANTmemory

Auditable persistent memory shared across Claude Code, Codex, Antigravity, Hermes, and other MCP clients.

**Current release: 2.0.0.** [日本語](README.ja.md) · [Upgrade from 1.0.0](docs/UPGRADING.md)

ANT originally meant **Agent Nest Tool**: agents bring context into a shared nest, with a physically separate SQLite database for each workspace. One agent can record a decision, another can continue from it, and both can inspect its source and revision history.

Version 2 owns its MCP server, storage, migration, and retrieval implementation. It no longer installs or patches `agent-memory-store` at runtime. The project's origins and retained attribution are documented in [THIRD_PARTY_NOTICES.md](THIRD_PARTY_NOTICES.md).

## What it does

- Shares workspace context and current state across clients. The connected client's `ANT_MEMORY_CLIENT` supplies the writer identity.
- Preserves original records through corrections, replacements, retractions, and expiry. Changes carry reasons and audit events; physical deletion is not exposed.
- Retrieves active hot/warm records by default as metadata plus short summaries. Raw bodies, historical versions, and cold/archive records are available explicitly.
- Combines Japanese-compatible trigram FTS5/BM25 with local MiniLM embeddings. If the model is unavailable, search falls back to BM25.
- Records observations, explicit UNKNOWNs, hypotheses, and reobservations without overwriting earlier evidence. Optional `source_ref` handles lead back to Raw.
- Uses a small default surface of **16 tools**. Maintenance and compatibility tools require an explicit advanced-mode setting.

Memory is a record of an observation or decision, not automatic truth. Search scores are relative to retrieved candidates; `1.0` does not guarantee relevance. Check timestamps, lifecycle, and source evidence when correctness matters.

## Quick start

Requires **Node.js 24 or newer** and npm.

```sh
git clone https://github.com/s9t2ksws87-jpg/ANTmemory.git
cd ANTmemory/server
npm ci
```

Add the launcher to your client's MCP configuration. Replace the path and client name:

```json
{
  "mcpServers": {
    "ant-memory": {
      "command": "node",
      "args": ["<ABSOLUTE_PATH_TO_ANTMEMORY>/bin/ant-memory.mjs"],
      "env": {
        "ANT_MEMORY_CLIENT": "claude-code"
      }
    }
  }
}
```

Start the client from the intended project directory, or set `ANT_MEMORY_WORKSPACE` to its absolute path in `env`. Agents must target the same workspace to share its database. Restart MCP connections after changing settings or upgrading.

The first model load downloads the local `Xenova/all-MiniLM-L6-v2` model (the fp32 ONNX file is about 90 MB). Completed downloads are cached atomically; interrupted downloads are not reused. Model-loading messages go to stderr. The transformers runtime's cache directory contains the `antmemory-atomic` subdirectory.

## Normal tools

| Purpose | Tools |
|---|---|
| Retrieve and store | `search_context`, `write_context`, `read_context`, `list_context` |
| Preserve and revise history | `correct_context`, `supersede_context`, `retract_context`, `restore_context`, `change_importance`, `get_context_history` |
| Visibility | `set_visibility` |
| Current shared state | `get_state`, `set_state`, `list_state` |
| Observation cycle | `observe_context`, `list_unknowns` |

Use `top_k` to set search result count, `token_budget` to bound approximate output tokens, and `full_content`, `include_history`, or `include_hidden` when needed. `read_context` returns the stored body with status, visibility, current-version state, timestamps, and source handle when available.

`correct_context` inherits omitted metadata and creates a revision. `supersede_context` replaces the overall content with explicitly supplied metadata. Both keep the old body. Changes such as revision, retraction, restoration, importance, and visibility require reasons. `delete_context` is unavailable even in advanced mode.

State is a current shared blackboard, not a durable history. An optional `goal.current` key can point at the current overall purpose; it is an ordinary state key and is not automatically populated, retrieved, or enforced. Use `set_state(expected_value=...)` to reject stale concurrent writes, and keep decisions and reasons in memory records.

See the [English operating guide](docs/MEMORY_USAGE.md) or [日本語の使い方](docs/MEMORY_USAGE.ja.md) for examples.

## Workspace routing

The launcher resolves a workspace in this order:

1. `ANT_MEMORY_WORKSPACE`: an absolute project path, or an explicit store name.
2. A project marker found by walking upward from cwd (`.git`, `AGENTS.md`, `CLAUDE.md`, `GEMINI.md`, `.agents`, `.claude`, `package.json`, `pyproject.toml`, `Cargo.toml`, `go.mod`).
3. Validated workspace environment hints when cwd does not resolve a project.
4. Antigravity workspace metadata on supported Windows installations.
5. `_shared` when no project can be resolved.

An absolute path uses a stable slug in `runtime/workspaces.json`; same-name collisions get a short hash. A plain store name selects `stores/<name>/store.db` directly. `_shared` is a real shared fallback, not a project-specific database. Inspect routing before relying on isolation:

```sh
node /absolute/path/to/ANTmemory/bin/ant-memory.mjs --where
```

Antigravity detection uses IDE internals and may choose the most recent workspace when multiple windows are ambiguous. Explicit `ANT_MEMORY_WORKSPACE` is the deterministic option. Clients launched from an IDE installation or home directory may otherwise resolve `_shared`. Check `--where` and `logs/launcher.log` for the actual selection.

For Hermes, ensure its saved `env` is a mapping containing `ANT_MEMORY_CLIENT`; some previously observed builds placed `--env` values in `args`. Restart the connection after correcting client configuration.

## Viewer

```sh
node viewer/server.mjs
```

The read-only Viewer binds to `127.0.0.1:3737` and browses workspace databases, history, visibility, usage, and recall reasons. Windows users can run `viewer/start-viewer.bat`. See [viewer/README.md](viewer/README.md).

## Maintenance and tests

```sh
npm run test:all --prefix server
npm audit --omit=dev --audit-level=high --prefix server
```

The release runner executes isolated regression suites and writes detailed results under ignored `test-logs/`. It covers preservation, retrieval, native MCP, the 16-tool surface, observation lineage, goal/source provenance, Viewer, concurrency, and download-cache integrity. [Release validation](docs/RELEASE_VALIDATION.md) separates these checks from real-world usefulness.

Set `ANT_MEMORY_ADVANCED_TOOLS=1` on the MCP server only when maintenance requires legacy aliases, deep-recall/session tools, summary diagnostics, recall logs, or visibility recommendations. Ordinary agents can perform their workflow through the 16 normal tools.

`stores/`, `runtime/`, `logs/`, `backups/`, dependencies, and generated test data are excluded from Git. Stop writers and back up stores before upgrades. See [UPGRADING.md](docs/UPGRADING.md), [CHANGELOG.md](CHANGELOG.md), and [SECURITY.md](SECURITY.md).

## Repository layout

```text
bin/ant-memory.mjs        workspace launcher
bin/test-*.mjs            isolated regression suites
server/native-server.mjs owned MCP entry point
server/lib/              storage, retrieval, lifecycle, observation, cache
viewer/                  read-only localhost Viewer
docs/                    usage, upgrade, research and validation documents
stores/                  per-workspace databases (ignored)
```

Historical version-1 evidence remains in [ARCHITECTURE_VALIDATION.md](docs/ARCHITECTURE_VALIDATION.md); it is not a measurement of version 2. Architecture research is in [ARCHITECTURE_RESEARCH.md](docs/ARCHITECTURE_RESEARCH.md).

## License

MIT for ANTmemory-owned code and documentation. Third-party software retains its own licenses. See [LICENSE](LICENSE) and [THIRD_PARTY_NOTICES.md](THIRD_PARTY_NOTICES.md).
