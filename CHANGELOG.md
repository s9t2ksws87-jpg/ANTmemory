# Changelog

## 4.0.0 — 2026-10-25

- Native Rust daemon/shim replaces the JavaScript server; public-2.x source is retained under legacy/v2 and old Node entry points bridge to V4.
- 19 normal tools, content-only saves with automatic Git provenance, source re-observation, UNKNOWNs and lightweight work/overall cards.
- Four default excerpts/~1,000-token search responses, partial Raw reads, bounded lists with totals/continuation, and safe C2 response reuse.
- Pinned local Ruri INT8 model and ONNX Runtime CPU distribution; file-fingerprinted model identity and background vectors.
- Correction/withdrawal/restore and explicit older-format migration retain Raw/history. Later rejection also affects search-derived recall words and past-read suggestions.
- Workspace links and fallback disclosure, old-shim rebinding, UNC paths and link-removal routing recovery. Viewing another card does not select its work.
- IDE-style native read-only Viewer and automatic maintenance/backups.
- Windows x64 offline binary ZIP, dependency notices and checksums. Existing 2.x stores require explicit migration; v1/v2 tags remain available.


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
