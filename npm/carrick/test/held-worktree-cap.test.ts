// A hard cap on live worktree slots, an off switch, and eviction and queueing when the cap or the budget is full (#5038, T3).

import test from "node:test";
import assert from "node:assert/strict";
import fs from "node:fs";
import os from "node:os";
import path from "node:path";
import { spawnSync } from "node:child_process";
import { HeldDaemon, type HeldAnswer, type HeldRequest, type ServiceEngine, type ServiceInit, type ServiceRuntime } from "../src/held/daemon.ts";
import { WORKTREE_SLOTS_MAX, worktreeSlotsMax, worktreesEnabled } from "../src/held/registry.ts";
import { logPath } from "../src/held/paths.ts";
import { enqueue, readEarly } from "../src/hook/rechecks.ts";

const MIN = 60_000;
const ON = { CARRICK_COMPILER_LINES: "1", CARRICK_CONTRACT_LINES: "1" };

const pause = (ms = 40): Promise<void> => new Promise((resolve) => setTimeout(resolve, ms));

function git(dir: string, ...args: string[]): void {
  spawnSync("git", args, { cwd: dir, stdio: "ignore", env: { ...process.env, GIT_AUTHOR_NAME: "t", GIT_AUTHOR_EMAIL: "t@t", GIT_COMMITTER_NAME: "t", GIT_COMMITTER_EMAIL: "t@t" } });
}

/**
 * One repo with a main checkout and linked worktrees `w1`..`w5`, one service at the repo top, a fake
 * compiler runtime (each engine reports `rss` MB; an engine whose checkout is in `block` holds its answers
 * until `release`), and a daemon on a 64 GB machine whose environment the test can change between requests.
 */
function world(env: Record<string, string> = {}, rss = 100) {
  const dir = fs.realpathSync(fs.mkdtempSync(path.join(os.tmpdir(), "wtcap")));
  const root = path.join(dir, "repo");
  const home = path.join(dir, "home");
  fs.mkdirSync(path.join(root, "src"), { recursive: true });
  fs.writeFileSync(path.join(root, "src", "x.ts"), "export const x = 1;\n");
  fs.writeFileSync(path.join(root, ".gitignore"), ".claude/worktrees/\n");
  git(root, "init", "-q", "-b", "main");
  git(root, "add", ".");
  git(root, "commit", "-q", "-m", "init");
  const tops = [1, 2, 3, 4, 5].map((n) => path.join(root, ".claude", "worktrees", `w${n}`));
  for (const [i] of tops.entries()) git(root, "worktree", "add", "-q", `.claude/worktrees/w${i + 1}`, "-b", `w${i + 1}`);
  const clock = { now: 1_000_000 };
  const started: ServiceInit[] = [];
  const closed: string[] = [];
  const block = new Set<string>();
  const waiting: Array<() => void> = [];
  const sizes = { rss };
  const engineFor = (init: ServiceInit): ServiceEngine => ({
    answer: async () => {
      if (block.has(init.checkout ?? "main")) await new Promise<void>((resolve) => waiting.push(resolve));
      return { lines: [] };
    },
    rss: () => sizes.rss,
    close: async () => {
      closed.push(init.checkout ?? "main");
    },
  });
  const runtime: ServiceRuntime = {
    start: async (init) => {
      started.push(init);
      return { engine: engineFor(init) };
    },
  };
  const init: ServiceInit = { repo: "shop", service: "api", repo_path: root, sidecar_root: root, tsconfig: null, scan_root: root };
  const settings: NodeJS.ProcessEnv = { ...ON, ...env };
  const daemon = new HeldDaemon({ root, home, env: settings, totalmem: () => 64 * 1024 ** 3, clock: () => clock.now, programs: async () => [init], runtime, queuePollMs: 10 });
  const file = (top: string | null): string => path.join(top ?? root, "src", "x.ts");
  const req = (top: string | null, extra: Partial<HeldRequest> = {}): HeldRequest => ({ session: "s1", files: [file(top)], kind: "edit", edited_at: 1, ...extra });
  const ask = (top: string | null, extra: Partial<HeldRequest> = {}): Promise<HeldAnswer> => daemon.handle(req(top, extra));
  /** Start a slot and wait until its engine is up. */
  const up = async (top: string | null): Promise<void> => {
    await ask(top);
    await pause();
  };
  const rows = (): Array<Record<string, unknown>> => {
    try {
      return fs.readFileSync(logPath(root, home), "utf8").trim().split("\n").map((l) => JSON.parse(l) as Record<string, unknown>).filter((r) => r["kind"] === "slot");
    } catch {
      return [];
    }
  };
  const release = (): void => {
    block.clear();
    for (const resolve of waiting.splice(0)) resolve();
  };
  return { dir, root, home, tops, clock, started, closed, block, settings, daemon, ask, up, rows, release, sizes, done: () => (release(), fs.rmSync(dir, { recursive: true, force: true })) };
}

