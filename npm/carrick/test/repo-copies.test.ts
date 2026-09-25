// The copy of Carrick's hooks and skills that `carrick init` puts inside each
// repo of a folder, and the `.git/info/exclude` lines that keep it out of git
// and record what `carrick remove` takes back (carrick#1512, option A).

import assert from "node:assert/strict";
import test from "node:test";
import fs from "node:fs";
import os from "node:os";
import path from "node:path";
import { execFileSync } from "node:child_process";
import {
  EXCLUDE_BEGIN,
  EXCLUDE_END,
  LOCAL_SETTINGS,
  excludeFile,
  removeRepoCopy,
  repoCopyPaths,
  withExcludeBlock,
  withoutExcludeBlock,
  writeRepoCopy,
} from "../src/init/repo-copies.ts";
import { CODEX_HOOKS_FILE } from "../src/init/codex.ts";
import { installedCarrickHooks } from "../src/init/settings.ts";
import { taskSkillPaths } from "../src/init/task-skills.ts";

const posix = { skip: process.platform === "win32" ? "git and POSIX paths" : false };

function repo(): { dir: string; cleanup: () => void } {
  const root = fs.realpathSync(fs.mkdtempSync(path.join(os.tmpdir(), "carrick-repo-copy-")));
  const dir = path.join(root, "shop-app");
  fs.mkdirSync(dir);
  execFileSync("git", ["init", "-q", dir]);
  fs.writeFileSync(path.join(dir, "package.json"), "{}\n");
  return { dir, cleanup: () => fs.rmSync(root, { recursive: true, force: true }) };
}

function status(dir: string): string {
  return execFileSync("git", ["-C", dir, "status", "--porcelain", "--untracked-files=all"], { encoding: "utf8" });
}

test("a repo's copy holds the hooks and the skills, and git sees none of it", posix, () => {
  const { dir, cleanup } = repo();
  try {
    const before = status(dir);
    const written = writeRepoCopy(dir, "carrick", { slug: "shop" });
    // The personal settings file, never the one a team commits.
    assert.deepEqual(written.sort(), repoCopyPaths().sort());
    assert.equal(fs.existsSync(path.join(dir, ".claude", "settings.json")), false);
    assert.ok(installedCarrickHooks(fs.readFileSync(path.join(dir, LOCAL_SETTINGS), "utf8")).length > 0);
    assert.ok(fs.existsSync(path.join(dir, CODEX_HOOKS_FILE)));
    for (const skill of taskSkillPaths()) {
      assert.match(fs.readFileSync(path.join(dir, skill), "utf8"), /project: "shop"/);
    }
    // Kept out of git: the tree reads as it did.
    assert.equal(status(dir), before);
    // And a second run changes nothing, the exclude file included.
    const exclude = fs.readFileSync(excludeFile(dir)!, "utf8");
    writeRepoCopy(dir, "carrick", { slug: "shop" });
    assert.equal(fs.readFileSync(excludeFile(dir)!, "utf8"), exclude);
  } finally {
    cleanup();
  }
});

test("a path the repo tracks is the team's copy, and is neither written nor excluded", posix, () => {
  const { dir, cleanup } = repo();
  try {
    const tracked = path.join(dir, CODEX_HOOKS_FILE);
    fs.mkdirSync(path.dirname(tracked), { recursive: true });
    fs.writeFileSync(tracked, '{ "hooks": {} }\n');
    execFileSync("git", ["-C", dir, "add", CODEX_HOOKS_FILE]);
    const written = writeRepoCopy(dir, "carrick", { slug: "shop" });
    assert.ok(!written.includes(CODEX_HOOKS_FILE), written.join("\n"));
    assert.equal(fs.readFileSync(tracked, "utf8"), '{ "hooks": {} }\n');
    assert.doesNotMatch(fs.readFileSync(excludeFile(dir)!, "utf8"), /\/\.codex\/hooks\.json/);
  } finally {
    cleanup();
  }
});

test("remove takes back exactly what the lines name, and the lines, and leaves the owner's own", posix, () => {
  const { dir, cleanup } = repo();
  try {
    const exclude = excludeFile(dir)!;
    // The owner's own exclusion, of a path the copy would also write.
    fs.appendFileSync(exclude, "/.claude/settings.local.json\n");
    const owned = fs.readFileSync(exclude, "utf8");
    // And a hook of their own in that file, which must survive.
    fs.mkdirSync(path.join(dir, ".claude"), { recursive: true });
    fs.writeFileSync(
      path.join(dir, LOCAL_SETTINGS),
      JSON.stringify({ hooks: { Stop: [{ hooks: [{ type: "command", command: "say done" }] }] } }, null, 2),
    );
    writeRepoCopy(dir, "carrick", { slug: "shop" });
    // Recorded in our block even though the owner excludes it too: the block
    // is the record of what init wrote.
    assert.equal(fs.readFileSync(exclude, "utf8").split("\n").filter((line) => line === "/.claude/settings.local.json").length, 2);

    const taken = removeRepoCopy(dir);
    assert.ok(taken !== null);
    // The owner's file exactly as it was, their line included.
    assert.equal(fs.readFileSync(exclude, "utf8"), owned);
    for (const skill of taskSkillPaths()) assert.equal(fs.existsSync(path.join(dir, skill)), false, skill);
    assert.equal(fs.existsSync(path.join(dir, CODEX_HOOKS_FILE)), false);
    assert.equal(fs.existsSync(path.join(dir, ".agents")), false);
    // Their hook stays, ours goes.
    const settings = fs.readFileSync(path.join(dir, LOCAL_SETTINGS), "utf8");
    assert.match(settings, /say done/);
    assert.deepEqual(installedCarrickHooks(settings), []);
    // And a repo with no lines of ours is not one init copied into.
    assert.equal(removeRepoCopy(dir), null);
  } finally {
    cleanup();
  }
});

test("a settings file left holding nothing goes with the copy", posix, () => {
  const { dir, cleanup } = repo();
  try {
    writeRepoCopy(dir, "carrick", { slug: null });
    removeRepoCopy(dir);
    assert.equal(fs.existsSync(path.join(dir, ".claude")), false);
    assert.equal(fs.existsSync(path.join(dir, ".codex")), false);
    assert.equal(status(dir), "?? package.json\n");
  } finally {
    cleanup();
  }
});

test("the exclude block is one marked run of lines, and a hand edit never takes another line with it", () => {
  const body = withExcludeBlock("# git's own comment\n*.log", [LOCAL_SETTINGS, CODEX_HOOKS_FILE]);
  assert.equal(
    body,
    `# git's own comment\n*.log\n${EXCLUDE_BEGIN}\n/.claude/settings.local.json\n/.codex/hooks.json\n${EXCLUDE_END}\n`,
  );
  // Rewritten in place, never appended twice.
  assert.equal(withExcludeBlock(body, [LOCAL_SETTINGS, CODEX_HOOKS_FILE]), body);
  // Nothing to exclude, no block.
  assert.equal(withExcludeBlock(body, []), "# git's own comment\n*.log\n");
  // An end line somebody deleted: the block stops at the first line not ours.
  const cut = `${EXCLUDE_BEGIN}\n/.codex/hooks.json\n*.tmp\n`;
  assert.deepEqual(withoutExcludeBlock(cut), { body: "*.tmp\n", lines: ["/.codex/hooks.json"], found: true });
});
