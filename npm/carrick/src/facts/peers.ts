// Whether the peer files an op or fn line names (its `others`) still read as
// they did at the index commit (plan section 33, R33.1 and R33.2). The line
// states where things sit in those files from the index; it prints only when
// every such file, in a repo checked out here, is byte-identical to its blob
// at that repo's index commit. A peer in a repo with no checkout here is
// stated from the index: it cannot have changed here.
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

/** A full object id: SHA-1 (40 hex) or SHA-256 (64 hex). Anything else (a branch, `HEAD`, a short id) proves nothing. */
const OBJECT_ID = /^(?:[0-9a-f]{40}|[0-9a-f]{64})$/;

/**
 * Where a row's repo is, for every kind of line (R33.1, R36.2): its checkout
 * here (`top`; the edited file's own repo is read in the edited file's
 * checkout), `"index"` when it is proven not checked out here (stated from
 * the index), or null when that is not known (the line is silent).
 */
export type Place = { top: string } | "index" | null;

export type LocalCheckouts = { place: (repo: string, at: { repo: string; top: string }) => Place };

/**
 * The one answer to "is this repo checked out here" for a fact file under
 * `root`. A repo the store did not map is proven absent only when, at build,
 * every repo of the local index mapped to a header repo (`meta.local_unmapped`
 * is 0) and `.carrick/index.json` has not changed since (`meta.mapped_at`): a
 * fork's origin, a renamed repo or a repo added later is otherwise unmapped
 * yet checked out. A store without those values proves nothing absent.
 */
export function localCheckouts(root: string, meta: Readonly<Record<string, string>>, where: ReadonlyMap<string, string>): LocalCheckouts {
  let proven: boolean | undefined;
  const absentProven = (): boolean => {
    if (proven !== undefined) return proven;
    proven = false;
    const mappedAt = Date.parse(meta["mapped_at"] ?? "");
    if (meta["local_unmapped"] !== "0" || !Number.isFinite(mappedAt)) return proven;
    try {
      proven = fs.statSync(path.join(root, ".carrick", "index.json")).mtimeMs <= mappedAt;
    } catch (error) {
      // No local index at all: no repo of it can be checked out unmapped.
      proven = (error as NodeJS.ErrnoException).code === "ENOENT";
    }
    return proven;
  };
  return {
    place: (repo, at) => {
      if (repo === at.repo) return { top: at.top };
      const top = where.get(repo);
      if (top !== undefined) return { top };
      return absentProven() ? "index" : null;
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
      // Proven not checked out here: stated from the index.
      if (place === "index") continue;
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
