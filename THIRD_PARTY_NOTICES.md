# Third-Party Notices

ANTmemory includes and depends on third-party open-source software. Each third-party component remains subject to its own copyright and license terms.

## agent-memory-store

- Project: `agent-memory-store`
- Upstream: https://github.com/vbfs/agent-memory-store
- Version targeted by ANTmemory v1.0.0: `0.0.9`
- Author declared by upstream package metadata: Vinícius Barreto
- License declared by upstream package metadata: MIT

ANTmemory installs `agent-memory-store` as an npm dependency under `server/node_modules/` and applies compatibility/bug-fix patches from `bin/apply-patches.mjs` during `postinstall`.

The upstream package's own copyright and license remain in force. ANTmemory's MIT license applies only to ANTmemory-owned code, documentation, tests, and patches to the extent those portions are independently copyrightable.

## Transitive dependencies

`agent-memory-store` brings additional npm dependencies and optional platform packages. Their individual license metadata is recorded in `server/package-lock.json` and/or their installed package metadata. ANTmemory does not relicense those packages.

For redistribution, preserve all license notices required by the corresponding dependency licenses.
