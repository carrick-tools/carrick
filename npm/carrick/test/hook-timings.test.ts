// Every hook writes its own timing line, the file stays bounded, `carrick
// status` shows the percentiles, and the session start names a slow hook only
// when there is one (`src/hook/timings.ts`).

import assert from "node:assert/strict";
import test from "node:test";
import fs from "node:fs";
import path from "node:path";
import { spawn } from "node:child_process";

import { editPayload, fakeEnv, fixturePath, makeWorkspace, pluginDir, runHook } from "./helpers.ts";
import {
  appendTiming,
  hookSpeeds,
  MAX_LINES,
  readTimings,
  timed,
  timingsPath,
  type TimingRecord,
} from "../src/hook/timings.ts";

function row(hook: TimingRecord["hook"], durationMs: number, ageMs = 0): TimingRecord {
  return {
    hook,
    start: new Date(Date.now() - ageMs).toISOString(),
    duration_ms: durationMs,
    waited_ms: 0,
    outcome: "silent",
  };
}

function seed(root: string, rows: TimingRecord[]): void {
  fs.writeFileSync(timingsPath(root), rows.map((r) => JSON.stringify(r)).join("\n") + "\n");
}

test("a hook run appends one timing line, with the edited file relative to the workspace", async (t) => {
  const workspace = makeWorkspace();
  t.after(() => workspace.cleanup());
  const run = await runHook("post-edit.ts", { payload: editPayload(workspace), cwd: workspace.root, env: fakeEnv() });
  assert.equal(run.code, 0);
  const rows = readTimings(workspace.root);
  assert.equal(rows.length, 1);
  assert.equal(rows[0]!.hook, "post-edit");
  assert.equal(rows[0]!.file, path.join("user-service", "src", "routes", "users.ts"));
  assert.ok(rows[0]!.duration_ms >= 0 && rows[0]!.waited_ms >= 0);
  assert.ok(["printed", "silent"].includes(rows[0]!.outcome));
  assert.deepEqual(Object.keys(rows[0]!).sort(), ["duration_ms", "file", "hook", "outcome", "start", "waited_ms"]);
});

test("the stop hook records its wait", async (t) => {
  const workspace = makeWorkspace();
  t.after(() => workspace.cleanup());
  await runHook("stop.ts", {
    payload: { session_id: "timing-sess" },
    cwd: workspace.root,
    env: fakeEnv({ HOME: workspace.root }),
  });
  const rows = readTimings(workspace.root);
  assert.equal(rows.length, 1);
  assert.equal(rows[0]!.hook, "stop");
});

test("the file keeps its last 2,000 lines", (t) => {
  const workspace = makeWorkspace();
  t.after(() => workspace.cleanup());
  for (let i = 0; i < MAX_LINES + 25; i += 1) appendTiming(workspace.root, row("stop", i));
  const rows = readTimings(workspace.root);
  assert.equal(rows.length, MAX_LINES);
  assert.equal(rows[0]!.duration_ms, 25);
  assert.equal(rows.at(-1)!.duration_ms, MAX_LINES + 24);
});

test("percentiles read only the last 7 days", (t) => {
  const workspace = makeWorkspace();
  t.after(() => workspace.cleanup());
  const day = 24 * 60 * 60_000;
  seed(workspace.root, [
    row("stop", 99_000, 8 * day),
    ...[100, 200, 300, 400, 500, 600, 700, 800, 900, 5000].map((ms) => row("stop", ms)),
  ]);
  const [stop] = hookSpeeds(workspace.root);
  assert.deepEqual({ runs: stop!.runs, p50: stop!.p50, p90: stop!.p90 }, { runs: 10, p50: 500, p90: 900 });
});

function carrickStatus(cwd: string): Promise<string> {
  return new Promise((resolve) => {
    const child = spawn(process.execPath, [path.join(pluginDir, "bin", "carrick.mjs"), "status"], {
      cwd,
      env: fakeEnv({ CARRICK_FAKE_FIXTURE: fixturePath("status-workspace.json") }),
      stdio: ["ignore", "pipe", "ignore"],
    });
    let out = "";
    child.stdout.on("data", (chunk: Buffer) => (out += chunk.toString("utf8")));
    child.on("close", () => resolve(out));
  });
}

test("carrick status shows each hook's p50 and p90, and nothing when no hook has run", async (t) => {
  const workspace = makeWorkspace();
  t.after(() => workspace.cleanup());
  assert.doesNotMatch(await carrickStatus(workspace.root), /Hook run time/);
  seed(workspace.root, [row("stop", 1000), row("stop", 3000), row("post-edit", 200)]);
  const out = await carrickStatus(workspace.root);
  assert.match(out, /Hook run time, last 7 days \(p50\/p90, seconds\): post-edit 0\.2\/0\.2, stop 1\.0\/3\.0\n/);
});

test("session start names a hook only when its p90 is above 1 s", async (t) => {
  const workspace = makeWorkspace();
  t.after(() => workspace.cleanup());
  const start = () =>
    runHook("session-start.ts", {
      cwd: workspace.root,
      env: fakeEnv({ CARRICK_FAKE_FIXTURE: fixturePath("status-workspace.json") }),
    });

  seed(workspace.root, [row("stop", 900), row("stop", 1000), row("post-edit", 300)]);
  assert.equal((await start()).stdout, "");

  seed(workspace.root, [row("stop", 900), row("stop", 1400), row("post-edit", 300)]);
  assert.equal(
    (await start()).stdout,
    "Carrick's stop hook has taken 1.4 s (p90) in recent turns; run `carrick status`.\n",
  );
});

test("the clock starts at the origin given, not at the call", async (t) => {
  const workspace = makeWorkspace();
  t.after(() => workspace.cleanup());
  const origin = Date.now() - 5000;
  await timed("stop", async (timing) => void (timing.root = workspace.root), origin);
  const [record] = readTimings(workspace.root);
  assert.equal(record!.start, new Date(origin).toISOString());
  assert.ok(record!.duration_ms >= 5000 && record!.duration_ms < 8000);
});

test("an empty emission is silent, and one with text is printed", async (t) => {
  const workspace = makeWorkspace();
  t.after(() => workspace.cleanup());
  const say = (context: string) => async (timing: { root: string | null }) => {
    timing.root = workspace.root;
    process.stdout.write(
      JSON.stringify({ hookSpecificOutput: { hookEventName: "Stop", additionalContext: context } }),
    );
  };
  await timed("stop", say(""));
  await timed("stop", say("a late answer"));
  assert.deepEqual(
    readTimings(workspace.root).map((r) => r.outcome),
    ["silent", "printed"],
  );
});
