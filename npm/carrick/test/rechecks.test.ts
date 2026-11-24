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
  askJob,
  CHANGED_BUDGET_MS,
  claimDelivery,
  enqueue,
  enqueueChanged,
  ensureRunner,
  finishedFor,
  lockPath,
  pendingFor,
  queueDir,
  requestPath,
  runQueue,
  takenDir,
  worktreeRereadLock,
} from "../src/hook/rechecks.ts";
import { checkedSinceLastEdit } from "../src/hook/checked.ts";
import { lateContext, owedWholeChange } from "../src/hook/late.ts";
import { recordEdit } from "../src/hook/reuse.ts";
import { sessionCoverage } from "../src/hook/stop-wait.ts";
import { readTimings } from "../src/hook/timings.ts";
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
    const cwds: Array<string | undefined> = [];
    const startCwd = (_command: string, args: string[], _env: NodeJS.ProcessEnv, cwd?: string) => void (started.push(args), cwds.push(cwd));
    assert.equal(ensureRunner(s.root, { start: startCwd }), true);
    assert.deepEqual(cwds, [s.root], "the runner starts in the checkout it works on, not the hook's folder (#5038)");
    assert.equal(started.length, 1);
    assert.equal(JSON.parse(fs.readFileSync(lockPath(s.root), "utf8")).pid, process.pid);
    // The holder (this process) is alive and fresh: no second runner.
    assert.equal(ensureRunner(s.root, { start }), false);
    assert.equal(started.length, 1);
  } finally {
    s.cleanup();
  }
});

function argvs(log: string): string[][] {
  return fs
    .readFileSync(log, "utf8")
    .split("\n")
    .filter(Boolean)
    .map((line) => (JSON.parse(line) as { argv: string[] }).argv);
}

test("the refresh and the hosted move are run by the runner under its lock, after the queued re-checks", async () => {
  const s = scratch();
  try {
    const now = Date.now();
    enqueue(s.root, "s1", [s.file("a.ts")], now, { home: s.home });
    const started: string[][] = [];
    // The hook's side: the requests, and one runner started under the hook's claim.
    const start = (_command: string, args: string[]) => void started.push(args);
    assert.equal(askJob(s.root, "forward", { now, start }), true);
    assert.equal(askJob(s.root, "refresh", { now, start }), false);
    assert.equal(started.length, 1);
    assert.ok(fs.existsSync(requestPath(s.root, "forward")));
    assert.ok(fs.existsSync(requestPath(s.root, "refresh")));
    let rebuildsBeforeRelease = 0;
    await runQueue(s.root, {
      env: env(s),
      home: s.home,
      spawner: process.pid,
      afterRelease: () => {
        // Both rebuilds ran before the lock went.
        rebuildsBeforeRelease = argvs(s.log).filter((argv) => argv[0] === "refresh").length;
      },
    });
    const run = argvs(s.log);
    assert.equal(run[0]![1], "a.ts");
    assert.deepEqual(run.slice(1), [
      ["refresh", "--workspace", s.root],
      ["refresh", "--hosted-only", "--workspace", s.root],
    ]);
    assert.equal(rebuildsBeforeRelease, 2);
    assert.equal(fs.existsSync(requestPath(s.root, "forward")), false);
    assert.equal(fs.existsSync(requestPath(s.root, "refresh")), false);
    assert.equal(fs.existsSync(lockPath(s.root)), false);
  } finally {
    s.cleanup();
  }
});

