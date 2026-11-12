// The background re-check queue and its runner (carrick#3198, B3-N1).
//
// Each test drives `runQueue` against the fake `carrick`, with a throwaway
// workspace and a throwaway home for the session files.

import assert from "node:assert/strict";
import test from "node:test";
import fs from "node:fs";
import os from "node:os";
import path from "node:path";
import { spawnSync } from "node:child_process";
import {
  answerInHand,
  CHANGED_BUDGET_MS,
  claimDelivery,
  enqueue,
  enqueueChanged,
  ensureRunner,
  finishedFor,
  lockPath,
  pendingFor,
  queueDir,
  runQueue,
  takenDir,
} from "../src/hook/rechecks.ts";
import { checkedSinceLastEdit } from "../src/hook/checked.ts";
import { lateContext } from "../src/hook/late.ts";
import { recordEdit } from "../src/hook/reuse.ts";
import { sessionCoverage } from "../src/hook/stop-wait.ts";
import { parseChangedResult, parseCheckResult } from "../src/contract.ts";
import { fakeBin, fakeEnv, fixturePath } from "./helpers.ts";

function scratch() {
  const dir = fs.mkdtempSync(path.join(os.tmpdir(), "carrick-rechecks-"));
  const root = path.join(dir, "ws");
  const home = path.join(dir, "home");
  fs.mkdirSync(root, { recursive: true });
  return {
    root,
    home,
    log: path.join(dir, "argv.log"),
    envLog: path.join(dir, "env.log"),
    file: (name: string) => path.join(root, name),
    cleanup: () => fs.rmSync(dir, { recursive: true, force: true }),
  };
}

function calls(log: string): string[] {
  try {
    return fs
      .readFileSync(log, "utf8")
      .split("\n")
      .filter(Boolean)
      .map((line) => (JSON.parse(line) as { argv: string[] }).argv[1]!);
  } catch {
    return [];
  }
}

function env(s: ReturnType<typeof scratch>, extra: Record<string, string> = {}) {
  return fakeEnv({ CARRICK_FAKE_ARGV_LOG: s.log, CARRICK_FAKE_ENV_LOG: s.envLog, ...extra });
}

function deadPid(): number {
  const child = spawnSync(process.execPath, ["-e", ""]);
  return child.pid ?? 999_999;
}

test("a lock naming a dead pid is replaced and one runner starts", () => {
  const s = scratch();
  try {
    fs.mkdirSync(path.dirname(lockPath(s.root)), { recursive: true });
    const stamp = new Date().toISOString();
    fs.writeFileSync(lockPath(s.root), JSON.stringify({ pid: deadPid(), started: stamp, touched: stamp }));
    const started: string[][] = [];
    const start = (_command: string, args: string[]) => void started.push(args);
    assert.equal(ensureRunner(s.root, { start }), true);
    assert.equal(started.length, 1);
    assert.equal(JSON.parse(fs.readFileSync(lockPath(s.root), "utf8")).pid, process.pid);
    // The holder (this process) is alive and fresh: no second runner.
    assert.equal(ensureRunner(s.root, { start }), false);
    assert.equal(started.length, 1);
  } finally {
    s.cleanup();
  }
});

test("two entries for one file in one session make one check; the older reads superseded", async () => {
  const s = scratch();
  try {
    const now = Date.now();
    const [older] = enqueue(s.root, "s1", [s.file("a.ts")], now, { home: s.home });
    const [newer] = enqueue(s.root, "s1", [s.file("a.ts")], now + 1, { home: s.home });
    await runQueue(s.root, { env: env(s), home: s.home });
    assert.deepEqual(calls(s.log), ["a.ts"]);
    const answer = (seq: string) =>
      JSON.parse(fs.readFileSync(path.join(s.home, ".carrick", "sessions", `s1.recheck.${seq}.answer.json`), "utf8"));
    assert.equal(answer(older!.seq).state, "superseded");
    assert.equal(answer(newer!.seq).state, "answered");
    assert.deepEqual(finishedFor("s1", s.home).map((a) => a.seq), [newer!.seq]);
  } finally {
    s.cleanup();
  }
});

test("answers land in queue order and the lock is gone once the queue is empty", async () => {
  const s = scratch();
  try {
    const now = Date.now();
    enqueue(s.root, "s1", [s.file("b.ts")], now, { home: s.home });
    enqueue(s.root, "s1", [s.file("a.ts")], now + 1, { home: s.home });
    enqueue(s.root, "s2", [s.file("c.ts")], now + 2, { home: s.home });
    assert.equal(pendingFor("s1", s.home).length, 2);
    await runQueue(s.root, { env: env(s), home: s.home });
    assert.deepEqual(calls(s.log), ["b.ts", "a.ts", "c.ts"]);
    assert.equal(fs.existsSync(lockPath(s.root)), false);
    assert.equal(pendingFor("s1", s.home).length, 0);
    const done = finishedFor("s1", s.home);
    assert.deepEqual(done.map((a) => a.relative), ["b.ts", "a.ts"]);
    assert.ok(done[0]!.started_at && done[0]!.finished_at && done[0]!.enqueued_at);
  } finally {
    s.cleanup();
  }
});

