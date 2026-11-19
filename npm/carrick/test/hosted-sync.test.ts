// The session start states the age of the hosted index the agent's MCP
// answers come from, apart from this machine's copy and its own scan
// (carrick#3432), and says once a day when the copy cannot move because HEAD
// lacks the hosted commit (carrick#3433). Both read the record `carrick
// refresh --hosted-only` writes, `.carrick/hosted/last-sync.json`.

import test from "node:test";
import assert from "node:assert/strict";
import fs from "node:fs";
import path from "node:path";
import { fakeEnv, fixturePath, makeWorkspace, runHook } from "./helpers.ts";
import { hostedLine } from "../src/render.ts";
import { keptLines, lastSync, type SyncRecord, type SyncedRepo } from "../src/hosted-snapshot.ts";

const HOUR = 60 * 60_000;
const DAY = 24 * HOUR;
const NOW = Date.parse("2026-11-06T10:00:00Z");
const HOSTED = "b".repeat(40);
const COPY = "a".repeat(40);

function row(overrides: Partial<SyncedRepo> = {}): SyncedRepo {
  return {
    repo: "example/api",
    hosted_commit: HOSTED,
    hosted_scanned_at: new Date(NOW - 2 * HOUR).toISOString(),
    copy_commit: HOSTED,
    copy_scanned_at: new Date(NOW - 2 * HOUR).toISOString(),
    head_holds_hosted: true,
    fetched: true,
    behind: null,
    head_detached: false,
    ...overrides,
  };
}

function record(repos: SyncedRepo[], at = NOW - 5 * 60_000, outcome = "current"): SyncRecord {
  return { schema: "carrick.hosted-sync/0", at: new Date(at).toISOString(), outcome, reason: null, repos };
}

/** Where the HEAD of the real case was: 93 commits behind, copy 8 days old. */
const behind = row({
  copy_commit: COPY,
  copy_scanned_at: new Date(NOW - 8 * DAY).toISOString(),
  head_holds_hosted: false,
  behind: 93,
  // As the Rust side writes them (`kept_sentence`): the tests below read the
  // sentence from the record, never from a literal of their own.
  kept_reason: "example/api: the hosted index is at bbbbbbb, which this checkout's HEAD does not hold",
  kept_line: "KEPT-LINE-API-FROM-THE-RECORD",
  kept_short: "KEPT-SHORT-API-FROM-THE-RECORD",
});

test("a copy at the hosted commit is fresh and has no hosted line", () => {
  assert.equal(hostedLine(record([row()]), NOW), null);
  assert.equal(hostedLine(record([row()], NOW - 3 * DAY), NOW), null);
});

test("an older copy on this machine is stated apart, with its own age", () => {
  assert.equal(
    hostedLine(record([behind]), NOW),
    "Carrick's MCP tools and fact lines read the hosted index: api at bbbbbbb (scanned 2 h ago). " +
      "The re-check, Read, uses and search lines read this machine's copy: api at aaaaaaa (scanned 8 days ago).",
  );
});

test("a record from an old check, with an older copy, says when it was taken", () => {
  assert.match(hostedLine(record([behind], NOW - 3 * DAY), NOW) ?? "", / Last checked 3 days ago\.$/);
});
test("every kept repo's sentence is the record's, one per repo, and a current repo has none", () => {
  const web = row({ repo: "example/web", kept_line: "KEPT-LINE-WEB-FROM-THE-RECORD", kept_short: "x" });
  assert.deepEqual(keptLines(record([behind, row({ repo: "example/jobs" }), web], NOW, "kept")), [
    behind.kept_line,
    web.kept_line,
  ]);
  assert.deepEqual(keptLines(record([row()])), []);
});

test("a hosted line skips a repo with no single hosted commit", () => {
  const noRow = row({ hosted_commit: "", copy_commit: "", kept_line: "KEPT-NO-ROW", kept_short: "y" });
  assert.equal(hostedLine(record([noRow]), NOW), null);
  assert.deepEqual(keptLines(record([noRow])), ["KEPT-NO-ROW"]);
  // Beside a repo that is older, it adds nothing to the hosted line.
  assert.equal(hostedLine(record([behind, noRow]), NOW), hostedLine(record([behind]), NOW));
});

test("a record of another shape reads as none", (t) => {
  const workspace = makeWorkspace();
  t.after(() => workspace.cleanup());
  const file = path.join(workspace.root, ".carrick", "hosted", "last-sync.json");
  fs.mkdirSync(path.dirname(file), { recursive: true });
  assert.equal(lastSync(workspace.root), null);
  fs.writeFileSync(file, JSON.stringify({ schema: "other/0", at: "x", repos: [] }));
  assert.equal(lastSync(workspace.root), null);
  fs.writeFileSync(file, JSON.stringify(record([behind])));
  assert.deepEqual(lastSync(workspace.root)?.repos, [behind]);
  // A full refresh rewrote the snapshot after the record: the copy the
  // record names is no longer the one on disk.
  const snapshot = path.join(path.dirname(file), "snapshot.json");
  fs.writeFileSync(snapshot, "{}");
  const later = new Date(fs.statSync(file).mtimeMs + 1000);
  fs.utimesSync(snapshot, later, later);
  assert.equal(lastSync(workspace.root), null);
});

