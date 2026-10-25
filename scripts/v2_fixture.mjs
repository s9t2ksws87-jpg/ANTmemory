// Build a genuine public-2.x store without downloading an embedding model.
import { getDb, insertChunk, setStateDb } from '../legacy/v2/server/lib/native-db.mjs';
import { readFileSync } from 'node:fs';

const now = '2026-10-25T00:00:00.000Z';
for (const [id, content] of [['legacykeep', 'Public v2 Raw: 原文🧠を保持する。'], ['legacygone', 'Retracted v2 Raw remains readable.']]) {
  insertChunk({ id, topic: 'Public v2 fixture', agent: 'v2-fixture', tags: ['legacy'], importance: 'high', content,
    embedding: null, createdAt: now, updatedAt: now, expiresAt: null });
}
// Execute the historical release's actual audit schema, without importing its
// optional Markdown/embedding dependencies. No v4-shaped tables are invented.
const source = readFileSync(new URL('../legacy/v2/server/lib/audit-lifecycle.mjs', import.meta.url), 'utf8');
const schema = source.match(/db\.exec\(`([\s\S]*?)`\);/)?.[1];
if (!schema) throw new Error('Public v2 audit schema not found');
getDb().exec(schema);
getDb().prepare('INSERT INTO memory_events(event_id,chunk_id,event_type,actor,new_value,created_at) VALUES (?,?,?,?,?,?)')
  .run('fixture-source', 'legacykeep', 'source_observed', 'v2-fixture', 'fixture.md', now);
getDb().prepare('UPDATE memory_lifecycle SET status=? WHERE chunk_id=?').run('retracted', 'legacygone');
getDb().prepare('INSERT INTO memory_events(event_id,chunk_id,event_type,actor,reason,created_at) VALUES (?,?,?,?,?,?)')
  .run('fixture-withdrawal', 'legacygone', 'retracted', 'v2-fixture', 'fixture withdrawal', now);
setStateDb('work.fixture', { phase: 'preserved', count: 2 });
getDb().exec("CREATE TABLE fixture_annotations(key TEXT PRIMARY KEY, value TEXT); INSERT INTO fixture_annotations VALUES ('extra', 'also preserved');");
getDb().close();
