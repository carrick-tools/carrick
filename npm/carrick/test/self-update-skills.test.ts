// A task skill that is new in a release reaches an install made before it
// (`src/init/self-update.ts`, carrick#1333). Every case runs in a git repo the
// way a team's checkout looks, and reads `git status` with no user git config,
// so a laptop that ignores `.claude/settings.local.json` reads the same as CI.

import assert from "node:assert/strict";
import test from "node:test";
import fs from "node:fs";
import os from "node:os";
import path from "node:path";
import { execFileSync } from "node:child_process";

import { makeWorkspace } from "./helpers.ts";
import { currentVersion } from "../src/update.ts";
import { bringUpToDate } from "../src/init/self-update.ts";
import { writeFolderCopy } from "../src/init/repo-copies.ts";
import { skillFile, SKILL_ROOTS, TASK_SKILLS, writeTaskSkills } from "../src/init/task-skills.ts";

const running = currentVersion()!;
const NEW_SKILL = "carrick-audit";
const NEW_PATHS = SKILL_ROOTS.map((root) => skillFile(root, NEW_SKILL));
const RECORD = path.join(".carrick", "task-skills-written.json");

const HERMETIC_GIT: NodeJS.ProcessEnv = {
  GIT_CONFIG_GLOBAL: "/dev/null",
  GIT_CONFIG_NOSYSTEM: "1",
  XDG_CONFIG_HOME: fs.mkdtempSync(path.join(os.tmpdir(), "carrick-no-git-config-")),
};

function git(repo: string, ...args: string[]): string {
  return execFileSync("git", ["-C", repo, "-c", "user.email=t@example.com", "-c", "user.name=t", ...args], {
    encoding: "utf8",
    env: { ...process.env, ...HERMETIC_GIT },
  });
}

/** What git shows, less `.carrick/`, which a real install keeps out of git and this bare fixture does not. */
function porcelain(repo: string): string {
  return git(repo, "status", "--porcelain", "--untracked-files=all")
    .split("\n")
    .filter((line) => !line.includes(".carrick/"))
    .join("\n");
}

function exists(root: string, relative: string): boolean {
  return fs.existsSync(path.join(root, relative));
}

/** A workspace that is a git repository holding every skill but the new one, with no record. */
function installBeforeTheSkill(): ReturnType<typeof makeWorkspace> {
  const workspace = makeWorkspace();
  git(workspace.root, "init", "-q");
  writeTaskSkills(workspace.root, { slug: "demo" });
  for (const relative of NEW_PATHS) fs.rmSync(path.join(workspace.root, relative));
  fs.rmSync(path.join(workspace.root, RECORD), { force: true });
  return workspace;
}

test("an untracked install without the new skill gets it under both roots, said in one line and recorded", (t) => {
  const workspace = installBeforeTheSkill();
  t.after(() => workspace.cleanup());

  const lines = bringUpToDate(workspace.root, null, running);

  for (const relative of NEW_PATHS) assert.ok(exists(workspace.root, relative), `${relative} written`);
  // Written with its siblings' scope.
  assert.match(fs.readFileSync(path.join(workspace.root, NEW_PATHS[0]!), "utf8"), /project: "demo"/);
  assert.deepEqual(lines, [`Carrick added its new task skill ${NEW_SKILL} in ${SKILL_ROOTS.join(", ")} for ${running}.`]);
  const record = JSON.parse(fs.readFileSync(path.join(workspace.root, RECORD), "utf8")) as Record<string, string[]>;
  for (const root of SKILL_ROOTS) assert.ok(record[root.split(path.sep).join("/")]?.includes(NEW_SKILL));

  assert.deepEqual(bringUpToDate(workspace.root, null, running), []);
});

test("a hidden install gets the new paths in its exclude block, and git shows nothing new", (t) => {
  const workspace = makeWorkspace();
  t.after(() => workspace.cleanup());
  git(workspace.root, "init", "-q");
  writeFolderCopy(workspace.root, { slug: "demo" });
  const exclude = path.join(workspace.root, ".git", "info", "exclude");
  const hidden = fs
    .readFileSync(exclude, "utf8")
    .split("\n")
    .filter((line) => !NEW_PATHS.some((relative) => line === `/${relative.split(path.sep).join("/")}`))
    .join("\n");
  fs.writeFileSync(exclude, hidden);
  for (const relative of NEW_PATHS) fs.rmSync(path.join(workspace.root, relative));
  fs.rmSync(path.join(workspace.root, RECORD), { force: true });
  const before = porcelain(workspace.root);

  bringUpToDate(workspace.root, null, running);

  const lines = fs.readFileSync(exclude, "utf8").split("\n");
  for (const relative of NEW_PATHS) {
    assert.ok(lines.includes(`/${relative.split(path.sep).join("/")}`), `${relative} is in the block`);
    assert.ok(exists(workspace.root, relative), `${relative} written`);
  }
  assert.equal(porcelain(workspace.root), before);
});

test("tracked siblings: nothing is written and the held line names the missing skill", (t) => {
  const workspace = installBeforeTheSkill();
  t.after(() => workspace.cleanup());
  git(workspace.root, "add", "-f", ".claude", ".agents");
  git(workspace.root, "commit", "-q", "-m", "team");

  const lines = bringUpToDate(workspace.root, null, running);

  for (const relative of NEW_PATHS) assert.equal(exists(workspace.root, relative), false);
  assert.equal(lines.length, 1);
  assert.match(lines[0]!, /^Carrick did not update .*carrick-audit.* because git tracks them here\./);
});

test("a skill the record names that is gone from disk stays gone", (t) => {
  const workspace = makeWorkspace();
  t.after(() => workspace.cleanup());
  git(workspace.root, "init", "-q");
  writeTaskSkills(workspace.root, { slug: "demo" });
  for (const relative of NEW_PATHS) fs.rmSync(path.join(workspace.root, relative));

  const lines = bringUpToDate(workspace.root, null, running);

  for (const relative of NEW_PATHS) assert.equal(exists(workspace.root, relative), false);
  assert.deepEqual(lines, []);
});

test("a root with no skill of ours gets nothing", (t) => {
  const workspace = makeWorkspace();
  t.after(() => workspace.cleanup());
  git(workspace.root, "init", "-q");
  fs.mkdirSync(path.join(workspace.root, SKILL_ROOTS[0]!, "team-skill"), { recursive: true });
  fs.writeFileSync(path.join(workspace.root, SKILL_ROOTS[0]!, "team-skill", "SKILL.md"), "The team's own skill.\n");

  const lines = bringUpToDate(workspace.root, null, running);

  for (const name of TASK_SKILLS) {
    for (const root of SKILL_ROOTS) assert.equal(exists(workspace.root, skillFile(root, name)), false);
  }
  assert.deepEqual(lines, []);
});
