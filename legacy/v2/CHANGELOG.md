# Changelog

## 2.0.0 — 2026-10-23

- Own the native MCP server, SQLite storage, migration, embedding, and retrieval core; remove the runtime dependency on `agent-memory-store` and the install-time patcher.
- Preserve old Raw bodies and metadata through lifecycle operations, migration, and concurrent use. Add metadata-inheriting `correct_context`.
- Reduce the normal agent surface to 16 tools; make maintenance/legacy tools opt-in with `ANT_MEMORY_ADVANCED_TOOLS=1`. Physical deletion is never exposed.
- Consolidate result count into `top_k` and current-state operations into `get_state`, `set_state`, and `list_state`, including conditional updates.
- Add explicit observation/UNKNOWN/hypothesis lineage, reobservation, and unresolved-UNKNOWN listing.
- Expose optional Raw source handles and timestamps through normal memory operations. Document `goal.current` as ordinary shared state without automatic enforcement.
- Update the read-only Viewer and regression coverage.
- Publish only completed model downloads into an atomic cache, avoiding reuse of interrupted initial downloads.
- Refresh dependency locks and public operating/upgrade documentation.

Breaking tool/input changes and database-preserving upgrade steps are in [docs/UPGRADING.md](docs/UPGRADING.md). Development efficiency prototypes are excluded.

## 1.0.0

Initial public stable release with auditable lifecycle, selective recall, workspace-separated stores, and a read-only Viewer. Its historical validation is preserved in [docs/ARCHITECTURE_VALIDATION.md](docs/ARCHITECTURE_VALIDATION.md).
