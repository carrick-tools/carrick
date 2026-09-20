// The per-session record of functions the index does not hold, and the one
// line the Stop hook speaks from (carrick#1330).

import test from "node:test";
import assert from "node:assert/strict";
import fs from "node:fs";
import os from "node:os";
import path from "node:path";

import {
  MAX_ENTRIES,
  markNudged,
  nudge,
  pending,
  prune,
  readSession,
  record,
  removeSessions,
  sessionFile,
  sessionsDir,
  type NewFunction,
} from "../src/hook/reuse.ts";

function home(): { dir: string; cleanup: () => void } {
  const dir = fs.realpathSync(fs.mkdtempSync(path.join(os.tmpdir(), "carrick-reuse-")));
  return { dir, cleanup: () => fs.rmSync(dir, { recursive: true, force: true }) };
}

function fn(name: string, file = "src/util/format.ts"): NewFunction {
  return { name, file, indexCommit: "6a1b2c3d4e5f60718293a4b5c6d7e8f900112233" };
}

test("the record lives outside every repository, one file per session", (t) => {
  const { dir, cleanup } = home();
  t.after(cleanup);

  record("abc-123", [fn("slugify")], dir);
  assert.equal(sessionFile("abc-123", dir), path.join(dir, ".carrick", "sessions", "abc-123.json"));
  assert.ok(fs.existsSync(sessionFile("abc-123", dir)!));
  // A session id that could walk out of the directory is not a file name.
  assert.equal(sessionFile("../../etc/passwd", dir), null);
  record("../../etc/passwd", [fn("slugify")], dir);
  assert.deepEqual(readSession("../../etc/passwd", dir).found, []);
});

test("one function is recorded once however many edits touch its file", (t) => {
  const { dir, cleanup } = home();
  t.after(cleanup);

  record("s1", [fn("slugify"), fn("titleCase")], dir);
  record("s1", [fn("slugify")], dir);
  record("s1", [fn("slugify", "src/other.ts")], dir);
  assert.deepEqual(
    readSession("s1", dir).found.map((entry) => `${entry.file}::${entry.name}`),
    ["src/util/format.ts::slugify", "src/util/format.ts::titleCase", "src/other.ts::slugify"],
  );
});

test("a set that has been named is not pending again", (t) => {
  const { dir, cleanup } = home();
  t.after(cleanup);

  record("s2", [fn("slugify"), fn("titleCase")], dir);
  const first = pending(readSession("s2", dir));
  assert.equal(first.length, 2);
  markNudged("s2", first, dir);
  assert.deepEqual(pending(readSession("s2", dir)), []);

  // A function added after the nudge is pending on its own: the rule is "not
  // this set again", never "this session is done".
  record("s2", [fn("truncate")], dir);
  assert.deepEqual(
    pending(readSession("s2", dir)).map((entry) => entry.name),
    ["truncate"],
  );
});

test("a record this build cannot read is replaced, not thrown at the caller", (t) => {
  const { dir, cleanup } = home();
  t.after(cleanup);

  const file = sessionFile("s3", dir)!;
  fs.mkdirSync(path.dirname(file), { recursive: true });
  fs.writeFileSync(file, "{ not json");
  assert.deepEqual(readSession("s3", dir).found, []);
  record("s3", [fn("slugify")], dir);
  assert.deepEqual(
    readSession("s3", dir).found.map((entry) => entry.name),
    ["slugify"],
  );
});

test("records nothing will read again are pruned", (t) => {
  const { dir, cleanup } = home();
  t.after(cleanup);

  record("old", [fn("slugify")], dir);
  record("new", [fn("titleCase")], dir);
  const old = sessionFile("old", dir)!;
  const ancient = Date.now() - 30 * 24 * 60 * 60 * 1000;
  fs.utimesSync(old, ancient / 1000, ancient / 1000);
  prune(dir);
  assert.equal(fs.existsSync(old), false);
  assert.equal(fs.existsSync(sessionFile("new", dir)!), true);
});

test("remove deletes every record and the directory holding them", (t) => {
  const { dir, cleanup } = home();
  t.after(cleanup);

  record("s4", [fn("slugify")], dir);
  record("s5", [fn("titleCase")], dir);
  assert.equal(removeSessions(dir), 2);
  assert.equal(fs.existsSync(sessionsDir(dir)), false);
  // A machine that never recorded one has nothing to say about it.
  assert.equal(removeSessions(dir), 0);
});

test("the nudge names the functions, the skill, and both limits on the answer", () => {
  const line = nudge([fn("slugify"), fn("titleCase")]);
  assert.match(line, /slugify \(src\/util\/format\.ts\)/);
  assert.match(line, /titleCase/);
  assert.match(line, /carrick-reuse/);
  assert.match(line, /find_similar/);
  // `name` entries resolve against the index and these are exactly what the
  // index does not hold, so the call has to use `description`.
  assert.match(line, /`description` entry/);
  // Limit one: what it was compared against, named by commit (open question 2
  // of carrick#1330 — it fires either way and states the commit).
  assert.match(line, /6a1b2c3/);
  assert.match(line, /default branch/);
  // Limit two: no body hash, so the comparison is on the description.
  assert.match(line, /not on its source/);
  // Nothing about what any of it cost.
  assert.doesNotMatch(line, /\$|cost|token|cheap|spend/i);
});

test("the nudge lists at most what one find_similar call takes, and says so", () => {
  const many = Array.from({ length: MAX_ENTRIES + 3 }, (_, index) => fn(`helper${index}`));
  const line = nudge(many);
  assert.match(line, new RegExp(`added ${MAX_ENTRIES} functions`));
  assert.match(line, /3 more are not listed/);
  assert.equal(line.includes(`helper${MAX_ENTRIES}`), false);
});

test("a set compared against more than one index says so instead of naming a commit", () => {
  const line = nudge([fn("slugify"), { ...fn("titleCase"), indexCommit: "ffffffffffffffff" }]);
  assert.match(line, /the index this workspace holds/);
  assert.doesNotMatch(line, /the index at /);
});
