// Which local checkout a hosted repo is. The fact file names a repo as the
// cloud does (the bare repo name, `api`, or `owner/name`); locally a repo is
// named after its folder (`.carrick/index.json`). The two agree only when the
// folder is called what the repo is. This is the one place that decides the
// join: `fetch.ts` builds the fact store's repo table from `matchRepos`, and
// the held daemon reads that table through `localNames` to key its rows by the
// local name (`held/runtime.ts`), so the edit lines and the compiler engine
// find the same checkout.

import fs from "node:fs";
import path from "node:path";
import { splitRemote } from "../git-remote.ts";
import type { LocalRepo } from "./store.ts";

export type LocalEntry = { name: string; path: string; remote: string | null };

/** Each repo of the local index and where it is checked out (`.carrick/index.json`). */
export function localRepos(root: string): LocalEntry[] {
  try {
    const index = JSON.parse(fs.readFileSync(path.join(root, ".carrick", "index.json"), "utf8")) as {
      repos?: Array<{ name?: unknown; path?: unknown; services?: Array<{ enrichment?: { remote?: unknown } }> }>;
    };
    return (index.repos ?? [])
      .filter((repo) => typeof repo.name === "string" && typeof repo.path === "string")
      .map((repo) => {
        const remote = (repo.services ?? []).map((service) => service.enrichment?.remote).find((value) => typeof value === "string");
        return { name: repo.name as string, path: repo.path as string, remote: (remote as string | undefined) ?? null };
      });
  } catch {
    return [];
  }
}

/** The `owner/name` an origin names, in either spelling git accepts, or the text itself. */
function ownerName(remote: string): string {
  return splitRemote(remote)?.name ?? remote;
}

/** The repo name an origin names (`acme/api-server` -> `api-server`), lower case as GitHub compares it; null when it names none. */
function originRepoName(remote: string | null): string | null {
  const full = remote === null ? null : splitRemote(remote)?.name ?? null;
  return full === null ? null : full.slice(full.lastIndexOf("/") + 1).toLowerCase();
}

/**
 * The fact file's repos matched to their checkouts here. A header repo that
 * is a remote (`owner/name`, as the index stores a GitHub repo) maps only to
 * the one checkout whose origin names exactly that. A bare name maps to the
 * one checkout of exactly that name; with none of that name, to the one
 * checkout whose origin's repo name is it (a clone in a folder called
 * something else). No match, or more than one, leaves the repo unmapped: its
 * rows are never read, so two clones of one repo in one index are neither
 * read. Two header repos that map to one checkout (`api` and `owner/api`) are
 * both left unmapped.
 */
export function matchRepos(repos: readonly string[], local: readonly LocalEntry[]): LocalRepo[] {
  const out: LocalRepo[] = [];
  for (const repo of repos) {
    let found: readonly LocalEntry[];
    if (repo.includes("/")) {
      found = local.filter((entry) => entry.remote !== null && ownerName(entry.remote) === repo);
    } else {
      found = local.filter((entry) => entry.name === repo);
      if (found.length === 0) found = local.filter((entry) => originRepoName(entry.remote) === repo.toLowerCase());
    }
    if (found.length === 1) out.push({ repo, path: found[0]!.path });
  }
  return out.filter((entry) => out.filter((other) => other.path === entry.path).length === 1);
}

/**
 * The local name of each stored repo whose checkout is called something else:
 * hosted name to local name, read off the store's repo table and the index.
 * A repo whose folder is named as it is, or whose checkout the index no longer
 * lists, is absent (its rows keep the name they have). A local name that is
 * also another repo's hosted name (`taken`) is never used.
 */
export function localNames(stored: readonly LocalRepo[], local: readonly LocalEntry[], taken: ReadonlySet<string> = new Set()): Map<string, string> {
  const out = new Map<string, string>();
  for (const entry of stored) {
    const at = local.filter((l) => l.path === entry.path);
    const name = at.length === 1 ? at[0]!.name : null;
    if (name !== null && name !== entry.repo && !taken.has(name)) out.set(entry.repo, name);
  }
  return out;
}