test("an entry whose claim fails for good is answered once with a failure and the runner ends", async () => {
  const s = scratch();
  try {
    const [entry] = enqueue(s.root, "s1", [s.file("a.ts")], Date.now(), { home: s.home });
    enqueue(s.root, "s1", [s.file("b.ts")], Date.now() + 1, { home: s.home });
    // A file where the runner's taken folder must go: every claim fails and the queue file stays.
    fs.writeFileSync(path.dirname(takenDir(s.root, process.pid)), "");
    let reads = 0;
    const now = () => {
      // A runner that spins reads the clock without end; fail instead of hanging.
      if (++reads > 2000) throw new Error("runner is spinning on an entry it cannot claim");
      return Date.now();
    };
    await runQueue(s.root, { env: env(s), home: s.home, now });
    assert.deepEqual(calls(s.log), []);
    const answers = finishedFor("s1", s.home);
    assert.equal(answers.length, 2);
    assert.ok(answers.every((a) => typeof a.failure === "string" && a.failure.length > 0));
    assert.equal(answers.filter((a) => a.seq === entry!.seq).length, 1);
    assert.equal(fs.existsSync(lockPath(s.root)), false);
  } finally {
    s.cleanup();
  }
});

test("an entry queued after the runner released its lock is still run", async () => {
  const s = scratch();
  try {
    enqueue(s.root, "s1", [s.file("a.ts")], Date.now(), { home: s.home });
    let late = false;
    await runQueue(s.root, {
      env: env(s),
      home: s.home,
      afterRelease: () => {
        if (late) return;
        late = true;
        enqueue(s.root, "s1", [s.file("late.ts")], Date.now(), { home: s.home });
      },
    });
    assert.deepEqual(calls(s.log), ["a.ts", "late.ts"]);
    assert.equal(fs.existsSync(lockPath(s.root)), false);
  } finally {
    s.cleanup();
  }
});

test("a dead runner's taken entry is re-queued once, then answered stopped; old entries are not run", async () => {
  const s = scratch();
  try {
    const small = env(s, { CARRICK_RECHECK_BACKGROUND_MS: "1000" });
    const now = Date.now();
    const dead = deadPid();
    const strand = (entry: { seq: string }, attempts: number) => {
      const from = path.join(queueDir(s.root), `${entry.seq}.json`);
      const body = JSON.parse(fs.readFileSync(from, "utf8"));
      fs.mkdirSync(takenDir(s.root, dead), { recursive: true });
      fs.writeFileSync(path.join(takenDir(s.root, dead), `${entry.seq}.json`), JSON.stringify({ ...body, attempts }));
      fs.unlinkSync(from);
    };
    const [first] = enqueue(s.root, "s1", [s.file("first.ts")], now, { home: s.home });
    const [second] = enqueue(s.root, "s1", [s.file("second.ts")], now + 1, { home: s.home });
    strand(first!, 0);
    strand(second!, 1);
    // 2 x 1000 + 60 000 ms is the expiry: this one is older.
    const [old] = enqueue(s.root, "s1", [s.file("old.ts")], now - 100_000, { home: s.home });
    await runQueue(s.root, { env: small, home: s.home });
    assert.deepEqual(calls(s.log), ["first.ts"]);
    const states = Object.fromEntries(
      finishedFor("s1", s.home).map((a) => [a.relative, a.state]),
    );
    assert.deepEqual(states, { "first.ts": "answered", "second.ts": "stopped", "old.ts": "stopped" });
    assert.ok(old);
  } finally {
    s.cleanup();
  }
});

test("claimDelivery is true once across concurrent claimers", async () => {
  const s = scratch();
  try {
    const [entry] = enqueue(s.root, "s1", [s.file("a.ts")], Date.now(), { home: s.home });
    const claims = await Promise.all(
      [1, 2, 3, 4].map(async () => claimDelivery("s1", entry!.seq, s.home)),
    );
    assert.equal(claims.filter(Boolean).length, 1);
    assert.equal(claimDelivery("s1", entry!.seq, s.home), false);
  } finally {
    s.cleanup();
  }
});

test("each call carries the background budget and a timeout past it", async () => {
  const s = scratch();
  try {
    enqueue(s.root, "s1", [s.file("a.ts")], Date.now(), { home: s.home });
    await runQueue(s.root, { env: env(s), home: s.home });
    const [line] = fs.readFileSync(s.envLog, "utf8").split("\n").filter(Boolean);
    assert.deepEqual(JSON.parse(line!), { budget: "180000", timeout: "195000" });
    assert.ok(fs.existsSync(fakeBin));
  } finally {
    s.cleanup();
  }
});

