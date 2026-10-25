# Upgrading from 1.0.0 to 2.0.0

This is a major release because the default MCP tool surface and some inputs changed. The existing SQLite store layout remains usable; replacing the runtime does not mean exporting or recreating memories.

1. Stop all MCP processes writing to this ANTmemory installation, and close the Viewer.
2. Back up the entire `stores/` directory while writers are stopped. Keep `runtime/workspaces.json` so existing absolute-path-to-store mappings are retained. Keep your MCP client configuration separately.
3. Update the source checkout to the release, then run `npm ci` inside `server/`. Node.js 24 or newer is required. The old upstream dependency and patcher are removed.
4. Keep the existing stores in place. Schema setup runs on startup. Do not copy development/private stores into a public repository or replace stores with test fixtures.
5. Run `node bin/ant-memory.mjs --where` with the same cwd and environment as your client. Check the resolved store before restarting every MCP connection.
6. Inspect known records with `read_context` and `get_context_history`, and inspect current state with `get_state` / `list_state`. Search normally, then explicitly include history/hidden records when checking older material.

To roll back after a schema-changing upgrade, stop all writers and restore the matching source/dependencies and backed-up stores together. Do not assume that an older server understands every field written by a newer release.

## Client instruction changes

| Old behavior/instruction | 2.0.0 |
|---|---|
| Supply `agent` when writing | Connection identity comes from `ANT_MEMORY_CLIENT`; no write-time `agent`/`actor` input |
| `max_results` | Use `top_k` for search count |
| `search_state` | `list_state(query=...)` |
| `compare_and_set_state` | `set_state(expected_value=...)` |
| `recall_deep` in routine work | `search_context(include_history/include_hidden/full_content)` or `read_context` |
| Re-send all metadata for a small correction | `correct_context` inherits omitted metadata |
| Summaries/recall sessions exposed by default | Maintenance tools require `ANT_MEMORY_ADVANCED_TOOLS=1` |
| `delete_context` | Not exposed, including advanced mode |

`source_ref` is optional and exposed by normal write/read/search/list operations. Earlier records without a source remain readable. A source-only `correct_context` can add a handle while retaining the older revision.

`goal.current` is an ordinary shared-state convention. It does not trigger automatic retrieval or enforcement. Experimental efficiency protocols from the development workspace are not part of this release.

## Verified scope

Release validation includes opening a fixture created through the actual 1.0.0 MCP server, checking both replacement revisions, their status/current flags, shared state, original chunk metadata, audit events, and SQLite integrity through the new runtime. This is a bounded compatibility test, not proof for every possible external database modification. See [RELEASE_VALIDATION.md](RELEASE_VALIDATION.md).