test("a full re-check that met a read model being rebuilt keeps the edit queued, unanswered", async () => {
  const s = scratch();
  try {
    const now = Date.now();
    const [entry] = enqueue(s.root, "s1", [s.file("a.ts")], now, { home: s.home });
    // The binary's answer when `.carrick/repos` is not the model `index.json`
    // names: nothing judged, `rebuilding`.
    await runQueue(s.root, {
      env: env(s, { CARRICK_FAKE_FIXTURE: fixturePath("check-rebuilding.json") }),
      home: s.home,
    });
    assert.deepEqual(calls(s.log), ["a.ts"]);
    assert.ok(fs.existsSync(path.join(queueDir(s.root), `${entry!.seq}.json`)));
    assert.deepEqual(finishedFor("s1", s.home), []);
    assert.equal(fs.existsSync(lockPath(s.root)), false);
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
  const ranOut = { ...base, recheck: { ran: "none" as const, elapsed_ms: 180_000, deadline: true, reason: "ran past its budget" } };
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

test("a background re-check writes a timing row that says whether the engine was ready (carrick#3948)", async () => {
  const s = scratch();
  try {
    const base = JSON.parse(fs.readFileSync(fixturePath("check-mismatch.json"), "utf8"));
    const fixture = (engineReady: boolean) => {
      const file = path.join(path.dirname(s.log), `recheck-${engineReady}.json`);
      const recheck = { ran: "extraction", elapsed_ms: 5, engine_ready: engineReady };
      fs.writeFileSync(file, JSON.stringify({ ...base, recheck }));
      return file;
    };
    fs.mkdirSync(path.join(s.root, ".carrick"), { recursive: true });
    for (const [name, ready] of [["a.ts", false], ["b.ts", true]] as const) {
      enqueue(s.root, "s1", [s.file(name)], Date.now(), { home: s.home });
      await runQueue(s.root, { env: env(s, { CARRICK_FAKE_FIXTURE: fixture(ready) }), home: s.home });
    }
    const rows = readTimings(s.root).filter((row) => row.hook === "recheck");
    assert.deepEqual(
      rows.map((row) => [row.file, row.engine_ready]),
      [["a.ts", false], ["b.ts", true]],
    );
    assert.ok(rows.every((row) => row.waited_ms >= 0 && row.duration_ms >= 0));
  } finally {
    s.cleanup();
  }
});

// A linked worktree's edits are the compiler engine's: the late re-check never re-reads the service for them,
// Stop states nothing from them and no whole-change check is queued for them; the main checkout is unchanged (#5038 T4).

/** `scratch` with the workspace a git repo and a nested linked worktree `.claude/worktrees/wt` of it. */
function scratchWithWorktree() {
  const s = scratch();
  const g = (cwd: string, ...args: string[]) => spawnSync("git", ["-c", "user.name=t", "-c", "user.email=t@t", ...args], { cwd, stdio: "ignore" });
  fs.writeFileSync(s.file("a.ts"), "export const a = 1;\n");
  fs.writeFileSync(s.file(".gitignore"), ".claude/worktrees/\n.carrick/\n");
  g(s.root, "init", "-q", "-b", "main");
  g(s.root, "add", "-A");
  g(s.root, "commit", "-q", "-m", "init");
  g(s.root, "worktree", "add", "-q", ".claude/worktrees/wt", "-b", "wt");
  return { ...s, inWorktree: path.join(s.root, ".claude", "worktrees", "wt", "a.ts") };
}

test("T4: with the machine's re-read slot free, a worktree file's re-check re-reads (no blanket skip) and frees the slot", async () => {
  const s = scratchWithWorktree();
  try {
    enqueue(s.root, "s1", [s.inWorktree], Date.now(), { home: s.home });
    await runQueue(s.root, { env: env(s), home: s.home });
    const argv = argvs(s.log).find((a) => a[1] === ".claude/worktrees/wt/a.ts");
    assert.ok(argv?.includes("--recheck") && !argv.includes("--no-rescan"), JSON.stringify(argv));
    assert.equal(fs.existsSync(worktreeRereadLock(s.home)), false, "the slot is free again");
  } finally {
    s.cleanup();
  }
});

test("T4: while another worktree re-read holds the machine's slot, a worktree file is answered with --no-rescan; a main file re-reads as before", async () => {
  const s = scratchWithWorktree();
  // A live process (the test's parent) holds the slot, freshly touched.
  const holder = { pid: process.ppid, started: new Date().toISOString(), touched: new Date().toISOString() };
  try {
    fs.writeFileSync(worktreeRereadLock(s.home), JSON.stringify(holder));
    const now = Date.now();
    enqueue(s.root, "s1", [s.inWorktree], now, { home: s.home });
    enqueue(s.root, "s2", [s.file("a.ts")], now, { home: s.home });
    await runQueue(s.root, { env: env(s), home: s.home });
    const byFile = new Map(argvs(s.log).map((argv) => [argv[1]!, argv]));
    assert.ok(byFile.get(".claude/worktrees/wt/a.ts")?.includes("--no-rescan"), JSON.stringify([...byFile]));
    assert.ok(byFile.get("a.ts")?.includes("--recheck") && !byFile.get("a.ts")?.includes("--no-rescan"), "main: unchanged");
    assert.equal(JSON.parse(fs.readFileSync(worktreeRereadLock(s.home), "utf8")).pid, process.ppid, "the holder's lock is left alone");
  } finally {
    s.cleanup();
  }
});

test("T4: Stop leaves out a worktree file whose re-read was skipped for a miss, and queues nothing for it; one with no miss is a gap as before", async () => {
  const s = scratchWithWorktree();
  try {
    const now = Date.now();
    recordEdit("s1", s.root, now - 10, s.home, [s.inWorktree]);
    enqueue(s.root, "s1", [s.inWorktree], now, { home: s.home });
    await runQueue(s.root, { env: env(s, { CARRICK_FAKE_FIXTURE: fixturePath("check-known-miss.json") }), home: s.home });
    const skipped = sessionCoverage("s1", s.root, s.home, now);
    assert.equal(skipped.files, 0);
    assert.deepEqual(skipped.gaps, []);
    recordEdit("s2", s.root, now, s.home, [s.inWorktree]);
    assert.deepEqual(sessionCoverage("s2", s.root, s.home, now).gaps.map((g) => g.file), [".claude/worktrees/wt/a.ts"], "no miss, no answer yet: a gap as before");
  } finally {
    s.cleanup();
  }
});

test("T4: a held-back break in a worktree file whose re-read was skipped for a miss queues no check --changed; with no miss, or in main, it is queued as before", async () => {
  const s = scratchWithWorktree();
  try {
    const now = Date.now();
    enqueue(s.root, "s1", [s.inWorktree], now - 20, { home: s.home });
    await runQueue(s.root, { env: env(s, { CARRICK_FAKE_FIXTURE: fixturePath("check-known-miss.json") }), home: s.home });
    const changed = (session: string) => pendingFor(session, s.home).filter((e) => e.kind === "changed").length;
    owedWholeChange("s1", s.root, s.inWorktree, now, s.home, () => true);
    assert.equal(changed("s1"), 0, "a recorded miss: nothing queued");
    owedWholeChange("s2", s.root, s.inWorktree, now, s.home, () => true);
    assert.equal(changed("s2"), 1, "no miss for this session's file: queued as before");
    owedWholeChange("s3", s.root, s.file("a.ts"), now, s.home, () => true);
    assert.equal(changed("s3"), 1, "main: queued as before");
  } finally {
    s.cleanup();
  }
});
