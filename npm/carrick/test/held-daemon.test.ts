// The held-compiler daemon's lifecycle and request routing (part 3, T5a), with
// fake engines: no compiler, no sidecar, no binary. Every name is invented.

import test from "node:test";
import assert from "node:assert/strict";
import fs from "node:fs";
import os from "node:os";
import path from "node:path";
import { spawnSync } from "node:child_process";
import { fileURLToPath } from "node:url";
import {
  HeldDaemon,
  serve,
  WORKTREE_IDLE_MS,
  type HeldLine,
  type HeldRequest,
  type ServiceEngine,
  type ServiceInit,
  type ServiceRuntime,
  type StartOptions,
  type StartResult,
} from "../src/held/daemon.ts";
import { askHeld, ensureHeld } from "../src/held/client.ts";
import { liveEntries, writeEntry } from "../src/held/registry.ts";
import { lockPath as heldLockPath, logPath, placedPath, rootKey, socketPath, statePath } from "../src/held/paths.ts";
import { readState, writeState } from "../src/held/state.ts";
import { compilerLinesEnabled } from "../src/facts/enabled.ts";
import { takeLock } from "../src/hook/lock.ts";
import { enqueue, readEarly } from "../src/hook/rechecks.ts";

const MIN = 60_000;
const ON = { CARRICK_COMPILER_LINES: "1", CARRICK_CONTRACT_LINES: "1" };

const LINE: HeldLine = {
  row_key: "GET /orders",
  group: 0,
  label: "GET /orders",
  consumer: "web",
  file: "src/orders.ts",
  line: 8,
  message: "broken",
  row_file: "src/orders.ts",
  row_line: 8,
  row_path: "/w/src/orders.ts",
  places: [{ service: "web", file: "src/view.ts", line: 3 }],
  expected: "{ id: string }",
  main_commit: null,
  named_files: [],
};

type Fake = { init: ServiceInit; closed: boolean; rss: number; asked: string[]; events: string[]; afterGate?: Promise<void> };

function world(services: string[]) {
  const dir = fs.realpathSync(fs.mkdtempSync(path.join(os.tmpdir(), "h")));
  const root = path.join(dir, "ws");
  const home = path.join(dir, "home");
  const inits: ServiceInit[] = services.map((name) => {
    const dirOf = path.join(root, name);
    fs.mkdirSync(path.join(dirOf, "src"), { recursive: true });
    fs.writeFileSync(path.join(dirOf, "src", "a.ts"), "export const a = 1;\n");
    return { repo: "shop", service: name, repo_path: root, sidecar_root: dirOf, tsconfig: null, scan_root: root };
  });
  const clock = { now: 1_000_000 };
  const fakes = new Map<string, Fake>();
  const startCalls: string[] = [];
  let hold = true;
  let outcome: "engine" | "no_compiler" = "engine";
  let shape: { holdsCompiler?: boolean; nullRss?: boolean; rss?: number } = {};
  const make = (init: ServiceInit): StartResult => {
    if (outcome === "no_compiler") return { state: "no_compiler", reason: "no native compiler" };
    const fake: Fake = { init, closed: false, rss: shape.rss ?? 2000, asked: [], events: [] };
    fakes.set(init.service, fake);
    const engine: ServiceEngine = {
      async answer(files) {
        fake.events.push("answer");
        fake.asked.push(...files);
        return {
          lines: [{ ...LINE, label: `GET /${init.service}` }],
          after: async () => {
            fake.events.push("after:start");
            await fake.afterGate;
            fake.events.push("after:end");
          },
        };
      },
      rss: () => (shape.nullRss ? null : fake.rss),
      ...(shape.holdsCompiler === undefined ? {} : { holdsCompiler: shape.holdsCompiler }),
      async close() {
        fake.closed = true;
      },
    };
    return { engine };
  };
  const runtime: ServiceRuntime = {
    start(init) {
      startCalls.push(init.service);
      if (!hold) return Promise.resolve(make(init));
      // Held: a start that never finishes, so a request sees a service still starting.
      return new Promise(() => undefined);
    },
  };
  const daemon = new HeldDaemon({
    root,
    home,
    env: { ...ON },
    totalmem: () => 16 * 1024 ** 3,
    clock: () => clock.now,
    programs: async () => inits,
    runtime,
  });
  const req = (file: string, extra: Partial<HeldRequest> = {}): HeldRequest => ({
    session: "s1",
    files: [file],
    kind: "edit",
    edited_at: clock.now,
    ...extra,
  });
  return {
    dir,
    root,
    home,
    daemon,
    clock,
    fakes,
    startCalls,
    inits,
    req,
    file: (name: string) => path.join(root, name, "src", "a.ts"),
    release: () => {
      hold = false;
    },
    setEngine: (next: { holdsCompiler?: boolean; nullRss?: boolean; rss?: number }) => {
      shape = next;
    },
    setNoCompiler: () => {
      outcome = "no_compiler";
    },
    done: () => fs.rmSync(dir, { recursive: true, force: true }),
  };
}

/** Let a held start finish: the world's pending start resolves with an engine. */
async function settle(): Promise<void> {
  for (let i = 0; i < 5; i += 1) await new Promise((r) => setImmediate(r));
}

test("the switch is on unless CARRICK_COMPILER_LINES is exactly 0, and needs the contract line on", () => {
  assert.equal(compilerLinesEnabled({}), true, "unset is on");
  assert.equal(compilerLinesEnabled({ CARRICK_COMPILER_LINES: "" }), true, "empty is on");
  assert.equal(compilerLinesEnabled({ CARRICK_COMPILER_LINES: "1" }), true);
  assert.equal(compilerLinesEnabled({ CARRICK_COMPILER_LINES: "true" }), true);
  assert.equal(compilerLinesEnabled({ CARRICK_COMPILER_LINES: "0" }), false);
  assert.equal(compilerLinesEnabled({ CARRICK_COMPILER_LINES: "1", CARRICK_CONTRACT_LINES: "0" }), false);
  assert.equal(compilerLinesEnabled({ CARRICK_CONTRACT_LINES: "0" }), false);
  assert.equal(compilerLinesEnabled({ CARRICK_COMPILER_LINES: "1", CARRICK_FACT_LINES: "0" }), false);
});

test("a file in no service answers none at once and starts nothing", async () => {
  const w = world(["api"]);
  try {
    const t0 = performance.now();
    const answer = await w.daemon.handle(w.req(path.join(w.dir, "elsewhere", "x.ts")));
    assert.equal(answer.state, "none");
    assert.deepEqual(answer.lines, []);
    assert.ok(performance.now() - t0 < 50);
    assert.deepEqual(w.startCalls, []);
  } finally {
    w.done();
  }
});

