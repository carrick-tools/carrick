// T10 of the worktree check (#5038): a worktree slot that has to wait for the main checkout's engine (no saved
// shadow yet, or main's inputs went stale) still answers the edits made while it waited. Each such edit's file is
// judged against the worktree's HEAD, as the edit that began the slot is (T2 F1), so a break made during the
// wait is stated, never swallowed by the start's snapshot. Through the real daemon class and its admit path.
// Invented names throughout.

import test from "node:test";
import assert from "node:assert/strict";
import fs from "node:fs";
import os from "node:os";
import path from "node:path";
import { spawnSync } from "node:child_process";
import { HeldDaemon, type HeldLine, type HeldRequest, type ServiceInit, type ServiceRuntime, type StartOptions, type StartResult } from "../src/held/daemon.ts";
import { enqueue, readEarly } from "../src/hook/rechecks.ts";
import { deliverEarly } from "../src/hook/late.ts";

const ON = { CARRICK_COMPILER_LINES: "1", CARRICK_CONTRACT_LINES: "1" };

function git(dir: string, ...args: string[]): void {
  spawnSync("git", args, { cwd: dir, stdio: "ignore", env: { ...process.env, GIT_AUTHOR_NAME: "t", GIT_AUTHOR_EMAIL: "t@t", GIT_COMMITTER_NAME: "t", GIT_COMMITTER_EMAIL: "t@t" } });
}

const settle = async (): Promise<void> => {
  for (let i = 0; i < 8; i += 1) await new Promise((r) => setImmediate(r));
};

/**
 * A repo-top service with two files and a nested worktree. The runtime gives the worktree's first start `first`,
 * holds the main checkout's start until `releaseMain`, and records each start's `edited` files. A worktree
 * engine answers a break line for any file whose name holds `broken`.
 */
function world(t: { after: (fn: () => void) => void }, first: StartResult) {
  const dir = fs.realpathSync(fs.mkdtempSync(path.join(os.tmpdir(), "mf")));
  t.after(() => fs.rmSync(dir, { recursive: true, force: true }));
  const root = path.join(dir, "repo");
  const home = path.join(dir, "home");
  fs.mkdirSync(path.join(root, "src"), { recursive: true });
  fs.writeFileSync(path.join(root, "src", "first.ts"), "export const a = 1;\n");
  fs.writeFileSync(path.join(root, "src", "broken.ts"), "export const b = 1;\n");
  fs.writeFileSync(path.join(root, ".gitignore"), ".claude/worktrees/\n");
  git(root, "init", "-q", "-b", "main");
  git(root, "add", ".");
  git(root, "commit", "-q", "-m", "init");
  git(root, "worktree", "add", "-q", ".claude/worktrees/b", "-b", "b");
  const top = path.join(root, ".claude", "worktrees", "b");
  const init: ServiceInit = { repo: "shop", service: "api", repo_path: root, sidecar_root: root, tsconfig: null, scan_root: root };
  const staleInputs = "refused" in first && first.refused === "stale_main_inputs";
  const worktreeStarts: Array<StartOptions | undefined> = [];
  const asked: string[][] = [];
  let mainStarts = 0;
  let releaseMain: () => void = () => undefined;
  const mainHeld = new Promise<void>((resolve) => (releaseMain = resolve));
  const line = (file: string): HeldLine => ({ row_key: "POST /orders", group: 0, label: "POST /orders", consumer: "web", file, line: 1, message: "broken", row_file: "src/broken.ts", row_line: 1, row_path: file, places: [], expected: "{}", main_commit: null, named_files: [] });
  const runtime: ServiceRuntime = {
    start: async (i, opts) => {
      if (i.checkout === undefined) {
        mainStarts += 1;
        // Main's first start is held while the worktree waits on it; with stale inputs, its restart (the second) is.
        if (mainStarts > 1 || !staleInputs) await mainHeld;
        return { engine: { answer: async () => ({ lines: [] }), rss: () => 100, close: async () => undefined } };
      }
      worktreeStarts.push(opts);
      if (worktreeStarts.length === 1) return first;
      return {
        engine: {
          answer: async (files: string[]) => (asked.push(files), { lines: files.filter((f) => f.includes("broken")).map(line) }),
          rss: () => 100,
          close: async () => undefined,
        },
      };
    },
  };
  const daemon = new HeldDaemon({ root, home, env: { ...ON }, totalmem: () => 64 * 1024 ** 3, programs: async () => [init], runtime, queuePollMs: 10 });
  /** An edit ask with its own queue entry, as the edit hook makes one. */
  const edit = (file: string, session = "s1"): { request: HeldRequest; seq: string } => {
    const entry = enqueue(root, session, [file], Date.now(), { home })[0]!;
    return { request: { session, files: [file], kind: "edit", edited_at: Date.now(), seq: entry.seq, wait_ms: 50 }, seq: entry.seq };
  };
  return {
    root,
    home,
    daemon,
    top,
    worktreeStarts,
    asked,
    releaseMain,
    mainStarts: () => mainStarts,
    edit,
    first: path.join(top, "src", "first.ts"),
    broken: path.join(top, "src", "broken.ts"),
    main: path.join(root, "src", "first.ts"),
  };
}

