// The changed lines of one file against `HEAD`, for a write whose own call
// says nothing about them: a `Write` over an existing file, a shell write
// (`sed -i`, a heredoc, a script), a patch hunk that cannot be placed.
//
// `git diff --no-ext-diff -U0 HEAD -- <file>` runs in the file's own folder
// (so a linked worktree answers for itself), and its `@@ -a,b +c,d @@` headers
// give the same shape `post-edit.ts` reads from an Edit's patch: the lines now
// in the file that were added, and where lines were removed (the line now
// sitting where they were; `facts/match.ts` `gapsOfLines` and `touched` apply
// the "strictly inside" rule to them).
//
// Limitation: the diff is against HEAD, not against the file before this
// call, so lines the session changed earlier and has not committed also
// count as changed. Each fact line is said once per session (`markFacts`),
// which stops the repeats.
//
// Which lines changed is not known, and nothing is said, when: the folder is
// not a git repository, git is missing or answers an error, the diff holds
// more than `MAX_DIFF_LINES` changed lines, or git takes longer than
// `DIFF_BUDGET_MS`. Never throws.

import path from "node:path";
import { cooldownMs } from "../hook/refresh.ts";
import { git, trackedPaths } from "../init/repo-copies.ts";

/** A diff with more changed lines than this is a rewrite or a generated file: no lines. */
export const MAX_DIFF_LINES = 2000;

/** Each git call gets this long; a slower one is a silence, not a wait. `CARRICK_FACTS_DIFF_BUDGET_MS` overrides it (tests on a busy machine). */
export const DIFF_BUDGET_MS = 150;

export type DiffChange = { lines?: number[]; removed?: number[]; whole?: boolean };

/**
 * The fact path's one git runner: `git` in `dir` under the budget, the output
 * or null on any failure or overrun. The diff below and the peer proof
 * (`peers.ts`) both run through it.
 */
export function gitFacts(dir: string, args: string[], env: NodeJS.ProcessEnv = process.env): string | null {
  return git(dir, args, factsGitBudgetMs(env));
}

/** How long each of the fact path's git calls may take. */
export function factsGitBudgetMs(env: NodeJS.ProcessEnv = process.env): number {
  return cooldownMs(env, "CARRICK_FACTS_DIFF_BUDGET_MS", DIFF_BUDGET_MS);
}

/**
 * Where `file` differs from `HEAD`, or null when that is not known (or there
 * is no difference). An untracked file counts whole only when `created`: the
 * session did not see the file before this call.
 */
export function changesAgainstHead(file: string, created: boolean, env: NodeJS.ProcessEnv = process.env): DiffChange | null {
  try {
    const dir = path.dirname(file);
    const out = gitFacts(dir, ["diff", "--no-ext-diff", "--no-textconv", "--no-color", "-U0", "HEAD", "--", path.basename(file)], env);
    if (out === null) return null;
    if (out === "") {
      // No difference from HEAD: a tracked file that reads the same, or one git does not track.
      if (!created) return null;
      const tracked = trackedPaths(dir, [path.basename(file)]);
      return tracked !== null && tracked.size === 0 ? { whole: true } : null;
    }
    const lines: number[] = [];
    const removed: number[] = [];
    let changed = 0;
    for (const header of out.matchAll(/^@@ -\d+(?:,(\d+))? \+(\d+)(?:,(\d+))? @@/gm)) {
      const was = header[1] === undefined ? 1 : Number(header[1]);
      const start = Number(header[2]);
      const now = header[3] === undefined ? 1 : Number(header[3]);
      changed += was + now;
      if (changed > MAX_DIFF_LINES) return null;
      for (let at = 0; at < now; at += 1) lines.push(start + at);
      // With nothing added, `start` is the line before the removal.
      if (was > 0) removed.push(now > 0 ? start : start + 1);
    }
    return lines.length > 0 || removed.length > 0 ? { lines, removed } : null;
  } catch {
    return null;
  }
}
