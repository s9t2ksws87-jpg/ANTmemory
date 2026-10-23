/** Complete downloads are published atomically; interrupted files are never hits. */
import { createHash, randomUUID } from 'node:crypto';
import { createReadStream, createWriteStream } from 'node:fs';
import { mkdir, readFile, rename, stat, unlink, writeFile } from 'node:fs/promises';
import path from 'node:path';
import { Readable } from 'node:stream';
import { pipeline } from 'node:stream/promises';

export function createAtomicCache(root) {
  const directory = path.resolve(root);
  const locations = request => {
    const key = createHash('sha256').update(String(request)).digest('hex');
    return { data: path.join(directory, key + '.bin'), metadata: path.join(directory, key + '.json') };
  };
  return {
    async match(request) {
      const p = locations(request);
      try {
        const metadata = JSON.parse(await readFile(p.metadata, 'utf8'));
        const info = await stat(p.data);
        if (metadata.request !== String(request) || metadata.size !== info.size) return undefined;
        return new Response(Readable.toWeb(createReadStream(p.data)), { headers: { 'Content-Length': String(info.size) } });
      } catch { return undefined; }
    },
    async put(request, response) {
      if (!response.ok || !response.body) throw new Error('Model cache requires a successful response body');
      await mkdir(directory, { recursive: true });
      const p = locations(request);
      const suffix = '.' + process.pid + '.' + randomUUID() + '.partial';
      const temporary = p.data + suffix;
      const metadataTemporary = p.metadata + suffix;
      try {
        await pipeline(Readable.fromWeb(response.body), createWriteStream(temporary, { flags: 'wx' }));
        const info = await stat(temporary);
        const header = response.headers.get('Content-Length');
        if (header !== null && Number(header) !== info.size) throw new Error('Incomplete model download');
        await writeFile(metadataTemporary, JSON.stringify({ request: String(request), size: info.size }), { flag: 'wx' });
        await rename(temporary, p.data);
        await rename(metadataTemporary, p.metadata);
      } finally {
        await unlink(temporary).catch(() => {});
        await unlink(metadataTemporary).catch(() => {});
      }
    },
  };
}