test("settings: the switch reads off, 0 and false in any case; the cap is a positive integer, else 3", () => {
  assert.equal(worktreesEnabled({}), true);
  for (const off of ["off", "OFF", "0", "false", "False"]) assert.equal(worktreesEnabled({ CARRICK_HELD_WORKTREES: off }), false, off);
  for (const on of ["on", "1", "true", ""]) assert.equal(worktreesEnabled({ CARRICK_HELD_WORKTREES: on }), true, on);
  assert.equal(WORKTREE_SLOTS_MAX, 3);
  assert.equal(worktreeSlotsMax({}), 3);
  assert.equal(worktreeSlotsMax({ CARRICK_HELD_WORKTREE_SLOTS: "5" }), 5);
  for (const bad of ["0", "-2", "abc", "1.5", ""]) assert.equal(worktreeSlotsMax({ CARRICK_HELD_WORKTREE_SLOTS: bad }), 3, bad);
});

test("off switch: no worktree slot starts, the file is answered worktree, and flipping it back needs no restart", async () => {
  const w = world({ CARRICK_HELD_WORKTREES: "off" });
  try {
    const answer = await w.ask(w.tops[0]!);
    assert.equal(answer.state, "worktree");
    assert.deepEqual(answer.not_checked, [{ service: "api", state: "worktree", checkout: w.tops[0] }]);
    await pause();
    assert.equal(w.started.length, 0, "nothing started");
    w.settings["CARRICK_HELD_WORKTREES"] = "false";
    assert.equal((await w.ask(w.tops[0]!)).state, "worktree");
    delete w.settings["CARRICK_HELD_WORKTREES"];
    assert.equal((await w.ask(w.tops[0]!)).state, "cold", "on again, in the same daemon");
    await pause();
    assert.equal(w.started.length, 1);
    assert.equal(w.started[0]!.checkout, w.tops[0]);
  } finally {
    w.done();
  }
});

test("cap: a 4th worktree with 3 busy waits its wait_ms, then answers capped (worktree slots busy), and its ask lands late once one frees", async () => {
  const w = world();
  try {
    for (const top of w.tops.slice(0, 3)) {
      await w.up(top);
      w.block.add(top);
      void w.ask(top);
    }
    await pause();
    const file = path.join(w.tops[3]!, "src", "x.ts");
    const entry = enqueue(w.root, "s1", [file], w.clock.now++, { home: w.home })[0]!;
    const before = performance.now();
    const answer = await w.ask(w.tops[3]!, { wait_ms: 120, seq: entry.seq });
    assert.ok(performance.now() - before >= 100, "it waited for a slot");
    assert.equal(answer.state, "capped");
    assert.equal(answer.not_checked?.[0]?.reason, "worktree slots busy");
    assert.equal(w.started.length, 3, "no fourth start");
    assert.deepEqual(w.rows().map((r) => r["decision"]), ["admit", "admit", "admit", "queue", "capped"]);
    assert.equal(readEarly("s1", entry.seq, w.home), null, "nothing yet");
    // One busy slot finishes: the parked ask takes its place (it is the least recently used idle one) and is answered late.
    w.release();
    for (let i = 0; i < 100 && readEarly("s1", entry.seq, w.home) === null; i += 1) await pause(20);
    assert.equal(readEarly("s1", entry.seq, w.home)?.service, "shop/api");
    assert.equal(w.closed.length, 1, "one worktree slot was closed for it");
    assert.equal(w.started.length, 4);
    assert.equal(w.rows().filter((r) => r["decision"] === "evict").length, 1);
  } finally {
    w.done();
  }
});

test("cap: a request waits for a slot that frees inside its wait_ms and is then served", async () => {
  const w = world();
  try {
    for (const top of w.tops.slice(0, 3)) {
      await w.up(top);
      w.block.add(top);
      void w.ask(top);
    }
    await pause();
    setTimeout(() => w.release(), 60);
    const answer = await w.ask(w.tops[3]!, { wait_ms: 2000 });
    assert.equal(answer.state, "cold", "admitted, and starting");
    await pause();
    assert.equal(w.started.length, 4);
  } finally {
    w.done();
  }
});

