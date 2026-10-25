/**
 * Migration: filesystem-based storage → SQLite database.
 *
 * Runs automatically on first startup if the legacy chunks/ directory exists
 * but store.db does not. Migrates all chunks and state, then renames the
 * legacy directories to *_backup/.
 */

import fs from "fs/promises";
import path from "path";
import matter from "gray-matter";
import { insertChunk, setStateDb, STORE_PATH } from "./native-db.mjs";

const CHUNKS_DIR = path.join(STORE_PATH, "chunks");
const STATE_DIR = path.join(STORE_PATH, "state");
const DB_PATH = path.join(STORE_PATH, "store.db");

/**
 * Checks if migration is needed and runs it.
 * @returns {Promise<boolean>} true if migration was performed
 */
export async function migrateIfNeeded() {
  // Check if legacy chunks dir exists
  const chunksExist = await fs
    .stat(CHUNKS_DIR)
    .then((s) => s.isDirectory())
    .catch(() => false);

  if (!chunksExist) return false;

  // Check if DB already exists (already migrated)
  const dbExists = await fs
    .stat(DB_PATH)
    .then((s) => s.isFile())
    .catch(() => false);

  if (dbExists) return false;

  process.stderr.write(
    "[ant-memory] Migrating filesystem storage to SQLite...\n",
  );

  let chunkCount = 0;
  let stateCount = 0;

  // Migrate chunks
  try {
    const files = await fs.readdir(CHUNKS_DIR);
    for (const file of files) {
      if (!file.endsWith(".md")) continue;
      try {
        const raw = await fs.readFile(path.join(CHUNKS_DIR, file), "utf8");
        const { data: meta, content } = matter(raw);

        // Skip expired chunks
        if (meta.expires && new Date(meta.expires) < new Date()) continue;

        const now = new Date().toISOString();

        // gray-matter(YAML) は日付らしき値を Date オブジェクトに変換するが、
        // node:sqlite は Date をバインドできない。ISO 文字列に揃える。
        const toIso = (v) =>
          v instanceof Date
            ? v.toISOString()
            : typeof v === "string" && v
              ? v
              : null;
        const updatedAt = toIso(meta.updated) || now;

        await insertChunk({
          id: meta.id || file.replace(".md", ""),
          topic: meta.topic || "Untitled",
          agent: meta.agent || "global",
          tags: meta.tags || [],
          importance: meta.importance || "medium",
          content: content.trim(),
          embedding: null, // Will be computed in background
          createdAt: updatedAt,
          updatedAt: updatedAt,
          expiresAt: toIso(meta.expires),
        });
        chunkCount++;
      } catch (err) {
        // 握り潰すと移行漏れに気づけないので、何が落ちたかは必ず出す
        process.stderr.write(
          `[ant-memory] Migration skipped ${file}: ${err.message}\n`,
        );
      }
    }
  } catch {
    // chunks dir not readable
  }

  // Migrate state
  try {
    const files = await fs.readdir(STATE_DIR);
    for (const file of files) {
      if (!file.endsWith(".json")) continue;
      try {
        const raw = await fs.readFile(path.join(STATE_DIR, file), "utf8");
        const { key, value } = JSON.parse(raw);
        if (key) {
          await setStateDb(key, value);
          stateCount++;
        }
      } catch {
        // Skip unreadable files
      }
    }
  } catch {
    // state dir not readable
  }

  // Rename legacy directories to backups
  try {
    await fs.rename(CHUNKS_DIR, CHUNKS_DIR + "_backup");
  } catch {
    // Rename failed — not critical
  }

  try {
    const stateExists = await fs
      .stat(STATE_DIR)
      .then((s) => s.isDirectory())
      .catch(() => false);
    if (stateExists) {
      await fs.rename(STATE_DIR, STATE_DIR + "_backup");
    }
  } catch {
    // Rename failed — not critical
  }

  process.stderr.write(
    `[ant-memory] Migration complete: ${chunkCount} chunks, ${stateCount} state entries.\n`,
  );

  return true;
}
