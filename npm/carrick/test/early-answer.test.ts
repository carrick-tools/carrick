// One entry with an early answer (part 3, B1): part 2's queue entry is filed
// first and the held compiler is asked with its seq; the daemon files
// `<seq>.early.json`; the early break prints once through the one renderer, in
// hand or on the next carrier; when the full answer lands it confirms or
// corrects (P10) and the differ record is written once (P12). The daemon here
// is the real `HeldDaemon` behind the checkout's socket with a stand-in
// engine; part 2 is the real runner over the fake binary. Every name is
// invented.

import test from "node:test";
import assert from "node:assert/strict";
import fs from "node:fs";
import net from "node:net";
import os from "node:os";
import path from "node:path";
import { drainRunner, fakeEnv, fixturePath, runHook, waitForSessionAnswers } from "./helpers.ts";
import { commitRepos } from "./facts-repos.ts";
import { context, lineOf } from "./facts-helpers.ts";
import type { Ws } from "./facts-helpers.ts";
import { buildFacts, factsPath } from "../src/facts/store.ts";
import { sessionsDir } from "../src/hook/reuse.ts";
import { readTimings } from "../src/hook/timings.ts";
import { stopAnswers } from "../src/hook/stop-wait.ts";
import { heldDir, socketPath } from "../src/held/paths.ts";
import { HeldDaemon } from "../src/held/daemon.ts";
import type { HeldLine, HeldRequest, ServiceEngine, ServiceInit } from "../src/held/daemon.ts";
import type { CheckItem, CheckResult } from "../src/contract.ts";

const SLUG = "early-answer-demo";
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
const IN_LIST = "res.json({ id: req.id });";
const ON = { CARRICK_COMPILER_LINES: "1", CARRICK_CONTRACT_LINES: "1" };
const EARLY_HEAD = "Carrick's compiler engine checked api/src/tasks.ts";
const DETAIL = "Type '{ name: string; }' is not assignable to type the type it expects.";

const group = (label: string, file: string, line: number) => ({
  type: A,
  symbol: null,
  home: null,
  places: [{ service: "ext", repo: "ext", file, line, loc: `ext/${file}:${line}` }],
  total: 1,
  shown: 1,
  lead: `Carrick: ${label} is called from `,
  more: "",
  tail: `, which expects: ${A}`,
  shortened: false,
});
const row = (label: string, line: number, name: string, place: [string, number]) => ({
  k: "contract",
  side: "producer",
  protocol: "http",
  label,
  direction: "response",
  service: "api",
  repo: "api",
  file: "src/tasks.ts",
  line,
  name,
  site: null,
  locator: "registration",
  groups: [group(label, place[0], place[1])],
  untyped: [],
  total: 1,
});

async function workspace(t: { after: (fn: () => void | Promise<void>) => void }): Promise<Ws> {
  const root = fs.realpathSync(fs.mkdtempSync(path.join(os.tmpdir(), "carrick-early-")));
  const home = fs.realpathSync(fs.mkdtempSync(path.join(os.tmpdir(), "ea-")));
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
  const rows = [row("GET /tasks", 2, "listTasks", ["src/view4.ts", 4]), row("GET /tasks/:id", 5, "getTask", ["src/view5.ts", 9])];
  const text = `${[header, ...rows.map((r) => JSON.stringify(r))].join("\n")}\n`;
  await buildFacts(commitRepos(root, text, ["api"]), factsPath(root), [{ repo: "api", path: path.join(root, "api") }], new Date().toISOString());
  return { root, home, at: (rel) => path.join(root, rel) };
}

/** The break the stand-in engine reports: GET /tasks, its send on line 3, read by ext at src/view4.ts:4. */
const broken = (ws: Ws, over: Partial<HeldLine> = {}): HeldLine => ({
  row_key: "contract::api::src/tasks.ts::producer::GET /tasks::0",
  group: 0,
  label: "GET /tasks",
  consumer: "ext",
  file: "src/tasks.ts",
  line: 3,
  message: "Type '{ name: string; }' is not assignable to type 'O1_0'.",
  row_file: "src/tasks.ts",
  row_line: 2,
  row_path: ws.at("api/src/tasks.ts"),
  places: [{ service: "ext", file: "src/view4.ts", line: 4 }],
  expected: A,
  ...over,
});

