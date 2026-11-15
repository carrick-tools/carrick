// Part 3 T9: the held compiler's verdict rides part 2's late carrier. The
// post-edit hook hands the ask to the daemon and returns; the daemon files the
// answer where part 2's late answers land; the next hook of any tool prints it
// once (`deliverLate`), the Stop hook delivers a finished one, and a newer edit
// of the same file drops an older ask. Every name is invented.

import test from "node:test";
import assert from "node:assert/strict";
import fs from "node:fs";
import net from "node:net";
import os from "node:os";
import path from "node:path";
import { drainRunner, fakeEnv, fixturePath, runHook } from "./helpers.ts";
import { commitRepos } from "./facts-repos.ts";
import { context, lineOf, patchEdit } from "./facts-helpers.ts";
import type { Ws } from "./facts-helpers.ts";
import { buildFacts, factsPath } from "../src/facts/store.ts";
import { factsSaid, sessionsDir } from "../src/hook/reuse.ts";
import { deliverLate } from "../src/hook/late.ts";
import { answerHeld, enqueue, enqueueHeld, finishedFor, heldSuperseded, pendingFor } from "../src/hook/rechecks.ts";
import { stopAnswers } from "../src/hook/stop-wait.ts";
import { probeTimeoutMs } from "../src/hook/probe.ts";
import { heldDir, socketPath } from "../src/held/paths.ts";
import { HeldDaemon } from "../src/held/daemon.ts";
import type { HeldAnswer, HeldLine, HeldRequest, ServiceEngine, ServiceInit } from "../src/held/daemon.ts";

const SLUG = "held-late-demo";
const COMMIT = "abcdef0123456789abcdef0123456789abcdef01";
const A = "{ id: string }";
const TASKS = `// tasks
export function listTasks(req, res) {
  res.json({ id: req.id });
}
`;
const IN_LIST = "res.json({ id: req.id });";
const ROW_KEY = "contract::api::src/tasks.ts::producer::GET /tasks::0";
const ON = { CARRICK_COMPILER_LINES: "1", CARRICK_CONTRACT_LINES: "1" };
const BREAK = "Carrick: this edit breaks GET /tasks for web (src/tasks.ts:3)";

const row = {
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
};

async function workspace(t: { after: (fn: () => void) => void }): Promise<Ws> {
  const root = fs.realpathSync(fs.mkdtempSync(path.join(os.tmpdir(), "carrick-held-late-")));
  const home = fs.realpathSync(fs.mkdtempSync(path.join(os.tmpdir(), "hl-")));
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
  const text = `${[header, JSON.stringify(row)].join("\n")}\n`;
  await buildFacts(commitRepos(root, text, ["api"]), factsPath(root), [{ repo: "api", path: path.join(root, "api") }], new Date().toISOString());
  return { root, home, at: (rel) => path.join(root, rel) };
}

const broken = (message = "Type '{ name: string; }' is not assignable to type 'O1_0'."): HeldLine => ({
  row_key: ROW_KEY,
  group: 0,
  label: "GET /tasks",
  consumer: "web",
  file: "src/tasks.ts",
  line: 3,
  message,
});

/**
 * A daemon on the checkout's socket that behaves as the real one does for a
 * request carrying `seq`: after `delayMs` it files the answer beside part 2's
 * (`answerHeld`), unless a newer edit of the file superseded the ask, and
 * replies on the socket (nobody is listening by then).
 */