test("cap: an idle worktree slot, of any idle age, is closed for a new one (the least recently used first)", async () => {
  const w = world();
  try {
    for (const top of w.tops.slice(0, 3)) {
      await w.up(top);
      w.clock.now += 1000;
    }
    const answer = await w.ask(w.tops[3]!);
    assert.equal(answer.state, "cold");
    await pause();
    assert.deepEqual(w.closed, [w.tops[0]], "the oldest idle one");
    assert.equal(w.started.length, 4);
    const evict = w.rows().find((r) => r["decision"] === "evict");
    assert.equal(evict?.["victim"], w.tops[0]);
    assert.equal(evict?.["cap"], 3);
    assert.equal(evict?.["worktrees"], 3);
  } finally {
    w.done();
  }
});

test("cap: CARRICK_HELD_WORKTREE_SLOTS overrides the default of 3, read per request", async () => {
  const w = world({ CARRICK_HELD_WORKTREE_SLOTS: "1" });
  try {
    await w.up(w.tops[0]!);
    w.block.add(w.tops[0]!);
    void w.ask(w.tops[0]!);
    await pause();
    assert.equal((await w.ask(w.tops[1]!)).state, "capped", "one slot, and it is busy");
    w.settings["CARRICK_HELD_WORKTREE_SLOTS"] = "2";
    assert.equal((await w.ask(w.tops[1]!)).state, "cold", "raised to 2 without a restart");
    await pause();
    assert.equal(w.started.length, 2);
  } finally {
    w.done();
  }
});

test("cap: a busy main slot is never closed for a worktree; an idle one is, by the usual idle rule", async () => {
  const w = world({ CARRICK_HELD_MEMORY_MB: "2500" }, 2000);
  try {
    await w.up(null);
    await w.daemon.tick();
    w.block.add("main");
    void w.ask(null);
    await pause();
    w.clock.now += 10 * MIN;
    const refused = await w.ask(w.tops[0]!);
    assert.equal(refused.state, "capped", "main's engine is busy: no room");
    assert.equal(refused.not_checked?.[0]?.reason, "worktree slots busy");
    assert.deepEqual(w.closed, []);
    assert.equal(w.started.length, 1);
    w.release();
    await pause();
    assert.equal((await w.ask(w.tops[0]!)).state, "cold", "main is idle now and past the idle age: it makes room");
    assert.deepEqual(w.closed, ["main"]);
  } finally {
    w.done();
  }
});

test("cap: an idle main slot younger than the idle age is not closed for a worktree", async () => {
  const w = world({ CARRICK_HELD_MEMORY_MB: "2500" }, 2000);
  try {
    await w.up(null);
    await w.daemon.tick();
    assert.equal((await w.ask(w.tops[0]!)).state, "capped");
    assert.deepEqual(w.closed, []);
  } finally {
    w.done();
  }
});

test("admission: a worktree slot is counted at its own settled size, else main's, else 2,048 MB", async () => {
  // Main settles at 900 MB; the budget is 2,100 MB. At 2,048 MB a worktree would never fit beside main.
  const w = world({ CARRICK_HELD_MEMORY_MB: "2100" }, 900);
  try {
    await w.up(null);
    await w.daemon.tick();
    const first = await w.ask(w.tops[0]!);
    assert.equal(first.state, "cold", "counted at main's 900 MB: 900 + 900 fits");
    const admit = w.rows().find((r) => r["decision"] === "admit");
    assert.equal(admit?.["need_mb"], 900);
    assert.equal(admit?.["budget_mb"], 2100);
    await pause();
    w.block.add(w.tops[0]!);
    void w.ask(w.tops[0]!);
    await w.daemon.tick();
    await pause();
    assert.equal((await w.ask(w.tops[1]!)).state, "capped", "a third engine would pass the budget, and the worktree one is busy");
  } finally {
    w.done();
  }
});

test("admission: with no size known for main, a worktree is counted at 2,048 MB", async () => {
  const w = world({ CARRICK_HELD_MEMORY_MB: "2000" });
  try {
    assert.equal((await w.ask(w.tops[0]!)).state, "capped", "2,048 MB is over the 2,000 MB budget");
    assert.equal(w.started.length, 0);
  } finally {
    w.done();
  }
});

test("main unchanged: the cap does not touch a main-checkout slot, and main is admitted and answered as before", async () => {
  const w = world({ CARRICK_HELD_WORKTREE_SLOTS: "1" });
  try {
    await w.up(w.tops[0]!);
    w.block.add(w.tops[0]!);
    void w.ask(w.tops[0]!);
    await pause();
    assert.equal((await w.ask(w.tops[1]!)).state, "capped", "the worktree cap is full");
    assert.equal((await w.ask(null)).state, "cold", "main still starts");
    w.release();
    await pause(100);
    assert.equal((await w.ask(null)).state, "ready");
    assert.ok(w.rows().every((r) => r["checkout"] !== undefined), "no decision row is written for a main-checkout slot");
  } finally {
    w.done();
  }
});
