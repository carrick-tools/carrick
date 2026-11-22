// The held compiler's early answer in the post-edit hook (part 3, B1): what an
// edit prints when the daemon answers, and that the hook never waits on a cold
// one. The daemon is a fake socket server in this process that files its
// answer as the real one does (`answerEarly`). Every name is invented.

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
import { factsSaid, sessionsDir } from "../src/hook/reuse.ts";
import { heldDir, socketPath } from "../src/held/paths.ts";
import { readTimings } from "../src/hook/timings.ts";
import { answerEarly } from "../src/hook/rechecks.ts";
import { askHeld } from "../src/held/client.ts";
import { heldBuildId } from "../src/held/build.ts";
import { heldDetail } from "../src/held/early.ts";
import type { HeldAnswer, HeldLine, HeldRequest } from "../src/held/daemon.ts";

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
  const home = fs.realpathSync(fs.mkdtempSync(path.join(os.tmpdir(), "hh-")));
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

const broken = (over: Partial<HeldLine> = {}, ws?: Ws): HeldLine => ({
  row_key: ROW_KEY,
  group: 0,
  label: "GET /tasks",
  consumer: "web",
  file: "src/tasks.ts",
  line: 3,
  message: "Type '{ name: string; }' is not assignable to type 'O1_0'.",
  row_file: "src/tasks.ts",
  row_line: 2,
  row_path: ws ? ws.at("api/src/tasks.ts") : "/w/api/src/tasks.ts",
  places: [{ service: "ext", file: "src/view4.ts", line: 4 }],
  expected: A,
  main_commit: null,
  named_files: [],
  ...over,
});

type Fake = { requests: Array<Record<string, unknown>>; close(): Promise<void> };

/** A daemon on the checkout's socket that answers every request with `answer` after `delayMs`, filing an edit ask's answer first as the real one does. */
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
      const frame = buffer.slice(0, end);
      buffer = buffer.slice(end + 1);
      // The client says hello first; the fake is of the client's own build.
      if ((JSON.parse(frame) as { hello?: boolean }).hello === true) {
        conn.write(`${JSON.stringify({ hello: { build_id: heldBuildId(), pid: process.pid, root } })}\n`);
        return;
      }
      const request = JSON.parse(frame) as HeldRequest;
      requests.push(request as unknown as Record<string, unknown>);
      setTimeout(() => {
        if (request.kind === "edit" && request.seq) answerEarly(request.session, request.seq, answer, Date.now(), ws.home);
        if (conn.writable) conn.write(`${JSON.stringify(answer)}\n`);
      }, delayMs);
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
const FORBIDDEN = /__C|\bsend\d|\bO\d|__carrick_/;

const EARLY = "Carrick's compiler engine checked api/src/tasks.ts after this edit: 1 contract problem.";

test("row 1 (K-c): a break prints the early answer IN ADDITION to part 1's contract line; both are claimed", async (t) => {
  const ws = await workspace(t);
  const daemon = await fakeDaemon(ws, ready([broken({}, ws)]));
  t.after(() => daemon.close());
  const out = await edit(ws, "h1");
  const said = context(out.stdout);
  assert.ok(said.includes(`${EARLY}\n- api/src/tasks.ts:2:1 route GET /tasks type_mismatch: This code sends '{ name: string; }'; the caller expects '{ id: string }'. Consumers: ext src/view4.ts:4`), said);
  assert.ok(said.includes("is called from"), "part 1's line stays: " + said);
  assert.ok(!FORBIDDEN.test(said), said);
  assert.equal(daemon.requests.length, 1);
  assert.equal(daemon.requests[0]!["kind"], "edit");
  assert.equal(typeof daemon.requests[0]!["seq"], "string", "the ask names part 2's entry");
  assert.ok([...factsSaid("h1", ws.home)].some((key) => key.startsWith("contract::")));
  assert.ok(fs.existsSync(path.join(sessionsDir(ws.home), `h1.recheck.${String(daemon.requests[0]!["seq"])}.early.said`)), "the early answer is claimed");
});

test("the ask waits only for what is left of the probe's budget since the edit", async (t) => {
  const ws = await workspace(t);
  const daemon = await fakeDaemon(ws, ready([]));
  t.after(() => daemon.close());
  await edit(ws, "h1w");
  const wait = Number(daemon.requests[0]!["wait_ms"]);
  assert.ok(wait >= 0 && wait < 600, String(wait));
});

test("an ask whose wait has already run out is still sent, and its answer is filed", async (t) => {
  const ws = await workspace(t);
  const daemon = await fakeDaemon(ws, ready([]));
  t.after(() => daemon.close());
  const answer = await askHeld(ws.root, [ws.at("api/src/tasks.ts")], { session: "h0", kind: "edit", waitMs: 0, seq: "0000000000001-1-0001", home: ws.home, env: ON });
  assert.equal(answer.state, "late");
  const end = Date.now() + 3000;
  while (daemon.requests.length === 0 && Date.now() < end) await new Promise((r) => setTimeout(r, 20));
  assert.equal(daemon.requests.length, 1);
});

