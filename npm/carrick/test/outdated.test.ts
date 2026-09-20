// What an upgrade left behind, and the one line a day that names it
// (carrick#1333).
//
// The claim under test throughout is that staleness is decided by CONTENT: a
// release that changes nothing leaves an install current, and a body an older
// version wrote is found however its version was numbered.

import assert from "node:assert/strict";
import test from "node:test";
import fs from "node:fs";
import os from "node:os";
import path from "node:path";
import { spawnSync } from "node:child_process";
import { fileURLToPath } from "node:url";

import {
  initialisedRoot,
  noticeFile,
  outdatedInstall,
  refreshNotice,
  removeNotice,
} from "../src/init/outdated.ts";
import { carrickHooks, mergeCarrickHooks, SETTINGS_FILES } from "../src/init/settings.ts";
import { skillFile, SKILL_ROOTS, stamped, writeTaskSkills } from "../src/init/task-skills.ts";

const packageRoot = path.dirname(path.dirname(fileURLToPath(import.meta.url)));
const posix = { skip: process.platform === "win32" ? "the fake scanner needs a POSIX shebang" : false };

function workspace(): string {
  const dir = fs.mkdtempSync(path.join(os.tmpdir(), "carrick-outdated-"));
  fs.mkdirSync(path.join(dir, ".carrick"), { recursive: true });
  return dir;
}

/** A settings file holding the entries this version writes. */
function currentHooks(dir: string): void {
  const file = path.join(dir, SETTINGS_FILES[0]!);
  fs.mkdirSync(path.dirname(file), { recursive: true });
  fs.writeFileSync(file, mergeCarrickHooks(null, "carrick").body);
}

/** The same file as an older version left it: one entry with a former timeout. */
function olderHooks(dir: string): void {
  const file = path.join(dir, SETTINGS_FILES[0]!);
  const document = JSON.parse(mergeCarrickHooks(null, "carrick").body);
  const [event] = Object.keys(carrickHooks("carrick"));
  document.hooks[event!][0].hooks[0].timeout = 99;
  fs.mkdirSync(path.dirname(file), { recursive: true });
  fs.writeFileSync(file, `${JSON.stringify(document, null, 2)}\n`);
}

test("an install this version wrote is not out of date", () => {
  const dir = workspace();
  try {
    writeTaskSkills(dir, { slug: "acme-index" });
    currentHooks(dir);
    assert.deepEqual(outdatedInstall(dir), { skills: [], hooks: [] });
  } finally {
    fs.rmSync(dir, { recursive: true, force: true });
  }
});

test("a body an older version rendered is found, and an edited one is not called stale", () => {
  const dir = workspace();
  try {
    writeTaskSkills(dir, { slug: "acme-index" });
    // What an older release shipped: the same file, stamped over a body that
    // has since moved. Written through the same stamp the writer uses, so this
    // is a file in every way ours.
    const stale = path.join(dir, skillFile(SKILL_ROOTS[0]!, "carrick-impact"));
    const body = fs.readFileSync(stale, "utf8");
    fs.writeFileSync(stale, stamped(`${body.split("\n<!-- carrick:skill")[0]!}\nOne step an older version had.\n`));

    // And a file somebody here has edited, which is theirs and is not this
    // finding at all.
    const edited = path.join(dir, skillFile(SKILL_ROOTS[1]!, "carrick-census"));
    fs.appendFileSync(edited, "\nOur own step.\n");

    const found = outdatedInstall(dir);
    assert.deepEqual(found.skills, [skillFile(SKILL_ROOTS[0]!, "carrick-impact")]);
    assert.deepEqual(found.hooks, []);
  } finally {
    fs.rmSync(dir, { recursive: true, force: true });
  }
});

test("a hook entry this version no longer writes is out of date", () => {
  const dir = workspace();
  try {
    writeTaskSkills(dir, { slug: "acme-index" });
    olderHooks(dir);
    assert.deepEqual(outdatedInstall(dir).hooks, [SETTINGS_FILES[0]!]);
  } finally {
    fs.rmSync(dir, { recursive: true, force: true });
  }
});

