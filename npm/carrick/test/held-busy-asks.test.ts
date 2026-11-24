// T5 of the worktree check (#5038): no held ask is lost while the daemon is busy. The ask is written with the
// hello, so a daemon whose loop is held past the asker's wait still reads it and files a late answer; an ask no
// daemon took is told; a worktree's shadow clone runs off the daemon's event loop. Invented names throughout.

import test from "node:test";
import assert from "node:assert/strict";
import { spawnSync } from "node:child_process";
import fs from "node:fs";
import net from "node:net";
import os from "node:os";
import path from "node:path";
import { askHeld } from "../src/held/client.ts";
import { HeldDaemon, serve, type HeldRequest, type ServiceInit, type ServiceRuntime } from "../src/held/daemon.ts";
import { socketPath, lockPath } from "../src/held/paths.ts";
import { cloneShadow, cloneShadowOffLoop, writeShadowStamp, type ShadowAnswer } from "../src/held/shadow.ts";
import { notCheckedLines } from "../src/hook/not-checked.ts";
import { takeLock } from "../src/hook/lock.ts";

const ON = { CARRICK_COMPILER_LINES: "1", CARRICK_CONTRACT_LINES: "1" };

function scratch(t: { after: (fn: () => void) => void }) {
  const dir = fs.realpathSync(fs.mkdtempSync(path.join(os.tmpdir(), "busy-")));
  t.after(() => fs.rmSync(dir, { recursive: true, force: true }));
  const root = path.join(dir, "ws");
  const home = path.join(dir, "h");
  fs.mkdirSync(path.join(root, "src"), { recursive: true });
  fs.writeFileSync(path.join(root, "src", "a.ts"), "export const a = 1;\n");
  return { dir, root, home, file: path.join(root, "src", "a.ts") };
}

/** A socket that takes connections and says nothing for `holdMs`, as a daemon whose loop is held; every line it got. */
async function silentDaemon(root: string, home: string, holdMs: number): Promise<{ lines: string[]; close: () => Promise<void> }> {
  fs.mkdirSync(path.dirname(socketPath(root, home)), { recursive: true });
  const lines: string[] = [];
  const server = net.createServer((conn) => {
    let buffer = "";
    conn.on("error", () => undefined);
    // Read only after the hold, as a blocked loop would.
    setTimeout(() => {
      conn.on("data", (chunk) => {
        buffer += chunk.toString("utf8");
        for (let end = buffer.indexOf("\n"); end >= 0; end = buffer.indexOf("\n")) {
          lines.push(buffer.slice(0, end));
          buffer = buffer.slice(end + 1);
        }
      });
    }, holdMs);
    conn.pause();
    setTimeout(() => conn.resume(), holdMs);
  });
  await new Promise<void>((resolve) => server.listen(socketPath(root, home), resolve));
  // A live lock and hello file of this build: the client keeps a busy daemon rather than replacing it.
  takeLock(lockPath(root, home), process.pid, Date.now(), 60_000);
  fs.writeFileSync(path.join(path.dirname(socketPath(root, home)), path.basename(socketPath(root, home)).replace(".sock", ".hello")), JSON.stringify({ pid: process.pid, build_id: "b1" }));
  return { lines, close: () => new Promise((resolve) => server.close(() => resolve())) };
}

test("T5: an edit ask is written with the hello, so a daemon busy past the asker's wait still gets it", async (t) => {
  const s = scratch(t);
  const daemon = await silentDaemon(s.root, s.home, 3_000);
  t.after(() => daemon.close());
  const answer = await askHeld(s.root, [s.file], { env: ON, home: s.home, session: "s1", kind: "edit", seq: "q1", waitMs: 300, buildId: "b1" });
  assert.equal(answer.state, "late");
  for (let i = 0; i < 100 && daemon.lines.length < 2; i += 1) await new Promise((r) => setTimeout(r, 50));
  const frames = daemon.lines.map((line) => JSON.parse(line) as Record<string, unknown>);
  assert.deepEqual(frames[0], { hello: true });
  assert.equal(frames[1]?.["build_id"], "b1");
  assert.equal((frames[1]?.["ask"] as HeldRequest | undefined)?.seq, "q1", "the ask reached the daemon after the asker stopped waiting");
});

