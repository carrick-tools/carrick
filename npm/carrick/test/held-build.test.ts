// The held daemon knows its build (ticket B4): a hello before any ask, a
// replaced daemon of another build, the crash back-off, and the build id's
// inputs. Fake engines, invented names, no compiler.

import test from "node:test";
import assert from "node:assert/strict";
import fs from "node:fs";
import net from "node:net";
import os from "node:os";
import path from "node:path";
import { serve, type HeldLine, type ServiceInit, type ServiceRuntime } from "../src/held/daemon.ts";
import { spawn } from "node:child_process";
import { askHeld, ensureHeld } from "../src/held/client.ts";
import { alive, takeLock } from "../src/hook/lock.ts";
import { lockPath } from "../src/held/paths.ts";
import { backedOff, BACK_OFF_MS, CRASH_WINDOW_MS, recordCrash } from "../src/held/backoff.ts";
import { heldBuildId } from "../src/held/build.ts";
import { crashesPath, helloPath, socketPath } from "../src/held/paths.ts";

const ON = { CARRICK_COMPILER_LINES: "1", CARRICK_CONTRACT_LINES: "1" };
const LINE: HeldLine = {
  row_key: "GET /orders", group: 0, label: "GET /orders", consumer: "web", file: "src/orders.ts", line: 8, message: "broken",
  row_file: "src/orders.ts", row_line: 8, row_path: "/w/src/orders.ts", places: [], expected: "{ id: string }",
  main_commit: null, named_files: [],
};

function world() {
  const dir = fs.realpathSync(fs.mkdtempSync(path.join(os.tmpdir(), "hb")));
  const root = path.join(dir, "ws");
  const home = path.join(dir, "home");
  fs.mkdirSync(path.join(root, "api", "src"), { recursive: true });
  const file = path.join(root, "api", "src", "a.ts");
  fs.writeFileSync(file, "export const a = 1;\n");
  const init: ServiceInit = { repo: "shop", service: "api", repo_path: root, sidecar_root: path.join(root, "api"), tsconfig: null, scan_root: root };
  return { dir, root, home, file, init, done: () => fs.rmSync(dir, { recursive: true, force: true }) };
}

async function listening(root: string, home: string): Promise<void> {
  for (let i = 0; i < 100 && !fs.existsSync(socketPath(root, home)); i += 1) await new Promise((r) => setTimeout(r, 20));
}

test("a daemon of another build is told to exit and gets only a build-stamped ask; the ask answers cold and the next reaches a daemon of the client's build", async () => {
  const w = world();
  const answered: string[] = [];
  const starts: string[] = [];
  const runtimeFor = (name: string): ServiceRuntime => ({
    start: async () => {
      starts.push(name);
      return { engine: { answer: async () => (answered.push(name), { lines: [LINE] }), rss: () => 100, close: async () => undefined } };
    },
  });
  const deps = (buildId: string) => ({ root: w.root, home: w.home, env: { ...ON }, buildId, programs: async () => [w.init], runtime: runtimeFor(buildId) });
  const stopOld = new AbortController();
  const stopNew = new AbortController();
  const oldServed = serve(deps("old"), { spawner: process.pid, signal: stopOld.signal, tickMs: 20 });
  let newServed: Promise<boolean> | null = null;
  const start = (): void => {
    newServed = serve(deps("new"), { spawner: process.pid, signal: stopNew.signal, tickMs: 20 });
  };
  try {
    await listening(w.root, w.home);
    const ask = (buildId: string) => askHeld(w.root, [w.file], { env: ON, home: w.home, session: "s", kind: "edit", buildId, start });
    assert.equal((await ask("old")).state, "cold");
    await new Promise((r) => setTimeout(r, 50));
    assert.equal((await ask("old")).state, "ready");
    assert.deepEqual(answered, ["old"]);
    // Another build asks: the old daemon's engine is not asked.
    const replaced = await ask("new");
    assert.equal(replaced.state, "cold");
    assert.deepEqual(replaced.lines, []);
    assert.deepEqual(answered, ["old"]);
    assert.equal(await oldServed, true, "the old daemon exited");
    assert.ok(newServed !== null, "a new daemon was started");
    await listening(w.root, w.home);
    assert.equal((await ask("new")).state, "cold");
    assert.deepEqual(starts, ["old", "new"], "the next ask reached the new build's daemon");
  } finally {
    stopOld.abort();
    stopNew.abort();
    await oldServed;
    await newServed;
    w.done();
  }
});