type Live = { requests: HeldRequest[]; close(): Promise<void> };

/** The real daemon for the checkout, warmed, behind its socket; its engine answers `lines()` after `delayMs`. */
async function liveDaemon(t: { after: (fn: () => void | Promise<void>) => void }, ws: Ws, lines: () => HeldLine[], delayMs = 0): Promise<Live> {
  const svc = ws.at("api");
  const init: ServiceInit = { repo: "api", service: "api", repo_path: svc, sidecar_root: svc, tsconfig: null, scan_root: svc };
  const engine: ServiceEngine = {
    async answer(_files, request) {
      await new Promise((r) => setTimeout(r, delayMs));
      return { lines: request.kind === "edit" ? lines() : [] };
    },
    rss: () => 100,
    close: async () => undefined,
  };
  const daemon = new HeldDaemon({ root: ws.root, home: ws.home, env: { ...ON }, programs: async () => [init], runtime: { start: async () => ({ engine }) } });
  await daemon.handle({ session: "warm", files: [ws.at("api/src/tasks.ts")], kind: "read" });
  await new Promise((r) => setTimeout(r, 30));
  fs.mkdirSync(heldDir(ws.home), { recursive: true });
  const requests: HeldRequest[] = [];
  const sockets = new Set<net.Socket>();
  const server = net.createServer((conn) => {
    sockets.add(conn);
    let buffer = "";
    conn.on("error", () => undefined);
    conn.on("data", (chunk) => {
      buffer += chunk.toString("utf8");
      const end = buffer.indexOf("\n");
      if (end < 0) return;
      const request = JSON.parse(buffer.slice(0, end)) as HeldRequest;
      buffer = "";
      requests.push(request);
      void daemon.handle(request).then((answer) => conn.writable && conn.end(`${JSON.stringify(answer)}\n`));
    });
  });
  await new Promise<void>((resolve) => server.listen(socketPath(ws.root, ws.home), resolve));
  const live = {
    requests,
    close: async () => {
      for (const conn of sockets) conn.destroy();
      await new Promise<void>((resolve) => server.close(() => resolve()));
      await daemon.close();
    },
  };
  t.after(() => live.close());
  return live;
}

/** Part 2's answer for the edited file, as the fake binary prints it to the runner. */
function part2(ws: Ws, items: CheckItem[], moved: unknown[] = []): Record<string, string> {
  const result = {
    schema: "carrick.check/0",
    file: "src/tasks.ts",
    service: "api",
    stale: false,
    items,
    recheck: { ran: "extraction+types", elapsed_ms: 5, ...(moved.length > 0 ? { moved } : {}) },
  };
  const file = path.join(ws.home, `part2-${Math.random().toString(36).slice(2)}.json`);
  fs.writeFileSync(file, JSON.stringify(result));
  return { CARRICK_FAKE_FIXTURE: file };
}

const route = (path: string, line: number, verdict: CheckItem["verdict"]): CheckItem => ({
  kind: "route",
  method: "GET",
  path,
  line,
  col: 1,
  source: "fact",
  direction: "response",
  counterparts: [{ role: "consumer", service: "ext", file: "src/view4.ts", line: 4 }],
  verdict,
});
const SAME = route("/tasks", 2, { state: "resolved", result: "type_mismatch", detail: "the reply lost name" });

function edit(ws: Ws, session: string, env: Record<string, string>, agent?: string) {
  const file = ws.at("api/src/tasks.ts");
  return runHook("post-edit.ts", {
    payload: {
      hook_event_name: "PostToolUse",
      tool_name: "Edit",
      session_id: session,
      ...(agent ? { agent_id: agent } : {}),
      cwd: ws.root,
      tool_input: { file_path: file, old_string: "x", new_string: "" },
      tool_response: { structuredPatch: [{ oldStart: lineOf(file, IN_LIST), oldLines: 0, newLines: 0, newStart: lineOf(file, IN_LIST), lines: [`+  ${IN_LIST}`] }] },
    },
    env: fakeEnv({ HOME: ws.home, CLAUDE_PROJECT_DIR: ws.root, CARRICK_FAKE_FIXTURE: fixturePath("check-silent.json"), ...ON, ...env }),
  });
}

