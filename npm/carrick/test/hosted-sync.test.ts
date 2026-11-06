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
import { heldLine, hostedLine } from "../src/render.ts";
import { lastSync, type SyncRecord, type SyncedRepo } from "../src/hosted-snapshot.ts";

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
});

test("the hosted line names the commit the MCP answers come from and its age", () => {
  assert.equal(
    hostedLine(record([row()]), NOW),
    "Carrick's MCP tools answer from the hosted index: api at bbbbbbb (scanned 2 h ago).",
  );
});

test("an older copy on this machine is stated apart, with its own age", () => {
  assert.equal(
    hostedLine(record([behind]), NOW),
    "Carrick's MCP tools answer from the hosted index: api at bbbbbbb (scanned 2 h ago). " +
      "The hooks and `carrick check` compare against this machine's older copy: api at aaaaaaa (scanned 8 days ago).",
  );
});

test("a record from an old check says when it was taken", () => {
  assert.match(hostedLine(record([row()], NOW - 3 * DAY), NOW) ?? "", / Last checked 3 days ago\.$/);
});

test("the held line names how far behind HEAD is and the move", () => {
  assert.equal(
    heldLine(record([behind], NOW, "kept")),
    "This checkout is 93 commits behind the hosted index of api (bbbbbbb), so this machine's copy cannot move forward. `git pull` (or a rebase onto the default branch) moves it.",
  );
  assert.match(heldLine(record([{ ...behind, head_detached: true }])) ?? "", /HEAD is detached\. Check out the default branch and `git pull` to move it\.$/);
  assert.match(
    heldLine(record([{ ...behind, fetched: false, behind: null }])) ?? "",
    /^This clone has not fetched the hosted index of api \(bbbbbbb\), .* `git fetch`, then `git pull` \(or a rebase onto the default branch\), moves it\.$/,
  );
  // Nothing held: a copy at the hosted commit, or one HEAD can take.
  assert.equal(heldLine(record([row()])), null);
  assert.equal(heldLine(record([{ ...behind, head_holds_hosted: true }])), null);
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
    "Carrick's MCP tools answer from the hosted index: api at bbbbbbb (scanned 2 h ago). " +
      "The hooks and `carrick check` compare against this machine's older copy: api at aaaaaaa (scanned 8 days ago).",
  );
  assert.match(lines[1] ?? "", /^This checkout is 93 commits behind the hosted index of api \(bbbbbbb\)/);
  // The machine's own scan keeps its date, said to be this machine's.
  assert.match(lines[2] ?? "", /^Carrick indexed 3 service\(s\) in \S+ on this machine at 2026-09-06T21:14:03Z/);

  const second = await runHook("session-start.ts", { cwd: workspace.root, env });
  assert.match(second.stdout, /^Carrick's MCP tools answer from the hosted index/);
  assert.doesNotMatch(second.stdout, /commits behind/, "the held line came twice in a day");

  const daily = await runHook("session-start.ts", {
    cwd: workspace.root,
    env: { ...env, CARRICK_HELD_NOTICE_COOLDOWN_MS: "0" },
  });
  assert.match(daily.stdout, /commits behind/, "a day later it is said again");
});

test("with no record the session start is what it was", async (t) => {
  const workspace = makeWorkspace();
  t.after(() => workspace.cleanup());
  const run = await runHook("session-start.ts", {
    cwd: workspace.root,
    env: fakeEnv({ CARRICK_FAKE_FIXTURE: fixturePath("status-workspace.json") }),
  });
  assert.match(run.stdout, /^Carrick indexed 3 service\(s\) in \S+ at 2026-09-06T21:14:03Z/);
  assert.doesNotMatch(run.stdout, /hosted index/);
});
