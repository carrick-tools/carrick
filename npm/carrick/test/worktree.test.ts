// Files a subagent edits in its own git worktree are not the session's change
// (carrick#3543). Decided by the checkout the file sits in, not by a folder name.

import assert from "node:assert/strict";
import test from "node:test";
import fs from "node:fs";
import os from "node:os";
import path from "node:path";

import { inOtherWorktree } from "../src/hook/worktree.ts";

function layout(t: { after: (fn: () => void) => void }) {
  const base = fs.realpathSync(fs.mkdtempSync(path.join(os.tmpdir(), "carrick-wt-")));
  t.after(() => fs.rmSync(base, { recursive: true, force: true }));
  const make = (dir: string, linked: boolean): string => {
    const full = path.join(base, dir);
    fs.mkdirSync(path.join(full, "src"), { recursive: true });
    if (linked) fs.writeFileSync(path.join(full, ".git"), "gitdir: /elsewhere/.git/worktrees/x\n");
    else fs.mkdirSync(path.join(full, ".git"));
    return full;
  };
  return { base, make };
}

test("a file in a linked worktree other than the session's checkout is not the session's change", (t) => {
  const { base, make } = layout(t);
  const own = make("repo", false);
  // Any layout: nothing in the path says "worktree".
  const theirs = make("repo/somewhere/odd-name", true);
  fs.mkdirSync(path.join(base, "sibling"), { recursive: true });
  assert.equal(inOtherWorktree(path.join(theirs, "src", "a.ts"), own), true);
  assert.equal(inOtherWorktree(path.join(theirs, "src", "a.ts"), base), true, "session in a plain parent folder");
});

test("a file in the session's own checkout, linked or not, still counts", (t) => {
  const { make } = layout(t);
  const plain = make("repo", false);
  const linked = make("linked", true);
  assert.equal(inOtherWorktree(path.join(plain, "src", "a.ts"), plain), false);
  assert.equal(inOtherWorktree(path.join(linked, "src", "a.ts"), path.join(linked, "src")), false);
});

test("a file in a sibling plain repository or outside any repository still counts", (t) => {
  const { base, make } = layout(t);
  const own = make("repo", false);
  const sibling = make("sibling", false);
  assert.equal(inOtherWorktree(path.join(sibling, "src", "a.ts"), own), false);
  assert.equal(inOtherWorktree(path.join(base, "loose.ts"), own), false);
});