test("a cold answer returns before the engine start's synchronous work", async () => {
  const w = world(["api"]);
  try {
    let busyDone = false;
    const held = w.daemon as unknown as { deps: { runtime: ServiceRuntime } };
    held.deps.runtime = {
      start: () => {
        const until = Date.now() + 300;
        while (Date.now() < until) {
          // busy-wait: the synchronous work before a start's first await
        }
        busyDone = true;
        const engine: ServiceEngine = { answer: async () => ({ lines: [], after: async () => undefined }), rss: () => 1, close: async () => undefined };
        return Promise.resolve({ engine });
      },
    };
    const cold = await w.daemon.handle(w.req(w.file("api")));
    assert.equal(cold.state, "cold");
    assert.deepEqual(cold.not_checked, [{ service: "api", state: "cold" }]);
    assert.ok(cold.ms < 100, `the answer took ${cold.ms} ms`);
    assert.equal(busyDone, false, "the start had not run when the answer returned");
    await new Promise((r) => setTimeout(r, 500));
    assert.equal(busyDone, true);
    assert.equal(readState(w.root, "api", w.home)?.state, "ready");
  } finally {
    w.done();
  }
});

test("the first request answers cold and starts the service once; a later one answers ready", async () => {
  const w = world(["api"]);
  try {
    w.release();
    const first = await w.daemon.handle(w.req(w.file("api")));
    assert.equal(first.state, "cold");
    const second = await w.daemon.handle(w.req(w.file("api")));
    await settle();
    assert.equal(w.startCalls.length, 1, "a second request while starting starts nothing");
    const third = await w.daemon.handle(w.req(w.file("api")));
    assert.equal(third.state, "ready");
    assert.deepEqual(third.lines.map((l) => l.label), ["GET /api"]);
    assert.ok(["cold", "ready"].includes(second.state));
    assert.equal(w.startCalls.length, 1);
  } finally {
    w.done();
  }
});

test("a path reached through a symlink maps to its service", async () => {
  const w = world(["api"]);
  try {
    const link = path.join(w.dir, "link");
    fs.symlinkSync(w.root, link);
    const answer = await w.daemon.handle(w.req(path.join(link, "api", "src", "a.ts")));
    assert.notEqual(answer.state, "none");
    await settle();
    assert.deepEqual(w.startCalls, ["api"]);
  } finally {
    w.done();
  }
});

test("a request straight after an answer waits for the twin pass: it sees the clean text, never the twin's lines", async () => {
  const w = world(["api"]);
  try {
    w.release();
    await w.daemon.handle(w.req(w.file("api")));
    await settle();
    let open!: () => void;
    const fake = w.fakes.get("api")!;
    fake.afterGate = new Promise<void>((r) => (open = r));
    const one = await w.daemon.handle(w.req(w.file("api")));
    assert.equal(one.state, "ready");
    const two = w.daemon.handle(w.req(w.file("api")));
    await settle();
    assert.deepEqual(fake.events, ["answer", "after:start"], "the second answer has not started");
    open();
    await two;
    assert.deepEqual(fake.events, ["answer", "after:start", "after:end", "answer", "after:start", "after:end"]);
  } finally {
    w.done();
  }
});

test("an engine idle for 15 minutes closes; the daemon leaves 5 minutes later, or 30 minutes after its last request", async () => {
  const w = world(["api"]);
  try {
    w.release();
    await w.daemon.handle(w.req(w.file("api")));
    await settle();
    assert.equal(liveEntries(w.home).length, 1);
    w.clock.now += 14 * MIN;
    await w.daemon.tick();
    assert.equal(w.fakes.get("api")!.closed, false);
    w.clock.now += 2 * MIN;
    await w.daemon.tick();
    assert.equal(w.fakes.get("api")!.closed, true);
    assert.equal(liveEntries(w.home).length, 0, "the registry entry goes with the engine");
    assert.equal(w.daemon.wantsExit(), false);
    w.clock.now += 5 * MIN;
    await w.daemon.tick();
    assert.equal(w.daemon.wantsExit(), true);

    const idle = world(["api"]);
    try {
      await idle.daemon.handle(idle.req(path.join(idle.dir, "x.ts")));
      idle.clock.now += 29 * MIN;
      await idle.daemon.tick();
      assert.equal(idle.daemon.wantsExit(), false, "a daemon that never held an engine is not on the 5-minute clock");
      idle.clock.now += 2 * MIN;
      await idle.daemon.tick();
      assert.equal(idle.daemon.wantsExit(), true);
    } finally {
      idle.done();
    }
  } finally {
    w.done();
  }
});

test("an engine closed for idle writes a close row to the held log", async () => {
  const w = world(["api"]);
  try {
    w.release();
    await w.daemon.handle(w.req(w.file("api")));
    await settle();
    w.clock.now += 16 * MIN;
    await w.daemon.tick();
    const rows = fs.readFileSync(logPath(w.root, w.home), "utf8").trim().split("\n").map((l) => JSON.parse(l) as Record<string, unknown>);
    const close = rows.find((r) => r["kind"] === "close");
    assert.equal(close?.["reason"], "idle");
    assert.equal(close?.["service"], "api");
  } finally {
    await w.daemon.close();
    w.done();
  }
});

test("over the memory budget answers capped, and the least recently used idle engine goes first", async () => {
  const w = world(["a", "b", "c"]);
  try {
    w.release();
    await w.daemon.handle(w.req(w.file("a")));
    await settle();
    w.clock.now += 1000;
    await w.daemon.handle(w.req(w.file("b")));
    await settle();
    // Two engines at 2048 each fill a 4096 budget. A third finds no room, and
    // neither engine has been idle for 2 minutes: capped, nothing started.
    const capped = await w.daemon.handle(w.req(w.file("c")));
    assert.equal(capped.state, "capped");
    await settle();
    assert.deepEqual(w.startCalls, ["a", "b"]);
    // Three minutes on, `a` is the older of the two: it is closed, `c` starts.
    w.clock.now += 3 * MIN;
    await w.daemon.handle(w.req(w.file("b"), { kind: "read" }));
    const fits = await w.daemon.handle(w.req(w.file("c")));
    assert.equal(fits.state, "cold");
    await settle();
    assert.equal(w.fakes.get("a")!.closed, true);
    assert.equal(w.fakes.get("b")!.closed, false);
    assert.deepEqual(w.startCalls, ["a", "b", "c"]);
  } finally {
    w.done();
  }
});

test("another process's engines count against the budget", async () => {
  const w = world(["a"]);
  try {
    w.release();
    writeEntry(w.home, { pid: process.ppid, root: "/other", service: "x", rss_mb: 3000, last_used: w.clock.now });
    const answer = await w.daemon.handle(w.req(w.file("a")));
    assert.equal(answer.state, "capped");
    assert.deepEqual(w.startCalls, []);
  } finally {
    w.done();
  }
});

test("a service that holds no compiler gives back its slot, so the next service is admitted", async () => {
  const w = world(["a", "b"]);
  try {
    w.release();
    w.setEngine({ holdsCompiler: false, nullRss: true });
    // Another checkout's engine holds half of the 4096 MB budget.
    writeEntry(w.home, { pid: process.ppid, root: "/other", service: "x", rss_mb: 2048, last_used: w.clock.now });
    await w.daemon.handle(w.req(w.file("a")));
    await settle();
    await w.daemon.tick();
    const next = await w.daemon.handle(w.req(w.file("b")));
    assert.equal(next.state, "cold");
    await settle();
    assert.deepEqual(w.startCalls, ["a", "b"]);
  } finally {
    w.done();
  }
});

