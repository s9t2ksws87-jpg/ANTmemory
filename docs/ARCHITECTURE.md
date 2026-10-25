# Native architecture

```text
MCP clients -> antmemory serve (stdio shim per connection)
            -> deployment-specific local IPC
               -> antmemory daemon (one writer per workspace)
                  -> stores/<workspace>/v4.db
                  -> local embedding worker
```

am-domain defines commands/contracts; am-store owns transactions, lifecycle, snapshots/import and learning indexes; am-anchor re-observes source handles; am-embed runs tokenizers/ONNX; am-tools implements 19 tools, ranking, output and cards; am-cli owns IPC, maintenance, migration, installation and Viewer.

Raw is append-only. Lifecycle, visibility and search indexes are separate. Search/read/use are distinct observations: opening a result is a recall association, not proof of usefulness. Explicit rejection also reduces recall. Derived summaries can be rebuilt from retained evidence without changing Raw.

Ruri uses retrieval prefixes, masked mean pooling and L2 normalization. Model identity includes the file fingerprint/contract, so model changes queue re-embedding rather than reusing incompatible vectors. The bundled INT8 ruri-v3-70m has 384 dimensions and up to 8,192 input tokens; project/language quality still needs observation. Arbitrary ONNX architectures are not automatically interchangeable: supported directory contracts are explicit.

Normal output prioritizes Raw excerpts, whole-response estimates and continuation. C2 reuse requires observable compaction generations. C0/C1 never assume an agent retained old replies. Unshown pages are not marked read.

Cards are short headings, not an autonomous planner. Agents control work cards; formal overall goals require supported verified user evidence. Viewing another card never implicitly selects its work. Reports differ from MCP-observed execution; actions outside MCP can remain unobserved.

The IDE-style Viewer is read-only on loopback. Human browsing neither trains recall nor changes a session's work. This is a trusted local-user tool, not a remote tenant boundary.
