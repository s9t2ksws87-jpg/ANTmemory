/**
 * ANTmemory native core module loader.
 *
 * Storage, search, embeddings and store primitives are all owned by ANTmemory.
 * This loader keeps one stable import surface for higher layers.
 */

import path from "node:path";
import { fileURLToPath, pathToFileURL } from "node:url";

const HERE = path.dirname(fileURLToPath(import.meta.url));

export async function loadCore() {
  const db = await import(pathToFileURL(path.join(HERE, "native-db.mjs")).href);
  const embeddings = await import(pathToFileURL(path.join(HERE, "native-embeddings.mjs")).href);
  const search = await import(pathToFileURL(path.join(HERE, "native-search.mjs")).href);
  const store = await import(pathToFileURL(path.join(HERE, "native-store.mjs")).href);
  return { db, embeddings, search, store };
}
