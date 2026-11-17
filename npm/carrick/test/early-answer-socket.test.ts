// HM4 T4: a socket emit's early break through the hook. The edited side is the
// consumer (the emitter), so the early answer states a `call` item at the
// emit's line; part 2's call item at the same line, method, response side and
// label confirms it, and part 2 calling it compatible corrects it once. The
// daemon is the real `HeldDaemon` behind the checkout's socket with a stand-in
// engine; part 2 is the real runner over the fake binary. Every name is
// invented: the storefront emits `order:update`, `orders-api` listens.

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
import { readTimings } from "../src/hook/timings.ts";
import { heldDir, socketPath } from "../src/held/paths.ts";
import { HeldDaemon } from "../src/held/daemon.ts";
import { heldBuildId } from "../src/held/build.ts";
import { earlyResult } from "../src/held/early.ts";
import { compareEarly } from "../src/hook/confirm.ts";
import type { HeldLine, ServiceEngine, ServiceInit } from "../src/held/daemon.ts";
import type { CheckItem, CheckResult } from "../src/contract.ts";

const SLUG = "early-socket-demo";
const LIVE = `import type { OrderUpdate } from "./live-types";
declare const socket: { emit(event: string, payload?: unknown): boolean };
export function sendUpdate(orderId: string): void {
  const first: OrderUpdate = { orderId, status: "paid" };
  socket.emit("order:update", first);
}
export function sendAgain(orderId: string): void {
  socket.emit("order:update", { orderId, status: "open" });
}
`;
const EMIT = 'socket.emit("order:update", first);';
const LISTENER = "{ orderId: string; } & { orderId: string; status: string; }";
const ON = { CARRICK_COMPILER_LINES: "1", CARRICK_CONTRACT_LINES: "1" };
/** The compiler's message as the real held engine gave it for this break (step 0, TS7). */
const MESSAGE =
  "Type '{ orderId: string; status: number; }' is not assignable to type 'O0_0'.\n  Type '{ orderId: string; status: number; }' is not assignable to type '{ orderId: string; status: string; }'.\n    Types of property 'status' are incompatible.\n      Type 'number' is not assignable to type 'string'.";
const PRINTED = "- web/src/live.ts:5:1 call CLIENT->SERVER order:update type_mismatch: 'status' has a different type. This code sends 'number'; the listener expects 'string'. Producers: orders-api src/realtime.ts:11";

const emitterRow = (line: number) => ({
  k: "contract",
  side: "consumer",
  protocol: "socket",
  label: "CLIENT->SERVER order:update",
  direction: "response",
  service: "web",
  repo: "web",
  file: "src/live.ts",
  line,
  name: null,
  site: null,
  locator: "call",
  groups: [{ type: LISTENER, symbol: null, home: null, places: [{ service: "orders-api", repo: "orders-api", file: "src/realtime.ts", line: 11, loc: "orders-api/src/realtime.ts:11" }], total: 1, shown: 1, lead: "", more: "", tail: "", shortened: false }],
  untyped: [],
  total: 1,
});

async function workspace(t: { after: (fn: () => void | Promise<void>) => void }): Promise<Ws> {
  const root = fs.realpathSync(fs.mkdtempSync(path.join(os.tmpdir(), "carrick-early-socket-")));
  const home = fs.realpathSync(fs.mkdtempSync(path.join(os.tmpdir(), "eas-")));
  t.after(async () => {
    await drainRunner(root);
    fs.rmSync(root, { recursive: true, force: true });
    fs.rmSync(home, { recursive: true, force: true });
  });
  fs.mkdirSync(path.join(root, "web", "src"), { recursive: true });
  fs.writeFileSync(path.join(root, "web", "src", "live.ts"), LIVE);
  fs.mkdirSync(path.join(root, ".carrick", "hosted"), { recursive: true });
  fs.writeFileSync(path.join(root, ".carrick", "hosted", "snapshot.json"), JSON.stringify({ resolution: { repos: [{ project_slug: SLUG }] } }));
  const header = JSON.stringify({
    schema: 1,
    writer: 4,
    project_id: "p",
    project_slug: SLUG,
    digest: "d1",
    built_at: "2026-01-01T00:00:00Z",
    repos: [{ repo: "web", service: "web", commit: "1" }],
    counts: { rows: 0, producers: 0, consumers: 0 },
  });
  const text = `${[header, JSON.stringify(emitterRow(5)), JSON.stringify(emitterRow(8))].join("\n")}\n`;
  await buildFacts(commitRepos(root, text, ["web"]), factsPath(root), [{ repo: "web", path: path.join(root, "web") }], new Date().toISOString());
  return { root, home, at: (rel) => path.join(root, rel) };
}

