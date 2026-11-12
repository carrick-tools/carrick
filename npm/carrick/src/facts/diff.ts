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

import fs from "node:fs";
import path from "node:path";
import { cooldownMs } from "../hook/refresh.ts";
import { checkoutOf, commonGitDir } from "../hook/worktree.ts";
import { git } from "../init/repo-copies.ts";

/** A diff with more changed lines than this is a rewrite or a generated file: no lines. */
export const MAX_DIFF_LINES = 2000;

/** Each git call gets this long; a slower one is a silence, not a wait. `CARRICK_FACTS_DIFF_BUDGET_MS` overrides it (tests on a busy machine). */
export const DIFF_BUDGET_MS = 150;

export type DiffChange = { lines?: number[]; removed?: number[]; whole?: boolean };

/**
 * The fact path's one git runner (R40.4): `git` in `dir` under the budget,
 * the output or null on any failure or overrun. Every git call the fact path
 * makes runs through it: the diff and the tracked-file check below and the
 * peer proof (`peers.ts`). In a partial clone, or a folder in no checkout, it
 * runs nothing and answers null.
 */
export function gitFacts(dir: string, args: string[], env: NodeJS.ProcessEnv = process.env): string | null {
  const checkout = checkoutOf(dir);
  if (checkout === null || isPartialClone(checkout.top)) return null;
  // No network from a hook, ever (R36.4): no lazy fetch of a missing object
  // (git 2.44 and later) and no credential prompt.
  return git(dir, args, factsGitBudgetMs(env), { ...process.env, ...env, GIT_NO_LAZY_FETCH: "1", GIT_TERMINAL_PROMPT: "0" });
}

/** Whether each checkout is a partial clone, by its top, decided once per process. */
const partialClones = new Map<string, boolean>();

/**
 * Whether the checkout at `top` is a partial clone (or not known not to be,
 * R40.3): its shared git dir holds a promisor pack (`objects/pack/*.promisor`),
 * or its config includes another file (`[include]`, `[includeIf]`: what that
 * file says is not read) or has any `promisor`, `partialclonefilter` or
 * `partialclone` key, whatever its value. Git there could fetch a missing
 * object over the network, which a hook never does on any git version. Read
 * as files, no git run. A git dir, pack folder or config that cannot be read
 * counts as partial.
 */
function isPartialClone(top: string): boolean {
  const known = partialClones.get(top);
  if (known !== undefined) return known;
  const answer = ((): boolean => {
    const common = commonGitDir(top);
    if (common === null) return true;
    try {
      if (fs.readdirSync(path.join(common, "objects", "pack")).some((name) => name.endsWith(".promisor"))) return true;
      const config = fs.readFileSync(path.join(common, "config"), "utf8");
      return /^\s*\[\s*include(?:if)?\b/im.test(config) || /^\s*(?:promisor|partialclonefilter|partialclone)\s*(?:[=;#]|$)/im.test(config);
    } catch {
      return true;
    }
  })();
  partialClones.set(top, answer);
  return answer;
}

/** Whether git, asked in the file's folder, says it does not track `file`; false when git cannot say. */
export function untrackedFile(file: string, env: NodeJS.ProcessEnv = process.env): boolean {
  return gitFacts(path.dirname(file), ["ls-files", "-z", "--", path.basename(file)], env) === "";
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
      return created && untrackedFile(file, env) ? { whole: true } : null;
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
