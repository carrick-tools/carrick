// Which directories in this folder are repos to index.
//
// A proposal, not a decision: the list goes into `carrick-workspace.json`,
// which the scanner reads literally and a user can edit. There is no walk
// deeper than one level and no search order to reason about (E20) — a repo is
// a directory here with a `package.json` in it.

import fs from "node:fs";
import path from "node:path";

/** Directories that are never a repo, whatever they hold. */
const NEVER = new Set(["node_modules", "dist", "build", "out", "coverage", "target"]);

export type FindOptions = {
  readdir?: (dir: string) => Array<{ name: string; isDirectory: () => boolean }>;
  exists?: (target: string) => boolean;
};

export function findRepos(workspace: string, options: FindOptions = {}): string[] {
  const readdir =
    options.readdir ?? ((dir: string) => fs.readdirSync(dir, { withFileTypes: true }));
  const exists = options.exists ?? ((target: string) => fs.existsSync(target));

  const found: string[] = [];
  for (const entry of readdir(workspace)) {
    if (!entry.isDirectory()) continue;
    if (entry.name.startsWith(".") || NEVER.has(entry.name)) continue;
    if (!exists(path.join(workspace, entry.name, "package.json"))) continue;
    found.push(`./${entry.name}`);
  }
  return found.sort();
}

/**
 * The workspace file to write: the repos already listed, in the order they were
 * listed, plus the ones found since. Re-running init updates rather than
 * duplicates, and never reorders or drops a path a user put there by hand —
 * including one pointing outside this folder.
 */
export function mergeWorkspace(existing: string | null, found: string[]): {
  body: string;
  repos: string[];
  added: string[];
} {
  let listed: string[] = [];
  if (existing) {
    const parsed = JSON.parse(existing) as { repos?: unknown };
    if (Array.isArray(parsed.repos)) {
      listed = parsed.repos.filter((entry): entry is string => typeof entry === "string");
    }
  }
  const normal = (entry: string) => entry.replace(/^\.\//, "").replace(/\/$/, "");
  const known = new Set(listed.map(normal));
  const added = found.filter((entry) => !known.has(normal(entry)));
  const repos = [...listed, ...added];
  return { body: `${JSON.stringify({ repos }, null, 2)}\n`, repos, added };
}