test("a service with no rows answers none with no note, even on a full budget", async () => {
  const w = world(["api", "web"]);
  try {
    const started: string[] = [];
    const runtime: ServiceRuntime = {
      rowCount: async (init) => (init.service === "web" ? 0 : 3),
      start: async (init) => {
        started.push(init.service);
        return { engine: { answer: async () => ({ lines: [] }), rss: () => 2048, close: async () => undefined } };
      },
    };
    const daemon = new HeldDaemon({ root: w.root, home: w.home, env: { ...ON, CARRICK_HELD_MEMORY_MB: "2048" }, programs: async () => w.inits, runtime });
    await daemon.handle(w.req(w.file("api")));
    await settle();
    const answer = await daemon.handle(w.req(w.file("web")));
    assert.equal(answer.state, "none");
    assert.equal(answer.not_checked, undefined, "no note");
    assert.deepEqual(started, ["api"], "start was never called for web");
  } finally {
    w.done();
  }
});

test("a service whose engine holds no compiler is admitted at 0 MB next time", async () => {
  const w = world(["a"]);
  try {
    w.release();
    w.setEngine({ holdsCompiler: false, nullRss: true });
    await w.daemon.handle(w.req(w.file("a")));
    await settle();
    assert.equal(readState(w.root, "a", w.home)?.settled_mb, 0);
    // Another checkout's engine fills the whole 4096 MB budget.
    writeEntry(w.home, { pid: process.ppid, root: "/other", service: "x", rss_mb: 4096, last_used: w.clock.now });
    const again: string[] = [];
    const runtime: ServiceRuntime = { start: async (init) => (again.push(init.service), new Promise(() => undefined)) };
    const second = new HeldDaemon({ root: w.root, home: w.home, env: { ...ON }, totalmem: () => 16 * 1024 ** 3, programs: async () => w.inits, runtime });
    const answer = await second.handle(w.req(w.file("a")));
    assert.equal(answer.state, "cold");
    await settle();
    assert.deepEqual(again, ["a"]);
  } finally {
    w.done();
  }
});

test("a real engine whose rss is unreadable keeps its slot", async () => {
  const w = world(["a", "b"]);
  try {
    w.release();
    w.setEngine({ nullRss: true });
    writeEntry(w.home, { pid: process.ppid, root: "/other", service: "x", rss_mb: 2048, last_used: w.clock.now });
    await w.daemon.handle(w.req(w.file("a")));
    await settle();
    await w.daemon.tick();
    const next = await w.daemon.handle(w.req(w.file("b")));
    assert.equal(next.state, "capped");
    assert.deepEqual(w.startCalls, ["a"]);
  } finally {
    w.done();
  }
});

test("an engine above 3072 MB is closed at the next tick", async () => {
  const w = world(["api"]);
  try {
    w.release();
    await w.daemon.handle(w.req(w.file("api")));
    await settle();
    w.fakes.get("api")!.rss = 3500;
    await w.daemon.tick();
    assert.equal(w.fakes.get("api")!.closed, true);
    const again = await w.daemon.handle(w.req(w.file("api")));
    assert.equal(again.state, "cold", "the next request starts a fresh engine");
  } finally {
    w.done();
  }
});

test("an engine that settled at 3500 MB is not closed, and one above 1.5 times that is", async () => {
  const w = world(["api"]);
  try {
    w.release();
    w.setEngine({ rss: 3500 });
    await w.daemon.handle(w.req(w.file("api")));
    await settle();
    assert.equal(readState(w.root, "api", w.home)?.settled_mb, 3500, "the first gate's size is stored in the service's state record");
    w.fakes.get("api")!.rss = 5000;
    await w.daemon.tick();
    assert.equal(w.fakes.get("api")!.closed, false, "5000 MB is under 1.5 x 3500");
    w.fakes.get("api")!.rss = 5300;
    await w.daemon.tick();
    assert.equal(w.fakes.get("api")!.closed, true);
  } finally {
    w.done();
  }
});

test("a new engine is counted at the service's last settled size", async () => {
  const w = world(["a", "b"]);
  try {
    w.release();
    writeState(w.root, "a", { at: "x", service: "a", state: "ready", settled_mb: 3500 }, w.home);
    await w.daemon.handle(w.req(w.file("a")));
    assert.equal(liveEntries(w.home).find((e) => e.service === "a")?.rss_mb, 3500);
    // 3500 + the default 2048 is over 4096: b finds no room.
    const second = await w.daemon.handle(w.req(w.file("b")));
    assert.equal(second.state, "capped");
  } finally {
    w.done();
  }
});

test("a service whose settled size alone exceeds the budget is not started: capped", async () => {
  const w = world(["big"]);
  try {
    w.release();
    writeState(w.root, "big", { at: "x", service: "big", state: "ready", settled_mb: 5000 }, w.home);
    const answer = await w.daemon.handle(w.req(w.file("big")));
    assert.equal(answer.state, "capped");
    assert.deepEqual(w.startCalls, []);
  } finally {
    w.done();
  }
});

test("no native compiler is answered as such and is not retried", async () => {
  const w = world(["api"]);
  try {
    w.release();
    w.setNoCompiler();
    await w.daemon.handle(w.req(w.file("api")));
    await settle();
    const answer = await w.daemon.handle(w.req(w.file("api")));
    assert.equal(answer.state, "no_compiler");
    assert.deepEqual(answer.lines, []);
    assert.equal(w.startCalls.length, 1);
  } finally {
    w.done();
  }
});

test("each request is one JSON line in the log, with the state and the time it took", async () => {
  const w = world(["api"]);
  try {
    await w.daemon.handle(w.req(path.join(w.dir, "x.ts")));
    const lines = fs.readFileSync(logPath(w.root, w.home), "utf8").trim().split("\n");
    assert.equal(lines.length, 1);
    const row = JSON.parse(lines[0]!) as Record<string, unknown>;
    assert.equal(row["state"], "none");
    assert.equal(row["session"], "s1");
    assert.equal(typeof row["ms"], "number");
  } finally {
    w.done();
  }
});

test("an answer's log row names where its time went, and the after-answer work gets its own row", async () => {
  const w = world(["api"]);
  try {
    w.release();
    await w.daemon.handle(w.req(w.file("api")));
    await settle();
    // The world's fake engine states no steps of its own: the daemon's are the queue wait and the engine call.
    await w.daemon.handle(w.req(w.file("api")));
    await settle();
    const rows = fs.readFileSync(logPath(w.root, w.home), "utf8").trim().split("\n").map((l) => JSON.parse(l) as Record<string, unknown>);
    const ready = rows.filter((r) => r["state"] === "ready");
    assert.equal(ready.length, 1);
    const steps = ready[0]!["steps"] as Record<string, number>;
    for (const name of ["programs", "queue_wait", "engine"]) assert.equal(typeof steps[name], "number", `${name} is logged`);
    // An engine that reports steps has them in the same row, beside the daemon's.
    assert.ok(!("sweep" in steps));
  } finally {
    w.done();
  }
});

