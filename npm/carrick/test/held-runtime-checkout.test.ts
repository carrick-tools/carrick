// Every per-engine file names its checkout (#5038, W1): starting an init that names a linked worktree
// leaves the main checkout's shadow folder and placed file as they were. A fake compiler engine; invented names.

import test from "node:test";
import assert from "node:assert/strict";
import { createHash } from "node:crypto";
import fs from "node:fs";
import os from "node:os";
import path from "node:path";
import { spawnSync } from "node:child_process";
import type { ServiceInit } from "../src/held/daemon.ts";
import { placedPath, shadowDir } from "../src/held/paths.ts";
import { createRuntime } from "../src/held/runtime.ts";

/** A fake compiler engine that writes a one-file shadow and logs each `write_shadow` it runs to `log`. */
const fakeEngine = (log: string): string => `
import fs from "node:fs";
import path from "node:path";
const lines = fs.readFileSync(0, "utf8").split("\\n").filter(Boolean).map((l) => JSON.parse(l));
let scanRoot = "";
for (const r of lines) {
  if (r.action === "init") { scanRoot = r.scan_root; console.log(JSON.stringify({ request_id: r.request_id, status: "ready" })); continue; }
  if (r.action !== "write_shadow") continue;
  fs.appendFileSync(${JSON.stringify(log)}, r.out_dir + "\\n");
  fs.mkdirSync(path.join(r.out_dir, "w", "src"), { recursive: true });
  fs.writeFileSync(path.join(r.out_dir, "w", "tsconfig.json"), JSON.stringify({ files: [path.join(r.out_dir, "w", "src", "own.ts")] }));
  fs.writeFileSync(path.join(r.out_dir, "w", "src", "own.ts"), fs.readFileSync(path.join(scanRoot, "src", "own.ts"), "utf8"));
  const fit = { strictNullChecks: true, strictFunctionTypes: true, exactOptionalPropertyTypes: false };
  console.log(JSON.stringify({
    request_id: r.request_id, status: "success", tsconfig: path.join(r.out_dir, "w", "tsconfig.json"),
    own_files: ["src/own.ts"], native_tsc: "/bin/true",
    wire: ["type __carrick_Wire<S, E> = S;"], tolerances: { unwidened_alias_suffix: "", declarations: [] },
    options: { service: fit, judge: fit },
  }));
}
`;

/** One checksum over every file under `dir` (path and bytes), or null when it is not there. */
function sum(dir: string): string | null {
  if (!fs.existsSync(dir)) return null;
  const hash = createHash("sha256");
  const walk = (d: string): void => {
    for (const name of fs.readdirSync(d).sort()) {
      const file = path.join(d, name);
      if (fs.statSync(file).isDirectory()) walk(file);
      else hash.update(file).update(fs.readFileSync(file));
    }
  };
  walk(dir);
  return hash.digest("hex");
}

function git(cwd: string, ...args: string[]): void {
  const r = spawnSync("git", args, { cwd, encoding: "utf8", env: { ...process.env, GIT_AUTHOR_NAME: "t", GIT_AUTHOR_EMAIL: "t@t", GIT_COMMITTER_NAME: "t", GIT_COMMITTER_EMAIL: "t@t" } });
  assert.equal(r.status, 0, r.stderr);
}

/** A repo whose one service is the whole checkout, and a nested linked worktree `.claude/worktrees/b` of it. */
function world(t: { after: (fn: () => void) => void }) {
  const dir = fs.realpathSync(fs.mkdtempSync(path.join(os.tmpdir(), "held-checkout-")));
  t.after(() => fs.rmSync(dir, { recursive: true, force: true }));
  const root = path.join(dir, "ws");
  const home = path.join(dir, "home");
  fs.mkdirSync(path.join(root, "src"), { recursive: true });
  fs.writeFileSync(path.join(root, "src", "own.ts"), "export const own = 1;\n");
  fs.writeFileSync(path.join(root, "package.json"), '{"name":"orders"}\n');
  fs.writeFileSync(path.join(root, ".gitignore"), ".claude/worktrees/\n");
  git(root, "init", "-q", "-b", "main");
  git(root, "add", "-A");
  git(root, "commit", "-q", "-m", "init");
  git(root, "worktree", "add", "-q", ".claude/worktrees/b", "-b", "b");
  const top = path.join(root, ".claude", "worktrees", "b");
  const writes = path.join(dir, "writes.log");
  const entry = path.join(dir, "engine.mjs");
  fs.writeFileSync(entry, fakeEngine(writes));
  const main: ServiceInit = { repo: "shop", service: "orders", repo_path: root, sidecar_root: root, tsconfig: null, scan_root: root };
  const other: ServiceInit = { ...main, repo_path: top, sidecar_root: top, scan_root: top, checkout: top };
  // No rows: the start writes (or clones) the shadow and the placed file, then answers with an idle service.
  const runtime = createRuntime({ root, home, entry, env: { ...process.env }, rows: async () => [], stamp: () => "s", commits: async () => null, repoPaths: async () => ({}) });
  const written = (): string[] => (fs.existsSync(writes) ? fs.readFileSync(writes, "utf8").trim().split("\n") : []);
  return { root, top, home, main, other, runtime, written };
}