/** The next carrier of `session`: a shell command that writes nothing. */
async function carrier(ws: Ws, session: string, agent?: string): Promise<string> {
  const run = await runHook("post-bash.ts", {
    payload: { hook_event_name: "PostToolUse", tool_name: "Bash", session_id: session, ...(agent ? { agent_id: agent } : {}), cwd: ws.root, tool_input: { command: "ls" }, tool_response: { stdout: "" } },
    env: fakeEnv({ HOME: ws.home, CLAUDE_PROJECT_DIR: ws.root, CARRICK_FAKE_FIXTURE: fixturePath("check-silent.json"), ...ON }),
  });
  return context(run.stdout);
}

/** Forget the queue entries a test filed with no runner to take them, so the clean-up does not wait for one. */
const forgetQueue = (ws: Ws): void => fs.rmSync(path.join(ws.root, ".carrick", "rechecks", "queue"), { recursive: true, force: true });

const names = (ws: Ws, key: string): string[] =>
  fs.existsSync(sessionsDir(ws.home)) ? fs.readdirSync(sessionsDir(ws.home)).filter((n) => n.startsWith(`${key}.recheck.`)) : [];

async function until(check: () => boolean, ms = 8000): Promise<void> {
  const end = Date.now() + ms;
  while (!check() && Date.now() < end) await new Promise((r) => setTimeout(r, 25));
  assert.ok(check(), "timed out");
}

/** How many printed texts state the early GET /tasks break. */
const statesBreak = (texts: string[]): number => texts.join("\n").split("\n").filter((line) => line.includes("route GET /tasks ") && line.includes("type_mismatch")).length;

/** The differ record for the session's one entry, from whichever row carries it. */
function record(ws: Ws): { held?: string; p2_state?: string; held_rows?: Record<string, number> } | undefined {
  return readTimings(ws.root).find((r) => r.held !== undefined);
}

async function settled(ws: Ws, session: string): Promise<void> {
  await waitForSessionAnswers(ws.home, session);
  await drainRunner(ws.root);
}

test("1: an edit with a fact file files exactly one entry; the daemon's answer is <seq>.early.json, no answer.json, no .delivered", async (t) => {
  const ws = await workspace(t);
  const live = await liveDaemon(t, ws, () => [broken(ws)]);
  await edit(ws, "e1", { CARRICK_FAKE_FULL_DELAY_MS: "3000" });
  const files = names(ws, "e1");
  const pending = files.filter((n) => n.endsWith(".pending.json"));
  assert.equal(pending.length, 1, files.join(" "));
  const seq = pending[0]!.slice("e1.recheck.".length, -".pending.json".length);
  assert.equal(live.requests.at(-1)?.seq, seq, "the ask carries part 2's entry");
  assert.ok(files.includes(`e1.recheck.${seq}.early.json`), files.join(" "));
  assert.ok(!files.some((n) => n.endsWith(".answer.json")), files.join(" "));
  assert.ok(!files.some((n) => n.endsWith(".delivered")), files.join(" "));
});

test("2a: an early break ready within the wait prints once, in hand, through the one renderer", async (t) => {
  const ws = await workspace(t);
  await liveDaemon(t, ws, () => [broken(ws)]);
  const first = context((await edit(ws, "e2a", part2(ws, []))).stdout);
  assert.ok(first.includes(`${EARLY_HEAD} after this edit: 1 contract problem.`), first);
  assert.ok(first.includes(`- api/src/tasks.ts:2:1 route GET /tasks type_mismatch: ${DETAIL}`), first);
  assert.ok(!/__C|\bsend\d|\bO\d/.test(first), first);
  await settled(ws, "e2a");
  const next = await carrier(ws, "e2a");
  assert.equal(statesBreak([first, next]), 1, next);
});

