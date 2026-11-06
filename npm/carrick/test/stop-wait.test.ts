// The Stop hook waits for the background re-check of a session's last edit
// (`src/hook/stop-wait.ts`, B3). The runner is stood in for by the test: it
// writes the answer file the real runner would, at the moment the test names,
// and a live lock keeps the hook from starting a runner of its own.

import assert from "node:assert/strict";
import test from "node:test";
import fs from "node:fs";
import os from "node:os";
import path from "node:path";

import { spawn } from "node:child_process";

import { fakeEnv, fixture, makeWorkspace, pluginDir, type HookRun } from "./helpers.ts";
import type { CheckResult } from "../src/contract.ts";
import { BACKGROUND_CAP_MS, enqueue, lockPath, STOP_TIMEOUT_S, STOP_WAIT_S } from "../src/hook/rechecks.ts";
import type { Answer, QueueEntry } from "../src/hook/rechecks.ts";
import { checkMarkerPath } from "../src/hook/checked.ts";
import { recordEdit, sessionsDir, writeAtomic } from "../src/hook/reuse.ts";
import { stopAnswers, waitSeconds } from "../src/hook/stop-wait.ts";
import { carrickHooks } from "../src/init/settings.ts";

const SESSION = "stop-sess";

function setup(t: { after: (fn: () => void) => void }) {
  const workspace = makeWorkspace();
  const home = fs.realpathSync(fs.mkdtempSync(path.join(os.tmpdir(), "carrick-home-")));
  t.after(() => {
    workspace.cleanup();
    fs.rmSync(home, { recursive: true, force: true });
  });
  return { workspace, home };
}

/** A live process holding the runner lock: the hook starts no runner of its own. */
function liveRunner(root: string): void {
  fs.mkdirSync(path.dirname(lockPath(root)), { recursive: true });
  const now = new Date().toISOString();
  fs.writeFileSync(lockPath(root), `${JSON.stringify({ pid: process.pid, started: now, touched: now })}\n`);
}

function queue(root: string, home: string, file: string, session = SESSION): QueueEntry {
  return enqueue(root, session, [file], Date.now(), { home })[0]!;
}

/** What the runner writes when a call ends. */
function answer(home: string, entry: QueueEntry, result: CheckResult): void {
  const body: Answer = {
    seq: entry.seq,
    session: entry.session,
    file: entry.file,
    relative: entry.relative,
    root: entry.root,
    state: "answered",
    enqueued_at: entry.enqueued_at,
    started_at: entry.enqueued_at,
    finished_at: new Date().toISOString(),
    result,
  };
  writeAtomic(path.join(sessionsDir(home), `${entry.session}.recheck.${entry.seq}.answer.json`), `${JSON.stringify(body)}\n`);
}

/** A mismatch the re-check answered for: its problems print. */
function problem(): CheckResult {
  return { ...fixture("check-mismatch.json"), recheck: { ran: "extraction+types", elapsed_ms: 1 } };
}

function clean(): CheckResult {
  return fixture("check-clean.json");
}

/** Run the Stop hook the way the settings entry does: `stop.ts --wait-s <n>`. */
function stopWith(home: string, waitS: number, payload: Record<string, unknown> = {}): Promise<HookRun> {
  const started = Date.now();
  return new Promise((resolve) => {
    const child = spawn(process.execPath, [path.join(pluginDir, "src", "hook", "stop.ts"), "--wait-s", String(waitS)], {
      env: fakeEnv({ HOME: home }),
      stdio: ["pipe", "pipe", "pipe"],
    });
    let stdout = "";
    child.stdout.on("data", (chunk: Buffer) => (stdout += chunk.toString("utf8")));
    child.on("close", (code) => resolve({ stdout, stderr: "", code, ms: Date.now() - started }));
    child.stdin.end(JSON.stringify({ hook_event_name: "Stop", session_id: SESSION, ...payload }));
  });
}

function context(stdout: string): string {
  return (JSON.parse(stdout) as { hookSpecificOutput: { additionalContext: string } }).hookSpecificOutput
    .additionalContext;
}