test("steps an engine reports reach the request's row, and the after-answer steps a row of their own", async () => {
  const w = world(["api"]);
  try {
    const runtime: ServiceRuntime = {
      start: async () => ({
        engine: {
          answer: async () => ({ lines: [], steps: { sweep: 12.5, pull: 3 }, after: async () => ({ twin: 7.25 }) }),
          rss: () => 100,
          close: async () => undefined,
        },
      }),
    };
    const daemon = new HeldDaemon({ root: w.root, home: w.home, env: { ...ON }, programs: async () => w.inits, runtime });
    await daemon.handle(w.req(w.file("api")));
    await settle();
    await daemon.handle(w.req(w.file("api"), { session: "s2" }));
    await settle();
    const rows = fs.readFileSync(logPath(w.root, w.home), "utf8").trim().split("\n").map((l) => JSON.parse(l) as Record<string, unknown>);
    const answered = rows.find((r) => r["session"] === "s2" && r["kind"] === "edit")!;
    assert.deepEqual(
      Object.keys(answered["steps"] as object).sort(),
      ["engine", "programs", "pull", "queue_wait", "sweep"],
    );
    assert.equal((answered["steps"] as Record<string, number>)["sweep"], 12.5);
    const after = rows.find((r) => r["session"] === "s2" && r["kind"] === "after")!;
    assert.equal(after["service"], "api");
    assert.deepEqual(after["steps"], { twin: 7.25 });
    assert.equal(typeof after["ms"], "number");
  } finally {
    w.done();
  }
});

test("a second ensureHeld starts nothing; with the switch off, none starts", () => {
  const w = world(["api"]);
  try {
    const started: string[][] = [];
    const start = (_c: string, args: string[]) => void started.push(args);
    assert.equal(ensureHeld(w.root, { env: { CARRICK_COMPILER_LINES: "0" }, home: w.home, start }), false);
    assert.equal(ensureHeld(w.root, { env: { CARRICK_COMPILER_LINES: "1", CARRICK_CONTRACT_LINES: "0" }, home: w.home, start }), false);
    assert.equal(started.length, 0);
    assert.equal(fs.existsSync(heldLockPath(w.root, w.home)), false);
    assert.equal(ensureHeld(w.root, { env: ON, home: w.home, start }), true);
    assert.equal(ensureHeld(w.root, { env: ON, home: w.home, start }), false);
    assert.equal(started.length, 1);
  } finally {
    w.done();
  }
});

test("an ask that finds no daemon starts one and answers cold with a note naming the file; a second ask in the lock's window starts nothing (#4939)", async () => {
  const w = world(["api"]);
  try {
    const started: string[][] = [];
    const start = (_c: string, args: string[]) => void started.push(args);
    const first = await askHeld(w.root, [w.file("api")], { env: ON, home: w.home, session: "s", kind: "edit", start });
    assert.equal(first.state, "cold");
    assert.deepEqual(first.not_checked, [{ service: path.join("api", "src", "a.ts"), state: "cold" }]);
    assert.equal(started.length, 1);
    assert.equal(started[0]!.at(-1), w.root);
    // The lock this ask took is live: a second ask in its window starts nothing and says nothing.
    const second = await askHeld(w.root, [w.file("api")], { env: ON, home: w.home, session: "s", kind: "edit", start });
    assert.equal(second.state, "unavailable");
    assert.equal(second.not_checked, undefined);
    assert.equal(started.length, 1);
    // Switch off: nothing is asked or started.
    assert.equal((await askHeld(w.root, [w.file("api")], { env: { CARRICK_COMPILER_LINES: "0" }, home: w.home, session: "s", kind: "edit", start })).state, "off");
    assert.equal(started.length, 1);
  } finally {
    w.done();
  }
});

test("a daemon does not start when another live process holds the root's lock", async () => {
  const w = world(["api"]);
  try {
    assert.equal(takeLock(heldLockPath(w.root, w.home), process.ppid, Date.now(), 30_000), true);
    const deps = { root: w.root, home: w.home, env: { ...ON }, programs: async () => w.inits, runtime: { start: async () => ({ state: "error" as const, reason: "x" }) } };
    assert.equal(await serve(deps, {}), false);
    assert.equal(fs.existsSync(socketPath(w.root, w.home)), false);
  } finally {
    w.done();
  }
});

test("over the socket: none, cold, ready", async () => {
  const w = world(["api"]);
  const runtime: ServiceRuntime = {
    start: async () => ({
      engine: { answer: async () => ({ lines: [LINE] }), rss: () => 100, close: async () => undefined },
    }),
  };
  const deps = { root: w.root, home: w.home, env: { ...ON }, programs: async () => w.inits, runtime };
  const stop = new AbortController();
  const served = serve(deps, { spawner: process.pid, signal: stop.signal, tickMs: 20 });
  try {
    for (let i = 0; i < 100 && !fs.existsSync(socketPath(w.root, w.home)); i += 1) await new Promise((r) => setTimeout(r, 20));
    const none = await askHeld(w.root, [path.join(w.dir, "x.ts")], { env: ON, home: w.home, session: "s", kind: "edit" });
    assert.equal(none.state, "none");
    const cold = await askHeld(w.root, [w.file("api")], { env: ON, home: w.home, session: "s", kind: "edit" });
    assert.equal(cold.state, "cold");
    await new Promise((r) => setTimeout(r, 50));
    const ready = await askHeld(w.root, [w.file("api")], { env: ON, home: w.home, session: "s", kind: "edit" });
    assert.equal(ready.state, "ready");
    assert.deepEqual(ready.lines, [LINE]);
    // No daemon: one is started and the ask answers cold, never a throw.
    const startedNone: string[][] = [];
    const none2 = await askHeld(w.root + "-none", [w.file("api")], { env: ON, home: w.home, session: "s", kind: "edit", start: (_c, args) => void startedNone.push(args) });
    assert.equal(none2.state, "cold");
    assert.equal(startedNone.length, 1);
    // Switch off: no connection is tried.
    assert.equal((await askHeld(w.root, [w.file("api")], { env: { CARRICK_COMPILER_LINES: "0" }, home: w.home, session: "s", kind: "edit" })).state, "off");
  } finally {
    stop.abort();
    await served;
    w.done();
  }
});

test("a request not answered within the wait is late, and the daemon still logs its lines", async () => {
  const w = world(["api"]);
  const runtime: ServiceRuntime = {
    start: async () => ({
      engine: {
        answer: async () => {
          await new Promise((r) => setTimeout(r, 300));
          return { lines: [LINE] };
        },
        rss: () => 100,
        close: async () => undefined,
      },
    }),
  };
  const stop = new AbortController();
  const served = serve({ root: w.root, home: w.home, env: { ...ON }, programs: async () => w.inits, runtime }, { spawner: process.pid, signal: stop.signal, tickMs: 20 });
  try {
    for (let i = 0; i < 100 && !fs.existsSync(socketPath(w.root, w.home)); i += 1) await new Promise((r) => setTimeout(r, 20));
    await askHeld(w.root, [w.file("api")], { env: ON, home: w.home, session: "s", kind: "edit" });
    await new Promise((r) => setTimeout(r, 50));
    const late = await askHeld(w.root, [w.file("api")], { env: ON, home: w.home, session: "s", kind: "edit", waitMs: 100 });
    assert.equal(late.state, "late");
    assert.deepEqual(late.lines, []);
    await new Promise((r) => setTimeout(r, 400));
    const logged = fs.readFileSync(logPath(w.root, w.home), "utf8").trim().split("\n").map((l) => JSON.parse(l) as { state: string; late?: boolean; lines: number });
    assert.ok(logged.some((row) => row.state === "ready" && row.late === true && row.lines === 1), JSON.stringify(logged));
  } finally {
    stop.abort();
    await served;
    w.done();
  }
});

