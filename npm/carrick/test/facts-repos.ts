// Test scaffolding for fact files: each repo folder of a test workspace made a
// git repository with its files committed, and the fact file's header naming
// that commit, as a real index does. An op or fn line prints only when its
// peer files read the same as at the index commit (`src/facts/peers.ts`), so a
// workspace that is not committed would say nothing.

import assert from "node:assert/strict";
import { spawnSync } from "node:child_process";
import fs from "node:fs";
import path from "node:path";

/** Run git in `dir` with a test identity; asserts it succeeded and returns its stdout. */
export function gitIn(dir: string, ...args: string[]): string {
  const run = spawnSync("git", ["-C", dir, "-c", "user.email=t@example.com", "-c", "user.name=t", "-c", "commit.gpgsign=false", ...args], { encoding: "utf8" });
  assert.equal(run.status, 0, run.stderr);
  return run.stdout;
}

/**
 * Commit every file in `dir` (a git repository made first when it is not one)
 * and return the commit. Committing again in a repository already committed
 * is fine: an empty commit when nothing changed.
 */
export function commitAll(dir: string): string {
  if (!fs.existsSync(path.join(dir, ".git"))) gitIn(dir, "init", "-q");
  gitIn(dir, "add", "-A");
  gitIn(dir, "commit", "-q", "--allow-empty", "-m", "commit");
  return gitIn(dir, "rev-parse", "HEAD").trim();
}

/**
 * Each repo of `repos` whose folder exists under `root` committed, and
 * `ndjson` with its header's `repos[].commit` set to that repo's commit. Before
 * committing, a peer file a row names (an `others` file, a `pkg` manifest) that
 * the workspace lacks is written: a stub source file, or a manifest holding
 * every spec the rows state for it. The rows themselves are not changed.
 */
export function commitRepos(root: string, ndjson: string, repos: readonly string[]): string {
  const lines = ndjson.split("\n");
  const exists = (repo: string) => fs.existsSync(path.join(root, repo));
  const manifests = new Map<string, Record<string, Record<string, string>>>();
  for (const line of lines.slice(1)) {
    if (line.trim() === "") continue;
    let row: Record<string, unknown>;
    try {
      row = JSON.parse(line) as Record<string, unknown>;
    } catch {
      continue;
    }
    if (Array.isArray(row["others"])) {
      for (const other of row["others"] as Array<Record<string, unknown> | null>) {
        if (typeof other?.["repo"] !== "string" || typeof other["file"] !== "string" || !exists(other["repo"])) continue;
        const file = path.join(root, other["repo"], other["file"]);
        if (fs.existsSync(file)) continue;
        fs.mkdirSync(path.dirname(file), { recursive: true });
        fs.writeFileSync(file, "// a peer file the test rows name\n");
      }
    }
    if (row["k"] === "pkg" && Array.isArray(row["services"])) {
      for (const entry of row["services"] as Array<Record<string, unknown>>) {
        if (typeof entry["repo"] !== "string" || typeof entry["manifest"] !== "string" || !exists(entry["repo"])) continue;
        const file = path.join(root, entry["repo"], entry["manifest"]);
        if (fs.existsSync(file) && !manifests.has(file)) continue;
        const deps = manifests.get(file) ?? { dependencies: {} };
        deps["dependencies"]![String(row["package"])] = String(entry["spec"]);
        manifests.set(file, deps);
      }
    }
  }
  for (const [file, body] of manifests) {
    fs.mkdirSync(path.dirname(file), { recursive: true });
    fs.writeFileSync(file, `${JSON.stringify(body, null, 2)}\n`);
  }
  const commits = new Map(repos.filter(exists).map((repo) => [repo, commitAll(path.join(root, repo))]));
  const header = JSON.parse(lines[0]!) as { repos?: Array<{ repo: string; commit: string }> };
  for (const entry of header.repos ?? []) {
    const commit = commits.get(entry.repo);
    if (commit) entry.commit = commit;
  }
  return [JSON.stringify(header), ...lines.slice(1)].join("\n");
}
