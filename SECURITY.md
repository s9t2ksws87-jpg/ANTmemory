# Security Policy

ANTmemory is a local-first MCP memory service. It stores agent memory in local SQLite databases and is designed for trusted single-user development environments.

## Supported version

Security fixes are currently targeted at the latest stable release line.

## Threat model

ANTmemory is not a hardened remote multi-user service. In particular:

- the MCP server is intended to be launched locally by trusted agent clients;
- the Viewer is intended for localhost use;
- ANTmemory does not provide user accounts, authentication, authorization, tenant isolation, or an internet-facing security boundary;
- anyone who can read the workspace database files can read the stored memory contents.

Do not expose the Viewer or MCP transport to an untrusted network without adding an appropriate security layer outside ANTmemory.

## Sensitive local data

Memory may contain conversation excerpts, project details, source-code context, file names, model/provider provenance, or other private information.

The following paths are runtime/local state and must not be committed or attached to public bug reports:

- `stores/`
- `runtime/`
- `logs/`
- `backups/`
- `backup-configs/`
- `*.db`, `*.db-wal`, `*.db-shm`
- local MCP configuration containing credentials or private paths

The repository `.gitignore` excludes these paths, but `.gitignore` does not remove data that was committed in earlier Git history. Before making a fork public, scan the full history for credentials and private data.

## Reporting a vulnerability

Please report security-sensitive findings privately to the repository owner through GitHub's private vulnerability reporting feature when available. If private reporting is not enabled, open a minimal issue that does not include secrets, memory contents, database files, or exploit details, and ask for a private contact channel.

For ordinary bugs that do not involve sensitive information, use GitHub Issues.

## Native runtime and dependencies

ANTmemory 2.0.0 owns its MCP/storage/search runtime. It does not install or patch `agent-memory-store`; derived-code attribution is retained in `THIRD_PARTY_NOTICES.md`.

Dependency updates and native-runtime changes require compatibility checks before release. Install from `server/package-lock.json` with `npm ci`, run the release regression suite, and audit production dependencies.