test("a settings file with no entry of ours is not an out-of-date install", () => {
  const dir = workspace();
  try {
    writeTaskSkills(dir, { slug: "acme-index" });
    const file = path.join(dir, SETTINGS_FILES[0]!);
    fs.mkdirSync(path.dirname(file), { recursive: true });
    // Somebody else's hooks, and a file init has never written to: a different
    // finding, and `carrick doctor`'s to make.
    fs.writeFileSync(file, `${JSON.stringify({ hooks: { Stop: [{ hooks: [{ type: "command", command: "make lint" }] }] } }, null, 2)}\n`);
    assert.deepEqual(outdatedInstall(dir).hooks, []);
  } finally {
    fs.rmSync(dir, { recursive: true, force: true });
  }
});

test("the line is said once a day, and only when there is something to say", () => {
  const dir = workspace();
  const home = fs.mkdtempSync(path.join(os.tmpdir(), "carrick-home-"));
  const now = new Date("2026-09-20T09:00:00Z");
  try {
    writeTaskSkills(dir, { slug: "acme-index" });
    currentHooks(dir);
    assert.equal(refreshNotice(dir, { home, now }), null, "a current install says nothing");
    assert.equal(fs.existsSync(noticeFile(home)), false, "and the day is not spent on silence");

    olderHooks(dir);
    const line = refreshNotice(dir, { home, now });
    assert.match(line ?? "", /1 file\(s\) here were written by an older carrick/);
    assert.match(line ?? "", /carrick init/);

    assert.equal(refreshNotice(dir, { home, now }), null, "not twice in one day");
    const tomorrow = new Date("2026-09-21T09:00:00Z");
    assert.notEqual(refreshNotice(dir, { home, now: tomorrow }), null, "and again the next day");

    assert.equal(removeNotice(home), true);
    assert.equal(removeNotice(home), false);
  } finally {
    fs.rmSync(dir, { recursive: true, force: true });
    fs.rmSync(home, { recursive: true, force: true });
  }
});

// The line has to come from a command somebody actually types, and from no
// other (carrick#1333). `hook` and `lsp` are protocol channels, and a sentence
// of prose on either is read as data by whatever is on the other end.
test("a typed command says it once; a hook says nothing at all", posix, () => {
  const dir = workspace();
  const home = fs.mkdtempSync(path.join(os.tmpdir(), "carrick-home-"));
  try {
    writeTaskSkills(dir, { slug: "acme-index" });
    olderHooks(dir);
    const env = {
      ...process.env,
      HOME: home,
      USERPROFILE: home,
      CARRICK_BIN: path.join(packageRoot, "test", "fake-carrick.mjs"),
    };
    const run = (...args: string[]): { stdout: string; stderr: string } =>
      spawnSync(process.execPath, [path.join(packageRoot, "bin", "carrick.mjs"), ...args], {
        cwd: dir,
        env,
        encoding: "utf8",
        input: JSON.stringify({ cwd: dir, tool_input: { file_path: path.join(dir, "src", "main.ts") } }),
      });

    const status = run("status");
    assert.match(status.stderr, /carrick: 1 file\(s\) here were written by an older carrick/);
    // Once. The second run of the same day is the same install and the same
    // sentence.
    assert.doesNotMatch(run("status").stderr, /written by an older carrick/);

    // And never on the channels an agent reads.
    fs.rmSync(noticeFile(home));
    const hook = run("hook", "post-edit");
    assert.doesNotMatch(hook.stdout, /older carrick/);
    assert.doesNotMatch(hook.stderr, /older carrick/);
    assert.equal(fs.existsSync(noticeFile(home)), false, "the hook spent the day's one line");
  } finally {
    fs.rmSync(dir, { recursive: true, force: true });
    fs.rmSync(home, { recursive: true, force: true });
  }
});

test("a folder carrick was never set up in has no install to speak for", () => {
  const dir = fs.mkdtempSync(path.join(os.tmpdir(), "carrick-bare-"));
  try {
    assert.equal(initialisedRoot(dir), null);
    const inside = workspace();
    try {
      assert.equal(initialisedRoot(inside), path.resolve(inside));
    } finally {
      fs.rmSync(inside, { recursive: true, force: true });
    }
  } finally {
    fs.rmSync(dir, { recursive: true, force: true });
  }
});