test("the same build id is asked as before, after one hello", async () => {
  const w = world();
  fs.mkdirSync(path.dirname(socketPath(w.root, w.home)), { recursive: true });
  const frames: string[] = [];
  const server = net.createServer((conn) => {
    let buffer = "";
    conn.on("data", (chunk) => {
      buffer += chunk.toString("utf8");
      for (let end = buffer.indexOf("\n"); end >= 0; end = buffer.indexOf("\n")) {
        const frame = JSON.parse(buffer.slice(0, end)) as Record<string, unknown>;
        buffer = buffer.slice(end + 1);
        frames.push(frame["hello"] === true ? "hello" : "ask" in frame ? "ask" : "request");
        if (frame["hello"] === true) conn.write(`${JSON.stringify({ hello: { build_id: "same", pid: process.pid, root: w.root } })}\n`);
        else conn.end(`${JSON.stringify({ state: "ready", lines: [LINE], ms: 1 })}\n`);
      }
    });
  });
  await new Promise<void>((resolve) => server.listen(socketPath(w.root, w.home), resolve));
  try {
    const answer = await askHeld(w.root, [w.file], { env: ON, home: w.home, session: "s", kind: "edit", buildId: "same" });
    assert.equal(answer.state, "ready");
    // The ask goes with the hello, stamped with the client's build (T5).
    assert.deepEqual(frames, ["hello", "ask"]);
  } finally {
    server.close();
    w.done();
  }
});

test("two crashes of a build within the window keep it from starting until the back-off is over", () => {
  const w = world();
  try {
    const started: string[] = [];
    const start = (): void => void started.push("start");
    const t0 = 1_800_000_000_000;
    const ensure = (now: number, buildId = "b1") => ensureHeld(w.root, { env: ON, home: w.home, now, start, buildId });
    recordCrash(w.root, "b1", t0, w.home);
    assert.equal(ensure(t0 + 1_000), true, "one crash is not a back-off");
    fs.rmSync(path.join(w.home, ".carrick", "held", fs.readdirSync(path.join(w.home, ".carrick", "held")).find((n) => n.endsWith(".lock"))!));
    recordCrash(w.root, "b1", t0 + CRASH_WINDOW_MS - 1_000, w.home);
    recordCrash(w.root, "other", t0 + 5_000, w.home);
    assert.equal(ensure(t0 + CRASH_WINDOW_MS), false, "two within the hour back the build off");
    assert.equal(started.length, 1);
    assert.equal(backedOff(w.root, "other", t0 + CRASH_WINDOW_MS, w.home), false, "another build's crash is its own");
    assert.equal(ensure(t0 + CRASH_WINDOW_MS - 1_000 + BACK_OFF_MS - 1), false, "still off just inside the day");
    assert.equal(ensure(t0 + CRASH_WINDOW_MS - 1_000 + BACK_OFF_MS), true, "started again after the day");
    assert.equal(started.length, 2);
    assert.equal(fs.readFileSync(crashesPath(w.root, w.home), "utf8").trim().split("\n").length, 3);
  } finally {
    w.done();
  }
});

test("crashes more than an hour apart are no back-off", () => {
  const w = world();
  try {
    const t0 = 1_800_000_000_000;
    recordCrash(w.root, "b", t0, w.home);
    recordCrash(w.root, "b", t0 + CRASH_WINDOW_MS + 1_000, w.home);
    assert.equal(backedOff(w.root, "b", t0 + CRASH_WINDOW_MS + 2_000, w.home), false);
  } finally {
    w.done();
  }
});

test("the build id changes when any one input file's bytes change, and is stable otherwise", () => {
  const dir = fs.mkdtempSync(path.join(os.tmpdir(), "hbid"));
  try {
    const own = path.join(dir, "own");
    const engine = path.join(dir, "engine");
    fs.mkdirSync(path.join(own, "held"), { recursive: true });
    fs.mkdirSync(engine);
    const tsc = path.join(dir, "tsc");
    fs.writeFileSync(path.join(own, "held", "a.js"), "a1");
    fs.writeFileSync(path.join(own, "b.js"), "b1");
    fs.writeFileSync(path.join(engine, "index.js"), "e1");
    fs.writeFileSync(tsc, "t1");
    const id = () => heldBuildId({ own, engine, tsc });
    const first = id();
    assert.equal(id(), first);
    for (const file of [path.join(own, "held", "a.js"), path.join(own, "b.js"), path.join(engine, "index.js"), tsc]) {
      const before = fs.readFileSync(file);
      fs.writeFileSync(file, `${before.toString()}x`);
      assert.notEqual(id(), first, `${path.relative(dir, file)} changed the id`);
      fs.writeFileSync(file, before);
      assert.equal(id(), first);
    }
    assert.notEqual(heldBuildId({ own, engine: null, tsc }), first, "no engine is another build");
    assert.notEqual(heldBuildId({ own, engine, tsc: null }), first, "no TypeScript 7 binary is another build");
  } finally {
    fs.rmSync(dir, { recursive: true, force: true });
  }
});

