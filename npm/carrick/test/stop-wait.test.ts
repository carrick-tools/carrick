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
import { readSession, recordEdit, sessionsDir, writeAtomic } from "../src/hook/reuse.ts";
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
    /^Carrick: the re-check of user-service\/src\/routes\/users\.ts is still running after \d+ s\.$/,
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

test("stop never carries the check clause: rule 5 and the post-edit line say it (carrick#3437)", async (t) => {
  const { workspace, home } = setup(t);
  const entry = queue(workspace.root, home, workspace.file);
  recordEdit(SESSION, workspace.root, 1_000_000, home);
  // No check ran after the edit: the state in which the clause used to print.
  stopped(home, entry);
  const stoppedText = (await stopAnswers(SESSION, { waitS: 0, home })).lines.join("\n");
  assert.match(stoppedText, /was not re-checked: the re-check ran past its budget\.$/);
  assert.doesNotMatch(stoppedText, CLAUSE);

  const nothing = queue(workspace.root, home, workspace.file);
  marker(workspace.root, 2_000_000, 2);
  stopped(home, nothing);
  assert.doesNotMatch((await stopAnswers(SESSION, { waitS: 0, home })).lines.join("\n"), CLAUSE, "exit 2 checked nothing");

  liveRunner(workspace.root);
  queue(workspace.root, home, workspace.file);
  recordEdit(SESSION, workspace.root, 3_000_000, home);
  const running = (await stopAnswers(SESSION, { waitS: 0, home })).lines.join("\n");
  assert.match(running, /is still running after \d+ s\.$/);
  assert.doesNotMatch(running, CLAUSE);
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

/** A route on the edited file that the compiler compared with its consumer and found compatible. */
function judgedRoute(path: string, line: number): NonNullable<CheckResult["items"]>[number] {
  return {
    kind: "route",
    method: "GET",
    path,
    line,
    col: 1,
    source: "fact",
    counterparts: [{ role: "consumer", service: "web", file: "src/api.ts", line: 4 }],
    verdict: { state: "resolved", result: "compatible", detail: "" },
  };
}

test("a Stop over covered files with no route or call prints nothing and records the change as stated", async (t) => {
  const { workspace, home } = setup(t);
  edit(workspace.root, home, workspace.file, Date.now() - 5000);
  answer(home, queue(workspace.root, home, workspace.file), reread(clean()));

  const first = await stopAnswers(SESSION, { waitS: 0, home, ensure: () => false });
  assert.deepEqual(first.lines, [], "no blank part either");
  assert.equal(checkedSinceLastEdit(SESSION, workspace.root, home), true, "the change is still settled");
  assert.ok(readSession(SESSION, home).stated[workspace.root]! >= readSession(SESSION, home).edited[workspace.root]!);

  edit(workspace.root, home, workspace.file, Date.now() - 1000);
  answer(home, queue(workspace.root, home, workspace.file), reread(clean()));
  assert.deepEqual((await stopAnswers(SESSION, { waitS: 0, home, ensure: () => false })).lines, []);
});

test("a Stop over 2 judged operations with no problem names both and says both sides agree", async (t) => {
  const { workspace, home } = setup(t);
  edit(workspace.root, home, workspace.file, Date.now() - 5000);
  const judged = reread(clean());
  judged.items = [judgedRoute("/api/users", 10), { ...judgedRoute("/orders", 40), kind: "call", method: "POST" }];
  answer(home, queue(workspace.root, home, workspace.file), judged);

  const first = await stopAnswers(SESSION, { waitS: 0, home, ensure: () => false });
  assert.equal(first.lines.length, 1);
  assert.equal(
    first.lines[0],
    "Carrick checked the 2 operations this session changed (GET /api/users at user-service/src/routes/users.ts:10; a call to POST /orders at user-service/src/routes/users.ts:40): both sides agree.",
  );
  assert.equal(changedQueued(workspace.root).length, 0);
});

test("a second Stop over the same operations prints nothing, a new operation prints again", async (t) => {
  const { workspace, home } = setup(t);
  edit(workspace.root, home, workspace.file, Date.now() - 5000);
  const judged = reread(clean());
  judged.items = [judgedRoute("/api/users", 10)];
  answer(home, queue(workspace.root, home, workspace.file), judged);
  assert.equal((await stopAnswers(SESSION, { waitS: 0, home, ensure: () => false })).lines.length, 1);

  // Edited again (the route moved down a line), answered with the same operation.
  edit(workspace.root, home, workspace.file, Date.now() - 1000);
  const moved = reread(clean());
  moved.items = [judgedRoute("/api/users", 12)];
  answer(home, queue(workspace.root, home, workspace.file), moved);
  assert.deepEqual((await stopAnswers(SESSION, { waitS: 0, home, ensure: () => false })).lines, []);

  // A new operation is a new set.
  edit(workspace.root, home, workspace.file, Date.now() - 200);
  const more = reread(clean());
  more.items = [judgedRoute("/api/users", 12), judgedRoute("/api/orders", 30)];
  answer(home, queue(workspace.root, home, workspace.file), more);
  const third = await stopAnswers(SESSION, { waitS: 0, home, ensure: () => false });
  assert.match(third.lines.join("\n"), /^Carrick checked the 2 operations this session changed/);
});

test("a Stop whose only gap is a file no service reads prints nothing and queues nothing", async (t) => {
  const { workspace, home } = setup(t);
  edit(workspace.root, home, workspace.file, Date.now() - 5000);
  const unread: CheckResult = {
    ...fixture("check-deferred.json"),
    recheck: { ran: "none", elapsed_ms: 0, reason: "no service reads this file" },
  };
  answer(home, queue(workspace.root, home, workspace.file), unread);

  const result = await stopAnswers(SESSION, { waitS: 0, home, ensure: () => false });
  assert.deepEqual(result.lines, []);
  assert.equal(changedQueued(workspace.root).length, 0);
});

test("a Stop with 1 problem prints the header and its line", async (t) => {
  const { workspace, home } = setup(t);
  edit(workspace.root, home, workspace.file, Date.now() - 5000);
  answer(home, queue(workspace.root, home, workspace.file), reread(problem()));

  const lines = (await stopAnswers(SESSION, { waitS: 0, home, ensure: () => false })).lines.join("\n").split("\n");
  assert.match(lines[0]!, CHECKED);
  assert.match(lines[0]!, /\d+ contract problems?\.$/);
  assert.match(lines[1]!, /^- \S*users\.ts:\d+:\d+ /);
});

test("an edit after the answer is not covered: Stop names the file and queues check --changed", async (t) => {
  const { workspace, home } = setup(t);
  edit(workspace.root, home, workspace.file, Date.now() - 5000);
  answer(home, queue(workspace.root, home, workspace.file), reread(clean()));
  edit(workspace.root, home, workspace.file, Date.now() + 5000);

  const text = (await stopAnswers(SESSION, { waitS: 0, home, ensure: () => false })).lines.join("\n");
  assert.doesNotMatch(text, CHECKED);
  assert.match(text, /^Carrick could not finish checking this session's change in time: user-service\/src\/routes\/users\.ts: it was edited after its last re-check\. Run `carrick check --changed`/m);
  assert.equal(changedQueued(workspace.root).length, 1);
});

/** A fake clock: `sleep` moves it, so a test reads how long Stop waited. */
function fakeClock() {
  let now = Date.now();
  return { now: () => now, sleep: async (ms: number) => void (now += Math.max(ms, 1)), at: () => now };
}

test("a Stop with a pending re-check waits for check --changed and prints its problem", async (t) => {
  const { workspace, home } = setup(t);
  edit(workspace.root, home, workspace.file, Date.now() - 5000);
  answer(home, queue(workspace.root, home, workspace.file), reread(clean()));
  edit(workspace.root, home, workspace.file, Date.now() + 5000);
  const clock = fakeClock();
  const started = clock.at();
  let slept = 0;
  const result = await stopAnswers(SESSION, {
    waitS: 20,
    home,
    now: clock.now,
    ensure: () => false,
    sleep: async (ms) => {
      await clock.sleep(ms);
      slept += 1;
      // The runner finishes the queued whole-change check on the third poll.
      if (slept === 3) {
        const changed = parseChangedResult(fs.readFileSync(fixturePath("check-changed-problem.json"), "utf8"), 1)!;
        const [entry] = changedQueued(workspace.root);
        writeAtomic(
          path.join(sessionsDir(home), `${SESSION}.recheck.${entry!.seq}.answer.json`),
          `${JSON.stringify({ ...entry, state: "answered", started_at: entry!.enqueued_at, finished_at: new Date().toISOString(), changed })}\n`,
        );
      }
    },
  });
  const text = result.lines.join("\n");
  assert.match(text, /^Carrick ran `carrick check --changed` for this session's change: 1 contract problem in 1 changed file\.$/m);
  assert.match(text, /^- order-service\/src\/client\.ts:14 call GET \/users\/:id: /m);
  assert.doesNotMatch(text, /could not finish|did not check/);
  assert.ok(clock.at() - started < 2000, `stopped waiting once it answered (${clock.at() - started} ms)`);
});

test("a Stop with a pending re-check whose whole-change check is clean prints nothing", async (t) => {
  const { workspace, home } = setup(t);
  edit(workspace.root, home, workspace.file, Date.now() - 5000);
  answer(home, queue(workspace.root, home, workspace.file), reread(clean()));
  edit(workspace.root, home, workspace.file, Date.now() + 5000);
  const clock = fakeClock();
  const result = await stopAnswers(SESSION, {
    waitS: 20,
    home,
    now: clock.now,
    ensure: () => false,
    sleep: async (ms) => {
      await clock.sleep(ms);
      const [entry] = changedQueued(workspace.root);
      answerChanged(home, entry!);
    },
  });
  assert.deepEqual(result.lines, []);
});

test("a Stop whose wait runs out names the unchecked files once", async (t) => {
  const { workspace, home } = setup(t);
  liveRunner(workspace.root);
  edit(workspace.root, home, workspace.file, Date.now() - 5000);
  queue(workspace.root, home, workspace.file);
  const clock = fakeClock();
  const first = await stopAnswers(SESSION, { waitS: 2, home, now: clock.now, sleep: clock.sleep, ensure: () => false });
  assert.equal(first.lines.length, 1);
  assert.match(
    first.lines[0]!,
    /^Carrick could not finish checking this session's change in time: user-service\/src\/routes\/users\.ts: its re-check is still running after \d+ s\. Run `carrick check --changed` to see the answer\.$/,
  );
  assert.equal(changedQueued(workspace.root).length, 1);
  const again = fakeClock();
  const second = await stopAnswers(SESSION, { waitS: 2, home, now: again.now, sleep: again.sleep, ensure: () => false });
  assert.deepEqual(second.lines, [], "once");
});

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
    assert.equal(changedQueued(workspace.root).length, 1);
    assert.ok(ensured.includes(workspace.root), "a runner is made sure of for the queued check");
    const waited = clock.at() - started;
    assert.ok(waited <= 2000 + 250, `Stop waited only for the per-file entries (${waited} ms)`);

    // The queued check is never waited for, and the change is not stated twice.
    const again = fakeClock();
    const from = again.at();
    const second = await stopAnswers(SESSION, { waitS: 2, home, now: again.now, sleep: again.sleep, ensure: () => false });
    assert.equal(again.at() - from, 0, "no wait for the changed entry");
    assert.doesNotMatch(second.lines.join("\n"), NOT_CHECKED);
    assert.equal(changedQueued(workspace.root).length, 1, "one changed entry, not one per Stop");
  });
}

/** The background check --changed answers, as the runner writes it. */
function answerChanged(home: string, entry: QueueEntry): void {
  const changed = parseChangedResult(fs.readFileSync(fixturePath("check-changed-clean.json"), "utf8"), 0)!;
  writeAtomic(
    path.join(sessionsDir(home), `${SESSION}.recheck.${entry.seq}.answer.json`),
    `${JSON.stringify({ ...entry, state: "answered", started_at: entry.enqueued_at, finished_at: new Date().toISOString(), changed })}\n`,
  );
}

test("a reported gap is not repeated once the whole-change check answered it, until the file changes again (carrick#3543)", async (t) => {
  const { workspace, home } = setup(t);
  edit(workspace.root, home, workspace.file, Date.now() - 5000);
  answer(home, queue(workspace.root, home, workspace.file), fixture("check-deferred.json"));

  const first = (await stopAnswers(SESSION, { waitS: 0, home, ensure: () => false })).lines.join("\n");
  assert.match(first, NOT_CHECKED);
  const [queued] = changedQueued(workspace.root);
  assert.ok(queued);
  answerChanged(home, queued!);

  // The agent keeps editing another file: the answered gap is not named again.
  const other = path.join(workspace.service, "src", "routes", "orders.ts");
  fs.writeFileSync(other, "export {};\n");
  edit(workspace.root, home, other, Date.now() + 1000);
  answerInHand(workspace.root, SESSION, other, reread(clean()), Date.now() + 2000, { home });
  const second = (await stopAnswers(SESSION, { waitS: 0, home, ensure: () => false })).lines.join("\n");
  assert.doesNotMatch(second, NOT_CHECKED);
  assert.doesNotMatch(second, /users\.ts/);

  // Nothing new: nothing said.
  const third = await stopAnswers(SESSION, { waitS: 0, home, ensure: () => false });
  assert.doesNotMatch(third.lines.join("\n"), NOT_CHECKED);

  // The file changes again: it is named again.
  edit(workspace.root, home, workspace.file, Date.now() + 60_000);
  const fourth = (await stopAnswers(SESSION, { waitS: 0, home, ensure: () => false })).lines.join("\n");
  assert.match(fourth, /^Carrick could not finish checking this session's change in time: user-service\/src\/routes\/users\.ts: /m);
});

/** A linked git worktree under the workspace (its `.git` is a file) holding one edited file. */
function linkedWorktree(root: string, name: string): { top: string; file: string } {
  const top = path.join(root, ".claude", "worktrees", name);
  const file = path.join(top, "user-service", "src", "a.ts");
  fs.mkdirSync(path.dirname(file), { recursive: true });
  fs.writeFileSync(file, "export {};\n");
  fs.writeFileSync(path.join(top, ".git"), "gitdir: /elsewhere/.git/worktrees/x\n");
  return { top, file };
}

test("a file edited in a worktree that was removed is not named at Stop (carrick#3595)", async (t) => {
  const { workspace, home } = setup(t);
  const { top, file } = linkedWorktree(workspace.root, "gone");
  edit(workspace.root, home, file, Date.now() - 5000);
  fs.rmSync(top, { recursive: true, force: true });

  const result = await stopAnswers(SESSION, { waitS: 0, home, ensure: () => false });
  assert.doesNotMatch(result.lines.join("\n"), /worktrees\/gone|a\.ts/, "the removed worktree's file is not named");
  assert.equal(changedQueued(workspace.root).length, 0, "no whole-change check is queued for it");
});

test("a file edited in a worktree that still exists is still named (carrick#3595)", async (t) => {
  const { workspace, home } = setup(t);
  const { file } = linkedWorktree(workspace.root, "live");
  edit(workspace.root, home, file, Date.now() - 5000);

  const text = (await stopAnswers(SESSION, { waitS: 0, home, ensure: () => false })).lines.join("\n");
  assert.match(text, /worktrees\/live\/user-service\/src\/a\.ts: no re-check answered it/);
});

test("a file deleted in the main checkout is still named: only a gone worktree drops an entry (carrick#3595)", async (t) => {
  const { workspace, home } = setup(t);
  fs.mkdirSync(path.join(workspace.root, ".git"));
  edit(workspace.root, home, workspace.file, Date.now() - 5000);
  fs.rmSync(workspace.file);

  const text = (await stopAnswers(SESSION, { waitS: 0, home, ensure: () => false })).lines.join("\n");
  assert.match(text, /user-service\/src\/routes\/users\.ts: no re-check answered it/);
});

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