test("stop waits for a re-check that finishes inside the wait and prints its problem", async (t) => {
  const { workspace, home } = setup(t);
  liveRunner(workspace.root);
  const entry = queue(workspace.root, home, workspace.file);
  setTimeout(() => answer(home, entry, problem()), 2000);

  const run = await stopWith(home, 20);
  assert.equal(run.code, 0);
  assert.ok(run.ms >= 1800 && run.ms < 8000, `waited for the answer, not the whole wait (${run.ms} ms)`);
  // The one Stop channel Claude Code delivers to the model while letting the
  // turn continue: `systemMessage` never reaches the model, and `decision:
  // "block"` reaches it by refusing to stop.
  const parsed = JSON.parse(run.stdout) as Record<string, unknown> & { hookSpecificOutput: { hookEventName: string } };
  assert.equal(parsed.hookSpecificOutput.hookEventName, "Stop");
  assert.equal(parsed["decision"], undefined);
  assert.equal(parsed["continue"], undefined);
  assert.equal(parsed["systemMessage"], undefined);
  const text = context(run.stdout);
  assert.match(text, /contract problem/);
  assert.match(
    text,
    /Carrick re-checked \S*users\.ts after your earlier edit to it: /,
    "a late answer says it is about an earlier edit",
  );
  assert.doesNotMatch(text, /after this edit/);
  assert.doesNotMatch(text, /still running/);
});

test("stop with nothing pending returns at once and names no function (carrick#3436)", async (t) => {
  const { workspace, home } = setup(t);
  // A session file an older install wrote, still listing a function it found.
  writeAtomic(
    path.join(sessionsDir(home), `${SESSION}.json`),
    `${JSON.stringify({ found: [{ name: "slugify", file: "src/a.ts", indexCommit: "abc" }], nudged: [], told: [], read: [], edited: {}, updated: new Date().toISOString() })}\n`,
  );
  const run = await stopWith(home, 20);
  assert.ok(run.ms < 1500, `no wait (${run.ms} ms)`);
  assert.equal(run.stdout, "");
  void workspace;
});

test("stop says the re-check is still running when it outlives the wait, once, and delivers the answer later", async (t) => {
  const { workspace, home } = setup(t);
  liveRunner(workspace.root);
  const entry = queue(workspace.root, home, workspace.file);

  const first = await stopWith(home, 1);
  assert.ok(first.ms < 2500, `returned inside its wait (${first.ms} ms)`);
  assert.match(
    context(first.stdout),
    /^Carrick: the re-check of user-service\/src\/routes\/users\.ts is still running after \d+ s\. If this change touches a route, a call to another service, an event or a type another service reads, run `carrick check --changed` before you finish\.$/,
  );

  const second = await stopWith(home, 20, { stop_hook_active: true });
  assert.ok(second.ms < 1500, `a second Stop never waits twice for one entry (${second.ms} ms)`);
  assert.equal(second.stdout, "");

  answer(home, entry, problem());
  const third = await stopWith(home, 20);
  assert.match(context(third.stdout), /contract problem/);
  const fourth = await stopWith(home, 20);
  assert.equal(fourth.stdout, "", "delivered once");
});

test("a clean late answer alone prints nothing and is marked delivered; beside a problem both print", async (t) => {
  const { workspace, home } = setup(t);
  const entry = queue(workspace.root, home, workspace.file);
  answer(home, entry, clean());
  const alone = await stopWith(home, 5);
  assert.equal(alone.stdout, "");
  assert.ok(fs.existsSync(path.join(sessionsDir(home), `${SESSION}.recheck.${entry.seq}.delivered`)));

  const other = path.join(workspace.service, "src", "routes", "orders.ts");
  fs.writeFileSync(other, "export {};\n");
  const a = queue(workspace.root, home, workspace.file);
  const b = queue(workspace.root, home, other);
  answer(home, a, clean());
  answer(home, b, problem());
  const both = context((await stopWith(home, 5)).stdout);
  assert.match(both, /contract problem/);
  assert.match(both, /no contract problem|re-checked/, "the clean answer rides along");
  assert.ok(both.split("\n").some((line) => line.includes("users.ts")));
  assert.ok(both.split("\n").some((line) => line.includes("orders.ts")));
});

test("a dead runner lock with queued entries: stop starts a runner before it waits", async (t) => {
  const { workspace, home } = setup(t);
  const entry = queue(workspace.root, home, workspace.file);
  const started: string[] = [];
  const result = await stopAnswers(SESSION, {
    waitS: 5,
    home,
    ensure: (root) => {
      started.push(root);
      answer(home, entry, problem());
      return true;
    },
  });
  assert.deepEqual(started, [workspace.root]);
  assert.ok(result.lines.some((line) => /contract problem/.test(line)));
});