test("run.ts with the switch off starts nothing and writes nothing", () => {
  const w = world(["api"]);
  try {
    const run = fileURLToPath(new URL("../src/held/run.ts", import.meta.url));
    const result = spawnSync(process.execPath, [run, w.root], {
      env: { ...process.env, HOME: w.home, CARRICK_COMPILER_LINES: "0", CARRICK_CONTRACT_LINES: "1" },
      encoding: "utf8",
      timeout: 15_000,
    });
    assert.equal(result.status, 0, result.stderr);
    assert.equal(fs.existsSync(path.join(w.home, ".carrick")), false);
  } finally {
    w.done();
  }
});

// T7 (#4654): an answer that did not check the edit says why, and the engine's state is kept for the doctor.
test("starting, ready, capped and no_compiler are each noted on the answer and kept in the service's state record", async () => {
  const w = world(["a", "b", "c"]);
  try {
    const cold = await w.daemon.handle(w.req(w.file("a")));
    assert.deepEqual(cold.not_checked, [{ service: "a", state: "cold" }]);
    assert.equal(readState(w.root, "a", w.home)?.state, "starting");
    w.release();
    await w.daemon.handle(w.req(w.file("b")));
    await settle();
    w.clock.now += 41_000;
    const ready = await w.daemon.handle(w.req(w.file("b")));
    assert.equal(ready.not_checked, undefined, "a checked edit carries no note");
    assert.equal(readState(w.root, "b", w.home)?.state, "ready");
    await w.daemon.handle(w.req(w.file("a")));
    await settle();
    const capped = await w.daemon.handle(w.req(w.file("c")));
    assert.deepEqual(capped.not_checked, [{ service: "c", state: "capped", budget_mb: 4096, holders: 2 }]);
    assert.deepEqual(readState(w.root, "c", w.home), { at: new Date(w.clock.now).toISOString(), service: "c", repo: "shop", state: "capped", budget_mb: 4096, holders: 2 });
  } finally {
    w.done();
  }
  const n = world(["x"]);
  try {
    n.setNoCompiler();
    n.release();
    await n.daemon.handle(n.req(n.file("x")));
    await settle();
    const answer = await n.daemon.handle(n.req(n.file("x")));
    assert.deepEqual(answer.not_checked, [{ service: "x", state: "no_compiler" }]);
    assert.deepEqual([readState(n.root, "x", n.home)?.state, readState(n.root, "x", n.home)?.reason], ["no_compiler", "no native compiler"]);
  } finally {
    n.done();
  }
});

test("a ready start's duration is kept in the state record", async () => {
  const w = world(["a"]);
  try {
    w.release();
    await w.daemon.handle(w.req(w.file("a")));
    await settle();
    const record = readState(w.root, "a", w.home);
    assert.equal(record?.state, "ready");
    assert.equal(typeof record?.start_ms, "number");
  } finally {
    w.done();
  }
});

test("an edit asked during the start is answered when the engine is ready", async () => {
  const w = world(["api"]);
  try {
    let release: () => void = () => undefined;
    const gate = new Promise<void>((resolve) => { release = resolve; });
    const asked: string[] = [];
    const file = w.file("api");
    const engine: ServiceEngine = {
      async answer(files) {
        asked.push(...files);
        return { lines: [{ ...LINE, file: "api/src/a.ts", row_file: "api/src/a.ts", row_path: file }] };
      },
      rss: () => 10,
      async close() {},
    };
    const daemon = new HeldDaemon({
      root: w.root, home: w.home, env: { ...ON }, totalmem: () => 16 * 1024 ** 3, clock: () => w.clock.now,
      programs: async () => w.inits,
      runtime: { start: async () => { await gate; return { engine }; } },
    });
    const other = path.join(w.root, "api", "src", "b.ts");
    fs.writeFileSync(other, "export const b = 1;\n");
    const entry = (f: string) => enqueue(w.root, "s1", [f], w.clock.now++, { home: w.home })[0]!;
    const ask = (f: string, seq: string) => daemon.handle({ session: "s1", files: [f], kind: "edit", edited_at: w.clock.now, seq });

    const older = entry(file);
    assert.equal((await ask(file, older.seq)).state, "cold", "the first ask starts the engine");
    const newer = entry(file);
    assert.equal((await ask(file, newer.seq)).state, "cold");
    const second = entry(other);
    assert.equal((await ask(other, second.seq)).state, "cold");
    assert.equal((await daemon.handle({ session: "s1", files: [file], kind: "read" })).state, "cold");
    assert.equal(readEarly("s1", newer.seq, w.home), null, "nothing is filed while it starts");

    release();
    for (let i = 0; i < 50 && readEarly("s1", second.seq, w.home) === null; i += 1) await new Promise((r) => setTimeout(r, 20));
    assert.deepEqual(readEarly("s1", newer.seq, w.home)?.result.items?.length, 1, "the newest edit of the file is answered with the engine's line");
    assert.ok(readEarly("s1", second.seq, w.home) !== null, "another file's edit is answered too");
    assert.equal(readEarly("s1", older.seq, w.home), null, "an older ask for the same file is not filed");
    assert.equal(asked.filter((f) => f === file).length, 1, "the engine was asked once for the file: older edit and the read are not asked again");
    await daemon.close();
  } finally {
    w.done();
  }
});

// A file in a linked git worktree is never judged by the main checkout's engine (T2, #5038).

function git(dir: string, ...args: string[]): void {
  spawnSync("git", args, { cwd: dir, stdio: "ignore", env: { ...process.env, GIT_AUTHOR_NAME: "t", GIT_AUTHOR_EMAIL: "t@t", GIT_COMMITTER_NAME: "t", GIT_COMMITTER_EMAIL: "t@t" } });
}