// A daemon from before the hello says nothing a hello says: another build by definition.
for (const [name, reply] of [
  ["answers no hello", null],
  ["answers a request-shaped reply", { state: "ready", lines: [LINE], ms: 1 }],
  ["answers a hello without a build id", { hello: { pid: 1, root: "/x" } }],
  ["answers something that is not JSON", "garbage"],
] as const) {
  test(`a daemon that ${name} gets only a build-stamped ask, is ended through the lock's pid, and is replaced`, async () => {
    const w = world();
    const child = spawn(process.execPath, ["-e", "setInterval(() => undefined, 1000)"], { stdio: "ignore" });
    const childPid = child.pid!;
    const exited = new Promise<void>((resolve) => child.once("exit", () => resolve()));
    fs.mkdirSync(path.dirname(socketPath(w.root, w.home)), { recursive: true });
    assert.equal(takeLock(lockPath(w.root, w.home), childPid, Date.now(), 30_000), true);
    const frames: string[] = [];
    const server = net.createServer((conn) => {
      let buffer = "";
      conn.on("error", () => undefined);
      conn.on("data", (chunk) => {
        buffer += chunk.toString("utf8");
        for (let end = buffer.indexOf("\n"); end >= 0; end = buffer.indexOf("\n")) {
          const frame = JSON.parse(buffer.slice(0, end)) as Record<string, unknown>;
          buffer = buffer.slice(end + 1);
          frames.push(frame["hello"] === true ? "hello" : frame["exit"] === true ? "exit" : "ask" in frame ? "ask" : "request");
          if (reply !== null && frame["hello"] === true) conn.write(`${typeof reply === "string" ? reply : JSON.stringify(reply)}\n`);
        }
      });
    });
    await new Promise<void>((resolve) => server.listen(socketPath(w.root, w.home), resolve));
    const started: string[] = [];
    try {
      const answer = await askHeld(w.root, [w.file], { env: ON, home: w.home, session: "s", kind: "edit", buildId: "mine", waitMs: 2_000, start: () => void started.push("start") });
      assert.equal(answer.state, "cold");
      assert.deepEqual(answer.lines, []);
      assert.ok(!frames.includes("request"), `never asked: ${frames.join(",")}`);
      await exited;
      assert.equal(alive(childPid), false, "the old daemon was ended");
      assert.deepEqual(started, ["start"], "a new daemon was started");
    } finally {
      child.kill("SIGKILL");
      server.close();
      w.done();
    }
  });
}

/** A fake daemon on the checkout's socket whose hello comes `helloAfterMs` late (never, when null); a live child holds the lock. */
async function lateDaemon(helloAfterMs: number | null, helloFile: boolean) {
  const w = world();
  const child = spawn(process.execPath, ["-e", "setTimeout(() => undefined, 60000)"], { stdio: "ignore" });
  const childPid = child.pid!;
  fs.mkdirSync(path.dirname(socketPath(w.root, w.home)), { recursive: true });
  assert.equal(takeLock(lockPath(w.root, w.home), childPid, Date.now(), 30_000), true);
  if (helloFile) fs.writeFileSync(helloPath(w.root, w.home), JSON.stringify({ pid: childPid, build_id: "mine" }));
  const frames: string[] = [];
  const server = net.createServer((conn) => {
    let buffer = "";
    conn.on("error", () => undefined);
    conn.on("data", (chunk) => {
      buffer += chunk.toString("utf8");
      for (let end = buffer.indexOf("\n"); end >= 0; end = buffer.indexOf("\n")) {
        const frame = JSON.parse(buffer.slice(0, end)) as Record<string, unknown>;
        buffer = buffer.slice(end + 1);
        frames.push(frame["hello"] === true ? "hello" : "ask" in frame ? "ask" : "request");
        if (frame["hello"] === true && helloAfterMs !== null) {
          setTimeout(() => conn.write(`${JSON.stringify({ hello: { build_id: "mine", pid: childPid, root: w.root } })}\n`), helloAfterMs);
        } else if (frame["hello"] !== true && helloAfterMs !== null) {
          // A daemon reads its frames in order: the answer follows its hello (T5).
          setTimeout(() => conn.end(`${JSON.stringify({ state: "ready", lines: [], ms: 1 })}\n`), helloAfterMs + 5);
        }
      }
    });
  });
  await new Promise<void>((resolve) => server.listen(socketPath(w.root, w.home), resolve));
  const started: string[] = [];
  const ask = () => askHeld(w.root, [w.file], { env: ON, home: w.home, session: "s", kind: "edit", buildId: "mine", waitMs: 1_500, start: () => void started.push("start") });
  const done = (): void => {
    child.kill("SIGKILL");
    server.close();
    w.done();
  };
  return { ask, started, frames, childPid, child, done };
}

test("a daemon of this build that says hello after 200 ms is not ended", async () => {
  const d = await lateDaemon(400, true);
  try {
    const answer = await d.ask();
    assert.equal(answer.state, "ready");
    assert.deepEqual(d.frames, ["hello", "ask"]);
    assert.equal(alive(d.childPid), true, "the busy daemon was not ended");
    assert.deepEqual(d.started, [], "no daemon was started");
  } finally {
    d.done();
  }
});

test("a silent daemon with no hello file is still replaced", async () => {
  const d = await lateDaemon(null, false);
  const exited = new Promise<void>((resolve) => d.child.once("exit", () => resolve()));
  try {
    const answer = await d.ask();
    assert.equal(answer.state, "cold");
    await exited;
    assert.equal(alive(d.childPid), false, "the lock's pid was ended");
    assert.deepEqual(d.started, ["start"], "a new daemon was started once");
  } finally {
    d.done();
  }
});
