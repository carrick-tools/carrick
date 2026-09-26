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
  ownRepository,
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
    assert.deepEqual(written?.sort(), repoCopyPaths().sort());
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
    const written = writeRepoCopy(dir, "carrick", { slug: "shop" }) ?? [];
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

// carrick#1512 review R1: a settings file somebody else already emptied of
// our entries — an older `carrick remove` did exactly that — is still init's
// file, and holding `{"hooks": {}}` it goes with the record that names it.
test("a recorded settings file already emptied of our hooks is deleted, not left behind", posix, () => {
  const { dir, cleanup } = repo();
  try {
    writeRepoCopy(dir, "carrick", { slug: null });
    fs.writeFileSync(path.join(dir, LOCAL_SETTINGS), '{\n  "hooks": {}\n}\n');
    removeRepoCopy(dir);
    assert.equal(fs.existsSync(path.join(dir, LOCAL_SETTINGS)), false);
    assert.equal(status(dir), "?? package.json\n");
  } finally {
    cleanup();
  }
});

test("the exclude block is one marked run of lines, and taking it out gives the file back byte for byte", () => {
  const original = "# git's own comment\n*.log";
  const body = withExcludeBlock(original, [LOCAL_SETTINGS, CODEX_HOOKS_FILE]);
  // First in the file, so what follows is the owner's, untouched, whatever
  // it ends in.
  assert.equal(body, `${EXCLUDE_BEGIN}\n/.claude/settings.local.json\n/.codex/hooks.json\n${EXCLUDE_END}\n${original}`);
  assert.equal(withoutExcludeBlock(body).body, original);
  for (const ending of ["", "*.log\n", "*.log", "\n"]) {
    assert.equal(withoutExcludeBlock(withExcludeBlock(ending, [LOCAL_SETTINGS])).body, ending, JSON.stringify(ending));
  }
  // Rewritten in place, never written twice.
  assert.equal(withExcludeBlock(body, [LOCAL_SETTINGS, CODEX_HOOKS_FILE]), body);
  // Nothing to exclude, no block.
  assert.equal(withExcludeBlock(body, []), original);
  // Everything between the markers is the block's, whatever it is.
  assert.deepEqual(withoutExcludeBlock(`${EXCLUDE_BEGIN}\n/.codex/hooks.json\n*.tmp\n${EXCLUDE_END}\nrest`), {
    body: "rest",
    lines: ["/.codex/hooks.json", "*.tmp"],
    found: true,
  });
  // An end line somebody deleted: the block stops at the first line not ours.
  const cut = `${EXCLUDE_BEGIN}\n/.codex/hooks.json\n*.tmp\n`;
  assert.deepEqual(withoutExcludeBlock(cut), { body: "*.tmp\n", lines: ["/.codex/hooks.json"], found: true });
});

// carrick#1512 review R2: a folder inside another repository is not a repo of
// its own, and its lines would land in that repository's exclude file,
// anchored where they name nothing.
test("a folder inside another repository gets no copy, and that repository's exclude file is untouched", posix, () => {
  const { dir, cleanup } = repo();
  try {
    const inner = path.join(dir, "packages", "web");
    fs.mkdirSync(inner, { recursive: true });
    fs.writeFileSync(path.join(inner, "package.json"), "{}\n");
    const before = fs.readFileSync(excludeFile(dir)!, "utf8");
    assert.equal(ownRepository(inner), false);
    assert.equal(writeRepoCopy(inner, "carrick", { slug: "shop" }), null);
    assert.equal(fs.readFileSync(excludeFile(dir)!, "utf8"), before);
    assert.deepEqual(fs.readdirSync(inner), ["package.json"]);
    assert.equal(removeRepoCopy(inner), null);
  } finally {
    cleanup();
  }
});

// carrick#1512 review R3: every body is decided before the first write, so a
// file that cannot be merged stops the copy before anything, record or hook,
// is written.
test("a hooks file that is not JSON stops the copy before anything is written", posix, () => {
  const { dir, cleanup } = repo();
  try {
    fs.mkdirSync(path.join(dir, ".codex"));
    fs.writeFileSync(path.join(dir, CODEX_HOOKS_FILE), "{ not json");
    const exclude = fs.readFileSync(excludeFile(dir)!, "utf8");
    assert.throws(() => writeRepoCopy(dir, "carrick", { slug: "shop" }));
    assert.equal(fs.readFileSync(excludeFile(dir)!, "utf8"), exclude);
    assert.equal(fs.existsSync(path.join(dir, LOCAL_SETTINGS)), false);
    for (const skill of taskSkillPaths()) assert.equal(fs.existsSync(path.join(dir, skill)), false, skill);
    assert.equal(fs.readFileSync(path.join(dir, CODEX_HOOKS_FILE), "utf8"), "{ not json");
  } finally {
    cleanup();
  }
});

test("an exclude file init created goes with the copy", posix, () => {
  const { dir, cleanup } = repo();
  try {
    fs.rmSync(excludeFile(dir)!);
    writeRepoCopy(dir, "carrick", { slug: "shop" });
    assert.ok(fs.existsSync(excludeFile(dir)!));
    removeRepoCopy(dir);
    assert.equal(fs.existsSync(excludeFile(dir)!), false);
    assert.equal(status(dir), "?? package.json\n");
  } finally {
    cleanup();
  }
});
