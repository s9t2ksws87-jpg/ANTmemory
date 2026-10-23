#!/usr/bin/env node
import { spawnSync } from 'node:child_process';
import { mkdirSync, writeFileSync } from 'node:fs';
import path from 'node:path';
import { fileURLToPath } from 'node:url';

const root = path.resolve(path.dirname(fileURLToPath(import.meta.url)), '..');
const logRoot = path.join(root, 'test-logs', 'release-' + new Date().toISOString().replace(/[:.]/g, '-'));
mkdirSync(logRoot, { recursive: true });
const suites = [
  'test-baseline', 'test-actors', 'test-lifecycle-search', 'test-viewer',
  'test-visibility', 'test-summaries', 'test-token-budget', 'test-progressive-disclosure',
  'test-usage-sessions', 'test-visibility-recommendations', 'test-field',
  'test-preservation', 'test-retrieval-core', 'test-native-server', 'test-memory-updates',
  'test-observation-cycle', 'test-goal-provenance', 'test-agent-surface', 'test-concurrent-writes', 'test-model-cache',
];
const results = [];
for (const suite of suites) {
  console.log(`START ${suite}`);
  const result = spawnSync(process.execPath, [path.join(root, 'bin', suite + '.mjs')], {
    cwd: root, encoding: 'utf8', windowsHide: true, timeout: 300_000, maxBuffer: 8 * 1024 * 1024,
  });
  const output = `${result.stdout ?? ''}\n${result.stderr ?? ''}`;
  writeFileSync(path.join(logRoot, suite + '.txt'), output);
  const counts = [...output.matchAll(/(\d+) passed\s*\/\s*(\d+) failed/g)].at(-1);
  const row = { suite, exitCode: result.status, passed: counts ? Number(counts[1]) : null,
    failed: counts ? Number(counts[2]) : null, error: result.error?.message ?? null };
  results.push(row);
  console.log(`${row.exitCode === 0 ? 'PASS' : 'FAIL'} ${suite}: ${row.passed ?? '?'} passed / ${row.failed ?? '?'} failed`);
  if (row.exitCode !== 0) console.error(output.slice(-9000));
}
const report = { suites: results, allPassed: results.every(r => r.exitCode === 0),
  passed: results.reduce((n, r) => n + (r.passed ?? 0), 0), logs: logRoot };
writeFileSync(path.join(logRoot, 'results.json'), JSON.stringify(report, null, 2));
console.log(JSON.stringify(report, null, 2));
if (!report.allPassed) process.exitCode = 1;
