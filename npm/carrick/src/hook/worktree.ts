// Which git checkout a path belongs to (carrick#3543). A subagent edits in its
// own linked worktree; those files are not the session's change, so the Stop
// hook does not state a check for them. Decided by the checkout the file sits
// in, never by a folder name, so any worktree layout works.

import fs from "node:fs";
import path from "node:path";

/** The nearest directory at or above `start` holding a `.git`, and whether it is a linked worktree (`.git` is a file). */
function checkoutOf(start: string): { top: string; linked: boolean } | null {
  let dir = start;
  for (;;) {
    try {
      const stat = fs.statSync(path.join(dir, ".git"));
      return { top: fs.realpathSync(dir), linked: stat.isFile() };
    } catch {
      // No .git here: look one level up.
    }
    const parent = path.dirname(dir);
    if (parent === dir) return null;
    dir = parent;
  }
}

/**
 * The git directory every worktree of the checkout at `top` shares: `top/.git`
 * itself, or for a linked worktree the one its `.git` file and `commondir`
 * point at. Read from the files, no git run. Null when there is none.
 */
export function commonGitDir(top: string): string | null {
  try {
    const dotGit = path.join(top, ".git");
    if (fs.statSync(dotGit).isDirectory()) return dotGit;
    const pointer = fs.readFileSync(dotGit, "utf8").match(/^gitdir:\s*(.+)$/m)?.[1]?.trim();
    if (!pointer) return null;
    const gitdir = path.resolve(top, pointer);
    try {
      return path.resolve(gitdir, fs.readFileSync(path.join(gitdir, "commondir"), "utf8").trim());
    } catch {
      const at = gitdir.lastIndexOf(`${path.sep}worktrees${path.sep}`);
      return at >= 0 ? gitdir.slice(0, at) : null;
    }
  } catch {
    return null;
  }
}

/** The top folder of the linked worktree `file` sits in, or null in the main checkout or none (carrick#3595). */
export function linkedCheckoutTop(file: string): string | null {
  const own = checkoutOf(path.dirname(file));
  return own !== null && own.linked ? own.top : null;
}

/** Whether a linked worktree top recorded at edit time has been removed. */
export function worktreeGone(top: string): boolean {
  return !fs.existsSync(path.join(top, ".git"));
}

/**
 * Whether `file` sits in a linked git worktree other than the checkout the
 * session runs in (`cwd`). A file in the session's own checkout, in a plain
 * repository beside it, or in no repository is the session's change.
 */
export function inOtherWorktree(file: string, cwd: string | null): boolean {
  const theirs = checkoutOf(path.dirname(file));
  if (theirs === null || !theirs.linked) return false;
  const own = cwd === null ? null : checkoutOf(cwd);
  return own === null || own.top !== theirs.top;
}
