// Nothing a hook starts that outlives the hook keeps the hook's directory (carrick#5038): a worktree that is
// removed under a running daemon made every later engine start fail on `process.cwd()`. Every name is invented.

import test from "node:test";
import assert from "node:assert/strict";
import { execFileSync, spawn } from "node:child_process";
import fs from "node:fs";
import net from "node:net";
import os from "node:os";
import path from "node:path";
import { ensureHeld } from "../src/held/client.ts";
import { heldDir, socketPath } from "../src/held/paths.ts";
import { startDetached } from "../src/hook/refresh.ts";
import { startFactsRefresh } from "../src/facts/fetch.ts";
import { testDir } from "./helpers.ts";

const ON = { CARRICK_COMPILER_LINES: "1", CARRICK_CONTRACT_LINES: "1" };
const RUN = path.resolve(testDir, "..", "src", "held", "run.ts");

function tmp(prefix: string): string {
  return fs.realpathSync(fs.mkdtempSync(path.join(os.tmpdir(), prefix)));
}

function until(check: () => boolean, ms: number): Promise<boolean> {
  return new Promise((resolve) => {
    const end = Date.now() + ms;
    const tick = () => {
      if (check()) return resolve(true);
      if (Date.now() > end) return resolve(false);
      setTimeout(tick, 50);
    };
    tick();
  });
}

/** The folder a live process runs in, read from the system. */
function cwdOf(pid: number): string {
  if (process.platform === "linux") return fs.readlinkSync(`/proc/${pid}/cwd`);
  const out = execFileSync("lsof", ["-a", "-p", String(pid), "-d", "cwd", "-Fn"], { encoding: "utf8" });
  return out.split("\n").find((line) => line.startsWith("n"))!.slice(1);
}

function hello(sock: string): Promise<string> {
  return new Promise((resolve, reject) => {
    const conn = net.connect(sock);
    let text = "";
    conn.on("data", (chunk) => {
      text += chunk.toString("utf8");
      if (text.includes("\n")) {
        conn.destroy();
        resolve(text);
      }
    });
    conn.on("error", reject);
    conn.write(`${JSON.stringify({ hello: true })}\n`);
  });
}

test("ensureHeld starts the daemon in the held state folder, which it creates", () => {
  const dir = tmp("held-cwd-a-");
  const home = path.join(dir, "home");
  const root = path.join(dir, "ws");
  fs.mkdirSync(root, { recursive: true });
  const calls: Array<string | undefined> = [];
  const start = (_c: string, _a: string[], _e: NodeJS.ProcessEnv, cwd?: string) => void calls.push(cwd);
  assert.equal(ensureHeld(root, { env: ON, home, start, buildId: "b1" }), true);
  assert.deepEqual(calls, [heldDir(home)]);
  assert.ok(fs.statSync(heldDir(home)).isDirectory());
});

test("startDetached runs the child in the folder it is given", async () => {
  const dir = tmp("held-cwd-b-");
  const where = path.join(dir, "cwd.txt");
  const cwd = path.join(dir, "elsewhere");
  fs.mkdirSync(cwd);
  startDetached(process.execPath, ["-e", `require("fs").writeFileSync(${JSON.stringify(where)}, process.cwd())`], process.env, cwd);
  assert.ok(await until(() => fs.existsSync(where) && fs.readFileSync(where, "utf8") !== "", 10_000));
  assert.equal(fs.readFileSync(where, "utf8"), fs.realpathSync(cwd));
});

test("a daemon started in a folder that is then removed leaves it at once, and still answers", async (t) => {
  // A socket path is capped near 104 bytes on macOS, and its tmpdir is longer than that allows.
  const dir = fs.realpathSync(fs.mkdtempSync("/tmp/hc-"));
  const home = path.join(dir, "home");
  const root = path.join(dir, "ws");
  const starter = path.join(dir, "worktree");
  fs.mkdirSync(root, { recursive: true });
  fs.mkdirSync(starter);
  // An indexed workspace with no services: the daemon starts, holds nothing and answers.
  const bin = path.join(dir, "carrick-fake");
  fs.writeFileSync(bin, `#!/bin/sh\necho '{"schema":"carrick.held-programs/0","services":[]}'\n`, { mode: 0o755 });
  const env: NodeJS.ProcessEnv = { ...process.env, ...ON, HOME: home, USERPROFILE: home, CARRICK_BIN: bin, CARRICK_NO_KEEP_AWAKE: "1" };
  delete env["NODE_TEST_CONTEXT"];
  const child = spawn(process.execPath, [RUN, root], { cwd: starter, env, stdio: ["ignore", "ignore", "inherit"], detached: true });
  t.after(() => {
    try {
      child.kill("SIGTERM");
    } catch {
      // Already gone.
    }
    fs.rmSync(dir, { recursive: true, force: true });
  });
  const sock = socketPath(root, home);
  assert.ok(await until(() => fs.existsSync(sock), 20_000), "the daemon listens");
  assert.equal(cwdOf(child.pid!), heldDir(home), "it left the starting folder");
  fs.rmSync(starter, { recursive: true });
  assert.match(await hello(sock), /hello/);
});

test("the facts refresh child starts in its checkout root", () => {
  const root = tmp("held-cwd-d-");
  fs.mkdirSync(path.join(root, ".carrick"));
  fs.writeFileSync(path.join(root, ".carrick", "index.json"), "{}");
  const calls: Array<string | undefined> = [];
  const start = (_c: string, _a: string[], _e: NodeJS.ProcessEnv, cwd?: string) => void calls.push(cwd);
  assert.equal(startFactsRefresh(root, { env: { ...process.env, CARRICK_NO_FACTS_REFRESH: "" }, now: Date.now(), start }), true);
  assert.deepEqual(calls, [root]);
});