/** A repo whose services sit at the repo top (`""`) or under it, plus a nested linked worktree `.claude/worktrees/b` of it. */
function worktreeWorld(serviceDir: string, rowCount?: number) {
  const dir = fs.realpathSync(fs.mkdtempSync(path.join(os.tmpdir(), "wt")));
  const root = path.join(dir, "repo");
  const home = path.join(dir, "home");
  const dirOf = path.join(root, serviceDir);
  fs.mkdirSync(path.join(dirOf, "src"), { recursive: true });
  fs.writeFileSync(path.join(dirOf, "src", "x.ts"), "export const x = 1;\n");
  fs.writeFileSync(path.join(root, ".gitignore"), ".claude/worktrees/\n");
  git(root, "init", "-q", "-b", "main");
  git(root, "add", ".");
  git(root, "commit", "-q", "-m", "init");
  git(root, "worktree", "add", "-q", ".claude/worktrees/b", "-b", "b");
  const init: ServiceInit = { repo: "shop", service: "api", repo_path: root, sidecar_root: dirOf, tsconfig: null, scan_root: root };
  const events: string[] = [];
  const runtime: ServiceRuntime = {
    ...(rowCount === undefined ? {} : { rowCount: async () => rowCount }),
    start: async () => {
      events.push("start");
      return { engine: { answer: async () => (events.push("answer"), { lines: [] }), rss: () => 100, close: async () => undefined } };
    },
  };
  const daemon = new HeldDaemon({ root, home, env: { ...ON }, totalmem: () => 64 * 1024 ** 3, programs: async () => [init], runtime });
  const req = (file: string): HeldRequest => ({ session: "s1", files: [file], kind: "edit", edited_at: 1 });
  return { dir, root, daemon, events, req, main: path.join(dirOf, "src", "x.ts"), twin: path.join(root, ".claude", "worktrees", "b", serviceDir, "src", "x.ts"), done: () => fs.rmSync(dir, { recursive: true, force: true }) };
}

// A worktree file gets its own slot of its twin's service, started from a clone of main's shadow (#5038, T2).

type Scripted = StartResult | "engine";

/** `worktreeWorld` with a clock, a 64 GB machine, and a runtime that records each start and gives scripted results in turn. */
function slotWorld(serviceDir: string, script: Scripted[] = []) {
  const w = worktreeWorld(serviceDir);
  const home = path.join(w.dir, "home");
  const clock = { now: 1_000_000 };
  const started: ServiceInit[] = [];
  const options: Array<StartOptions | undefined> = [];
  const closed: string[] = [];
  const results = [...script];
  /** Set to make a worktree engine's answers never end (an ask in flight). */
  const hang = { worktree: false };
  const engineFor = (init: ServiceInit): ServiceEngine => ({
    answer: async () => (init.checkout !== undefined && hang.worktree ? new Promise<never>(() => undefined) : { lines: [] }),
    rss: () => 100,
    close: async () => {
      closed.push(init.checkout ?? "main");
    },
  });
  const runtime: ServiceRuntime = {
    start: async (init, opts) => {
      started.push(init);
      options.push(opts);
      const next = results.shift() ?? "engine";
      return next === "engine" ? { engine: engineFor(init) } : next;
    },
  };
  const init: ServiceInit = { repo: "shop", service: "api", repo_path: w.root, sidecar_root: path.join(w.root, serviceDir), tsconfig: null, scan_root: w.root };
  const daemon = new HeldDaemon({ root: w.root, home, env: { ...ON }, totalmem: () => 64 * 1024 ** 3, clock: () => clock.now, programs: async () => [init], runtime });
  const top = path.join(w.root, ".claude", "worktrees", "b");
  return { ...w, home, clock, daemon, started, options, closed, top, hang };
}

test("T2: a worktree file under a repo-top service starts the worktree's own slot (checkout set, folders under the worktree); the next ask is ready", async () => {
  const w = slotWorld("");
  try {
    const first = await w.daemon.handle(w.req(w.twin));
    assert.equal(first.state, "cold");
    assert.deepEqual(first.not_checked, [{ service: "api", state: "cold" }]);
    await settle();
    assert.equal(w.started.length, 1, "one start");
    const init = w.started[0]!;
    assert.equal(init.checkout, w.top);
    assert.equal(init.service, "api");
    assert.equal(init.repo_path, w.top);
    assert.equal(init.scan_root, w.top);
    assert.equal(init.sidecar_root, w.top);
    assert.equal((await w.daemon.handle(w.req(w.twin))).state, "ready");
    assert.equal(w.started.length, 1, "no second start");
  } finally {
    w.done();
  }
});

test("T2: a worktree file under a sub-folder service starts a slot whose program folder is that sub-folder of the worktree", async () => {
  const w = slotWorld("apps/api");
  try {
    assert.equal((await w.daemon.handle(w.req(w.twin))).state, "cold");
    await settle();
    assert.equal(w.started[0]?.sidecar_root, path.join(w.top, "apps", "api"));
    assert.equal(w.started[0]?.scan_root, w.top);
    assert.equal((await w.daemon.handle(w.req(w.twin))).state, "ready");
  } finally {
    w.done();
  }
});

for (const stale of ["no_saved_answer", "stale_build", "stale_main_inputs"]) {
  test(`T2: a clone refused for ${stale} starts the main checkout's service first, then clones once more`, async () => {
    const w = slotWorld("", [{ state: "refused", refused: stale, detail: "d" }]);
    try {
      assert.equal((await w.daemon.handle(w.req(w.twin))).state, "cold");
      await settle();
      assert.deepEqual(w.started.map((i) => i.checkout ?? "main"), [w.top, "main", w.top]);
      assert.equal(readState(w.root, "api", w.home)?.state, "ready", "main started through its normal path: state record");
      assert.ok(liveEntries(w.home).some((e) => e.root === w.root), "and registry entry");
      assert.equal((await w.daemon.handle(w.req(w.twin))).state, "ready");
    } finally {
      w.done();
    }
  });
}

test("T2: a clone refused twice answers worktree with the refusal, never main's answer", async () => {
  const refused: StartResult = { state: "refused", refused: "no_saved_answer", detail: "d" };
  const w = slotWorld("", [refused, "engine", refused]);
  try {
    await w.daemon.handle(w.req(w.twin));
    await settle();
    const answer = await w.daemon.handle(w.req(w.twin));
    assert.equal(answer.state, "worktree");
    assert.deepEqual(answer.lines, []);
    assert.deepEqual(answer.not_checked, [{ service: "api", state: "worktree", reason: "no_saved_answer", checkout: w.top }]);
    assert.equal(liveEntries(w.home).some((e) => e.root === w.top), false, "a refused slot holds no memory");
  } finally {
    w.done();
  }
});

for (const refusal of ["worktree_deps_differ", "vendored_reaches_own", "vendored_repo_file_differs", "destination_in_main", "worktree_unreadable"]) {
  test(`T2: a clone refused for ${refusal} starts nothing else and answers worktree with the reason`, async () => {
    const w = slotWorld("", [{ state: "refused", refused: refusal, detail: "d" }]);
    try {
      await w.daemon.handle(w.req(w.twin));
      await settle();
      assert.equal(w.started.length, 1, "no main start, no second clone");
      const answer = await w.daemon.handle(w.req(w.twin));
      assert.equal(answer.state, "worktree");
      assert.deepEqual(answer.not_checked, [{ service: "api", state: "worktree", reason: refusal, checkout: w.top }]);
      assert.equal(w.started.length, 1, "not retried at once");
    } finally {
      w.done();
    }
  });
}