/** A daemon over a socket, with a runtime that records every start. */
async function realDaemon(t: { after: (fn: () => void | Promise<void>) => void }) {
  const s = scratch(t);
  const init: ServiceInit = { repo: "shop", service: "api", repo_path: s.root, sidecar_root: s.root, tsconfig: null, scan_root: s.root };
  const started: string[] = [];
  const runtime: ServiceRuntime = { start: async () => (started.push("start"), { engine: { answer: async () => ({ lines: [] }), rss: () => 1, close: async () => undefined } }) };
  const stop = new AbortController();
  const served = serve({ root: s.root, home: s.home, env: { ...ON }, totalmem: () => 64 * 1024 ** 3, programs: async () => [init], runtime, buildId: "b1" }, { spawner: process.pid, signal: stop.signal, tickMs: 20 });
  t.after(async () => {
    stop.abort();
    await served;
  });
  for (let i = 0; i < 100 && !fs.existsSync(socketPath(s.root, s.home)); i += 1) await new Promise((r) => setTimeout(r, 20));
  return { ...s, started };
}

/** Write raw lines to the daemon and read what it says back; with `endAfterMs`, this side ends the connection then. */
function exchange(root: string, home: string, lines: string[], endAfterMs?: number): Promise<string[]> {
  return new Promise((resolve) => {
    const got: string[] = [];
    let buffer = "";
    const conn = net.createConnection(socketPath(root, home));
    conn.on("data", (chunk) => {
      buffer += chunk.toString("utf8");
      for (let end = buffer.indexOf("\n"); end >= 0; end = buffer.indexOf("\n")) {
        got.push(buffer.slice(0, end));
        buffer = buffer.slice(end + 1);
      }
    });
    conn.on("close", () => resolve(got));
    conn.on("error", () => resolve(got));
    conn.write(lines.map((line) => `${line}\n`).join(""));
    if (endAfterMs !== undefined) setTimeout(() => conn.end(), endAfterMs);
  });
}

test("T5: the daemon answers an ask frame of its own build and never one of another build", async (t) => {
  const d = await realDaemon(t);
  const ask = (build: string) => JSON.stringify({ ask: { session: "s", files: [d.file], kind: "edit" }, build_id: build });
  const other = await exchange(d.root, d.home, [JSON.stringify({ hello: true }), ask("b0")], 300);
  assert.equal(other.length, 1, `only the hello: ${other.join(" | ")}`);
  assert.equal(d.started.length, 0, "another build's ask starts nothing");
  const own = await exchange(d.root, d.home, [JSON.stringify({ hello: true }), ask("b1")]);
  assert.equal(own.length, 2, own.join(" | "));
  assert.equal((JSON.parse(own[1]!) as { state: string }).state, "cold");
});

test("T5: an ask no daemon took (one is starting, or backed off) is told busy, never silent", async (t) => {
  const s = scratch(t);
  // A live lock with no socket: a daemon is starting, so this ask starts none.
  takeLock(lockPath(s.root, s.home), process.pid, Date.now(), 60_000);
  const answer = await askHeld(s.root, [s.file], { env: ON, home: s.home, session: "s1", kind: "edit", seq: "q1", buildId: "b1", start: () => undefined });
  assert.equal(answer.state, "unavailable");
  assert.deepEqual(answer.not_checked, [{ service: path.join("src", "a.ts"), state: "busy" }]);
  const said = notCheckedLines("s1", s.root, answer.not_checked ?? [], s.home);
  assert.deepEqual(said.lines, ["Carrick: the compiler engine did not check this edit of src/a.ts: the compiler engine was busy and did not take this edit. `carrick doctor` lists what is not checked."]);
});