// The whole-change check a Stop queues when it cannot state the session's
// check from the per-file answers (carrick#3466).

test("a changed entry runs after every per-file entry, one per session and root, the newer superseding the older; the next carrier delivers it and its marker drops the clause", async () => {
  const s = scratch();
  try {
    const now = Date.now();
    recordEdit("s1", s.root, now - 1000, s.home, [s.file("a.ts")]);
    const older = enqueueChanged(s.root, "s1", now, { home: s.home })!;
    const newer = enqueueChanged(s.root, "s1", now + 1, { home: s.home })!;
    enqueue(s.root, "s1", [s.file("a.ts")], now + 2, { home: s.home });
    enqueue(s.root, "s2", [s.file("b.ts")], now + 3, { home: s.home });
    await runQueue(s.root, { env: env(s), home: s.home });
    assert.deepEqual(calls(s.log), ["a.ts", "b.ts", "--changed"], "the changed check runs last, once");
    // Its own budget, above the per-file cap: a whole-change check takes longer.
    const envs = fs.readFileSync(s.envLog, "utf8").trim().split("\n").map((line) => JSON.parse(line) as { budget: string | null; timeout: string | null });
    assert.equal(envs[2]!.budget, null, "no per-file re-check budget on the changed call");
    assert.equal(envs[2]!.timeout, String(CHANGED_BUDGET_MS + 15_000));

    const answers = finishedFor("s1", s.home);
    assert.equal(answers.some((a) => a.seq === older.seq), false, "the older one is superseded, silently");
    const changed = answers.find((a) => a.seq === newer.seq)!;
    assert.equal(changed.kind, "changed");
    assert.equal(changed.changed?.exit, 0);
    const line = lateContext(changed, false);
    assert.equal(line, null, "a finished check --changed with exit 0 delivers nothing");
    // Exit 2 (could not check) and a run that gave no answer are silent too; exit 1 speaks.
    const exit = (code: number) => ({ ...changed, changed: { ...changed.changed!, exit: code, not_checked: "no usable type" } });
    assert.equal(lateContext(exit(2), false), null);
    assert.equal(lateContext({ ...changed, changed: undefined, state: "stopped", failure: "it stopped" }, false), null);
    assert.equal(lateContext({ ...changed, changed: undefined, state: "answered" }, false), null);
    const problem = parseChangedResult(fs.readFileSync(fixturePath("check-changed-problem.json"), "utf8"), 1)!;
    assert.match(
      lateContext({ ...changed, changed: problem }, false) ?? "",
      /^Carrick ran `carrick check --changed` for this session's change: 1 contract problem in 1 changed file\.\n- /,
    );
    // The binary's marker (exit 0, begun after the edit) settles later lines.
    assert.equal(checkedSinceLastEdit("s1", s.root, s.home), true);
  } finally {
    s.cleanup();
  }
});

// R46.4: an answer queued in place of the probe skips the `answersTheEdit`
// gate only where the probe would have printed it in hand. A re-check that
// ran out of time, or a re-scan that started and failed, is one the probe
// would have deferred: gated as an ordinary late answer is.
test("an answer queued in place of the probe is gated as before when the probe would have deferred it", () => {
  const base = parseCheckResult(fs.readFileSync(fixturePath("check-stale-uses.json"), "utf8"))!;
  const scope = { reread: ["inventory-svc"], not_reread: [], stale: [] };
  const answer = (result: typeof base, flagged: boolean) => ({
    seq: "1",
    session: "s",
    file: "/w/src/client.ts",
    relative: "src/client.ts",
    root: "/w",
    state: "answered" as const,
    enqueued_at: new Date().toISOString(),
    started_at: null,
    finished_at: new Date().toISOString(),
    ...(flagged ? { instead_of_probe: true as const } : {}),
    result,
  });
  // The probe would have printed this one in hand (a known miss): its uses line arrives late.
  assert.match(lateContext(answer(base, true), true) ?? "", /Other files use what this file declares/);
  const ranOut = { ...base, recheck: { ran: "none" as const, elapsed_ms: 180_000, deadline: true, scope, reason: "ran past its budget" } };
  const failed = { ...base, recheck: { ran: "none" as const, elapsed_ms: 900, scope, reason: "the re-scan failed" } };
  for (const result of [ranOut, failed]) {
    assert.equal(lateContext(answer(result, false), true), null, "unflagged: gated");
    assert.equal(lateContext(answer(result, true), true), null, "flagged: gated as before");
  }
});