test("2b: an early break after the wait is not waited for; the next carrier prints it once", async (t) => {
  const ws = await workspace(t);
  await liveDaemon(t, ws, () => [broken(ws)], 900);
  const first = await edit(ws, "e2b", { ...part2(ws, []), CARRICK_FAKE_FULL_DELAY_MS: "3000" });
  assert.ok(first.ms < 900 + 600, `the hook took ${first.ms} ms`);
  assert.equal(statesBreak([context(first.stdout)]), 0, first.stdout);
  await until(() => names(ws, "e2b").some((n) => n.endsWith(".early.json")));
  const next = await carrier(ws, "e2b");
  assert.ok(next.includes(`${EARLY_HEAD} after your earlier edit to it: 1 contract problem.`), next);
  const again = await carrier(ws, "e2b");
  assert.equal(statesBreak([next, again]), 1, again);
});

test("3: the full answer states the same break: nothing more prints; the record is same", async (t) => {
  const ws = await workspace(t);
  await liveDaemon(t, ws, () => [broken(ws)]);
  const first = context((await edit(ws, "e3", part2(ws, [SAME]))).stdout);
  await settled(ws, "e3");
  const next = await carrier(ws, "e3");
  assert.equal(statesBreak([first, next]), 1, next);
  assert.ok(!next.includes("GET /tasks"), next);
  assert.equal(record(ws)?.held, "same");
});

test("4: the full answer calls the route compatible: one correction line; the record is differ", async (t) => {
  const ws = await workspace(t);
  await liveDaemon(t, ws, () => [broken(ws)]);
  await edit(ws, "e4", part2(ws, [route("/tasks", 2, { state: "resolved", result: "compatible" })]));
  await settled(ws, "e4");
  const next = await carrier(ws, "e4");
  const corrections = next.split("\n").filter((line) => line.includes("found GET /tasks compatible"));
  assert.equal(corrections.length, 1, next);
  assert.equal(record(ws)?.held, "differ");
});

test("5: the full answer leaves the route unresolved, or holds no row for it: nothing more; the record is part3_only with p2_state", async (t) => {
  for (const [name, items, state] of [
    ["unresolved", [route("/tasks", 2, { state: "unresolved", result: null })], "unresolved"],
    ["absent", [], "absent"],
  ] as const) {
    const ws = await workspace(t);
    await liveDaemon(t, ws, () => [broken(ws)]);
    const first = context((await edit(ws, `e5-${name}`, part2(ws, [...items]))).stdout);
    await settled(ws, `e5-${name}`);
    const next = await carrier(ws, `e5-${name}`);
    assert.equal(statesBreak([first]), 1, `${name}: ${first}`);
    assert.equal(next, "", `${name}: ${next}`);
    assert.equal(record(ws)?.held, "part3_only", name);
    assert.equal(record(ws)?.p2_state, state, name);
  }
});

test("6: the full answer has a break the early answer lacked: printed; the record is part2_only", async (t) => {
  const ws = await workspace(t);
  await liveDaemon(t, ws, () => [broken(ws)]);
  const other = route("/tasks/:id", 5, { state: "resolved", result: "type_mismatch", detail: "the reply lost id" });
  await edit(ws, "e6", part2(ws, [SAME, other]));
  await settled(ws, "e6");
  const next = await carrier(ws, "e6");
  assert.ok(next.includes("route GET /tasks/:id"), next);
  assert.ok(!next.split("\n").some((line) => line.includes("route GET /tasks ")), next);
  assert.equal(record(ws)?.held, "part2_only");
  assert.deepEqual(record(ws)?.held_rows, { differ: 0, part3_only: 0, part2_only: 1, same: 1 });
});

test("6b: a break of another kind on the same route is printed, and the record is differ (a disagreement is never hidden)", async (t) => {
  const ws = await workspace(t);
  await liveDaemon(t, ws, () => [broken(ws)]);
  await edit(ws, "e6b", part2(ws, [route("/tasks", 2, { state: "resolved", result: "method_mismatch" })]));
  await settled(ws, "e6b");
  const next = await carrier(ws, "e6b");
  assert.ok(next.includes("route GET /tasks [fact] method_mismatch"), next);
  assert.equal(record(ws)?.held, "differ");
});

