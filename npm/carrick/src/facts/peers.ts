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
// is not a full object id, git fails or runs past its budget (and then every
// later proof in the same call is false too), the commit is not in the clone,
// a file is missing, over `MAX_FACT_FILE_BYTES`, or differs. Never throws.

import crypto from "node:crypto";
import path from "node:path";
import { gitFacts } from "./diff.ts";
import type { FileMemo } from "./files.ts";

/** A full object id: SHA-1 (40 hex) or SHA-256 (64 hex). Anything else (a branch, `HEAD`, a short id) proves nothing. */
const OBJECT_ID = /^(?:[0-9a-f]{40}|[0-9a-f]{64})$/;

/** What one `factLines` call proves against. */
export type PeerContext = {
  /** The edited file's repo and the checkout it sits in: a peer in the same repo is read there. */
  at: { repo: string; top: string };
  /** Each mapped repo's main checkout here. */
  where: ReadonlyMap<string, string>;
  /** Each repo's index commit, from the fact file's `meta`; null when the file holds none. */
  commits: Readonly<Record<string, string>> | null;
  memo: FileMemo;
  env?: NodeJS.ProcessEnv;
  /** Answers already given in this call, by repo top, commit and files. */
  proofs: Map<string, boolean>;
  /** A git call failed or ran out of time in this call: no further git call, every proof false. */
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
function inRepo(file: string): string | null {
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
    if (ctx.gitFailed) return false;
    const out = gitFacts(top, ["--literal-pathspecs", "ls-tree", "-z", "--full-tree", commit, "--", ...files], ctx.env);
    if (out === null) {
      ctx.gitFailed = true;
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
      const top = repo === ctx.at.repo ? ctx.at.top : ctx.where.get(repo);
      // Not checked out here: stated from the index.
      if (top === undefined) continue;
      const commit = ctx.commits?.[repo];
      if (typeof commit !== "string" || !OBJECT_ID.test(commit)) return false;
      if (!sameAsCommit(ctx, top, commit, [...files].sort())) return false;
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