test("T2: no saved answer while main's engine is ready does not restart main: one more clone, then worktree", async () => {
  const refused: StartResult = { state: "refused", refused: "no_saved_answer", detail: "d" };
  const w = slotWorld("", ["engine", refused, refused]);
  try {
    await w.daemon.handle(w.req(w.main));
    await settle();
    await w.daemon.handle(w.req(w.twin));
    await settle();
    assert.deepEqual(w.started.map((i) => i.checkout ?? "main"), ["main", w.top, w.top]);
    assert.equal((await w.daemon.handle(w.req(w.main))).state, "ready", "main's engine kept");
    assert.equal((await w.daemon.handle(w.req(w.twin))).state, "worktree");
  } finally {
    w.done();
  }
});

for (const stale of ["stale_build", "stale_main_inputs"]) {
  test(`T2 F3: ${stale} while main's engine is ready closes and restarts main, then clones: the worktree is checked`, async () => {
    const refused: StartResult = { state: "refused", refused: stale, detail: "d" };
    const w = slotWorld("", ["engine", refused]);
    try {
      await w.daemon.handle(w.req(w.main));
      await settle();
      await w.daemon.handle(w.req(w.twin));
      for (let i = 0; i < 3; i += 1) await settle();
      assert.deepEqual(w.started.map((i) => i.checkout ?? "main"), ["main", w.top, "main", w.top]);
      assert.deepEqual(w.closed, ["main"], "the stale main engine was closed");
      assert.equal((await w.daemon.handle(w.req(w.twin))).state, "ready");
      assert.equal((await w.daemon.handle(w.req(w.main))).state, "ready");
    } finally {
      w.done();
    }
  });
}

test("T2 F1: a worktree slot begun by an edit starts with that edit's files; one begun by a read, and a main start, get none", async () => {
  const w = slotWorld("");
  try {
    await w.daemon.handle(w.req(w.twin));
    await w.daemon.handle(w.req(w.main));
    await settle();
    assert.deepEqual(w.options, [{ edited: [w.twin] }, undefined]);
    w.clock.now += WORKTREE_IDLE_MS;
    await w.daemon.tick();
    await w.daemon.handle({ ...w.req(w.twin), kind: "read" });
    await settle();
    assert.deepEqual(w.options[2], {}, "a read begins it with no edited files");
    w.clock.now += WORKTREE_IDLE_MS;
    await w.daemon.tick();
    await w.daemon.handle(w.req(w.twin));
    await settle();
    assert.deepEqual(w.options[3], { edited: [w.twin] }, "the first edit after an idle close starts it again with that edit's files");
  } finally {
    w.done();
  }
});

test("T2 F4: a closed worktree slot's state and placed files go with it; main's stay", async () => {
  const w = slotWorld("");
  try {
    await w.daemon.handle(w.req(w.main));
    await w.daemon.handle(w.req(w.twin));
    await settle();
    fs.writeFileSync(placedPath(w.top, "api", w.home), "{}");
    assert.ok(fs.existsSync(statePath(w.top, "api", w.home)));
    w.clock.now += WORKTREE_IDLE_MS;
    await w.daemon.tick();
    assert.equal(fs.existsSync(statePath(w.top, "api", w.home)), false, "state gone");
    assert.equal(fs.existsSync(placedPath(w.top, "api", w.home)), false, "placed gone");
    assert.equal(fs.existsSync(path.join(w.home, ".carrick", "held", rootKey(w.top))), false, "the worktree's folder gone");
    assert.ok(fs.existsSync(statePath(w.root, "api", w.home)), "main's state kept");
  } finally {
    w.done();
  }
});

test("T2 F4: a refused worktree slot does not keep the daemon from its empty exit", async () => {
  const w = slotWorld("", [{ state: "refused", refused: "worktree_deps_differ", detail: "d" }]);
  try {
    await w.daemon.handle(w.req(w.twin));
    await settle();
    w.clock.now += 5 * MIN;
    assert.equal(w.daemon.wantsExit(), true);
  } finally {
    w.done();
  }
});

test("T2 F5: a main start waits at most 2 s for a worktree engine's ask in flight, then closes that engine", async () => {
  const w = slotWorld("");
  try {
    await w.daemon.handle(w.req(w.twin));
    await settle();
    w.hang.worktree = true;
    void w.daemon.handle(w.req(w.twin));
    await settle();
    const asked = Date.now();
    await w.daemon.handle(w.req(w.main));
    for (let i = 0; i < 300 && !w.started.some((s) => s.checkout === undefined); i += 1) await new Promise((r) => setTimeout(r, 20));
    const waited = Date.now() - asked;
    assert.ok(w.started.some((s) => s.checkout === undefined), "main started");
    assert.ok(waited >= 1_900 && waited < 4_000, `main waited ${waited} ms`);
    assert.deepEqual(w.closed, [w.top]);
  } finally {
    w.done();
  }
});

test("T2: a worktree slot closes after one idle minute; main's stays", async () => {
  const w = slotWorld("");
  try {
    await w.daemon.handle(w.req(w.main));
    await w.daemon.handle(w.req(w.twin));
    await settle();
    w.clock.now += WORKTREE_IDLE_MS - 1;
    await w.daemon.tick();
    assert.deepEqual(w.closed, [], "not yet");
    w.clock.now += 1;
    await w.daemon.tick();
    assert.deepEqual(w.closed, [w.top], "the worktree engine closed; main's did not");
    assert.equal((await w.daemon.handle(w.req(w.main))).state, "ready");
    assert.equal((await w.daemon.handle(w.req(w.twin))).state, "cold", "a later ask starts it again");
  } finally {
    w.done();
  }
});

test("T2: a main start first closes the service's ready worktree engines (it rewrites the packages their clones read)", async () => {
  const w = slotWorld("");
  try {
    await w.daemon.handle(w.req(w.twin));
    await settle();
    assert.equal((await w.daemon.handle(w.req(w.twin))).state, "ready");
    await w.daemon.handle(w.req(w.main));
    await settle();
    assert.deepEqual(w.closed, [w.top]);
    assert.deepEqual(w.started.map((i) => i.checkout ?? "main"), [w.top, "main"]);
  } finally {
    w.done();
  }
});

test("T2: a worktree engine whose start ended just as a main start queued behind it is closed, never served on main's rewritten shadow", async () => {
  const w = worktreeWorld("");
  try {
    const home = path.join(w.dir, "home");
    const closed: string[] = [];
    const started: string[] = [];
    let release: () => void = () => undefined;
    const held = new Promise<void>((resolve) => (release = resolve));
    const runtime: ServiceRuntime = {
      start: async (init) => {
        started.push(init.checkout ?? "main");
        if (init.checkout !== undefined && started.length === 1) await held;
        return { engine: { answer: async () => ({ lines: [] }), rss: () => 100, close: async () => void closed.push(init.checkout ?? "main") } };
      },
    };
    const init: ServiceInit = { repo: "shop", service: "api", repo_path: w.root, sidecar_root: w.root, tsconfig: null, scan_root: w.root };
    const daemon = new HeldDaemon({ root: w.root, home, env: { ...ON }, totalmem: () => 64 * 1024 ** 3, programs: async () => [init], runtime });
    const top = path.join(w.root, ".claude", "worktrees", "b");
    assert.equal((await daemon.handle(w.req(w.twin))).state, "cold");
    await settle();
    assert.equal((await daemon.handle(w.req(w.main))).state, "cold", "main's start queues behind the worktree's");
    await settle();
    release();
    for (let i = 0; i < 4; i += 1) await settle();
    assert.deepEqual(started, [top, "main"]);
    assert.deepEqual(closed, [top], "the worktree engine was closed");
    assert.equal((await daemon.handle(w.req(w.twin))).state, "cold", "the next worktree ask starts a fresh clone");
  } finally {
    w.done();
  }
});