test("7: part 2's moved call row at a place of the early row is not printed again (place identity)", async (t) => {
  const ws = await workspace(t);
  await liveDaemon(t, ws, () => [broken(ws)]);
  const mirror = {
    file: "ext/src/view4.ts",
    line: 4,
    role: "call",
    method: "GET",
    path: "/tasks",
    before: "compatible",
    after: "type_mismatch",
    counterparts: ["api src/tasks.ts:2"],
    reason: "the reply lost name",
    not_judged_before: false,
  };
  await edit(ws, "e7", part2(ws, [SAME], [mirror]));
  await settled(ws, "e7");
  const next = await carrier(ws, "e7");
  assert.ok(!next.includes("ext/src/view4.ts"), next);
  assert.equal(next, "", next);
});

test("8: Stop counts the early break in its header and never names that route as agreeing", async (t) => {
  const ws = await workspace(t);
  await liveDaemon(t, ws, () => [broken(ws)]);
  const agreed = route("/tasks/:id", 5, { state: "resolved", result: "compatible" });
  await edit(ws, "e8", part2(ws, [route("/tasks", 2, { state: "unresolved", result: null }), agreed]));
  await settled(ws, "e8");
  const stop = stopAnswers("e8", { home: ws.home, ensure: () => false }).lines.join("\n");
  assert.ok(stop.includes("1 contract problem"), stop);
  assert.ok(!stop.includes("both sides agree"), stop);
});

// Needs a sub-agent's edit to take the fact-file path (carrick#4615): until it
// lands a sub-agent's edit is probed in hand and asks the daemon nothing. Drop
// the `todo` with the rebase onto it.
test("9: a sub-agent's early break goes to the sub-agent; one it never heard goes once to the main session after it ends", { todo: "waits on carrick#4615" }, async (t) => {
  const ws = await workspace(t);
  await liveDaemon(t, ws, () => [broken(ws)]);
  const inHand = context((await edit(ws, "e9", part2(ws, []), "a1")).stdout);
  assert.equal(statesBreak([inHand]), 1, inHand);
  // A second sub-agent whose early answer lands after its last hook.
  const ws2 = await workspace(t);
  await liveDaemon(t, ws2, () => [broken(ws2)], 900);
  await edit(ws2, "e9b", { ...part2(ws2, []), CARRICK_FAKE_FULL_DELAY_MS: "3000" }, "a2");
  await until(() => names(ws2, "e9b.a2").some((n) => n.endsWith(".early.json")));
  await runHook("subagent-stop.ts", {
    payload: { hook_event_name: "SubagentStop", session_id: "e9b", agent_id: "a2", cwd: ws2.root },
    env: fakeEnv({ HOME: ws2.home, CLAUDE_PROJECT_DIR: ws2.root }),
  });
  const main = await carrier(ws2, "e9b");
  const again = await carrier(ws2, "e9b");
  assert.equal(statesBreak([main, again]), 1, `${main}\n--\n${again}`);
});

test("10: a dispatch row's early item carries the label's parts, so its item label is the fact row's label", async () => {
  const { earlyResult } = await import("../src/held/early.ts");
  const { itemLabel } = await import("../src/hook/confirm.ts");
  const root = fs.realpathSync(os.tmpdir());
  const file = path.join(root, "svc", "src", "events.ts");
  const label = "POST /events {type=refund}";
  const result: CheckResult = earlyResult({ file, root }, [
    {
      row_key: "k",
      group: 0,
      label,
      consumer: "web",
      file: "src/events.ts",
      line: 9,
      message: "Type 'number' is not assignable to type 'O3_0'.",
      row_file: "src/events.ts",
      row_line: 7,
      row_path: file,
      places: [{ service: "web", file: "src/pay.ts", line: 2 }],
      expected: "{ ok: boolean }",
    },
  ]);
  const item = result.items![0]!;
  assert.equal(item.dispatch_label, " {type=refund}");
  assert.equal(itemLabel(item), label);
  assert.equal(item.line, 7, "the row's line, never the send's");
});

// Re-homed from the held lane's tests: supersede, the shared claim, switch off.

