// Whether the peer files an op or fn line names (its `others`) still read as
// they did at the index commit (plan section 33, R33.1 and R33.2). The line
// states where things sit in those files from the index; it prints only when
// every such file is in a repo checked out here and byte-identical to its
// blob at that repo's index commit. A peer in a repo with no checkout here
// makes the line silent (R40.2).
//
// Each file's git blob id is hashed in process and compared with one
// `git ls-tree <commit> -- <files>` per repo, run by `diff.ts`'s git runner
// under its budget (`CARRICK_FACTS_DIFF_BUDGET_MS`). The answer is false (the
// line is silent) when: the fact file has no commit for the repo, the commit
// is not a full object id, git fails or runs past its budget (an overrun makes
// every later proof in the same call false too), the commit is not in the clone,
// a file is missing, over `MAX_FACT_FILE_BYTES`, or differs. Never throws.

import crypto from "node:crypto";
import fs from "node:fs";
import path from "node:path";
import { performance } from "node:perf_hooks";
import { factsGitBudgetMs, gitFacts } from "./diff.ts";
import type { FileMemo } from "./files.ts";
import { commonGitDir } from "../hook/worktree.ts";

/** A full object id: SHA-1 (40 hex) or SHA-256 (64 hex). Anything else (a branch, `HEAD`, a short id) proves nothing. */
const OBJECT_ID = /^(?:[0-9a-f]{40}|[0-9a-f]{64})$/;

/**
 * Where a row's repo is, for every kind of line: its checkout here (`top`;
 * the edited file's own repo is read in the edited file's checkout), or null
 * when the store mapped it to none (the line is silent, R40.2).
 */
export type Place = { top: string } | null;

export type LocalCheckouts = { place: (repo: string, at: { repo: string; top: string }) => Place };

/** The one answer to "where is this repo checked out here", from the store's repo mapping. */
export function localCheckouts(where: ReadonlyMap<string, string>): LocalCheckouts {
  return {
    place: (repo, at) => {
      if (repo === at.repo) return { top: at.top };
      const top = where.get(repo);
      return top === undefined ? null : { top };
    },
  };
}

/** What one `factLines` call proves against. */
export type PeerContext = {
  /** The edited file's repo and the checkout it sits in. */
  at: { repo: string; top: string };
  checkouts: LocalCheckouts;
  /** Each repo's index commit, from the fact file's `meta`; null when the file holds none. */
  commits: Readonly<Record<string, string>> | null;
  memo: FileMemo;
  env?: NodeJS.ProcessEnv;
  /** Answers already given in this call, by repo top, commit and files. */
  proofs: Map<string, boolean>;
  /** Commits a clone answered it lacks in this call, by repo top and commit: not asked again (R36.3). */
  missing: Set<string>;
  /** Whether each checkout is a partial clone, by repo top, read once per call (R36.4). */
  partial: Map<string, boolean>;
  /** A git call ran out of time in this call: no further git call, every proof false. */
  gitFailed: boolean;
};

/** The git blob id of `bytes`, in the hash the commit's length names. */
function blobId(bytes: Buffer, sha256: boolean): string {
  const hash = crypto.createHash(sha256 ? "sha256" : "sha1");
  hash.update(`blob ${bytes.length}\0`);
  hash.update(bytes);
  return hash.digest("hex");
}

/** A repo-relative path that stays inside the repo, in git's spelling; null otherwise. */
export function inRepo(file: string): string | null {
  if (file === "" || path.isAbsolute(file) || file.includes("\0")) return null;
  const parts = file.split(/[\\/]/);
  if (parts.some((part) => part === ".." || part === "")) return null;
  return parts.join("/");
}

/**
 * Whether the checkout at `top` is a partial clone (or not known not to be,
 * R40.3): its shared git dir holds a promisor pack (`objects/pack/*.promisor`),
 * or its config includes another file (`[include]`, `[includeIf]`: what that
 * file says is not read) or has any `promisor`, `partialclonefilter` or
 * `partialclone` key, whatever its value. Read as files, no git run. A git
 * dir, pack folder or config that cannot be read counts as partial.
 */
