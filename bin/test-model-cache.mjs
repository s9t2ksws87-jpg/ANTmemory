#!/usr/bin/env node
import assert from 'node:assert/strict';
import { mkdtemp, readFile, readdir, writeFile } from 'node:fs/promises';
import { tmpdir } from 'node:os';
import path from 'node:path';
import { createAtomicCache } from '../server/lib/atomic-cache.mjs';
const root = await mkdtemp(path.join(tmpdir(), 'antmemory-model-cache-'));
const cache = createAtomicCache(root);
let passed = 0;
const check = async (name, fn) => { await fn(); passed++; console.log('PASS ' + name); };
await check('complete body is reusable across cache instances', async () => {
  await cache.put('model/a', new Response('complete', { headers: { 'Content-Length': '8' } }));
  assert.equal(await (await createAtomicCache(root).match('model/a')).text(), 'complete');
});
await check('interrupted body is not published', async () => {
  const stream = new ReadableStream({ start(controller) { controller.enqueue(new Uint8Array([1, 2])); controller.error(new Error('interrupted fixture')); } });
  await assert.rejects(cache.put('model/interrupted', new Response(stream)));
  assert.equal(await cache.match('model/interrupted'), undefined);
});
await check('truncated response is not published', async () => {
  await assert.rejects(cache.put('model/truncated', new Response('short', { headers: { 'Content-Length': '20' } })));
  assert.equal(await cache.match('model/truncated'), undefined);
});
await check('failed replacement preserves previous completed body', async () => {
  await assert.rejects(cache.put('model/a', new Response('bad', { headers: { 'Content-Length': '20' } })));
  assert.equal(await (await cache.match('model/a')).text(), 'complete');
});
await check('changed file size invalidates cache hit', async () => {
  const metadata = (await readdir(root)).filter(n => n.endsWith('.json'));
  let name;
  for (const candidate of metadata) {
    if (JSON.parse(await readFile(path.join(root, candidate), 'utf8')).request === 'model/a') { name = candidate; break; }
  }
  assert.ok(name, 'metadata for model/a exists');
  await writeFile(path.join(root, name.replace('.json', '.bin')), 'x');
  assert.equal(await cache.match('model/a'), undefined);
});
await check('partial files are cleaned after handled errors', async () => {
  assert.equal((await readdir(root)).some(n => n.endsWith('.partial')), false);
});
console.log(`\n結果: ${passed} passed / 0 failed`);
