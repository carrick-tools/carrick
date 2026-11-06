import fs from "node:fs";
import path from "node:path";

/**
 * `resolution.repos` of the workspace's cached hosted snapshot
 * (`.carrick/hosted/snapshot.json`, written by the hosted refresh), or null
 * where the file is missing, unreadable, or holds no such array.
 */
export function snapshotRepos(root: string): unknown[] | null {
  try {
    const parsed: unknown = JSON.parse(
      fs.readFileSync(path.join(root, ".carrick", "hosted", "snapshot.json"), "utf8"),
    );
    const repos = (parsed as { resolution?: { repos?: unknown } } | null)?.resolution?.repos;
    return Array.isArray(repos) ? repos : null;
  } catch {
    return null;
  }
}
