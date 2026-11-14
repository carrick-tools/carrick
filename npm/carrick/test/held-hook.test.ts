// The held compiler's line in the post-edit hook (part 3, T6): what an edit
// prints when the daemon answers, and that the hook never waits on a cold one.
// The daemon is a fake socket server in this process. Every name is invented.

import test from "node:test";
import assert from "node:assert/strict";
import fs from "node:fs";
import net from "node:net";
import os from "node:os";
import path from "node:path";
import { execFileSync } from "node:child_process";
import { drainRunner, fakeEnv, fixturePath, runHook } from "./helpers.ts";
import { commitRepos } from "./facts-repos.ts";
import { context, lineOf, patchEdit } from "./facts-helpers.ts";
import type { Ws } from "./facts-helpers.ts";
import { buildFacts, factsPath } from "../src/facts/store.ts";
import { factsSaid } from "../src/hook/reuse.ts";
import { heldDir, socketPath } from "../src/held/paths.ts";
import { readTimings } from "../src/hook/timings.ts";
import { renderHeldLine } from "../src/held/line.ts";
import type { HeldAnswer, HeldLine } from "../src/held/daemon.ts";

const SLUG = "held-hook-demo";
const NOTE = " (another repo, indexed at abcdef01, not checked out here)";
const COMMIT = "abcdef0123456789abcdef0123456789abcdef01";
const A = "{ id: string }";
const TASKS = `// tasks
export function listTasks(req, res) {
  res.json({ id: req.id });
}
export function getTask(req, res) {
  res.json({ id: req.id });
}
`;
const PART1 = `Carrick: GET /tasks is called from ext/src/view4.ts:4${NOTE}, which expects: ${A}`;
const IN_LIST = "res.json({ id: req.id });";
const ROW_KEY = "contract::api::src/tasks.ts::producer::GET /tasks::0";
const ON = { CARRICK_COMPILER_LINES: "1", CARRICK_CONTRACT_LINES: "1" };

const contractRow = () => ({
  k: "contract",
  side: "producer",
  protocol: "http",
  label: "GET /tasks",
  direction: "response",
  service: "api",
  repo: "api",
  file: "src/tasks.ts",
  line: 2,
  name: "listTasks",
  site: null,
  locator: "registration",
  groups: [
    {
      type: A,
      symbol: null,
      home: null,
      places: [{ service: "ext", repo: "ext", file: "src/view4.ts", line: 4, loc: "ext/src/view4.ts:4" }],
      total: 1,
      shown: 1,
      lead: "Carrick: GET /tasks is called from ",
      more: "",
      tail: `, which expects: ${A}`,
      shortened: false,
    },
  ],
  untyped: [],
  total: 1,
});

async function workspace(t: { after: (fn: () => void) => void }): Promise<Ws> {
  const root = fs.realpathSync(fs.mkdtempSync(path.join(os.tmpdir(), "carrick-held-hook-")));
  const home = fs.realpathSync(fs.mkdtempSync(path.join(os.tmpdir(), "carrick-held-hook-home-")));
  t.after(async () => {
    await drainRunner(root);
    fs.rmSync(root, { recursive: true, force: true });
    fs.rmSync(home, { recursive: true, force: true });
  });
  fs.mkdirSync(path.join(root, "api", "src"), { recursive: true });
  fs.writeFileSync(path.join(root, "api", "src", "tasks.ts"), TASKS);
  fs.mkdirSync(path.join(root, ".carrick", "hosted"), { recursive: true });
  fs.writeFileSync(path.join(root, ".carrick", "hosted", "snapshot.json"), JSON.stringify({ resolution: { repos: [{ project_slug: SLUG }] } }));
  const header = JSON.stringify({
    schema: 1,
    writer: 4,
    project_id: "p",
    project_slug: SLUG,
    digest: "d1",
    built_at: "2026-01-01T00:00:00Z",
    repos: [
      { repo: "api", service: "api", commit: "1" },
      { repo: "ext", service: "ext", commit: COMMIT },
    ],
    counts: { rows: 0, producers: 0, consumers: 0 },
  });
  const text = `${[header, JSON.stringify(contractRow())].join("\n")}\n`;
  await buildFacts(commitRepos(root, text, ["api"]), factsPath(root), [{ repo: "api", path: path.join(root, "api") }], new Date().toISOString());
  return { root, home, at: (rel) => path.join(root, rel) };
}