function isPartialClone(top: string): boolean {
  const common = commonGitDir(top);
  if (common === null) return true;
  try {
    if (fs.readdirSync(path.join(common, "objects", "pack")).some((name) => name.endsWith(".promisor"))) return true;
    const config = fs.readFileSync(path.join(common, "config"), "utf8");
    return /^\s*\[\s*include(?:if)?\b/im.test(config) || /^\s*(?:promisor|partialclonefilter|partialclone)\s*(?:[=;#]|$)/im.test(config);
  } catch {
    return true;
  }
}

/** Whether every file in `files` under `top` is its blob at `commit`. */
function sameAsCommit(ctx: PeerContext, top: string, commit: string, files: readonly string[]): boolean {
  const key = `${top}\0${commit}\0${files.join("\0")}`;
  const known = ctx.proofs.get(key);
  if (known !== undefined) return known;
  const answer = ((): boolean => {
    const wanted = new Map<string, string>();
    for (const file of files) {
      const bytes = ctx.memo.bytes(path.join(top, file));
      if (bytes === null) return false;
      wanted.set(file, blobId(bytes, commit.length === 64));
    }
    const clone = `${top}\0${commit}`;
    if (ctx.gitFailed || ctx.missing.has(clone)) return false;
    // A partial clone would fetch a missing object over the network, which a
    // hook never does, on any git version: no proof there at all (R36.4).
    let partial = ctx.partial.get(top);
    if (partial === undefined) {
      partial = isPartialClone(top);
      ctx.partial.set(top, partial);
    }
    if (partial) return false;
    const started = performance.now();
    const out = gitFacts(top, ["--literal-pathspecs", "ls-tree", "-z", "--full-tree", commit, "--", ...files], ctx.env);
    if (out === null) {
      // A git that ran out of time will again: no further call this action.
      // A quick failure (a commit this clone lacks) is this clone's alone,
      // and is not asked again in this action.
      if (performance.now() - started >= factsGitBudgetMs(ctx.env)) ctx.gitFailed = true;
      else ctx.missing.add(clone);
      return false;
    }
    const found = new Map<string, string>();
    for (const entry of out.split("\0")) {
      const match = /^\d+ blob ([0-9a-f]+)\t(.+)$/s.exec(entry);
      if (match) found.set(match[2]!, match[1]!);
    }
    return [...wanted].every(([file, id]) => found.get(file) === id);
  })();
  ctx.proofs.set(key, answer);
  return answer;
}

/**
 * Whether the line naming `others` may print: every peer file in a repo
 * checked out here reads as it did at the index commit.
 */
export function peersUnchanged(others: ReadonlyArray<{ repo: string; file: string }>, ctx: PeerContext): boolean {
  try {
    const byRepo = new Map<string, Set<string>>();
    for (const other of others) {
      const files = byRepo.get(other.repo) ?? new Set<string>();
      byRepo.set(other.repo, files);
      const file = inRepo(other.file);
      if (file === null) return false;
      files.add(file);
    }
    for (const [repo, files] of byRepo) {
      const place = ctx.checkouts.place(repo, ctx.at);
      if (place === null) return false;
      const commit = ctx.commits?.[repo];
      if (typeof commit !== "string" || !OBJECT_ID.test(commit)) return false;
      if (!sameAsCommit(ctx, place.top, commit, [...files].sort())) return false;
    }
    return true;
  } catch {
    return false;
  }
}

/** Each repo's index commit from the fact file's `meta` (`commits`, written at build), or null. */
export function commitsOf(meta: Readonly<Record<string, string>>): Record<string, string> | null {
  try {
    const value = JSON.parse(meta["commits"] ?? "null") as unknown;
    if (value === null || typeof value !== "object" || Array.isArray(value)) return null;
    const out: Record<string, string> = {};
    for (const [repo, commit] of Object.entries(value)) if (typeof commit === "string") out[repo] = commit;
    return out;
  } catch {
    return null;
  }
}
