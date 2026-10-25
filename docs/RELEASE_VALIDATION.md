# Release 4.0.0 validation

The V4 source is based on reviewed development revision 4634d5d9ac38d1ff4abe982f534601a5cf557a12. Public metadata uses version 4.0.0/MIT and an exact MCP version; legacy-writer discovery also recognizes the archived public-2.x path. No development stores, registries or conversation material are part of the export.

## Reproduce

Local Windows validation for this release: 89 Rust tests passed, one model-family stress test was ignored, and clippy completed without warnings. The isolated source smoke passed 25 checks including public-2.x migration. A complete installation with the pinned runtime/model passed 31 checks, including actual semantic inference and both Node compatibility launchers. These results cover the fixtures and build described here.

```text
cargo test --workspace --release --locked
cargo clippy --workspace --all-targets --release --locked -- -D warnings
cargo build --release --locked -p am-cli
python scripts/smoke_mcp.py --migration
```

The smoke runner creates disposable isolated data under ignored runtime/. Migration fixtures use the real public-2.x native DB layer and its original audit SQL declarations, without model downloads. It exercises Raw, withdrawal/state, and unmapped tables; real user databases still require rehearsal and equality checks.

For a full Windows ZIP, run smoke_mcp.py with --exe and --release-root pointing into a freshly extracted package, plus --semantic --migration --compat. This clears inherited ANT_MEMORY/ORT settings, verifies normal executable-relative root/model discovery, requires actual semantic inference, tests normal MCP operations and checks the read-only Viewer via HTTP. Node is needed only for --migration and --compat. Source/build/model checks are distinct from user data migration and real-world usefulness.

The ignored per-run result.json and text/log files are executable evidence. The release SHA256SUMS and manifest bind the published package to its files and source commit. GitHub's test workflow checks Windows and Linux core paths; Windows model inference is additionally checked locally against the packaged runtime/model.

One existing model stress test requires both downloaded model families and is ignored by default. Passing synthetic tests does not prove every query relevant, every client routed correctly, provider token savings, or exhaustive local conversation compatibility. C0/C1 auto-reuse, unmatched goal authority and unlinked historical revisions retain their documented limits.
