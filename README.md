# ANTmemory 4

Local, auditable memory shared by Claude Code, Codex, Antigravity, Hermes and other MCP clients. **Release 4.0.0** replaces the JavaScript server with a Rust daemon and small stdio clients.

[日本語](README.ja.md) · [Download](https://github.com/s9t2ksws87-jpg/ANTmemory/releases/latest) · [Upgrade from 2.x](docs/UPGRADING.md)

ANT originally meant **Agent Nest Tool**: agents bring decisions, evidence and stopping points into a shared nest. Each resolved workspace has a separate SQLite database. Another agent can recall a record, inspect its origin, and return to the real file.

## Windows x64 quick start

1. Download ANTmemory-4.0.0-windows-x64.zip and verify it with SHA256SUMS.txt.
2. Extract the whole ZIP into a writable folder. Keep runtime/, models/, stores/ and the notices together.
3. Add the executable to your MCP client; replace this path with the extracted location:

```json
{"mcpServers":{"ant-memory":{"command":"C:/tools/ANTmemory/runtime/bin/antmemory.exe","args":["serve"]}}}
```

The ZIP includes the executable, ONNX Runtime CPU libraries and a pinned INT8 Ruri model. Normal use needs no Node.js, npm, Rust compiler, cloud embedding service or first-run model download. Components retain their own licenses.

Start the client from your project. If its working folder is elsewhere, put the intended project path on one UTF-8 line in that folder's .antmemory-workspace, or set ANT_MEMORY_WORKSPACE in that project's MCP configuration. Reconnect after changing routing. Unresolved clients use _shared and receive a short notice; this is a real shared fallback, not project isolation.

The optional Node entry points bin/ant-memory.mjs and server/native-server.mjs forward to the installed executable without npm dependencies. Historical public-2.x code is retained under legacy/v2/.

## Everyday memory

```text
search_context(query="previous parser decision")
read_context(id="the read reference returned by search")
write_context(content="We chose B because… Remaining unknown: …")
```

Only content is required to save. The title, client identity, workspace and recording-time Git information are automatic; tags and anchors are optional.

- Default search: four Raw excerpts within about 1,000 estimated tokens. Read the full body or request a range.
- Corrections, replacement, withdrawal and expiry preserve the original body and audit history. Physical deletion is not exposed.
- A selected read learns recall words separately from usefulness; a later not_relevant verdict also reduces the association.
- Work cards are short headings for unfinished work. show_goals(id) only inspects; enter=true selects work. Overall goals require a user quote verified against supported conversation evidence; unmatched evidence stays a proposal.
- UNKNOWNs, hypotheses and observations are separate records. Memory records what was stored, not automatic truth about the world. Check lifecycle, dates and source evidence for important decisions.

| Purpose | Normal tools (19 total) |
| --- | --- |
| Recall/save | search_context, write_context, read_context, list_context |
| Preserve/revise | correct_context, supersede_context, retract_context, restore_context, change_importance, get_context_history, set_visibility |
| Shared state | get_state, set_state, list_state |
| Observation | observe_context, list_unknowns |
| Goal cards | post_goal, close_goal, show_goals |

[Operating guide](docs/MEMORY_USAGE.md). Inventories have totals and continuation. Automatic reuse of sent bodies requires observable C2 compaction generations; C0/C1 get compact replies but are not assumed to remember them after compaction.

## Viewer and maintenance

Run viewer/start-viewer.bat, or:

```powershell
./runtime/bin/antmemory.exe viewer
./runtime/bin/antmemory.exe status
./runtime/bin/antmemory.exe doctor --all
```

The IDE-style Viewer is read-only at http://127.0.0.1:3737; --port 3738 chooses another port. Human viewing does not train search or change agent work. The daemon periodically checks integrity, checkpoints WAL, rotates logs and makes daily backups retained for 14 days. [Architecture](docs/ARCHITECTURE.md).

## Build from source

Use a current stable Rust toolchain. The binary package targets Windows x64. Linux CI checks the core/BM25 path, not Windows runtime libraries.

```powershell
cargo test --workspace --release --locked
cargo clippy --workspace --all-targets --release --locked -- -D warnings
cargo build --release --locked -p am-cli
python scripts/fetch_assets.py
New-Item -ItemType Directory -Force stores, runtime | Out-Null
$env:ANT_MEMORY_ROOT = (Get-Location).Path
./target/release/antmemory.exe install
python scripts/smoke_mcp.py
```

The asset fetcher verifies pinned SHA-256 values in assets.lock.json. Without a supported model/runtime, search explicitly falls back to BM25. Public-2.x migration checks require Node.js 24+: python scripts/smoke_mcp.py --migration. Windows packaging: python scripts/collect_licenses.py, then python scripts/build_release.py. [Validation](docs/RELEASE_VALIDATION.md) states the actual test scope.

## Upgrade and license

**Existing 2.x users must follow [UPGRADING.md](docs/UPGRADING.md).** V4 uses stores/<workspace>/v4.db and does not silently overwrite store.db. Rehearsal snapshots and verifies converted data. Stop old writers and retain backups before the final switch.

User databases, registries, conversation logs, models and build/test output are excluded from Git. The binary ZIP includes no user memory. ANTmemory-owned code/docs are MIT licensed; [third-party notices](THIRD_PARTY_NOTICES.md) retain upstream attribution and model/runtime/dependency notices. [Security policy](SECURITY.md).
