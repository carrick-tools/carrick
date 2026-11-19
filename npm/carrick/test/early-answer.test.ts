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
import { asStated, drainRunner, fakeEnv, fixturePath, runHook, waitForSessionAnswers } from "./helpers.ts";
import { commitRepos } from "./facts-repos.ts";
import { context, lineOf } from "./facts-helpers.ts";
import type { Ws } from "./facts-helpers.ts";
import { buildFacts, factsPath } from "../src/facts/store.ts";
import { sessionsDir } from "../src/hook/reuse.ts";
import { readTimings } from "../src/hook/timings.ts";
import { stopAnswers } from "../src/hook/stop-wait.ts";
import { heldDir, socketPath } from "../src/held/paths.ts";
import { HeldDaemon } from "../src/held/daemon.ts";
import { heldBuildId } from "../src/held/build.ts";
import type { HeldLine, HeldRequest, ServiceEngine, ServiceInit } from "../src/held/daemon.ts";
import type { CheckItem, CheckResult, MovedRow } from "../src/contract.ts";

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
const DETAIL = "This code sends '{ name: string; }'; the caller expects '{ id: string }'.";

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
      const frame = buffer.slice(0, end);
      buffer = buffer.slice(end + 1);
      // The client says hello first; the fake is of the client's own build.
      if ((JSON.parse(frame) as { hello?: boolean }).hello === true) {
        conn.write(`${JSON.stringify({ hello: { build_id: heldBuildId(), pid: process.pid, root: ws.root } })}\n`);
        return;
      }
      const request = JSON.parse(frame) as HeldRequest;
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

/**
 * Part 2's answer for the edited file, as the fake binary prints it to the
 * runner: its judge states every mismatch and moved row as the edit's
 * (`stated`), unless `unstated` (a row it holds but does not state, such as
 * one only the model reads).
 */