/** A main checkout with a big service, its saved shadow, and a nested worktree: a clone that takes the loop a while when run on it. */
function bigBed(t: { after: (fn: () => void) => void }, files: number) {
  const s = scratch(t);
  const g = (cwd: string, ...args: string[]) => spawnSync("git", ["-c", "user.name=t", "-c", "user.email=t@t", ...args], { cwd, stdio: "ignore" });
  const body = `export const v = ${JSON.stringify("x".repeat(4000))};\n`;
  const own: string[] = [];
  for (let i = 0; i < files; i += 1) {
    const rel = path.join("src", `f${i}.ts`);
    fs.writeFileSync(path.join(s.root, rel), body);
    own.push(rel);
  }
  fs.writeFileSync(path.join(s.root, ".gitignore"), ".claude/worktrees/\n");
  g(s.root, "init", "-q", "-b", "main");
  g(s.root, "add", "-A");
  g(s.root, "commit", "-q", "-m", "init");
  g(s.root, "worktree", "add", "-q", ".claude/worktrees/b", "-b", "b");
  const mainOut = path.join(s.dir, "held", "main");
  for (const rel of own) {
    fs.mkdirSync(path.dirname(path.join(mainOut, "w", rel)), { recursive: true });
    fs.copyFileSync(path.join(s.root, rel), path.join(mainOut, "w", rel));
  }
  fs.writeFileSync(path.join(mainOut, "w", "tsconfig.json"), JSON.stringify({ files: own.map((rel) => path.join(mainOut, "w", rel)) }));
  const fit = { strictNullChecks: true, strictFunctionTypes: true, exactOptionalPropertyTypes: false };
  const answer: ShadowAnswer = { tsconfig: path.join(mainOut, "w", "tsconfig.json"), own_files: own, native_tsc: null, wire: [], tolerances: { unwidened_alias_suffix: "", declarations: [] }, options: { service: fit, judge: fit } };
  assert.equal(writeShadowStamp(mainOut, answer, s.root, "b1"), true);
  return { mainOut, top: path.join(s.root, ".claude", "worktrees", "b"), out: path.join(s.dir, "held", "wt") };
}

/** The longest gap between 5 ms timer ticks while `run` is in flight. */
async function longestStall(run: () => Promise<unknown>): Promise<number> {
  let last = performance.now();
  let worst = 0;
  const timer = setInterval(() => {
    const now = performance.now();
    worst = Math.max(worst, now - last);
    last = now;
  }, 5);
  try {
    await run();
  } finally {
    clearInterval(timer);
  }
  // A synchronous run ends before any tick fires: the gap still open counts too.
  return Math.max(worst, performance.now() - last);
}

test("T5: a worktree's shadow clone runs off the daemon's event loop, with the same answer", async (t) => {
  const b = bigBed(t, 3000);
  const blocked = await longestStall(async () => cloneShadow(b.mainOut, b.top, "", `${b.out}-sync`, { buildId: "b1" }));
  let answer: unknown;
  const free = await longestStall(async () => {
    answer = await cloneShadowOffLoop(b.mainOut, b.top, "", b.out, { buildId: "b1" });
  });
  assert.ok(!(answer !== null && typeof answer === "object" && "refused" in answer), JSON.stringify(answer));
  assert.equal(fs.readdirSync(path.join(b.out, "w", "src")).length, 3000);
  assert.ok(blocked > 150, `the clone on the loop stalls it (${Math.round(blocked)} ms): the bed is big enough to tell`);
  assert.ok(free < blocked / 3, `off the loop the longest stall is ${Math.round(free)} ms, against ${Math.round(blocked)} ms on it`);
});

// The daemon class is imported so a type change to its frames is caught here too.
void HeldDaemon;
