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
  type HeldLine,
  type HeldRequest,
  type ServiceEngine,
  type ServiceInit,
  type ServiceRuntime,
  type StartResult,
} from "../src/held/daemon.ts";
import { askHeld, ensureHeld } from "../src/held/client.ts";
import { liveEntries, writeEntry } from "../src/held/registry.ts";
import { lockPath as heldLockPath, logPath, socketPath } from "../src/held/paths.ts";
import { compilerLinesEnabled } from "../src/facts/enabled.ts";

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
  const make = (init: ServiceInit): StartResult => {
    if (outcome === "no_compiler") return { state: "no_compiler", reason: "no native compiler" };
    const fake: Fake = { init, closed: false, rss: 2000, asked: [], events: [] };
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
      rss: () => fake.rss,
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

test("the switch needs CARRICK_COMPILER_LINES=1 and the contract line on", () => {
  assert.equal(compilerLinesEnabled({}), false);
  assert.equal(compilerLinesEnabled({ CARRICK_COMPILER_LINES: "1" }), false);
  assert.equal(compilerLinesEnabled({ CARRICK_CONTRACT_LINES: "1" }), false);
  assert.equal(compilerLinesEnabled({ CARRICK_COMPILER_LINES: "1", CARRICK_CONTRACT_LINES: "1" }), true);
  assert.equal(compilerLinesEnabled({ CARRICK_COMPILER_LINES: "true", CARRICK_CONTRACT_LINES: "1" }), false);
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

test("the first request answers cold and starts the service once; a later one answers ready", async () => {
  const w = world(["api"]);
  try {
    w.release();
    const first = await w.daemon.handle(w.req(w.file("api")));
    assert.equal(first.state, "cold");
    const second = await w.daemon.handle(w.req(w.file("api")));
    assert.equal(w.startCalls.length, 1, "a second request while starting starts nothing");
    await settle();
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
    assert.deepEqual(w.startCalls, ["a", "b"]);
    // Three minutes on, `a` is the older of the two: it is closed, `c` starts.
    w.clock.now += 3 * MIN;
    await w.daemon.handle(w.req(w.file("b"), { kind: "read" }));
    const fits = await w.daemon.handle(w.req(w.file("c")));
    assert.equal(fits.state, "cold");
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

test("a second ensureHeld starts nothing; with the switch off, none starts", () => {
  const w = world(["api"]);
  try {
    const started: string[][] = [];
    const start = (_c: string, args: string[]) => void started.push(args);
    assert.equal(ensureHeld(w.root, { env: {}, home: w.home, start }), false);
    assert.equal(ensureHeld(w.root, { env: { CARRICK_COMPILER_LINES: "1" }, home: w.home, start }), false);
    assert.equal(started.length, 0);
    assert.equal(fs.existsSync(heldLockPath(w.root, w.home)), false);
    assert.equal(ensureHeld(w.root, { env: ON, home: w.home, start }), true);
    assert.equal(ensureHeld(w.root, { env: ON, home: w.home, start }), false);
    assert.equal(started.length, 1);
  } finally {
    w.done();
  }
});

test("over the socket: none, cold, ready; a second daemon on the same root does not start", async () => {
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
    // The lock is held by this process (the daemon), so another serve returns false.
    assert.equal(await serve(deps, { spawner: process.ppid, signal: new AbortController().signal }), false);
    // No daemon: unavailable, never a throw.
    assert.equal((await askHeld(w.root + "-none", [w.file("api")], { env: ON, home: w.home, session: "s", kind: "edit" })).state, "unavailable");
    // Switch off: no connection is tried.
    assert.equal((await askHeld(w.root, [w.file("api")], { env: {}, home: w.home, session: "s", kind: "edit" })).state, "off");
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

test("run.ts with the switch unset starts nothing and writes nothing", () => {
  const w = world(["api"]);
  try {
    const run = fileURLToPath(new URL("../src/held/run.ts", import.meta.url));
    const result = spawnSync(process.execPath, [run, w.root], {
      env: { ...process.env, HOME: w.home, CARRICK_COMPILER_LINES: "", CARRICK_CONTRACT_LINES: "1" },
      encoding: "utf8",
      timeout: 15_000,
    });
    assert.equal(result.status, 0, result.stderr);
    assert.equal(fs.existsSync(path.join(w.home, ".carrick")), false);
  } finally {
    w.done();
  }
});
