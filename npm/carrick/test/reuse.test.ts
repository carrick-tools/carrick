// The per-session record the hooks keep between calls: the files whose uses
// lines a session has been shown, the files the Read hook asked about, and the
// last edit per workspace (`src/hook/reuse.ts`).

import test from "node:test";
import assert from "node:assert/strict";
import fs from "node:fs";
import os from "node:os";
import path from "node:path";

import {
  prune,
  readSession,
  recordEdit,
  removeSessions,
  sessionFile,
  sessionRecords,
  sessionsDir,
  markRead,
  markTold,
  toldKey,
  wasRead,
  wasTold,
  snapshotProjectSlug,
} from "../src/hook/reuse.ts";

function home(): { dir: string; cleanup: () => void } {
  const dir = fs.realpathSync(fs.mkdtempSync(path.join(os.tmpdir(), "carrick-reuse-")));
  return { dir, cleanup: () => fs.rmSync(dir, { recursive: true, force: true }) };
}

test("the record lives outside every repository, one file per session", (t) => {
  const { dir, cleanup } = home();
  t.after(cleanup);

  markRead("abc-123", "/w/api/src/a.ts", dir);
  assert.equal(sessionFile("abc-123", dir), path.join(dir, ".carrick", "sessions", "abc-123.json"));
  assert.ok(fs.existsSync(sessionFile("abc-123", dir)!));
  // A session id that could walk out of the directory is not a file name.
  assert.equal(sessionFile("../../etc/passwd", dir), null);
  markRead("../../etc/passwd", "/w/api/src/a.ts", dir);
  assert.deepEqual(readSession("../../etc/passwd", dir).read, []);
});

test("a record this build cannot read is replaced, not thrown at the caller", (t) => {
  const { dir, cleanup } = home();
  t.after(cleanup);

  const file = sessionFile("s3", dir)!;
  fs.mkdirSync(path.dirname(file), { recursive: true });
  fs.writeFileSync(file, "{ not json");
  assert.deepEqual(readSession("s3", dir).read, []);
  markRead("s3", "/w/api/src/a.ts", dir);
  assert.deepEqual(readSession("s3", dir).read, ["/w/api/src/a.ts"]);
});

test("a record an older install wrote loads, and its function list is dropped on the next write (carrick#3436)", (t) => {
  const { dir, cleanup } = home();
  t.after(cleanup);

  const file = sessionFile("s-old", dir)!;
  fs.mkdirSync(path.dirname(file), { recursive: true });
  fs.writeFileSync(
    file,
    JSON.stringify({
      found: [{ name: "slugify", file: "src/a.ts", indexCommit: "abc" }],
      nudged: [],
      told: ["/w/api::src/a.ts"],
      read: [],
      edited: {},
      updated: "",
    }),
  );
  assert.equal(wasTold("s-old", "/w/api::src/a.ts", dir), true);
  recordEdit("s-old", "/w", 5, dir);
  const written = JSON.parse(fs.readFileSync(file, "utf8")) as Record<string, unknown>;
  assert.equal("found" in written, false);
  assert.equal("nudged" in written, false);
  assert.deepEqual(written["told"], ["/w/api::src/a.ts"]);
  assert.deepEqual(written["edited"], { "/w": 5 });
});

test("records nothing will read again are pruned", (t) => {
  const { dir, cleanup } = home();
  t.after(cleanup);

  markRead("old", "/w/a.ts", dir);
  markRead("new", "/w/b.ts", dir);
  const old = sessionFile("old", dir)!;
  const ancient = Date.now() - 30 * 24 * 60 * 60 * 1000;
  fs.utimesSync(old, ancient / 1000, ancient / 1000);
  prune(dir);
  assert.equal(fs.existsSync(old), false);
  assert.equal(fs.existsSync(sessionFile("new", dir)!), true);
});

test("a session write prunes at most once an hour", (t) => {
  const { dir, cleanup } = home();
  t.after(cleanup);

  markRead("a", "/w/a.ts", dir);
  const old = path.join(sessionsDir(dir), "ancient.json");
  fs.writeFileSync(old, "{}");
  const eightDays = (Date.now() - 8 * 24 * 60 * 60 * 1000) / 1000;
  fs.utimesSync(old, eightDays, eightDays);
  markRead("b", "/w/b.ts", dir);
  assert.equal(fs.existsSync(old), true);

  const stamp = path.join(sessionsDir(dir), ".pruned-at");
  const twoHours = (Date.now() - 2 * 60 * 60 * 1000) / 1000;
  fs.utimesSync(stamp, twoHours, twoHours);
  markRead("c", "/w/c.ts", dir);
  assert.equal(fs.existsSync(old), false);
  // The stamp is not a session record.
  assert.equal(sessionRecords(dir), 3);

  const reads = t.mock.method(fs, "readdirSync");
  for (let i = 0; i < 10; i += 1) markRead(`n${i}`, "/w/a.ts", dir);
  const sessionReads = reads.mock.calls.filter(
    (c) => String(c.arguments[0]) === sessionsDir(dir),
  ).length;
  assert.ok(sessionReads <= 1, `read the sessions folder ${sessionReads} times`);
});

