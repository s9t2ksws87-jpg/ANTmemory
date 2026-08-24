/**
 * 上流 agent-memory-store モジュールへの安定した参照（ANTmemory所有コード）。
 * パッチ適用済みの node_modules を ESM で読む。
 */

import path from "node:path";
import { fileURLToPath, pathToFileURL } from "node:url";

const SERVER_SRC = path.join(
  path.dirname(fileURLToPath(import.meta.url)),
  "..",
  "node_modules",
  "agent-memory-store",
  "src",
);

export async function loadUpstream() {
  const db = await import(pathToFileURL(path.join(SERVER_SRC, "db.js")).href);
  const embeddings = await import(pathToFileURL(path.join(SERVER_SRC, "embeddings.js")).href);
  return { db, embeddings };
}