test("a file answered in hand counts as covered", () => {
  const s = scratch();
  try {
    const now = Date.now();
    recordEdit("s1", s.root, now, s.home, [s.file("a.ts")]);
    const result = parseCheckResult(fs.readFileSync(fixturePath("check-clean.json"), "utf8"))!;
    answerInHand(s.root, "s1", s.file("a.ts"), result, now, { home: s.home });
    assert.deepEqual(pendingFor("s1", s.home), [], "nothing to wait for");
    assert.deepEqual(finishedFor("s1", s.home), [], "nothing owed: the post-edit hook printed it");
    const coverage = sessionCoverage("s1", s.root, s.home, now);
    assert.equal(coverage.files, 1);
    assert.deepEqual(coverage.gaps, []);
    assert.equal(coverage.covered.length, 1);
  } finally {
    s.cleanup();
  }
});

function sessionFile(home: string, session: string, seq: string, kind: string): string {
  return path.join(home, ".carrick", "sessions", `${session}.recheck.${seq}.${kind}`);
}


/** An entry with a pending file, an answer and a delivered marker, written by hand. */
function plantDelivered(s: ReturnType<typeof scratch>, session: string, seq: string, file: string): void {
  const entry = { seq, session, file, relative: path.basename(file), root: s.root, enqueued_at: new Date(0).toISOString(), attempts: 0 };
  fs.mkdirSync(path.join(s.home, ".carrick", "sessions"), { recursive: true });
  fs.writeFileSync(sessionFile(s.home, session, seq, "pending.json"), JSON.stringify(entry));
  fs.writeFileSync(sessionFile(s.home, session, seq, "answer.json"), JSON.stringify({ ...entry, state: "answered", result: undefined }));
  fs.writeFileSync(sessionFile(s.home, session, seq, "delivered"), "{}");
}

test("finishedFor marks answers no carrier will deliver", async () => {
  const s = scratch();
  try {
    const now = Date.now();
    for (let i = 0; i < 100; i++) plantDelivered(s, "s1", `000000000${String(i).padStart(4, "0")}-1-0001`, s.file(`other${i}.ts`));
    const [one] = enqueue(s.root, "s1", [s.file("a.ts")], now, { home: s.home });
    await runQueue(s.root, { env: env(s), home: s.home });
    const [two] = enqueue(s.root, "s1", [s.file("a.ts")], now + 1, { home: s.home });
    const [three] = enqueue(s.root, "s1", [s.file("a.ts")], now + 2, { home: s.home });
    await runQueue(s.root, { env: env(s), home: s.home });
    assert.deepEqual(finishedFor("s1", s.home).map((a) => a.seq), [three!.seq]);
    for (const e of [one!, two!]) {
      assert.ok(fs.existsSync(sessionFile(s.home, "s1", e.seq, "delivered")), `${e.seq} claimed`);
    }
    assert.equal(fs.existsSync(sessionFile(s.home, "s1", three!.seq, "delivered")), false, "the newest is still owed");
  } finally {
    s.cleanup();
  }
});

test("a session with nothing owed reads no file", async (t) => {
  const s = scratch();
  try {
    const now = Date.now();
    for (let i = 0; i < 100; i++) plantDelivered(s, "s1", `000000000${String(i).padStart(4, "0")}-1-0001`, s.file(`other${i}.ts`));
    const [one] = enqueue(s.root, "s1", [s.file("a.ts")], now, { home: s.home });
    await runQueue(s.root, { env: env(s), home: s.home });
    assert.equal(claimDelivery("s1", one!.seq, s.home), true);
    const read = t.mock.method(fs, "readFileSync");
    assert.deepEqual(finishedFor("s1", s.home), []);
    const sessionReads = read.mock.calls.filter((c) => /\.(pending|answer)\.json$/.test(String(c.arguments[0])));
    assert.equal(sessionReads.length, 0);
  } finally {
    s.cleanup();
  }
});

test("one new owed answer among 100 delivered entries reads only pending entries from its seq", async (t) => {
  const s = scratch();
  try {
    const now = Date.now();
    for (let i = 0; i < 100; i++) plantDelivered(s, "s1", `000000000${String(i).padStart(4, "0")}-1-0001`, s.file(`other${i}.ts`));
    const [one] = enqueue(s.root, "s1", [s.file("a.ts")], now, { home: s.home });
    await runQueue(s.root, { env: env(s), home: s.home });
    const read = t.mock.method(fs, "readFileSync");
    assert.deepEqual(finishedFor("s1", s.home).map((a) => a.seq), [one!.seq]);
    const pendingReads = read.mock.calls.map((c) => String(c.arguments[0])).filter((f) => f.endsWith(".pending.json"));
    assert.equal(pendingReads.length, 1, "only the owed entry's own pending file");
    assert.ok(pendingReads[0]!.includes(one!.seq));
  } finally {
    s.cleanup();
  }
});