test("remove deletes every record and the directory holding them", (t) => {
  const { dir, cleanup } = home();
  t.after(cleanup);

  markRead("s4", "/w/a.ts", dir);
  markRead("s5", "/w/b.ts", dir);
  assert.equal(removeSessions(dir), 2);
  assert.equal(fs.existsSync(sessionsDir(dir)), false);
  // A machine that never recorded one has nothing to say about it.
  assert.equal(removeSessions(dir), 0);
});

function workspace(repos: unknown): string {
  const root = fs.mkdtempSync(path.join(os.tmpdir(), "carrick-slug-"));
  if (repos !== undefined) {
    fs.mkdirSync(path.join(root, ".carrick", "hosted"), { recursive: true });
    fs.writeFileSync(
      path.join(root, ".carrick", "hosted", "snapshot.json"),
      JSON.stringify({ resolution: { repos } }),
    );
  }
  return root;
}

test("the snapshot's project slug is read when it holds exactly one", (t) => {
  const root = workspace([{ project_slug: "shop-system" }, { project_slug: "shop-system" }]);
  t.after(() => fs.rmSync(root, { recursive: true, force: true }));
  assert.equal(snapshotProjectSlug(root), "shop-system");
});

test("no slug with no snapshot, bad JSON or several slugs", (t) => {
  const none = workspace(undefined);
  const bad = workspace(undefined);
  fs.mkdirSync(path.join(bad, ".carrick", "hosted"), { recursive: true });
  fs.writeFileSync(path.join(bad, ".carrick", "hosted", "snapshot.json"), "{nope");
  const two = workspace([{ project_slug: "a" }, { project_slug: "b" }]);
  t.after(() => {
    for (const dir of [none, bad, two]) fs.rmSync(dir, { recursive: true, force: true });
  });
  for (const root of [none, bad, two]) assert.equal(snapshotProjectSlug(root), "");
});

// The files whose uses lines a session has been shown (carrick#2067).
test("a told file is remembered, and an edit stamp keeps it", (t) => {
  const { dir, cleanup } = home();
  t.after(cleanup);

  const key = toldKey({ schema: "carrick.check/0", repo: "/w/api", file: "src/a.ts" }, "ignored.ts");
  assert.equal(key, "/w/api::src/a.ts");
  assert.equal(wasTold("sess-told", key, dir), false);
  markTold("sess-told", [key], dir);
  assert.equal(wasTold("sess-told", key, dir), true);

  recordEdit("sess-told", "/w", Date.now(), dir);
  assert.deepEqual(readSession("sess-told", dir).told, [key]);
  // Without a repo or file in the answer, the hook's own path stands in.
  assert.equal(toldKey({ schema: "carrick.check/0" }, "src/b.ts"), "::src/b.ts");
});

// The files the Read hook has asked about (carrick#2069), kept in the same
// record and written whole: no partial or temp file is left behind.
test("a read file survives a write and a read back, and a write leaves no temp file", (t) => {
  const { dir, cleanup } = home();
  t.after(cleanup);

  assert.equal(wasRead("sess-read", "/w/api/src/a.ts", dir), false);
  markRead("sess-read", "/w/api/src/a.ts", dir);
  markRead("sess-read", "/w/api/src/a.ts", dir);
  assert.equal(wasRead("sess-read", "/w/api/src/a.ts", dir), true);
  assert.deepEqual(readSession("sess-read", dir).read, ["/w/api/src/a.ts"]);
  // Neither list drops the other.
  markTold("sess-read", ["/w/api::src/a.ts"], dir);
  const after = readSession("sess-read", dir);
  assert.deepEqual(after.read, ["/w/api/src/a.ts"]);
  assert.deepEqual(after.told, ["/w/api::src/a.ts"]);
  assert.deepEqual(
    fs.readdirSync(sessionsDir(dir)).filter((name) => name !== ".pruned-at"),
    ["sess-read.json"],
  );

  // A write replaces the file rather than emptying it in place, so a reader
  // that arrives mid-write sees the old record whole and not an empty one: the
  // file after a write is a different file (a new inode), not the same one.
  const file = sessionFile("sess-read", dir)!;
  const before = fs.statSync(file).ino;
  markRead("sess-read", "/w/api/src/b.ts", dir);
  assert.notEqual(fs.statSync(file).ino, before);
});