test("a newer entry of the same file supersedes an older ask: only the newest reaches the compiler, and only it is filed", async (t) => {
  const ws = await workspace(t);
  const svc = ws.at("api");
  const asked: string[] = [];
  const engine: ServiceEngine = {
    async answer(_files, request) {
      asked.push(request.seq ?? "");
      await new Promise((r) => setTimeout(r, 150));
      return { lines: [broken(ws)] };
    },
    rss: () => 100,
    close: async () => undefined,
  };
  const init: ServiceInit = { repo: "api", service: "api", repo_path: svc, sidecar_root: svc, tsconfig: null, scan_root: svc };
  const daemon = new HeldDaemon({ root: ws.root, home: ws.home, env: { ...ON }, programs: async () => [init], runtime: { start: async () => ({ engine }) } });
  t.after(() => daemon.close());
  const { enqueue } = await import("../src/hook/rechecks.ts");
  const file = ws.at("api/src/tasks.ts");
  await daemon.handle({ session: "s1", files: [file], kind: "read" });
  await new Promise((r) => setTimeout(r, 30));
  const [running] = enqueue(ws.root, "s1", [file], Date.now(), { home: ws.home });
  const [stale] = enqueue(ws.root, "s1", [file], Date.now() + 10, { home: ws.home });
  const [newest] = enqueue(ws.root, "s1", [file], Date.now() + 20, { home: ws.home });
  asked.length = 0;
  await Promise.all([running!, stale!, newest!].map((entry) => daemon.handle({ session: "s1", files: [file], kind: "edit", seq: entry.seq })));
  assert.deepEqual(asked, [newest!.seq]);
  const early = names(ws, "s1").filter((n) => n.endsWith(".early.json"));
  assert.deepEqual(early, [`s1.recheck.${newest!.seq}.early.json`]);
  forgetQueue(ws);
});

test("two carriers at once print an early answer once (the claim is shared)", async (t) => {
  const ws = await workspace(t);
  const { enqueue, answerEarly } = await import("../src/hook/rechecks.ts");
  const { deliverLate } = await import("../src/hook/late.ts");
  const [entry] = enqueue(ws.root, "s2", [ws.at("api/src/tasks.ts")], Date.now(), { home: ws.home });
  assert.ok(answerEarly("s2", entry!.seq, { state: "ready", lines: [broken(ws)], ms: 3 }, Date.now(), ws.home));
  const [one, two] = [deliverLate("s2", Date.now(), new Set(), ws.home), deliverLate("s2", Date.now(), new Set(), ws.home)];
  assert.equal(statesBreak(one), 1, one.join("\n"));
  assert.equal(statesBreak(two), 0, two.join("\n"));
  forgetQueue(ws);
});

test("an early answer with no break is filed, never printed, and never reads as answered", async (t) => {
  const ws = await workspace(t);
  const { enqueue, answerEarly, pendingFor, newestPerFile } = await import("../src/hook/rechecks.ts");
  const { deliverLate } = await import("../src/hook/late.ts");
  const [entry] = enqueue(ws.root, "s3", [ws.at("api/src/tasks.ts")], Date.now(), { home: ws.home });
  assert.ok(answerEarly("s3", entry!.seq, { state: "ready", lines: [], ms: 3 }, Date.now(), ws.home));
  assert.equal(answerEarly("s3", entry!.seq, { state: "cold", lines: [], ms: 3 }, Date.now(), ws.home), false, "only a ready answer is filed");
  assert.deepEqual(deliverLate("s3", Date.now(), new Set(), ws.home), []);
  assert.equal(pendingFor("s3", ws.home).length, 1, "still pending");
  assert.equal(newestPerFile("s3", ws.home).get(ws.at("api/src/tasks.ts"))?.answer, null);
  assert.ok(!names(ws, "s3").some((n) => n.endsWith(".early.said")), "never claimed");
  forgetQueue(ws);
});

test("switch off: nothing is asked, no early answer is filed", async (t) => {
  const ws = await workspace(t);
  const live = await liveDaemon(t, ws, () => [broken(ws)]);
  await edit(ws, "s4", { CARRICK_COMPILER_LINES: "" });
  assert.equal(live.requests.length, 0);
  assert.ok(!names(ws, "s4").some((n) => n.includes(".early.")), names(ws, "s4").join(" "));
});
