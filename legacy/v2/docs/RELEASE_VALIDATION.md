# ANTmemory 2.0.0 release validation

Observed on 2026-10-23. These checks establish the behaviors below for the tested inputs; counts do not establish universal usefulness or correctness of recalled claims.

## Local release gate (Windows, Node.js 24.18.0 / npm 11.17.0)

`npm run test:all --prefix server`: **20 suites, 262 passed, 0 failed**.

| Suite | Passed |
|---|---:|
| baseline | 15 |
| actors | 9 |
| lifecycle search | 10 |
| Viewer | 10 |
| visibility | 10 |
| summaries | 11 |
| token budget | 6 |
| progressive disclosure | 11 |
| usage sessions (advanced) | 8 |
| visibility recommendations (advanced) | 6 |
| multi-client field scenarios | 13 |
| preservation | 12 |
| retrieval core | 10 |
| native MCP server | 18 |
| memory updates | 25 |
| observation cycle | 23 |
| goal and source provenance | 17 |
| normal agent surface | 31 |
| concurrent writes | 11 |
| model download cache | 6 |

The suites use isolated synthetic stores. Detailed stdout/stderr and a machine-readable summary are generated under ignored `test-logs/`. Production/private memories are not release fixtures.

## Existing public database compatibility

A fixture was created through the actual 1.0.0 MCP runtime before replacing it. It contained an original memory, its replacement, two audit events, and a current-state value. The 2.0.0 runtime was then connected as a different client.

**9 checks passed:** 16-tool discovery; original Raw still readable and superseded/non-current; replacement Raw still active/current; shared state unchanged; replacement history accessible; normal search only exposes current revision; all original chunk fields unchanged; audit event count retained; SQLite `integrity_check` returns `ok`.

This tests the normal public 1.0.0 schema and operations in this fixture. It does not cover every possible external schema or manual database modification.

## Clean installation and semantic inference

- `npm ci` succeeds, including the ONNX runtime dependency setup.
- The lockfile SHA-256 is unchanged by reinstalling: `3790d284510f19e1af96da53e026f60819802cce282a5fa6c832d5907814f02b`.
- Actual MiniLM inference returns **384 finite values**, including after clean dependency installation and a fresh model download. This check waits for the model and does not count BM25 fallback as semantic success.
- Cache tests cover complete reuse, interrupted streams, declared-length truncation, failed replacement, file-size corruption, and handled-error partial cleanup. Interrupted-process partial files are ignored, not reused as cache hits.
- `npm audit --omit=dev --json` reports **0 vulnerabilities** at observation time. This is a dated dependency-audit result.

## Publication checks

The release excludes stores, runtime routing data, logs, backups, private paths, credentials, and development-only efficiency prototypes. Syntax and relative imports are checked for release JavaScript modules; local documentation links are checked too.

GitHub Actions is configured to run locked-install baselines on Windows and Linux, real semantic inference and all release suites on Windows, dependency audit, and current-tree/history publication audits. The workflow on the released commit is the source for its CI result.

## Limits of the observation

`goal.current` remains an ordinary shared-state convention. The tests demonstrate that agents can share it and update it conditionally; they do not establish that every agent retrieves it or uses it to guide work. The release does not ship experimental automatic freshness, keyed upsert, or revision protocols.

The older architecture report and its token-reduction figures remain [historical version-1 evidence](ARCHITECTURE_VALIDATION.md). They were not remeasured as version-2 efficiency claims.