test("the Stop entry waits as long as the cap says, and its timeout sits above the wait", () => {
  const entry = carrickHooks("carrick").Stop![0]!.hooks[0]!;
  assert.equal(STOP_WAIT_S, BACKGROUND_CAP_MS / 1000 + 20);
  assert.equal(STOP_TIMEOUT_S, STOP_WAIT_S + 10);
  assert.equal(entry.command, `carrick hook stop --wait-s ${STOP_WAIT_S}`);
  assert.equal(entry.timeout, STOP_TIMEOUT_S);
  assert.ok(entry.timeout! > waitSeconds(entry.command.split(" ")));
  assert.equal(entry.statusMessage, "Carrick: waiting for the re-check of your last edit");
});

test("the wait is read from the command and defaults to 3 s", () => {
  assert.equal(waitSeconds(["hook", "stop", "--wait-s", "200"]), 200);
  assert.equal(waitSeconds(["--wait-s=7"]), 7);
  assert.equal(waitSeconds(["hook", "stop"]), 3);
});

// A `carrick check --changed` that ran after the session's last edit has
// already said what the clause "Run `carrick check --changed` before you
// finish" asks for (carrick#3357): the agent ran it twice, 137 s then 133 s.

const CLAUSE = /If this change touches a route, a call to another service, an event or a type another service reads, run `carrick check --changed` before you finish/;

/** What `carrick check --changed` leaves in the workspace when it ends. */
function marker(root: string, startedMs: number, exit: number): void {
  writeAtomic(
    checkMarkerPath(root),
    `${JSON.stringify({ started_at_ms: startedMs, finished_at_ms: startedMs + 1000, exit })}\n`,
  );
}

/** A re-check the runner gave up on: the line that carries the clause. */
function stopped(home: string, entry: QueueEntry): void {
  const body: Answer = {
    seq: entry.seq,
    session: entry.session,
    file: entry.file,
    relative: entry.relative,
    root: entry.root,
    state: "stopped",
    enqueued_at: entry.enqueued_at,
    started_at: null,
    finished_at: new Date().toISOString(),
    failure: "the re-check ran past its budget",
  };
  writeAtomic(path.join(sessionsDir(home), `${entry.session}.recheck.${entry.seq}.answer.json`), `${JSON.stringify(body)}\n`);
}

test("stop drops the check clause when a clean check --changed ran after the last edit", async (t) => {
  const { workspace, home } = setup(t);
  const entry = queue(workspace.root, home, workspace.file);
  recordEdit(SESSION, workspace.root, 1_000_000, home);
  marker(workspace.root, 2_000_000, 0);
  stopped(home, entry);
  const text = (await stopAnswers(SESSION, { waitS: 0, home })).lines.join("\n");
  assert.match(text, /was not re-checked: the re-check ran past its budget\./);
  assert.doesNotMatch(text, CLAUSE);
});

test("stop keeps the check clause when an edit came after the check", async (t) => {
  const { workspace, home } = setup(t);
  const entry = queue(workspace.root, home, workspace.file);
  marker(workspace.root, 2_000_000, 0);
  recordEdit(SESSION, workspace.root, 3_000_000, home);
  stopped(home, entry);
  assert.match((await stopAnswers(SESSION, { waitS: 0, home })).lines.join("\n"), CLAUSE);
});

test("stop keeps the check clause when no check --changed ran, or the one that ran failed", async (t) => {
  const { workspace, home } = setup(t);
  const entry = queue(workspace.root, home, workspace.file);
  recordEdit(SESSION, workspace.root, 1_000_000, home);
  stopped(home, entry);
  assert.match((await stopAnswers(SESSION, { waitS: 0, home })).lines.join("\n"), CLAUSE, "no check");

  const again = queue(workspace.root, home, workspace.file);
  marker(workspace.root, 2_000_000, 2);
  stopped(home, again);
  assert.match((await stopAnswers(SESSION, { waitS: 0, home })).lines.join("\n"), CLAUSE, "exit 2 checked nothing");
});

test("stop drops the clause from the still-running line when a clean check followed the last edit", async (t) => {
  const { workspace, home } = setup(t);
  liveRunner(workspace.root);
  queue(workspace.root, home, workspace.file);
  recordEdit(SESSION, workspace.root, 1_000_000, home);
  marker(workspace.root, 2_000_000, 0);
  const text = (await stopAnswers(SESSION, { waitS: 0, home })).lines.join("\n");
  assert.match(text, /is still running after \d+ s\.$/);
  assert.doesNotMatch(text, CLAUSE);
});