async function fakeDaemon(ws: Ws, lines: (n: number) => HeldLine[], delayMs: number) {
  fs.mkdirSync(heldDir(ws.home), { recursive: true });
  const requests: Array<Record<string, unknown>> = [];
  const sockets = new Set<net.Socket>();
  const server = net.createServer((conn) => {
    sockets.add(conn);
    let buffer = "";
    conn.on("error", () => undefined);
    conn.on("data", (chunk) => {
      buffer += chunk.toString("utf8");
      const end = buffer.indexOf("\n");
      if (end < 0) return;
      const request = JSON.parse(buffer.slice(0, end)) as HeldRequest & { seq?: string };
      buffer = "";
      requests.push(request as unknown as Record<string, unknown>);
      const n = requests.length;
      setTimeout(() => {
        if (request.kind === "edit" && request.seq && !heldSuperseded(request.session, request.seq, ws.home)) {
          answerHeld(request.session, request.seq, { state: "ready", lines: lines(n) }, Date.now(), ws.home);
        }
        const reply: HeldAnswer = { state: "ready", lines: [], ms: delayMs };
        if (conn.writable) conn.write(`${JSON.stringify(reply)}\n`);
      }, delayMs);
    });
  });
  await new Promise<void>((resolve) => server.listen(socketPath(ws.root, ws.home), resolve));
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

const nextHook = (ws: Ws, session: string) =>
  runHook("post-read.ts", {
    payload: {
      hook_event_name: "PostToolUse",
      tool_name: "Read",
      session_id: session,
      cwd: ws.root,
      tool_input: { file_path: ws.at("api/src/tasks.ts") },
      tool_response: { file: { filePath: ws.at("api/src/tasks.ts"), content: TASKS } },
    },
    env: fakeEnv({ HOME: ws.home, CLAUDE_PROJECT_DIR: ws.root, CARRICK_FAKE_FIXTURE: fixturePath("check-silent.json"), ...ON }),
  });

async function until(check: () => boolean, ms = 5000): Promise<void> {
  const end = Date.now() + ms;
  while (!check() && Date.now() < end) await new Promise((r) => setTimeout(r, 25));
  assert.ok(check(), "timed out");
}

const answersOnDisk = (ws: Ws, session: string): string[] =>
  fs.readdirSync(sessionsDir(ws.home)).filter((n) => n.startsWith(`${session}.recheck.`) && n.endsWith(".answer.json"));

test("the hook waits for the daemon under the probe's budget, not a number of its own", async (t) => {
  const ws = await workspace(t);
  const daemon = await fakeDaemon(ws, () => [broken()], 3000);
  t.after(() => daemon.close());
  const out = await edit(ws, "late0");
  assert.equal(daemon.requests[0]!["wait_ms"], probeTimeoutMs());
  assert.ok(out.ms < 3000, `the hook took ${out.ms} ms`);
});

test("an answer ready within the budget prints in the same action", async (t) => {
  const ws = await workspace(t);
  const daemon = await fakeDaemon(ws, () => [broken()], 50);
  t.after(() => daemon.close());
  assert.ok(context((await edit(ws, "late0b")).stdout).includes(BREAK));
});

test("a slow daemon answer is not waited for and is printed by the NEXT hook call, once", async (t) => {
  const ws = await workspace(t);
  const daemon = await fakeDaemon(ws, () => [broken()], 1500);
  t.after(() => daemon.close());
  const first = await edit(ws, "late1");
  assert.ok(!context(first.stdout).includes("this edit breaks"), first.stdout);
  assert.ok(first.ms < 1400, `the hook took ${first.ms} ms`);
  assert.equal(daemon.requests.length, 1);
  assert.ok(typeof daemon.requests[0]!["seq"] === "string", "the ask names its queue entry");
  const heldSeq = String(daemon.requests[0]!["seq"]);
  await until(() => answersOnDisk(ws, "late1").includes(`late1.recheck.${heldSeq}.answer.json`));
  const next = context((await nextHook(ws, "late1")).stdout);
  assert.ok(next.includes(`${BREAK}: Type '{ name: string; }' is not assignable to type the type it expects.`), next);
  assert.ok(!/__C|\bsend\d|\bO\d/.test(next), next);
  assert.ok([...factsSaid("late1", ws.home)].some((key) => key.startsWith("held::")), "T6's claim is taken");
  const again = context((await nextHook(ws, "late1")).stdout);
  assert.ok(!again.includes("this edit breaks"), "said once: " + again);
});

test("a late verdict is not printed twice by two carriers (the claim is shared)", async (t) => {
  const ws = await workspace(t);
  const entry = enqueueHeld(ws.root, "late2", ws.at("api/src/tasks.ts"), Date.now(), { home: ws.home })!;
  answerHeld("late2", entry.seq, { state: "ready", lines: [broken()] }, Date.now(), ws.home);
  const [one, two] = [deliverLate("late2", Date.now(), [], ws.home), deliverLate("late2", Date.now(), [], ws.home)];
  assert.equal(one.filter((c) => c.includes("this edit breaks")).length, 1);
  assert.equal(two.length, 0);
});

test("a newer edit of the same file supersedes an older ask: the older verdict never prints", async (t) => {
  const ws = await workspace(t);
  const file = ws.at("api/src/tasks.ts");
  const older = enqueueHeld(ws.root, "late3", file, 1_000_000, { home: ws.home })!;
  const newer = enqueueHeld(ws.root, "late3", file, 1_000_050, { home: ws.home })!;
  assert.equal(heldSuperseded("late3", older.seq, ws.home), true);
  assert.equal(heldSuperseded("late3", newer.seq, ws.home), false);
  // The older ask answers after all (a daemon already running it); the newer one answers too.
  answerHeld("late3", older.seq, { state: "ready", lines: [broken("Type 'OLD' is not assignable to type 'O1_0'.")] }, Date.now(), ws.home);
  answerHeld("late3", newer.seq, { state: "ready", lines: [broken("Type 'NEW' is not assignable to type 'O1_0'.")] }, Date.now(), ws.home);
  const said = deliverLate("late3", Date.now(), [], ws.home).join("\n");
  assert.ok(said.includes("'NEW'"), said);
  assert.ok(!said.includes("'OLD'"), said);
  // An older answer that lands later still never prints.
  const ancient = enqueueHeld(ws.root, "late3b", file, 1_000_000, { home: ws.home })!;
  enqueueHeld(ws.root, "late3b", file, 1_000_100, { home: ws.home });
  answerHeld("late3b", ancient.seq, { state: "ready", lines: [broken("Type 'OLD' is not assignable to type 'O1_0'.")] }, Date.now(), ws.home);
  assert.deepEqual(deliverLate("late3b", Date.now(), [], ws.home), []);
  assert.deepEqual(finishedFor("late3b", ws.home), []);
});

test("a held ask and part 2's re-check of the same file do not supersede each other", async (t) => {
  const ws = await workspace(t);
  const file = ws.at("api/src/tasks.ts");
  const held = enqueueHeld(ws.root, "late4", file, 1_000_000, { home: ws.home })!;
  enqueue(ws.root, "late4", [file], 1_000_100, { home: ws.home });
  assert.equal(heldSuperseded("late4", held.seq, ws.home), false);
  answerHeld("late4", held.seq, { state: "ready", lines: [broken()] }, Date.now(), ws.home);
  assert.ok(deliverLate("late4", Date.now(), [], ws.home).join("\n").includes("this edit breaks"));
});

test("a held ask is never a pending re-check: Stop does not wait for it or restart a runner", async (t) => {
  const ws = await workspace(t);
  enqueueHeld(ws.root, "late5", ws.at("api/src/tasks.ts"), Date.now(), { home: ws.home });
  assert.deepEqual(pendingFor("late5", ws.home), []);
});

test("the Stop hook delivers a finished held verdict, once (the wake path part 2 uses)", async (t) => {
  const ws = await workspace(t);
  const entry = enqueueHeld(ws.root, "late6", ws.at("api/src/tasks.ts"), Date.now(), { home: ws.home })!;
  answerHeld("late6", entry.seq, { state: "ready", lines: [broken()] }, Date.now(), ws.home);
  const stop = stopAnswers("late6", { home: ws.home, ensure: () => false });
  assert.ok(stop.lines.join("\n").includes("this edit breaks GET /tasks"), stop.lines.join("\n"));
  assert.deepEqual(deliverLate("late6", Date.now(), [], ws.home), [], "the next hook does not say it again");
});

test("an answer with no break says nothing and is not owed again", async (t) => {
  const ws = await workspace(t);
  const entry = enqueueHeld(ws.root, "late7", ws.at("api/src/tasks.ts"), Date.now(), { home: ws.home })!;
  answerHeld("late7", entry.seq, { state: "cold", lines: [] }, Date.now(), ws.home);
  assert.deepEqual(deliverLate("late7", Date.now(), [], ws.home), []);
  assert.deepEqual(finishedFor("late7", ws.home), []);
});

test("switch off: no held entry is filed and the output is today's", async (t) => {
  const ws = await workspace(t);
  const off = await edit(ws, "late8", { CARRICK_CONTRACT_LINES: "1" });
  const entries = fs.readdirSync(sessionsDir(ws.home)).filter((n) => n.startsWith("late8.recheck."));
  for (const name of entries) {
    if (!name.endsWith(".pending.json")) continue;
    assert.ok(!fs.readFileSync(path.join(sessionsDir(ws.home), name), "utf8").includes('"held"'), name);
  }
  assert.ok(!off.stdout.includes("this edit breaks"));
});

// The real daemon.

function daemonWorld(t: { after: (fn: () => void) => void }, engineMs = 0) {
  const dir = fs.realpathSync(fs.mkdtempSync(path.join(os.tmpdir(), "hld-")));
  const root = path.join(dir, "ws");
  const home = path.join(dir, "home");
  const svc = path.join(root, "api");
  fs.mkdirSync(path.join(svc, "src"), { recursive: true });
  fs.writeFileSync(path.join(svc, "src", "a.ts"), "export const a = 1;\n");
  t.after(() => fs.rmSync(dir, { recursive: true, force: true }));
  const init: ServiceInit = { repo: "shop", service: "api", repo_path: root, sidecar_root: svc, tsconfig: null, scan_root: root };
  const asked: string[] = [];
  const engine: ServiceEngine = {
    async answer(_files, request) {
      asked.push(`${request.seq ?? ""}`);
      await new Promise((r) => setTimeout(r, engineMs));
      return { lines: [broken()] };
    },
    rss: () => 100,
    close: async () => undefined,
  };
  const daemon = new HeldDaemon({
    root,
    home,
    env: { ...ON },
    programs: async () => [init],
    runtime: { start: async () => ({ engine }) },
  });
  return { root, home, file: path.join(svc, "src", "a.ts"), daemon, asked };
}

const settle = () => new Promise((r) => setTimeout(r, 40));

test("the daemon files the answer for a request that names its entry, after the asker has gone", async (t) => {
  const w = daemonWorld(t);
  const entry = enqueueHeld(w.root, "d1", w.file, Date.now(), { home: w.home })!;
  const request = { session: "d1", files: [w.file], kind: "edit", seq: entry.seq } as HeldRequest;
  await w.daemon.handle(request); // cold: starts the engine, files an answer with no lines
  await settle();
  const entry2 = enqueueHeld(w.root, "d1", w.file, Date.now() + 10, { home: w.home })!;
  const ready = await w.daemon.handle({ ...request, seq: entry2.seq } as HeldRequest);
  assert.equal(ready.state, "ready");
  const said = deliverLate("d1", Date.now(), [], w.home).join("\n");
  assert.ok(said.includes("this edit breaks GET /tasks"), said);
  await w.daemon.close();
});

test("the daemon drops an ask a newer edit superseded without running it, and files nothing", async (t) => {
  const w = daemonWorld(t, 150);
  const first = enqueueHeld(w.root, "d2", w.file, Date.now(), { home: w.home })!;
  await w.daemon.handle({ session: "d2", files: [w.file], kind: "edit", seq: first.seq } as HeldRequest); // cold
  await settle();
  const running = enqueueHeld(w.root, "d2", w.file, Date.now() + 10, { home: w.home })!;
  const stale = enqueueHeld(w.root, "d2", w.file, Date.now() + 20, { home: w.home })!;
  const newest = enqueueHeld(w.root, "d2", w.file, Date.now() + 30, { home: w.home })!;
  w.asked.length = 0;
  await Promise.all(
    [running, stale, newest].map((entry) => w.daemon.handle({ session: "d2", files: [w.file], kind: "edit", seq: entry.seq } as HeldRequest)),
  );
  assert.deepEqual(w.asked, [newest.seq], "only the newest ask reaches the compiler");
  assert.equal(answersOnDiskFor(w.home, "d2").includes(`d2.recheck.${stale.seq}.answer.json`), false);
  assert.equal(answersOnDiskFor(w.home, "d2").includes(`d2.recheck.${newest.seq}.answer.json`), true);
  await w.daemon.close();
});

function answersOnDiskFor(home: string, session: string): string[] {
  return fs.readdirSync(sessionsDir(home)).filter((n) => n.startsWith(`${session}.recheck.`) && n.endsWith(".answer.json"));
}