/** The emit break the stand-in engine reports, as the runtime states a consumer line. */
const broken = (ws: Ws, callLine = 5): HeldLine => ({
  row_key: `contract::web::src/live.ts::consumer::CLIENT->SERVER order:update::0::line::${callLine}`,
  group: 0,
  label: "CLIENT->SERVER order:update",
  consumer: "orders-api",
  file: "src/live.ts",
  line: callLine,
  message: MESSAGE.replace("O0_0", `O${callLine}_0`),
  row_file: "src/live.ts",
  row_line: callLine,
  row_path: ws.at("web/src/live.ts"),
  places: [{ service: "orders-api", file: "src/realtime.ts", line: 11 }],
  expected: LISTENER,
  side: "consumer",
  protocol: "socket",
  call_line: callLine,
});

async function liveDaemon(t: { after: (fn: () => void | Promise<void>) => void }, ws: Ws, lines: () => HeldLine[]): Promise<void> {
  const svc = ws.at("web");
  const init: ServiceInit = { repo: "web", service: "web", repo_path: svc, sidecar_root: svc, tsconfig: null, scan_root: svc };
  const engine: ServiceEngine = {
    async answer(_files, request) {
      return { lines: request.kind === "edit" ? lines() : [] };
    },
    rss: () => 100,
    close: async () => undefined,
  };
  const daemon = new HeldDaemon({ root: ws.root, home: ws.home, env: { ...ON }, programs: async () => [init], runtime: { start: async () => ({ engine }) } });
  await daemon.handle({ session: "warm", files: [ws.at("web/src/live.ts")], kind: "read" });
  await new Promise((r) => setTimeout(r, 30));
  fs.mkdirSync(heldDir(ws.home), { recursive: true });
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
      if ((JSON.parse(frame) as { hello?: boolean }).hello === true) {
        conn.write(`${JSON.stringify({ hello: { build_id: heldBuildId(), pid: process.pid, root: ws.root } })}\n`);
        return;
      }
      void daemon.handle(JSON.parse(frame)).then((answer) => conn.writable && conn.end(`${JSON.stringify(answer)}\n`));
    });
  });
  await new Promise<void>((resolve) => server.listen(socketPath(ws.root, ws.home), resolve));
  t.after(async () => {
    for (const conn of sockets) conn.destroy();
    await new Promise<void>((resolve) => server.close(() => resolve()));
    await daemon.close();
  });
}

/**
 * Part 2's call item for the emit, in the shape the re-check states it on a
 * message row (step 0, carrick#4770): the emitter's payload in `actual_type`,
 * the listener's type in `expected_type`, `sent_by: "consumer"`.
 */
const callItem = (line: number, verdict: CheckItem["verdict"]): CheckItem => ({
  kind: "call",
  method: "CLIENT->SERVER",
  path: "order:update",
  line,
  source: "fact",
  evidence: "library_claim",
  resolution_source: "library_claim",
  counterparts: [{ role: "producer", service: "orders-api", file: "src/realtime.ts", line: 11 }],
  verdict,
  ...(verdict?.result === "type_mismatch" ? { direction: "response", actual_type: "{ orderId: string; status: number; }", expected_type: "OrderUpdate", sent_by: "consumer" as const } : {}),
});
const MISMATCH = { state: "resolved", result: "type_mismatch", detail: "Types of property 'status' are incompatible." } as const;

function part2(ws: Ws, items: CheckItem[]): Record<string, string> {
  const result = asStated({ schema: "carrick.check/0", file: "src/live.ts", service: "web", stale: false, items, recheck: { ran: "extraction+types", elapsed_ms: 5 } });
  const file = path.join(ws.home, `part2-${Math.random().toString(36).slice(2)}.json`);
  fs.writeFileSync(file, JSON.stringify(result));
  return { CARRICK_FAKE_FIXTURE: file };
}

function edit(ws: Ws, session: string, env: Record<string, string>) {
  const file = ws.at("web/src/live.ts");
  return runHook("post-edit.ts", {
    payload: {
      hook_event_name: "PostToolUse",
      tool_name: "Edit",
      session_id: session,
      cwd: ws.root,
      tool_input: { file_path: file, old_string: "x", new_string: "" },
      tool_response: { structuredPatch: [{ oldStart: lineOf(file, EMIT), oldLines: 0, newLines: 0, newStart: lineOf(file, EMIT), lines: [`+  ${EMIT}`] }] },
    },
    env: fakeEnv({ HOME: ws.home, CLAUDE_PROJECT_DIR: ws.root, CARRICK_FAKE_FIXTURE: fixturePath("check-silent.json"), ...ON, ...env }),
  });
}

async function carrier(ws: Ws, session: string): Promise<string> {
  const run = await runHook("post-bash.ts", {
    payload: { hook_event_name: "PostToolUse", tool_name: "Bash", session_id: session, cwd: ws.root, tool_input: { command: "ls" }, tool_response: { stdout: "" } },
    env: fakeEnv({ HOME: ws.home, CLAUDE_PROJECT_DIR: ws.root, CARRICK_FAKE_FIXTURE: fixturePath("check-silent.json"), ...ON }),
  });
  return context(run.stdout);
}

async function settled(ws: Ws, session: string): Promise<void> {
  await waitForSessionAnswers(ws.home, session);
  await drainRunner(ws.root);
}

