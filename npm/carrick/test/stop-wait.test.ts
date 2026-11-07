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

import { fakeEnv, fixture, fixturePath, makeWorkspace, pluginDir, type HookRun } from "./helpers.ts";
import { parseChangedResult, type CheckResult } from "../src/contract.ts";
import {
  answerInHand,
  BACKGROUND_CAP_MS,
  enqueue,
  enqueueChanged,
  lockPath,
  queueDir,
  STOP_TIMEOUT_S,
  STOP_WAIT_S,
} from "../src/hook/rechecks.ts";
import type { Answer, QueueEntry } from "../src/hook/rechecks.ts";
import { checkedSinceLastEdit, checkMarkerPath } from "../src/hook/checked.ts";
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

// The Stop hook states the session's contract check from the per-file
// re-checks (carrick#3466): one line when every edited file was answered after
// its last edit, else it names the file, queues one `check --changed` behind
// the re-checks and never waits for it.

/** A re-check that re-read the edited service and left no service stale. */
function reread(result: CheckResult, stale: string[] = []): CheckResult {
  return {
    ...result,
    recheck: { ran: "extraction+types", elapsed_ms: 1, scope: { reread: ["user-service"], not_reread: [], stale } },
  };
}

/** What the post-edit hook records before it probes: the file and its edit time. */
function edit(root: string, home: string, file: string, at: number): void {
  recordEdit(SESSION, root, at, home, [file]);
}

/** The `check --changed` entries waiting in the workspace's queue. */
function changedQueued(root: string): QueueEntry[] {
  const dir = queueDir(root);
  if (!fs.existsSync(dir)) return [];
  return fs
    .readdirSync(dir)
    .map((name) => JSON.parse(fs.readFileSync(path.join(dir, name), "utf8")) as QueueEntry)
    .filter((entry) => entry.kind === "changed");
}

const CHECKED = /^Carrick checked the (\d+) files? this session edited \(([^)]*)\): /;
const NOT_CHECKED = /^Carrick did not check the whole change: /;

test("every edited file answered after its last edit: one checked-the-change line, no check clause, the per-answer lines folded into it", async (t) => {
  const { workspace, home } = setup(t);
  const other = path.join(workspace.service, "src", "routes", "orders.ts");
  fs.writeFileSync(other, "export {};\n");
  const at = Date.now() - 5000;
  edit(workspace.root, home, workspace.file, at);
  edit(workspace.root, home, other, at);
  const entry = queue(workspace.root, home, workspace.file);
  answer(home, entry, reread(problem()));
  // Answered in hand by the post-edit probe: no re-check was queued.
  answerInHand(workspace.root, SESSION, other, reread(clean()), Date.now(), { home });

  const result = await stopAnswers(SESSION, { waitS: 0, home, ensure: () => false });
  const text = result.lines.join("\n");
  const head = result.lines[0]!.split("\n")[0]!;
  assert.match(head, CHECKED);
  assert.equal(CHECKED.exec(head)![1], "2");
  assert.equal(CHECKED.exec(head)![2], "user-service");
  assert.match(head, /\d+ contract problems?\.$/);
  assert.doesNotMatch(text, /^Carrick re-checked/m, "the per-answer headlines are folded into the one line");
  assert.match(text, /^- \S*users\.ts:\d+:\d+ /m, "the problem lines of the answer this Stop claimed still print");
  assert.doesNotMatch(text, CLAUSE);
  assert.doesNotMatch(text, NOT_CHECKED);
  assert.equal(changedQueued(workspace.root).length, 0, "nothing to fall back on");
  assert.equal(checkedSinceLastEdit(SESSION, workspace.root, home), true, "later lines drop the clause");
});

test("a covered clean change is stated once, and again after the next edit is answered", async (t) => {
  const { workspace, home } = setup(t);
  edit(workspace.root, home, workspace.file, Date.now() - 5000);
  answer(home, queue(workspace.root, home, workspace.file), reread(clean()));

  const first = await stopAnswers(SESSION, { waitS: 0, home, ensure: () => false });
  assert.equal(first.lines.length, 1);
  assert.match(first.lines[0]!, /^Carrick checked the 1 file this session edited \(user-service\): no route or call in it\.$/);
  const second = await stopAnswers(SESSION, { waitS: 0, home, ensure: () => false });
  assert.deepEqual(second.lines, [], "not again without an edit");

  edit(workspace.root, home, workspace.file, Date.now() - 1000);
  assert.equal(checkedSinceLastEdit(SESSION, workspace.root, home), false, "an edit unsettles it");
  answer(home, queue(workspace.root, home, workspace.file), reread(clean()));
  const third = await stopAnswers(SESSION, { waitS: 0, home, ensure: () => false });
  assert.match(third.lines.join("\n"), CHECKED);
});