function part2(ws: Ws, items: CheckItem[], moved: unknown[] = [], recheck: Record<string, unknown> = {}, unstated = false): Record<string, string> {
  const base: CheckResult = {
    schema: "carrick.check/0",
    file: "src/tasks.ts",
    service: "api",
    stale: false,
    items,
    recheck: { ran: "extraction+types", elapsed_ms: 5, ...(moved.length > 0 ? { moved: moved as MovedRow[] } : {}), ...recheck },
  };
  const result = unstated ? base : asStated(base);
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

test("4b: a full answer that ran nothing never retracts, whatever it holds for the route (deadline, or an older file state)", async (t) => {
  const stored = { state: "resolved", result: "compatible" } as const;
  for (const [name, recheck] of [
    ["deadline", { ran: "none", deadline: true, stale_since: "2026-01-01T00:00:00Z", reason: "the re-scan ran past the budget" }],
    ["older", { ran: "none", stale_since: "2026-01-01T00:00:00Z", reason: "queued behind another check; verdicts are the index's" }],
  ] as const) {
    const ws = await workspace(t);
    await liveDaemon(t, ws, () => [broken(ws)]);
    const first = context((await edit(ws, `e4b-${name}`, part2(ws, [route("/tasks", 2, stored)], [], recheck))).stdout);
    await settled(ws, `e4b-${name}`);
    const next = await carrier(ws, `e4b-${name}`);
    assert.equal(statesBreak([first]), 1, `${name}: ${first}`);
    assert.ok(!next.includes("compatible"), `${name}: ${next}`);
    assert.equal(record(ws)?.held, "part3_only", name);
    assert.equal(record(ws)?.p2_state, "not_judged", name);
  }
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

test("9: a sub-agent's early break goes to the sub-agent; one it never heard goes once to the main session after it ends", async (t) => {
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

const callRow = (after: string, over: Record<string, unknown> = {}) => ({
  file: "ext/src/view4.ts",
  line: 4,
  role: "call",
  method: "GET",
  path: "/tasks",
  before: "compatible",
  after,
  counterparts: after === "no_route" ? [] : ["api src/tasks.ts:2"],
  reason: "the consumer's end of the same route",
  not_judged_before: false,
  ...over,
});

/** The stand-in engine's break on a route in another file of the same service. */
const otherFile = (ws: Ws): HeldLine =>
  broken(ws, {
    row_key: "contract::api::src/other.ts::producer::GET /others::0",
    label: "GET /others",
    row_file: "src/other.ts",
    row_line: 6,
    row_path: ws.at("api/src/other.ts"),
    places: [{ service: "ext", file: "src/view7.ts", line: 7 }],
  });

const otherMoved = (over: Record<string, unknown> = {}) => ({
  file: "api/src/other.ts",
  line: 6,
  role: "route",
  method: "GET",
  path: "/others",
  before: "compatible",
  after: "type_mismatch",
  counterparts: ["ext src/view7.ts:7"],
  reason: "the reply lost name",
  not_judged_before: false,
  ...over,
});

test("B1: a part-2 consumer call row of another kind at the early row's place is printed, and the record is differ", async (t) => {
  const ws = await workspace(t);
  await liveDaemon(t, ws, () => [broken(ws)]);
  const first = context((await edit(ws, "eb1", part2(ws, [], [callRow("no_route")]))).stdout);
  assert.equal(statesBreak([first]), 1, first);
  await settled(ws, "eb1");
  const next = await carrier(ws, "eb1");
  assert.ok(next.includes("ext/src/view4.ts:4") && next.includes("left it with none"), next);
  assert.equal(record(ws)?.held, "differ");
});

test("B1: a call row of the same kind at the place is still a mirror; one of method_mismatch is printed", async (t) => {
  const ws = await workspace(t);
  await liveDaemon(t, ws, () => [broken(ws)]);
  await edit(ws, "eb1b", part2(ws, [], [callRow("method_mismatch")]));
  await settled(ws, "eb1b");
  const next = await carrier(ws, "eb1b");
  assert.ok(next.includes("ext/src/view4.ts:4"), next);
  assert.equal(record(ws)?.held, "differ");
});

test("B2: a part-2 request-side break on the route the early answer flagged is printed, and counts as part2_only", async (t) => {
  const ws = await workspace(t);
  await liveDaemon(t, ws, () => [broken(ws)]);
  const request = { ...SAME, direction: "request" };
  const first = context((await edit(ws, "eb2", part2(ws, [request]))).stdout);
  assert.equal(statesBreak([first]), 1, first);
  await settled(ws, "eb2");
  const next = await carrier(ws, "eb2");
  assert.ok(next.includes("route GET /tasks"), next);
  assert.equal(record(ws)?.held_rows?.part2_only, 1);
  assert.equal(record(ws)?.held_rows?.same, 0);
});

test("a break on a route in another file: printed once, part 2's same row is not said again, the record is same", async (t) => {
  const ws = await workspace(t);
  await liveDaemon(t, ws, () => [otherFile(ws)]);
  const first = context((await edit(ws, "eo1", part2(ws, [], [otherMoved()]))).stdout);
  assert.ok(first.includes("route GET /others"), first);
  await settled(ws, "eo1");
  const next = await carrier(ws, "eo1");
  assert.ok(!next.includes("GET /others"), next);
  assert.equal(record(ws)?.held, "same");
});

test("a break on a route in another file that part 2 holds no row for stands: nothing more, part3_only with p2_state absent", async (t) => {
  const ws = await workspace(t);
  await liveDaemon(t, ws, () => [otherFile(ws)]);
  const first = context((await edit(ws, "eo2", part2(ws, []))).stdout);
  assert.ok(first.includes("route GET /others"), first);
  await settled(ws, "eo2");
  assert.equal(await carrier(ws, "eo2"), "");
  assert.equal(record(ws)?.held, "part3_only");
  assert.equal(record(ws)?.p2_state, "absent");
});

test("N3: Stop counts an early break on a route in another file and never says both sides agree beside it", async (t) => {
  const ws = await workspace(t);
  await liveDaemon(t, ws, () => [otherFile(ws)]);
  await edit(ws, "en3", part2(ws, [route("/tasks", 2, { state: "resolved", result: "compatible" })]));
  await settled(ws, "en3");
  const stop = stopAnswers("en3", { home: ws.home, ensure: () => false }).lines.join("\n");
  assert.ok(stop.includes("1 contract problem"), stop);
  assert.ok(!stop.includes("both sides agree"), stop);
});

test("the full answer landing before the early one: the break prints once, and the daemon's held row is the one record", async (t) => {
  const ws = await workspace(t);
  await liveDaemon(t, ws, () => [broken(ws)], 1500);
  const first = context((await edit(ws, "ef1", part2(ws, [SAME]))).stdout);
  await waitForSessionAnswers(ws.home, "ef1");
  await drainRunner(ws.root);
  assert.ok(!names(ws, "ef1").some((n) => n.endsWith(".early.json")), "the full answer is first");
  await until(() => names(ws, "ef1").some((n) => n.endsWith(".early.json")));
  await until(() => readTimings(ws.root).some((r) => r.hook === "held"));
  const next = await carrier(ws, "ef1");
  assert.equal(statesBreak([first, next]), 1, `${first}\n--\n${next}`);
  const held = readTimings(ws.root).filter((r) => r.hook === "held");
  assert.equal(held.length, 1);
  assert.equal(held[0]?.held, "same");
  assert.equal(readTimings(ws.root).filter((r) => r.held !== undefined).length, 1);
});

test("the full answer landing before the early one and not judging the route: the early break stands and prints once", async (t) => {
  const ws = await workspace(t);
  await liveDaemon(t, ws, () => [broken(ws)], 1500);
  const first = context((await edit(ws, "ef2", part2(ws, []))).stdout);
  await waitForSessionAnswers(ws.home, "ef2");
  await drainRunner(ws.root);
  await until(() => names(ws, "ef2").some((n) => n.endsWith(".early.json")));
  const next = await carrier(ws, "ef2");
  assert.equal(statesBreak([first, next]), 1, `${first}\n--\n${next}`);
  assert.equal(await carrier(ws, "ef2"), "");
  assert.equal(readTimings(ws.root).find((r) => r.hook === "held")?.held, "part3_only");
});

test("N5: the early-answer reader uses the listing it is given and reads the folder no more", async (t) => {
  const ws = await workspace(t);
  await liveDaemon(t, ws, () => [broken(ws)], 900);
  await edit(ws, "en5", { ...part2(ws, []), CARRICK_FAKE_FULL_DELAY_MS: "3000" });
  await until(() => names(ws, "en5").some((n) => n.endsWith(".early.json")));
  const { earlyOwedFor } = await import("../src/hook/rechecks.ts");
  assert.equal(earlyOwedFor("en5", ws.home).length, 1);
  assert.equal(earlyOwedFor("en5", ws.home, []).length, 0, "a given empty listing is believed");
});

test("compareEarly: a call item of another kind at the place is not a mirror; of the same kind it is", async () => {
  const { compareEarly } = await import("../src/hook/confirm.ts");
  const early: CheckResult = {
    schema: "carrick.check/0",
    file: "src/tasks.ts",
    items: [{ ...SAME, counterparts: [{ role: "consumer", service: "ext", file: "src/tasks.ts", line: 9 }], stated: true }],
  };
  const call = (result: string): CheckItem => ({ kind: "call", method: "GET", path: "/tasks", line: 9, source: "fact", verdict: { state: "resolved", result }, stated: true });
  const full = (result: string): CheckResult => ({ schema: "carrick.check/0", file: "src/tasks.ts", items: [call(result)] });
  const other = compareEarly(early, full("no_route"));
  assert.equal(other.mirrors.length, 0);
  assert.equal(other.rows[0]?.outcome, "differ");
  const same = compareEarly(early, full("type_mismatch"));
  assert.equal(same.mirrors.length, 1);
  assert.equal(same.part2Only.length, 0);
});

// A1: part 2 says a break only where the CLI stated it. The same mismatch held
// but not stated (here read off a model-only row) leaves the early break
// standing, as an unresolved row does (R5a), so it still prints and Stop still
// counts it; a stated one is `same`.
test("compareEarly: part 2's same mismatch it did not state leaves the early break standing", async () => {
  const { compareEarly, standingEarly, withEarlyBreaks } = await import("../src/hook/confirm.ts");
  const { problemItems } = await import("../src/contract.ts");
  const early: CheckResult = { schema: "carrick.check/0", file: "src/tasks.ts", items: [{ ...SAME, stated: true }] };
  const unstated: CheckResult = { schema: "carrick.check/0", file: "src/tasks.ts", items: [{ ...SAME, source: "candidate" }], recheck: { ran: "extraction+types", elapsed_ms: 5 } };
  const compared = compareEarly(early, unstated);
  assert.equal(compared.rows[0]?.outcome, "part3_only");
  assert.equal(problemItems(standingEarly(early, unstated)).length, 1, "the early break still prints");
  assert.equal(problemItems(withEarlyBreaks(unstated, early)).length, 1, "and Stop counts it once");
  const stated: CheckResult = { ...unstated, items: [{ ...SAME, stated: true }] };
  assert.equal(compareEarly(early, stated).rows[0]?.outcome, "same");
  assert.equal(problemItems(withEarlyBreaks(stated, early)).length, 1);
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

test("T1: what an answer spoke is committed only when its early answer is filed; a filed one names its service", async (t) => {
  const ws = await workspace(t);
  const svc = ws.at("api");
  const committed: string[] = [];
  let gate: (() => void) | null = null;
  const engine: ServiceEngine = {
    async answer(_files, request) {
      if (request.kind === "edit" && gate === null) await new Promise<void>((r) => (gate = r));
      return { lines: [broken(ws)], commit: () => committed.push(request.seq ?? "") };
    },
    rss: () => 100,
    close: async () => undefined,
  };
  const init: ServiceInit = { repo: "api", service: "api", repo_path: svc, sidecar_root: svc, tsconfig: null, scan_root: svc };
  const daemon = new HeldDaemon({ root: ws.root, home: ws.home, env: { ...ON }, programs: async () => [init], runtime: { start: async () => ({ engine }) } });
  t.after(() => daemon.close());
  const { enqueue, readEarly } = await import("../src/hook/rechecks.ts");
  const file = ws.at("api/src/tasks.ts");
  await daemon.handle({ session: "s1", files: [file], kind: "read" });
  await new Promise((r) => setTimeout(r, 30));
  const [first] = enqueue(ws.root, "s1", [file], Date.now(), { home: ws.home });
  // The first ask computes; a newer edit of the file supersedes it before it is filed.
  const pending = daemon.handle({ session: "s1", files: [file], kind: "edit", seq: first!.seq });
  while (gate === null) await new Promise((r) => setTimeout(r, 5));
  const [second] = enqueue(ws.root, "s1", [file], Date.now() + 10, { home: ws.home });
  (gate as () => void)();
  await pending;
  assert.deepEqual(committed, [], "a superseded answer is never committed");
  await daemon.handle({ session: "s1", files: [file], kind: "edit", seq: second!.seq });
  assert.deepEqual(committed, [second!.seq], "the filed answer is committed once");
  assert.equal(readEarly("s1", second!.seq, ws.home)?.service, "api/api");
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

// L-G1b (P11, measured at step 0): part 2 states a GraphQL resolver as a route
// item, `method` the operation kind and `path` the root field, with the
// resolver's line and no `direction`. The early GraphQL line joins it by label.

const gqlLine = (root: string, file: string, over: Partial<HeldLine> = {}): HeldLine => ({
  row_key: "contract::gateway::src/resolvers.ts::producer::QUERY order::0",
  group: 0,
  label: "QUERY order",
  consumer: "web",
  file: "src/resolvers.ts",
  line: 9,
  message: "Type 'string' is not assignable to type 'O0_0'.",
  row_file: "src/resolvers.ts",
  row_line: 6,
  row_path: path.join(root, file),
  places: [{ service: "web", file: "src/order.ts", line: 4 }],
  expected: "{ order: { id: string; } | null; }",
  ...over,
});

/** Part 2's item for a resolver, in the shape step 0 read. */
const gqlItem = (verdict: CheckItem["verdict"]): CheckItem => ({ kind: "route", method: "QUERY", path: "order", line: 6, source: "fact", verdict });

test("GraphQL: the early QUERY item joins part 2's resolver item by label: same break, compatible (one correction), not checked (stands)", async () => {
  const { earlyResult } = await import("../src/held/early.ts");
  const { compareEarly, itemLabel } = await import("../src/hook/confirm.ts");
  const root = fs.realpathSync(os.tmpdir());
  const early = earlyResult({ file: path.join(root, "src/resolvers.ts"), root }, [gqlLine(root, "src/resolvers.ts")]);
  const item = early.items![0]!;
  assert.deepEqual([item.method, item.path, itemLabel(item), item.line], ["QUERY", "order", "QUERY order", 6]);
  assert.equal(item.expected_type, "{ order: { id: string; } | null; }");
  // Part 2's judge states its mismatch (`stated`), as the CLI writes it.
  const full = (verdict: CheckItem["verdict"]): CheckResult => asStated({ schema: "carrick.check/0", file: "src/resolvers.ts", items: [gqlItem(verdict)], recheck: { ran: "extraction+types", elapsed_ms: 1 } });
  assert.equal(compareEarly(early, full({ state: "resolved", result: "type_mismatch" })).rows[0]?.outcome, "same");
  const fixed = compareEarly(early, full({ state: "resolved", result: "compatible" })).rows[0]!;
  assert.deepEqual([fixed.outcome, fixed.corrected], ["differ", true]);
  const open = compareEarly(early, full({ state: "not_checked", result: null })).rows[0]!;
  assert.deepEqual([open.outcome, open.p2_state], ["part3_only", "not_judged"]);
});

test("GraphQL: a break on a resolver in another file is a moved row that joins part 2's moved row by file and label", async () => {
  const { earlyResult } = await import("../src/held/early.ts");
  const { compareEarly } = await import("../src/hook/confirm.ts");
  const root = fs.realpathSync(os.tmpdir());
  const early = earlyResult({ file: path.join(root, "src/types.ts"), root }, [gqlLine(root, "src/resolvers.ts")]);
  const moved = early.recheck!.moved![0]!;
  assert.deepEqual([moved.file, moved.method, moved.path, moved.after], ["src/resolvers.ts", "QUERY", "order", "type_mismatch"]);
  const full: CheckResult = { schema: "carrick.check/0", file: "src/types.ts", items: [], recheck: { ran: "extraction+types", elapsed_ms: 1, moved: [{ ...moved, reason: "part 2" }] } };
  assert.equal(compareEarly(early, full).rows[0]?.outcome, "same");
});

test("GraphQL through the hook: the early QUERY break prints once through the one renderer; part 2 calling it compatible corrects it once", async (t) => {
  const query = (ws: Ws): HeldLine =>
    broken(ws, { row_key: "contract::api::src/tasks.ts::producer::QUERY task::0", label: "QUERY task", message: "Type 'string' is not assignable to type 'O0_0'." });
  const item = (verdict: CheckItem["verdict"]): CheckItem => ({ kind: "route", method: "QUERY", path: "task", line: 2, source: "fact", counterparts: [{ role: "consumer", service: "ext", file: "src/view4.ts", line: 4 }], verdict });
  const ws = await workspace(t);
  await liveDaemon(t, ws, () => [query(ws)]);
  const first = context((await edit(ws, "eg1", part2(ws, [item({ state: "resolved", result: "compatible" })]))).stdout);
  assert.ok(first.includes("route QUERY task type_mismatch: This code sends 'string'; the caller expects '{ id: string }'."), first);
  assert.ok(!/__C|\bsend\d|\bO\d/.test(first), first);
  await settled(ws, "eg1");
  const next = await carrier(ws, "eg1");
  assert.equal(next.split("\n").filter((line) => line.includes("found QUERY task compatible")).length, 1, next);
  assert.equal(record(ws)?.held, "differ");
});

// --- a break is said once, as the edit's that caused it (T1, carrick#4728) ---

const OTHER = "api/src/other.ts";
const C_FILE = "api/src/c.ts";

/** File one ready early answer for a fresh entry of `rel`, as the daemon does for service `api/api` from engine `engine`. */
async function filed(ws: Ws, session: string, rel: string, lines: HeldLine[], engine = "e1"): Promise<string> {
  const { enqueue, answerEarly } = await import("../src/hook/rechecks.ts");
  const file = ws.at(rel);
  if (!fs.existsSync(file)) fs.writeFileSync(file, "export {};\n");
  const [entry] = enqueue(ws.root, session, [file], Date.now(), { home: ws.home });
  assert.ok(answerEarly(session, entry!.seq, { state: "ready", lines, ms: 3, service: "api/api", engine }, Date.now(), ws.home), `filed ${rel}`);
  return entry!.seq;
}

/** Part 2's answer for entry `seq` of `rel`, as the runner writes it. */
function answered(ws: Ws, session: string, seq: string, rel: string, result: CheckResult): void {
  const at = new Date().toISOString();
  const body = { seq, session, file: ws.at(rel), relative: rel, root: ws.root, state: "answered", enqueued_at: at, started_at: at, finished_at: at, result };
  fs.writeFileSync(path.join(sessionsDir(ws.home), `${session}.recheck.${seq}.answer.json`), `${JSON.stringify(body)}\n`);
}

/** A part-2 answer for `rel` whose only stated row is GET /tasks moved in api/src/tasks.ts. */
function movedTasks(rel: string): CheckResult {
  const moved: MovedRow = { file: "api/src/tasks.ts", line: 2, role: "route", method: "GET", path: "/tasks", before: "compatible", after: "type_mismatch", counterparts: ["ext src/view4.ts:4"], reason: "the reply lost id", not_judged_before: false, stated: true };
  return { schema: "carrick.check/0", file: rel.slice("api/".length), service: "api", stale: false, items: [], recheck: { ran: "extraction+types", elapsed_ms: 5, moved: [moved] } };
}

const run1 = (ws: Ws, over: Partial<HeldLine> = {}): HeldLine => broken(ws, { run: "e1.1", ...over });

test("T1: a break told at its edit is kept, standing, in a later edit's early answer and not printed again", async (t) => {
  const ws = await workspace(t);
  const { deliverLate } = await import("../src/hook/late.ts");
  await filed(ws, "t1a", "api/src/tasks.ts", [run1(ws)]);
  const first = deliverLate("t1a", Date.now(), new Set(), ws.home);
  assert.equal(statesBreak(first), 1, first.join("\n"));
  const later = await filed(ws, "t1a", OTHER, [run1(ws, { standing: true })]);
  const next = deliverLate("t1a", Date.now(), new Set(), ws.home);
  assert.equal(statesBreak(next), 0, next.join("\n"));
  const kept = JSON.parse(fs.readFileSync(path.join(sessionsDir(ws.home), `t1a.recheck.${later}.early.json`), "utf8")) as { service?: string; result: CheckResult };
  assert.deepEqual(kept.result.recheck?.moved?.map((m) => [m.path, m.standing, m.stated]), [["/tasks", true, true]], "the later answer still lists it");
  assert.equal(kept.service, "api/api");
  forgetQueue(ws);
});

test("T1: an earlier edit's unprinted break is said as that edit's, before the answer in hand that lists it standing", async (t) => {
  const ws = await workspace(t);
  await filed(ws, "t1g", OTHER, [run1(ws)]);
  await liveDaemon(t, ws, () => [run1(ws, { standing: true })]);
  const out = context((await edit(ws, "t1g", part2(ws, []))).stdout);
  assert.equal(statesBreak([out]), 1, out);
  const said = out.split("\n\n").find((block) => block.includes("route GET /tasks ")) ?? "";
  assert.ok(said.includes("api/src/other.ts"), `said as the earlier edit's:\n${out}`);
  assert.equal(statesBreak([await carrier(ws, "t1g")]), 0);
  forgetQueue(ws);
});

test("T1: a standing break the session was never told prints once (an edit of the same file before any carrier)", async (t) => {
  const ws = await workspace(t);
  const { deliverLate } = await import("../src/hook/late.ts");
  await filed(ws, "t1b", "api/src/tasks.ts", [run1(ws)]);
  await filed(ws, "t1b", "api/src/tasks.ts", [run1(ws, { standing: true })]);
  const texts = [...deliverLate("t1b", Date.now(), new Set(), ws.home), ...deliverLate("t1b", Date.now(), new Set(), ws.home)];
  assert.equal(statesBreak(texts), 1, texts.join("\n"));
  forgetQueue(ws);
});

test("T1: a part-2 moved row already told is not printed at a later entry", async (t) => {
  const ws = await workspace(t);
  const { deliverLate } = await import("../src/hook/late.ts");
  // The early answer of an edit of tasks.ts states the break in hand.
  await filed(ws, "t1c", "api/src/tasks.ts", [run1(ws)]);
  assert.equal(statesBreak(deliverLate("t1c", Date.now(), new Set(), ws.home)), 1);
  // A later edit of another file got no early answer (cold, say); part 2 states the same break as moved.
  const { enqueue } = await import("../src/hook/rechecks.ts");
  fs.writeFileSync(ws.at(OTHER), "export {};\n");
  const [entry] = enqueue(ws.root, "t1c", [ws.at(OTHER)], Date.now(), { home: ws.home });
  answered(ws, "t1c", entry!.seq, OTHER, movedTasks(OTHER));
  const next = deliverLate("t1c", Date.now(), new Set(), ws.home);
  assert.equal(statesBreak(next), 0, next.join("\n"));
  forgetQueue(ws);
});

test("T1: with no early answer (the switch off), part 2's moved row prints at each entry, as before", async (t) => {
  const ws = await workspace(t);
  const { deliverLate } = await import("../src/hook/late.ts");
  const { enqueue } = await import("../src/hook/rechecks.ts");
  fs.writeFileSync(ws.at(OTHER), "export {};\n");
  fs.writeFileSync(ws.at(C_FILE), "export {};\n");
  const texts: string[] = [];
  for (const rel of [OTHER, C_FILE]) {
    const [entry] = enqueue(ws.root, "t1d", [ws.at(rel)], Date.now(), { home: ws.home });
    answered(ws, "t1d", entry!.seq, rel, movedTasks(rel));
    texts.push(...deliverLate("t1d", Date.now(), new Set(), ws.home));
  }
  assert.equal(statesBreak(texts), 2, texts.join("\n"));
  forgetQueue(ws);
});

test("T1: a break told, fixed in another file, broken again and edited again before a carrier prints again", async (t) => {
  const ws = await workspace(t);
  const { deliverLate } = await import("../src/hook/late.ts");
  await filed(ws, "t1e", "api/src/tasks.ts", [run1(ws)]);
  assert.equal(statesBreak(deliverLate("t1e", Date.now(), new Set(), ws.home)), 1);
  await filed(ws, "t1e", C_FILE, []);
  await filed(ws, "t1e", "api/src/tasks.ts", [broken(ws, { run: "e1.2" })]);
  await filed(ws, "t1e", "api/src/tasks.ts", [broken(ws, { run: "e1.2", standing: true })]);
  const again = deliverLate("t1e", Date.now(), new Set(), ws.home);
  assert.equal(statesBreak(again), 1, again.join("\n"));
  forgetQueue(ws);
});

test("T1: an early break fixed in another file before any carrier is not printed", async (t) => {
  const ws = await workspace(t);
  const { deliverLate } = await import("../src/hook/late.ts");
  await filed(ws, "t1f", "api/src/tasks.ts", [run1(ws)]);
  await filed(ws, "t1f", C_FILE, []);
  const texts = deliverLate("t1f", Date.now(), new Set(), ws.home);
  assert.equal(statesBreak(texts), 0, texts.join("\n"));
  forgetQueue(ws);
});

/** An emit of `task:update` at `line` of api/src/tasks.ts whose payload breaks the listener's type. */
const emitAt = (ws: Ws, line: number, over: Partial<HeldLine> = {}): HeldLine => ({
  row_key: `contract::api::src/tasks.ts::consumer::CLIENT->SERVER task:update::0::line::${line}`,
  group: 0,
  label: "CLIENT->SERVER task:update",
  consumer: "listener",
  file: "src/tasks.ts",
  line,
  message: "Type 'string' is not assignable to type 'O0_0'.",
  row_file: "src/tasks.ts",
  row_line: line,
  row_path: ws.at("api/src/tasks.ts"),
  places: [{ service: "listener", file: "src/listen.ts", line: 3 }],
  expected: "{ id: number }",
  side: "consumer",
  protocol: "socket",
  call_line: line,
  ...over,
});
const emitBreaks = (texts: string[]): number => texts.join("\n").split("\n").filter((line) => line.includes("task:update") && line.includes("type_mismatch")).length;

test("T1: two emits of one event in one file: the second, broken at a later edit, prints and counts beside the first", async (t) => {
  const ws = await workspace(t);
  const { deliverLate } = await import("../src/hook/late.ts");
  const { withEarlyBreaks } = await import("../src/hook/confirm.ts");
  const { readEarly } = await import("../src/hook/rechecks.ts");
  await filed(ws, "t1h", "api/src/tasks.ts", [emitAt(ws, 2, { run: "e1.1" })]);
  assert.equal(emitBreaks(deliverLate("t1h", Date.now(), new Set(), ws.home)), 1);
  const later = await filed(ws, "t1h", OTHER, [emitAt(ws, 2, { run: "e1.1", standing: true }), emitAt(ws, 3, { run: "e1.2" })]);
  const next = deliverLate("t1h", Date.now(), new Set(), ws.home);
  assert.equal(emitBreaks(next), 1, next.join("\n"));
  const early = readEarly("t1h", later, ws.home)!;
  const clean: CheckResult = { schema: "carrick.check/0", file: "src/other.ts", items: [], recheck: { ran: "extraction+types", elapsed_ms: 5 } };
  assert.equal((withEarlyBreaks(clean, early.result, early).items ?? []).length, 2, "Stop counts both");
  forgetQueue(ws);
});

test("T1: a restarted engine's answer is no evidence a break was fixed: it still prints and Stop still counts it", async (t) => {
  const ws = await workspace(t);
  const { deliverLate } = await import("../src/hook/late.ts");
  const { withEarlyBreaks } = await import("../src/hook/confirm.ts");
  const { readEarly } = await import("../src/hook/rechecks.ts");
  const seq = await filed(ws, "t1i", "api/src/tasks.ts", [run1(ws)]);
  // The engine restarted: it re-gated and armed no check that speaks, so its answer lists nothing.
  await filed(ws, "t1i", C_FILE, [], "e2");
  assert.equal(statesBreak(deliverLate("t1i", Date.now(), new Set(), ws.home)), 1);
  const { newestEarlies } = await import("../src/hook/rechecks.ts");
  const newest = newestEarlies("t1i", ws.home).get("api/api");
  const clean: CheckResult = { schema: "carrick.check/0", file: "src/tasks.ts", items: [], recheck: { ran: "extraction+types", elapsed_ms: 5 } };
  assert.equal((withEarlyBreaks(clean, readEarly("t1i", seq, ws.home)!.result, newest).items ?? []).length, 1);
  forgetQueue(ws);
});

test("T1: the differ record counts an entry's own breaks, not the standing ones", async () => {
  const { compareEarly, heldRecord } = await import("../src/hook/confirm.ts");
  const { earlyResult } = await import("../src/held/early.ts");
  const root = fs.realpathSync(os.tmpdir());
  const ws = { root, home: root, at: (rel: string) => path.join(root, rel) } as Ws;
  const early = earlyResult({ file: ws.at("api/src/other.ts"), root }, [run1(ws, { standing: true })]);
  assert.deepEqual(heldRecord(compareEarly(early, undefined)), { held: "none", held_rows: { differ: 0, part3_only: 0, part2_only: 0, same: 0 } });
});