const record = (ws: Ws) => readTimings(ws.root).find((r) => r.held !== undefined);
const statesBreak = (texts: string[]): number => texts.join("\n").split("\n").filter((line) => line.includes("call CLIENT->SERVER order:update") && line.includes("type_mismatch")).length;

test("socket through the hook: the emit break prints once through the one renderer; part 2's call item at the same line confirms it", async (t) => {
  const ws = await workspace(t);
  await liveDaemon(t, ws, () => [broken(ws)]);
  const first = context((await edit(ws, "s1", part2(ws, [callItem(5, MISMATCH)]))).stdout);
  assert.ok(first.includes("Carrick's compiler engine checked web/src/live.ts after this edit: 1 contract problem."), first);
  assert.ok(first.includes(PRINTED), first);
  assert.ok(!/__C|\bsend\d|\bO\d/.test(first), first);
  await settled(ws, "s1");
  const next = await carrier(ws, "s1");
  assert.equal(statesBreak([first, next]), 1, next);
  assert.equal(record(ws)?.held, "same");
});

test("socket through the hook: part 2 calling the emit compatible corrects it once, naming the call's place", async (t) => {
  const ws = await workspace(t);
  await liveDaemon(t, ws, () => [broken(ws)]);
  const first = context((await edit(ws, "s2", part2(ws, [callItem(5, { state: "resolved", result: "compatible" })]))).stdout);
  assert.equal(statesBreak([first]), 1, first);
  await settled(ws, "s2");
  const next = await carrier(ws, "s2");
  const corrections = next.split("\n").filter((line) => line.includes("found CLIENT->SERVER order:update at web/src/live.ts:5 compatible"));
  assert.equal(corrections.length, 1, next);
  assert.equal(record(ws)?.held, "differ");
});

test("socket through the hook: part 2 holding no call at the emit's line leaves the early break standing (part3_only, absent)", async (t) => {
  const ws = await workspace(t);
  await liveDaemon(t, ws, () => [broken(ws)]);
  const first = context((await edit(ws, "s3", part2(ws, [callItem(8, MISMATCH)]))).stdout);
  assert.equal(statesBreak([first]), 1, first);
  await settled(ws, "s3");
  assert.equal(record(ws)?.held, "part3_only");
  assert.equal(record(ws)?.p2_state, "absent");
});

test("the early socket item states the same fields as the full re-check's item", () => {
  const root = fs.realpathSync(os.tmpdir());
  const ws: Ws = { root, home: root, at: (rel) => path.join(root, rel) };
  const early = earlyResult({ file: ws.at("web/src/live.ts"), root }, [broken(ws)]);
  early.file = "src/live.ts";
  const own = early.items![0]!;
  const full = callItem(5, MISMATCH);
  const pick = (item: CheckItem) => ({
    kind: item.kind,
    method: item.method,
    path: item.path,
    line: item.line,
    direction: item.direction,
    sent_by: item.sent_by,
    verdict: [item.verdict?.state, item.verdict?.result],
    counterparts: item.counterparts?.map((c) => [c.role, c.service, c.file, c.line]),
  });
  assert.deepEqual(pick(own), pick(full));
  // The listener's type is in `expected_type` on both; the emitter's own (the edited side) is part 2's alone.
  assert.equal(own.expected_type, LISTENER);
  assert.equal(own.actual_type, undefined);
  assert.equal(full.expected_type, "OrderUpdate");
  assert.equal(compareEarly(early, asStated({ schema: "carrick.check/0", file: "src/live.ts", items: [full], recheck: { ran: "extraction+types", elapsed_ms: 1 } })).rows[0]?.outcome, "same");
});

test("two emits of one event give two early items, each joined to its own re-check item", () => {
  const root = fs.realpathSync(os.tmpdir());
  const ws: Ws = { root, home: root, at: (rel) => path.join(root, rel) };
  const early = earlyResult({ file: ws.at("web/src/live.ts"), root }, [broken(ws, 5), broken(ws, 8)]);
  early.file = "src/live.ts";
  assert.deepEqual(early.items!.map((i) => i.line), [5, 8]);
  const full = (second: CheckItem["verdict"]): CheckResult =>
    asStated({ schema: "carrick.check/0", file: "src/live.ts", items: [callItem(5, MISMATCH), callItem(8, second)], recheck: { ran: "extraction+types", elapsed_ms: 1 } });
  const both = compareEarly(early, full(MISMATCH));
  assert.deepEqual(both.rows.map((r) => [r.row.key.split("\0").at(-1), r.outcome, (r.match as CheckItem | null)?.line]), [["5", "same", 5], ["8", "same", 8]]);
  assert.equal(both.part2Only.length, 0);
  // As step 0 measured on the re-check: it judges only the first emit of an event, so the second stands, not judged.
  const firstOnly = compareEarly(early, full({ state: "not_checked", result: null }));
  assert.deepEqual(firstOnly.rows.map((r) => [r.outcome, r.p2_state]), [["same", undefined], ["part3_only", "not_judged"]]);
});
