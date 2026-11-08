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