test("an edit after the answer is not covered: Stop names the file and queues check --changed", async (t) => {
  const { workspace, home } = setup(t);
  edit(workspace.root, home, workspace.file, Date.now() - 5000);
  answer(home, queue(workspace.root, home, workspace.file), reread(clean()));
  edit(workspace.root, home, workspace.file, Date.now() + 5000);

  const text = (await stopAnswers(SESSION, { waitS: 0, home, ensure: () => false })).lines.join("\n");
  assert.doesNotMatch(text, CHECKED);
  assert.match(text, /^Carrick did not check the whole change: user-service\/src\/routes\/users\.ts: it was edited after its last re-check\./m);
  assert.equal(changedQueued(workspace.root).length, 1);
});

/** A fake clock: `sleep` moves it, so a test reads how long Stop waited. */
function fakeClock() {
  let now = Date.now();
  return { now: () => now, sleep: async (ms: number) => void (now += Math.max(ms, 1)), at: () => now };
}

for (const [name, arrange, reason] of [
  [
    "an answer with a stale service",
    (root: string, home: string, file: string) => answer(home, queue(root, home, file), reread(clean(), ["billing"])),
    /1 service that reads it was judged at its indexed version \(billing\)/,
  ],
  [
    "a deferred answer",
    (root: string, home: string, file: string) => answer(home, queue(root, home, file), fixture("check-deferred.json")),
    /this edit needs a re-scan, which this call was asked not to start/,
  ],
  [
    "a failed answer",
    (root: string, home: string, file: string) => stopped(home, queue(root, home, file)),
    /the re-check ran past its budget/,
  ],
  [
    "one still running past the wait",
    (root: string, home: string, file: string) => {
      liveRunner(root);
      queue(root, home, file);
    },
    /its re-check is still running after \d+ s/,
  ],
] as const) {
  test(`${name} makes Stop name the file, print no checked line, and queue one changed entry it does not wait for`, async (t) => {
    const { workspace, home } = setup(t);
    edit(workspace.root, home, workspace.file, Date.now() - 5000);
    arrange(workspace.root, home, workspace.file);
    const clock = fakeClock();
    const started = clock.at();
    const ensured: string[] = [];
    const result = await stopAnswers(SESSION, {
      waitS: 2,
      home,
      now: clock.now,
      sleep: clock.sleep,
      ensure: (root) => {
        ensured.push(root);
        return false;
      },
    });
    const text = result.lines.join("\n");
    assert.doesNotMatch(text, CHECKED);
    const gap = text.split("\n").find((line) => NOT_CHECKED.test(line));
    assert.ok(gap, text);
    assert.match(gap!, /user-service\/src\/routes\/users\.ts: /);
    assert.match(gap!, reason);
    assert.match(gap!, /A background `carrick check --changed` is queued; its answer comes with the next Carrick line\.$/);
    assert.doesNotMatch(text, CLAUSE);
    assert.doesNotMatch(text, /is still running after \d+ s\.$/m, "a running file is named once, on the gap line");
    assert.equal(changedQueued(workspace.root).length, 1);
    assert.ok(ensured.includes(workspace.root), "a runner is made sure of for the queued check");
    const waited = clock.at() - started;
    assert.ok(waited <= 2000 + 250, `Stop waited only for the per-file entries (${waited} ms)`);

    // The queued check is never waited for, and the change is not stated twice.
    const again = fakeClock();
    const from = again.at();
    const second = await stopAnswers(SESSION, { waitS: 2, home, now: again.now, sleep: again.sleep, ensure: () => false });
    if (name !== "one still running past the wait") assert.equal(again.at() - from, 0, "no wait for the changed entry");
    assert.doesNotMatch(second.lines.join("\n"), NOT_CHECKED);
    assert.equal(changedQueued(workspace.root).length, 1, "one changed entry, not one per Stop");
  });
}

test("stop delivers a finished check --changed answer: its verdict, never a clause", async (t) => {
  const { workspace, home } = setup(t);
  const entry = enqueueChanged(workspace.root, SESSION, Date.now(), { home })!;
  const changed = parseChangedResult(
    fs.readFileSync(fixturePath("check-changed-problem.json"), "utf8"),
    1,
  )!;
  writeAtomic(
    path.join(sessionsDir(home), `${SESSION}.recheck.${entry.seq}.answer.json`),
    `${JSON.stringify({ ...entry, state: "answered", started_at: entry.enqueued_at, finished_at: new Date().toISOString(), changed })}\n`,
  );
  const text = (await stopAnswers(SESSION, { waitS: 0, home, ensure: () => false })).lines.join("\n");
  assert.match(text, /^Carrick ran `carrick check --changed` for this session's change: 1 contract problem in 1 changed file\.$/m);
  assert.match(text, /^- order-service\/src\/client\.ts:14 call GET \/users\/:id: the response type does not match: email is missing/m);
  assert.doesNotMatch(text, CLAUSE);
});