const broken = (over: Partial<HeldLine> = {}): HeldLine => ({
  row_key: ROW_KEY,
  group: 0,
  label: "GET /tasks",
  consumer: "web",
  file: "src/tasks.ts",
  line: 3,
  message: "Type '{ name: string; }' is not assignable to type 'O1_0'.",
  ...over,
});

type Fake = { requests: Array<Record<string, unknown>>; close(): Promise<void> };

/** A daemon on the checkout's socket that answers every request with `answer` after `delayMs`. */
async function fakeDaemon(ws: Ws, answer: HeldAnswer, delayMs = 0, root: string = ws.root): Promise<Fake> {
  fs.mkdirSync(heldDir(ws.home), { recursive: true });
  const requests: Array<Record<string, unknown>> = [];
  const sockets = new Set<net.Socket>();
  const server = net.createServer((conn) => {
    sockets.add(conn);
    let buffer = "";
    conn.on("data", (chunk) => {
      buffer += chunk.toString("utf8");
      const end = buffer.indexOf("\n");
      if (end < 0) return;
      requests.push(JSON.parse(buffer.slice(0, end)) as Record<string, unknown>);
      setTimeout(() => conn.writable && conn.write(`${JSON.stringify(answer)}\n`), delayMs);
    });
    conn.on("error", () => undefined);
  });
  await new Promise<void>((resolve) => server.listen(socketPath(root, ws.home), resolve));
  return {
    requests,
    close: () =>
      new Promise<void>((resolve) => {
        for (const conn of sockets) conn.destroy();
        server.close(() => resolve());
      }),
  };
}

const edit = (ws: Ws, session: string, env: Record<string, string> = ON) =>
  patchEdit(ws, ws.at("api/src/tasks.ts"), [{ newStart: lineOf(ws.at("api/src/tasks.ts"), IN_LIST), lines: [`+  ${IN_LIST}`] }], { session, env });

const ready = (lines: HeldLine[]): HeldAnswer => ({ state: "ready", lines, ms: 5 });
const FORBIDDEN = /__C|\breg\d|\bO\d|__carrick_/;

test("row 1: a break prints one line that replaces part 1's contract line, and the replaced line is not claimed", async (t) => {
  const ws = await workspace(t);
  const daemon = await fakeDaemon(ws, ready([broken()]));
  t.after(() => daemon.close());
  const out = await edit(ws, "h1");
  const said = context(out.stdout);
  assert.ok(said.includes("Carrick: this edit breaks GET /tasks for web (src/tasks.ts:3): Type '{ name: string; }' is not assignable to type the type it expects."), said);
  assert.ok(!said.includes("is called from"), said);
  assert.ok(!FORBIDDEN.test(said), said);
  assert.equal(daemon.requests.length, 1);
  assert.equal(daemon.requests[0]!["kind"], "edit");
  const claimed = [...factsSaid("h1", ws.home)];
  assert.ok(!claimed.some((key) => key.startsWith("contract::")), claimed.join(" | "));
  assert.ok(claimed.some((key) => key.startsWith("held::") && key.includes(ROW_KEY)), claimed.join(" | "));
});

test("the same break in the same session is said once", async (t) => {
  const ws = await workspace(t);
  const daemon = await fakeDaemon(ws, ready([broken()]));
  t.after(() => daemon.close());
  await edit(ws, "h1b");
  const again = context((await edit(ws, "h1b")).stdout);
  assert.ok(!again.includes("this edit breaks"), again);
});

test("row 2: a safe edit prints part 1's line as it always did", async (t) => {
  const ws = await workspace(t);
  const daemon = await fakeDaemon(ws, ready([]));
  t.after(() => daemon.close());
  const said = context((await edit(ws, "h2")).stdout);
  assert.ok(said.includes(PART1), said);
  assert.ok(!said.includes("this edit breaks"), said);
  assert.equal(readTimings(ws.root).at(-1)?.held_state, "ready");
});