test("T10 A: no main engine yet, a worktree edit, then a break in another file while main starts: the slot reads that file at HEAD and its ask is answered", async (t) => {
  const w = world(t, { state: "refused", refused: "no_saved_answer", detail: "d" });
  assert.equal((await w.daemon.handle(w.edit(w.first).request)).state, "cold");
  await settle();
  assert.equal(w.mainStarts(), 1, "main is starting first");
  const breakAsk = w.edit(w.broken);
  assert.equal((await w.daemon.handle(breakAsk.request)).state, "cold", "the slot is still waiting for main");
  w.releaseMain();
  for (let i = 0; i < 20 && w.asked.length === 0; i += 1) await settle();
  assert.equal(w.worktreeStarts.length, 2, "one refused clone, one after main started");
  assert.deepEqual([...(w.worktreeStarts[1]?.edited ?? [])].sort(), [w.broken, w.first].sort(), "the break's file is read at HEAD too");
  assert.ok(w.asked.some((files) => files.includes(w.broken)), "the break's ask was asked again");
  const early = readEarly("s1", breakAsk.seq, w.home);
  assert.ok(early !== null && JSON.stringify(early).includes("\"type_mismatch\""), `the break's early answer: ${JSON.stringify(early)}`);
});

test("T10 B: main ready, its inputs go stale, a worktree edit, then a break in another file while main restarts: read at HEAD and answered", async (t) => {
  const w = world(t, { state: "refused", refused: "stale_main_inputs", detail: "d" });
  await w.daemon.handle(w.edit(w.main, "m").request);
  await settle();
  assert.equal(w.mainStarts(), 1);
  assert.equal((await w.daemon.handle(w.edit(w.first).request)).state, "cold");
  await settle();
  assert.equal(w.mainStarts(), 2, "main restarts for its stale inputs");
  const breakAsk = w.edit(w.broken);
  assert.equal((await w.daemon.handle(breakAsk.request)).state, "cold");
  w.releaseMain();
  for (let i = 0; i < 20 && w.asked.length === 0; i += 1) await settle();
  assert.deepEqual([...(w.worktreeStarts[1]?.edited ?? [])].sort(), [w.broken, w.first].sort(), "the break's file is read at HEAD too");
  const early = readEarly("s1", breakAsk.seq, w.home);
  assert.ok(early !== null && JSON.stringify(early).includes("\"type_mismatch\""), `the break's early answer: ${JSON.stringify(early)}`);
});

test("T10: a worktree slot refused after it kept an edit ask tells that ask why, through its early answer, once", async (t) => {
  const w = world(t, { state: "refused", refused: "worktree_deps_differ", detail: "d" });
  const ask = w.edit(w.first);
  assert.equal((await w.daemon.handle(ask.request)).state, "cold");
  for (let i = 0; i < 10 && readEarly("s1", ask.seq, w.home) === null; i += 1) await settle();
  const early = readEarly("s1", ask.seq, w.home);
  assert.deepEqual(early?.not_checked, [{ service: "api", state: "worktree", reason: "worktree_deps_differ", checkout: w.top }]);
  const said = deliverEarly("s1", early!, new Set(), { home: w.home });
  assert.deepEqual(said, ["Carrick: the compiler engine did not check this edit of api: the file is in a git worktree whose package or config files differ from the main checkout."]);
});

test("T10: a worktree slot whose start failed after main started tells its kept asks it could not start", async (t) => {
  const w = world(t, { state: "error", reason: "the compiler engine is not built" });
  const ask = w.edit(w.first);
  await w.daemon.handle(ask.request);
  for (let i = 0; i < 10 && readEarly("s1", ask.seq, w.home) === null; i += 1) await settle();
  assert.equal(readEarly("s1", ask.seq, w.home)?.not_checked?.[0]?.state, "error");
});