test("T2: a main-checkout ask starts and writes the same with and without a worktree slot alive", async () => {
  /** Main's start init, every file under main's held folder (path and bytes), main's registry entry and its log row. */
  const record = async (withWorktree: boolean) => {
    const w = slotWorld("");
    try {
      if (withWorktree) {
        await w.daemon.handle(w.req(w.twin));
        await settle();
        assert.equal((await w.daemon.handle(w.req(w.twin))).state, "ready");
      }
      const answer = await w.daemon.handle(w.req(w.main));
      await settle();
      const ready = await w.daemon.handle(w.req(w.main));
      const dir = path.join(w.home, ".carrick", "held", rootKey(w.root));
      const files = fs.readdirSync(dir).sort().map((name) => `${name}\n${fs.readFileSync(path.join(dir, name), "utf8")}`);
      const entry = liveEntries(w.home).filter((e) => e.root === w.root).map((e) => ({ ...e, pid: 0 }));
      const rows = fs.readFileSync(logPath(w.root, w.home), "utf8").trim().split("\n").map((l) => JSON.parse(l) as Record<string, unknown>);
      const mainRows = rows.filter((r) => r["kind"] !== "close").slice(withWorktree ? 2 : 0).map((r) => ({ ...r, ms: 0, steps: undefined }));
      const main = w.started.find((i) => i.checkout === undefined);
      return { answer: { ...answer, ms: 0 }, ready: { ...ready, ms: 0 }, main, files, entry, mainRows };
    } finally {
      w.done();
    }
  };
  const alone = await record(false);
  const beside = await record(true);
  // The two worlds sit in different temporary folders: compare with each world's folder taken out.
  const norm = (v: unknown, root: string): string => JSON.stringify(v).split(root).join("<root>");
  assert.ok(alone.main !== undefined);
  assert.equal(norm(beside, path.dirname(beside.main!.repo_path)), norm(alone, path.dirname(alone.main!.repo_path)));
});

test("a worktree file whose twin's service has no rows answers none with no note", async () => {
  const w = worktreeWorld("apps/api", 0);
  try {
    const answer = await w.daemon.handle(w.req(w.twin));
    assert.equal(answer.state, "none");
    assert.equal(answer.not_checked, undefined);
    assert.deepEqual(w.events, []);
  } finally {
    w.done();
  }
});

test("a file in the main checkout is served as before", async () => {
  const w = worktreeWorld("");
  try {
    const answer = await w.daemon.handle(w.req(w.main));
    assert.equal(answer.state, "cold");
    assert.deepEqual(answer.not_checked, [{ service: "api", state: "cold" }]);
    await settle();
    assert.deepEqual(w.events, ["start"]);
  } finally {
    w.done();
  }
});

test("a worktree that holds its own program (its own init) is served as today", async () => {
  const w = worktreeWorld("");
  try {
    const top = path.join(w.root, ".claude", "worktrees", "b");
    const own: ServiceInit = { repo: "shop", service: "api", repo_path: top, sidecar_root: top, tsconfig: null, scan_root: top };
    const daemon = new HeldDaemon({
      root: top,
      home: path.join(w.dir, "home2"),
      env: { ...ON },
      programs: async () => [own],
      runtime: { start: () => new Promise(() => undefined) },
    });
    const answer = await daemon.handle(w.req(w.twin));
    assert.equal(answer.state, "cold");
  } finally {
    w.done();
  }
});

// Every per-engine key and file names its checkout (#5038, W1): an init that names a linked worktree gets its own slot, state file and registry entry.

function twoCheckouts() {
  const dir = fs.realpathSync(fs.mkdtempSync(path.join(os.tmpdir(), "co")));
  const root = path.join(dir, "repo");
  const top = path.join(dir, "other-checkout");
  const home = path.join(dir, "home");
  for (const base of [root, top]) {
    fs.mkdirSync(path.join(base, "src"), { recursive: true });
    fs.writeFileSync(path.join(base, "src", "x.ts"), "export const x = 1;\n");
  }
  const main: ServiceInit = { repo: "shop", service: "api", repo_path: root, sidecar_root: root, tsconfig: null, scan_root: root };
  const other: ServiceInit = { ...main, repo_path: top, sidecar_root: top, scan_root: top, checkout: top };
  const started: ServiceInit[] = [];
  const runtime: ServiceRuntime = {
    start: async (init) => {
      started.push(init);
      return { engine: { answer: async () => ({ lines: [] }), rss: () => 100, close: async () => undefined } };
    },
  };
  const daemon = new HeldDaemon({ root, home, env: { ...ON }, totalmem: () => 64 * 1024 ** 3, programs: async () => [main, other], runtime });
  const req = (file: string): HeldRequest => ({ session: "s1", files: [file], kind: "edit", edited_at: 1 });
  return { dir, root, top, home, daemon, started, req, mainFile: path.join(root, "src", "x.ts"), otherFile: path.join(top, "src", "x.ts"), done: () => fs.rmSync(dir, { recursive: true, force: true }) };
}

test("W1: two inits of one service, one naming a checkout, get two slots, two state files and two registry entries", async () => {
  const w = twoCheckouts();
  try {
    await w.daemon.handle(w.req(w.mainFile));
    await settle();
    await w.daemon.handle(w.req(w.otherFile));
    await settle();
    assert.equal(w.started.length, 2, "one start each");
    assert.deepEqual(w.started.map((i) => i.checkout), [undefined, w.top]);
    assert.equal(readState(w.root, "api", w.home)?.state, "ready");
    assert.equal(readState(w.top, "api", w.home)?.state, "ready");
    assert.deepEqual(liveEntries(w.home).map((e) => e.root).sort(), [w.root, w.top].sort());
    // Asks reach each slot's own engine: both now answer ready.
    assert.equal((await w.daemon.handle(w.req(w.mainFile))).state, "ready");
    assert.equal((await w.daemon.handle(w.req(w.otherFile))).state, "ready");
    assert.equal(w.started.length, 2, "no third start");
  } finally {
    w.done();
  }
});

test("W1: a main-only run writes its state and registry entry under the daemon's own checkout", async () => {
  const w = twoCheckouts();
  try {
    await w.daemon.handle(w.req(w.mainFile));
    await settle();
    assert.deepEqual(liveEntries(w.home).map((e) => e.root), [w.root]);
    assert.equal(readState(w.root, "api", w.home)?.state, "ready");
    assert.equal(readState(w.top, "api", w.home), null);
    assert.ok(fs.existsSync(statePath(w.root, "api", w.home)));
    assert.equal(fs.existsSync(path.join(w.home, ".carrick", "held", rootKey(w.top))), false, "nothing written under another checkout");
  } finally {
    w.done();
  }
});