test("W1: starting an init that names a checkout leaves the main shadow folder and placed file untouched", async (t) => {
  const w = world(t);
  const first = await w.runtime.start(w.main);
  assert.ok("engine" in first, JSON.stringify(first));
  const mainShadow = shadowDir(w.root, "orders", w.home);
  const mainPlaced = placedPath(w.root, "orders", w.home);
  const before = { shadow: sum(mainShadow), placed: fs.readFileSync(mainPlaced, "utf8") };
  assert.notEqual(before.shadow, null);
  const second = await w.runtime.start(w.other);
  assert.ok("engine" in second, JSON.stringify(second));
  assert.equal(sum(mainShadow), before.shadow, "main's shadow is the same bytes");
  assert.equal(fs.readFileSync(mainPlaced, "utf8"), before.placed, "main's placed file is the same bytes");
  assert.notEqual(sum(shadowDir(w.top, "orders", w.home)), null, "the checkout's own shadow exists");
  assert.ok(fs.existsSync(placedPath(w.top, "orders", w.home)), "the checkout's own placed file exists");
});

test("W1: a main-only start writes the shadow and placed file at the exact main paths", async (t) => {
  const w = world(t);
  await w.runtime.start(w.main);
  const key = createHash("sha256").update(fs.realpathSync(w.root)).digest("hex").slice(0, 12);
  const seg = `orders-${createHash("sha256").update("orders").digest("hex").slice(0, 6)}`;
  const base = path.join(w.home, ".carrick", "held", key);
  assert.equal(shadowDir(w.root, "orders", w.home), path.join(base, seg));
  assert.ok(fs.existsSync(path.join(base, seg, "w", "tsconfig.json")));
  assert.ok(fs.existsSync(path.join(base, `${seg}.placed.json`)));
});

test("T2: a worktree start clones main's shadow, never writes one, and reads the worktree's own files", async (t) => {
  const w = world(t);
  assert.ok("engine" in (await w.runtime.start(w.main)));
  assert.equal(w.written().length, 1, "main wrote its shadow");
  fs.writeFileSync(path.join(w.top, "src", "own.ts"), "export const own = 2;\n");
  const result = await w.runtime.start(w.other);
  assert.ok("engine" in result, JSON.stringify(result));
  assert.equal(w.written().length, 1, "no write_shadow for the worktree");
  const clone = shadowDir(w.top, "orders", w.home);
  assert.equal(fs.readFileSync(path.join(clone, "w", "src", "own.ts"), "utf8"), "export const own = 2;\n", "the worktree's bytes");
  assert.deepEqual(JSON.parse(fs.readFileSync(path.join(clone, "w", "tsconfig.json"), "utf8")).files, [path.join(clone, "w", "src", "own.ts")]);
  assert.equal(fs.existsSync(path.join(clone, "answer.json")), false, "a clone writes no stamp of its own");
});

test("T2: with no main shadow a worktree start is refused (no_saved_answer) and leaves no clone", async (t) => {
  const w = world(t);
  const result = await w.runtime.start(w.other);
  assert.equal("refused" in result && result.refused, "no_saved_answer");
  assert.equal(w.written().length, 0, "nothing written");
  assert.equal(fs.existsSync(shadowDir(w.top, "orders", w.home)), false);
});

test("T2: a worktree whose package file differs from main's is refused (worktree_deps_differ)", async (t) => {
  const w = world(t);
  await w.runtime.start(w.main);
  fs.writeFileSync(path.join(w.top, "package.json"), '{"name":"orders","dependencies":{"left-pad":"1"}}\n');
  const result = await w.runtime.start(w.other);
  assert.equal("refused" in result && result.refused, "worktree_deps_differ");
  assert.equal(fs.existsSync(shadowDir(w.top, "orders", w.home)), false);
});

test("T2: a worktree engine's clone is removed when the engine closes; main's shadow stays", async (t) => {
  const w = world(t);
  await w.runtime.start(w.main);
  const result = await w.runtime.start(w.other);
  assert.ok("engine" in result);
  assert.ok(fs.existsSync(shadowDir(w.top, "orders", w.home)));
  await result.engine.close();
  assert.equal(fs.existsSync(shadowDir(w.top, "orders", w.home)), false, "clone removed");
  assert.ok(fs.existsSync(shadowDir(w.root, "orders", w.home)), "main's kept");
});