test("row 2: a safe edit prints part 1's line as it always did", async (t) => {
  const ws = await workspace(t);
  const daemon = await fakeDaemon(ws, ready([]));
  t.after(() => daemon.close());
  const said = context((await edit(ws, "h2")).stdout);
  assert.ok(said.includes(PART1), said);
  assert.ok(!said.includes("compiler engine"), said);
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

test("row 6: an answer after the wait is not printed by this hook, the hook is not held past it, and the state is late; the daemon files it for the next carrier (early-answer.test.ts)", async (t) => {
  const ws = await workspace(t);
  const daemon = await fakeDaemon(ws, ready([broken({}, ws)]), 2500);
  t.after(() => daemon.close());
  const out = await edit(ws, "h6");
  const said = context(out.stdout);
  assert.ok(said.includes(PART1), said);
  assert.ok(!said.includes("compiler engine"), said);
  assert.ok(out.ms < 2300, `the hook took ${out.ms} ms`);
  const row = readTimings(ws.root).at(-1);
  assert.equal(row?.held_state, "late");
  assert.ok((row?.held_ms ?? 0) >= 40 && (row?.held_ms ?? 0) < 900, String(row?.held_ms));
});

test("no daemon at all (it left after its idle time): the edit starts one, part 1 prints, and the edit says once that the check is starting (#4939)", async (t) => {
  const ws = await workspace(t);
  // The edits start real daemons for this workspace: end them, by the pids that name its own root.
  t.after(() => {
    const out = (() => { try { return execFileSync("pgrep", ["-f", ws.root], { encoding: "utf8" }); } catch { return ""; } })();
    for (const pid of out.split("\n").map(Number).filter((p) => p > 0 && p !== process.pid)) {
      try { process.kill(pid); } catch { /* already gone */ }
    }
  });
  const STARTING = "Carrick: the compiler engine did not check this edit of api/src/tasks.ts: it is starting. `carrick doctor` lists what is not checked.";
  const said = context((await edit(ws, "h-none")).stdout);
  assert.ok(said.includes(PART1), said);
  assert.ok(said.includes(STARTING), said);
  assert.equal(readTimings(ws.root).at(-1)?.held_state, "cold");
  // A second edit does not say the same line again in the same session.
  const again = context((await edit(ws, "h-none")).stdout);
  assert.ok(!again.includes(STARTING), again);
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
  await edit(ws, "h-needs", { CARRICK_COMPILER_LINES: "1", CARRICK_CONTRACT_LINES: "0" });
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
  assert.ok(!context(out.stdout).includes("compiler engine"), out.stdout);
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
  assert.ok(!read.stdout.includes("compiler engine"), read.stdout);
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

// The detail (D13): the compiler's message in words an agent may read.

test("the alias becomes the group's symbol, or the expected type when there is none", () => {
  const message = "Type 'string' is not assignable to type 'O12_3'.";
  assert.equal(heldDetail(broken({ message, symbol: "TaskView" })), "This code sends 'string'; the caller expects 'TaskView'.");
  assert.equal(heldDetail(broken({ message })), "This code sends 'string'; the caller expects '{ id: string }'.");
});

test("a detail is one line, cut to its cap", () => {
  const text = heldDetail(broken({ message: `Property 'x' is missing.\n  ${"because of a long reason ".repeat(40)}` }))!;
  assert.ok(text.length <= 240, String(text.length));
  assert.ok(!text.includes("\n"));
});

test("a message that still names the check's machinery is never written", () => {
  for (const message of ["Property '__carrick_never' is missing", "Type 'x' is not assignable to type '__C.Pick2xx'", "Cannot find name 'send3'"]) {
    assert.equal(heldDetail(broken({ message })), null, message);
  }
});

// T7 (#4654): an edit the compiler engine did not check says so, once per session,
// service and state. Agent-facing text.

const note = (state: "cold" | "capped" | "no_compiler" | "error" | "worktree", more: Record<string, unknown> = {}) => ({ service: "api", state, ...more });

test("each not-checked state prints one line with its reason, once per session per service and state", async (t) => {
  const cases = [
    { state: "cold" as const, more: { took_s: 41 }, want: "the compiler engine did not check this edit of api: it is starting, which took about 41 s last time." },
    { state: "capped" as const, more: { budget_mb: 4096, holders: 2 }, want: "the compiler engine did not check this edit of api: the memory budget of 4096 MB is held by 2 other engines." },
    { state: "no_compiler" as const, more: {}, want: "the compiler engine did not check this edit of api: no TypeScript 7 compiler is installed." },
    { state: "error" as const, more: { reason: "the shadow tree could not be written" }, want: "the compiler engine did not check this edit of api: it could not start: the shadow tree could not be written." },
  ];
  for (const c of cases) {
    const ws = await workspace(t);
    const daemon = await fakeDaemon(ws, { state: c.state, lines: [], ms: 1, not_checked: [note(c.state, c.more)] });
    t.after(() => daemon.close());
    const first = context((await edit(ws, `n-${c.state}`)).stdout);
    assert.ok(first.includes(`Carrick: ${c.want} \`carrick doctor\` lists what is not checked.`), `${c.state}: ${first}`);
    assert.ok(first.includes(PART1), `part 1 stays: ${first}`);
    const second = context((await edit(ws, `n-${c.state}`)).stdout);
    assert.ok(!second.includes("compiler engine did not check"), `${c.state} said again: ${second}`);
  }
});

test("the worktree line has no doctor tail and comes once per session, service and worktree (#5038)", async (t) => {
  const ws = await workspace(t);
  const checkout = path.join(ws.root, ".claude", "worktrees", "a");
  const daemon = await fakeDaemon(ws, { state: "worktree", lines: [], ms: 1, not_checked: [note("worktree", { checkout })] });
  t.after(() => daemon.close());
  const WANT =
    "Carrick: the compiler engine did not check this edit of api: the file is in a git worktree and it reads only the main checkout.";
  const first = context((await edit(ws, "n-wt")).stdout);
  assert.ok(first.includes(WANT), first);
  assert.ok(!first.includes("doctor"), first);
  assert.ok(!context((await edit(ws, "n-wt")).stdout).includes("did not check"), "said again");
  await daemon.close();
  const other = await fakeDaemon(ws, { state: "worktree", lines: [], ms: 1, not_checked: [note("worktree", { checkout: `${checkout}2` })] });
  t.after(() => other.close());
  assert.ok(context((await edit(ws, "n-wt")).stdout).includes(WANT), "a second worktree is told too");
});

test("a different state or service of the same session is a new line; a late answer and a read print none", async (t) => {
  const ws = await workspace(t);
  const daemon = await fakeDaemon(ws, { state: "cold", lines: [], ms: 1, not_checked: [note("cold")] });
  t.after(() => daemon.close());
  assert.ok(context((await edit(ws, "n-two")).stdout).includes("did not check this edit of api: it is starting"));
  await daemon.close();
  const capped = await fakeDaemon(ws, { state: "capped", lines: [], ms: 1, not_checked: [note("capped", { budget_mb: 2048, holders: 1 })] });
  t.after(() => capped.close());
  assert.ok(context((await edit(ws, "n-two")).stdout).includes("memory budget of 2048 MB is held by 1 other engines"));
  await capped.close();
  const slow = await fakeDaemon(ws, { state: "cold", lines: [], ms: 1, not_checked: [note("cold")] }, 2500);
  t.after(() => slow.close());
  assert.ok(!context((await edit(ws, "n-late")).stdout).includes("did not check"), "late says nothing");
  const file = ws.at("api/src/tasks.ts");
  const read = await runHook("post-read.ts", {
    payload: { hook_event_name: "PostToolUse", tool_name: "Read", session_id: "n-read", cwd: ws.root, tool_input: { file_path: file }, tool_response: { file: { filePath: file, content: TASKS } } },
    env: fakeEnv({ HOME: ws.home, CLAUDE_PROJECT_DIR: ws.root, CARRICK_FAKE_FIXTURE: fixturePath("check-silent.json"), ...ON }),
  });
  assert.ok(!read.stdout.includes("did not check"), read.stdout);
});

// S4: the compiler adds at most 600 ms to an edit and 300 ms to a read, even from a daemon that accepts and never answers.
test("a daemon that never answers: the edit hook is back within 900 ms of the edit, the read hook within 600 ms", async (t) => {
  const ws = await workspace(t);
  const daemon = await fakeDaemon(ws, ready([]), 60_000);
  t.after(() => daemon.close());
  const out = await edit(ws, "s4-edit");
  assert.equal(readTimings(ws.root).at(-1)?.held_state, "late");
  assert.ok(out.ms < 900 + 500, `the edit hook took ${out.ms} ms (900 ms after the edit, 500 ms of process start allowed)`);
  const row = readTimings(ws.root).at(-1);
  assert.ok((row?.held_ms ?? 0) <= 900, String(row?.held_ms));
  const file = ws.at("api/src/tasks.ts");
  const read = await runHook("post-read.ts", {
    payload: { hook_event_name: "PostToolUse", tool_name: "Read", session_id: "s4-read", cwd: ws.root, tool_input: { file_path: file }, tool_response: { file: { filePath: file, content: TASKS } } },
    env: fakeEnv({ HOME: ws.home, CLAUDE_PROJECT_DIR: ws.root, CARRICK_FAKE_FIXTURE: fixturePath("check-silent.json"), ...ON }),
  });
  assert.ok(read.ms < 600 + 500, `the read hook took ${read.ms} ms`);
});