test("rows 5 and 9: cold and none print part 1 only and name the state in the timing row", async (t) => {
  for (const state of ["cold", "none"] as const) {
    const ws = await workspace(t);
    const daemon = await fakeDaemon(ws, { state, lines: [], ms: 1 });
    t.after(() => daemon.close());
    const said = context((await edit(ws, `h5-${state}`)).stdout);
    assert.ok(said.includes(PART1), `${state}: ${said}`);
    assert.equal(readTimings(ws.root).at(-1)?.held_state, state);
  }
});

test("row 6: an answer after the wait is dropped, the hook is not held past it, and the state is late", async (t) => {
  const ws = await workspace(t);
  const daemon = await fakeDaemon(ws, ready([broken()]), 2500);
  t.after(() => daemon.close());
  const out = await edit(ws, "h6");
  const said = context(out.stdout);
  assert.ok(said.includes(PART1), said);
  assert.ok(!said.includes("this edit breaks"), said);
  assert.ok(out.ms < 2300, `the hook took ${out.ms} ms`);
  const row = readTimings(ws.root).at(-1);
  assert.equal(row?.held_state, "late");
  assert.ok((row?.held_ms ?? 0) >= 900, String(row?.held_ms));
});

test("no daemon at all: part 1 prints and the state is unavailable", async (t) => {
  const ws = await workspace(t);
  const said = context((await edit(ws, "h-none")).stdout);
  assert.ok(said.includes(PART1), said);
  assert.equal(readTimings(ws.root).at(-1)?.held_state, "unavailable");
});

test("row 15: with the switch off the output is today's, byte for byte, even with a daemon listening", async (t) => {
  const ws = await workspace(t);
  const off = await edit(ws, "h15", { CARRICK_CONTRACT_LINES: "1" });
  const daemon = await fakeDaemon(ws, ready([broken()]));
  t.after(() => daemon.close());
  const withDaemon = await edit(ws, "h15b", { CARRICK_CONTRACT_LINES: "1" });
  assert.equal(withDaemon.stdout, off.stdout);
  assert.equal(daemon.requests.length, 0);
  assert.equal(readTimings(ws.root).every((row) => row.held_state === undefined && row.held_ms === undefined), true);
  // Today's text, pinned: the contract line, nothing from part 3.
  assert.ok(context(off.stdout).includes(PART1), off.stdout);
});

test("the switch needs the contract line on too", async (t) => {
  const ws = await workspace(t);
  const daemon = await fakeDaemon(ws, ready([broken()]));
  t.after(() => daemon.close());
  await edit(ws, "h-needs", { CARRICK_COMPILER_LINES: "1" });
  assert.equal(daemon.requests.length, 0);
});

function git(dir: string, ...args: string[]): void {
  execFileSync("git", args, {
    cwd: dir,
    stdio: "ignore",
    env: { ...process.env, GIT_AUTHOR_NAME: "t", GIT_AUTHOR_EMAIL: "t@t", GIT_COMMITTER_NAME: "t", GIT_COMMITTER_EMAIL: "t@t" },
  });
}

test("row 10: a file in a linked worktree is not asked about", async (t) => {
  const ws = await workspace(t);
  const repo = path.join(ws.root, "api");
  fs.writeFileSync(path.join(repo, ".gitignore"), ".claude/worktrees/\n");
  git(repo, "init", "-q", "-b", "main");
  git(repo, "add", ".");
  git(repo, "commit", "-q", "-m", "init");
  git(repo, "worktree", "add", "-q", ".claude/worktrees/wt", "-b", "wt");
  const file = path.join(repo, ".claude", "worktrees", "wt", "src", "tasks.ts");
  const daemon = await fakeDaemon(ws, ready([broken()]));
  t.after(() => daemon.close());
  const out = await runHook("post-edit.ts", {
    payload: {
      hook_event_name: "PostToolUse",
      tool_name: "Edit",
      session_id: "h10",
      cwd: ws.root,
      tool_input: { file_path: file, old_string: "x", new_string: "" },
      tool_response: { structuredPatch: [{ oldStart: 3, oldLines: 0, newLines: 0, newStart: 3, lines: [`+  ${IN_LIST}`] }] },
    },
    env: fakeEnv({ HOME: ws.home, CLAUDE_PROJECT_DIR: ws.root, CARRICK_FAKE_FIXTURE: fixturePath("check-silent.json"), ...ON }),
  });
  assert.ok(!context(out.stdout).includes("this edit breaks"), out.stdout);
  assert.equal(daemon.requests.length, 0);
});