function writeRecord(root: string, value: SyncRecord): void {
  const file = path.join(root, ".carrick", "hosted", "last-sync.json");
  fs.mkdirSync(path.dirname(file), { recursive: true });
  fs.writeFileSync(file, JSON.stringify(value));
}

test("the session start opens with the hosted index, then says once a day that the copy is held", async (t) => {
  const workspace = makeWorkspace();
  t.after(() => workspace.cleanup());
  const now = Date.now();
  writeRecord(
    workspace.root,
    record(
      [
        {
          ...behind,
          hosted_scanned_at: new Date(now - 2 * HOUR).toISOString(),
          copy_scanned_at: new Date(now - 8 * DAY).toISOString(),
        },
      ],
      now - 60_000,
      "kept",
    ),
  );
  const env = fakeEnv({ CARRICK_FAKE_FIXTURE: fixturePath("status-workspace.json") });

  const first = await runHook("session-start.ts", { cwd: workspace.root, env });
  assert.equal(first.code, 0);
  const lines = first.stdout.split("\n");
  assert.equal(
    lines[0],
    "Carrick's MCP tools and fact lines read the hosted index: api at bbbbbbb (scanned 2 h ago). " +
      "The re-check, Read, uses and search lines read this machine's copy: api at aaaaaaa (scanned 8 days ago).",
  );
  // The record's own sentence, word for word.
  assert.equal(lines[1], behind.kept_line);
  // A healthy index adds nothing beside them.
  assert.equal(lines.length, 3, first.stdout);

  const second = await runHook("session-start.ts", { cwd: workspace.root, env });
  assert.match(second.stdout, /^Carrick's MCP tools and fact lines read the hosted index/);
  assert.doesNotMatch(second.stdout, new RegExp(behind.kept_line ?? "x"), "the kept line came twice in a day");

  const daily = await runHook("session-start.ts", {
    cwd: workspace.root,
    env: { ...env, CARRICK_HELD_NOTICE_COOLDOWN_MS: "0" },
  });
  assert.match(daily.stdout, new RegExp(behind.kept_line ?? "x"), "a day later it is said again");
});

test("a workspace with one kept repo and two current ones names only the kept one", async (t) => {
  const workspace = makeWorkspace();
  t.after(() => workspace.cleanup());
  const web = row({ repo: "example/web" });
  const jobs = row({ repo: "example/jobs" });
  writeRecord(workspace.root, record([row(), { ...behind, repo: "example/api" }, web, jobs], Date.now() - 60_000, "moved"));
  const run = await runHook("session-start.ts", {
    cwd: workspace.root,
    env: fakeEnv({ CARRICK_FAKE_FIXTURE: fixturePath("status-workspace.json") }),
  });
  assert.equal(run.stdout.split("\n").filter((line) => line === behind.kept_line).length, 1, run.stdout);
  assert.doesNotMatch(run.stdout, /example\/(web|jobs)/);
});

test("a copy kept back for a reason that leaves it at the hosted commit is still said", async (t) => {
  // A copy the heal could not carry, or a repo with no single commit: the
  // hosted line has nothing to say, and the kept sentence is still printed.
  const workspace = makeWorkspace();
  t.after(() => workspace.cleanup());
  const dirty = row({ kept_reason: "example/api: dirty", kept_line: "KEPT-LINE-DIRTY", kept_short: "z" });
  writeRecord(workspace.root, record([dirty], Date.now() - 60_000, "kept"));
  const env = fakeEnv({ CARRICK_FAKE_FIXTURE: fixturePath("status-workspace.json") });
  const first = await runHook("session-start.ts", { cwd: workspace.root, env });
  assert.equal(first.stdout.trim(), "KEPT-LINE-DIRTY");
  // The 24 hour claim holds for it too.
  const second = await runHook("session-start.ts", { cwd: workspace.root, env });
  assert.equal(second.stdout, "");
});

test("with no record a healthy session start prints nothing", async (t) => {
  const workspace = makeWorkspace();
  t.after(() => workspace.cleanup());
  const run = await runHook("session-start.ts", {
    cwd: workspace.root,
    env: fakeEnv({ CARRICK_FAKE_FIXTURE: fixturePath("status-workspace.json") }),
  });
  assert.equal(run.stdout, "");
});