test("a Read asks the daemon with kind read and prints nothing of part 3", async (t) => {
  const ws = await workspace(t);
  const daemon = await fakeDaemon(ws, { state: "cold", lines: [], ms: 1 });
  t.after(() => daemon.close());
  const file = ws.at("api/src/tasks.ts");
  const read = await runHook("post-read.ts", {
    payload: { hook_event_name: "PostToolUse", tool_name: "Read", session_id: "h-read", cwd: ws.root, tool_input: { file_path: file }, tool_response: { file: { filePath: file, content: TASKS } } },
    env: fakeEnv({ HOME: ws.home, CLAUDE_PROJECT_DIR: ws.root, CARRICK_FAKE_FIXTURE: fixturePath("check-silent.json"), ...ON }),
  });
  assert.equal(read.code, 0);
  assert.equal(daemon.requests.length, 1);
  assert.equal(daemon.requests[0]!["kind"], "read");
  assert.ok(!read.stdout.includes("this edit breaks"), read.stdout);
});

test("the Read hook with the switch off asks nothing", async (t) => {
  const ws = await workspace(t);
  const daemon = await fakeDaemon(ws, { state: "cold", lines: [], ms: 1 });
  t.after(() => daemon.close());
  const file = ws.at("api/src/tasks.ts");
  await runHook("post-read.ts", {
    payload: { hook_event_name: "PostToolUse", tool_name: "Read", session_id: "h-read-off", cwd: ws.root, tool_input: { file_path: file }, tool_response: { file: { filePath: file, content: TASKS } } },
    env: fakeEnv({ HOME: ws.home, CLAUDE_PROJECT_DIR: ws.root, CARRICK_FAKE_FIXTURE: fixturePath("check-silent.json"), CARRICK_CONTRACT_LINES: "1" }),
  });
  assert.equal(daemon.requests.length, 0);
});

// The wording (D13), on the renderer itself.

test("the alias becomes the group's symbol, or the type it expects when there is none", () => {
  const message = "Type 'string' is not assignable to type 'O12_3'.";
  assert.equal(renderHeldLine(broken({ message, symbol: "TaskView" }))?.text, "Carrick: this edit breaks GET /tasks for web (src/tasks.ts:3): Type 'string' is not assignable to type 'TaskView'.");
  assert.equal(renderHeldLine(broken({ message }))?.text, "Carrick: this edit breaks GET /tasks for web (src/tasks.ts:3): Type 'string' is not assignable to type the type it expects.");
});

test("a quoted inline object type over 80 characters becomes the reply; a short one stays", () => {
  const long = `'{ ${"alpha: string; ".repeat(6)}}'`;
  const said = renderHeldLine(broken({ message: `Type ${long} is not assignable to type 'O1_0'.` }))!.text;
  assert.ok(said.includes("Type the reply is not assignable"), said);
  assert.ok(renderHeldLine(broken({ message: "Type '{ id: number; }' is not assignable to type 'O1_0'." }))!.text.includes("'{ id: number; }'"));
});

test("a line is at most 300 characters on one line", () => {
  const text = renderHeldLine(broken({ message: `Property 'x' is missing.\n  ${"because of a long reason ".repeat(40)}` }))!.text;
  assert.ok(text.length <= 300, String(text.length));
  assert.ok(!text.includes("\n"));
});

test("a message that still names the check's machinery is not printed", () => {
  for (const message of ["Property '__carrick_never' is missing", "Type 'x' is not assignable to type '__C.Pick2xx'", "Cannot find name 'reg3'"]) {
    assert.equal(renderHeldLine(broken({ message })), null, message);
  }
});
