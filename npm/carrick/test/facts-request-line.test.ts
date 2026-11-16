// The request-direction contract line (`facts/lines.ts`): worded request rows
// print one line each, after every response line, from tiers 1 and 2 only.
// Every service, file and type here is invented.

import test from "node:test";
import assert from "node:assert/strict";
import fs from "node:fs";
import os from "node:os";
import path from "node:path";
import { drainRunner } from "./helpers.ts";
import { commitRepos } from "./facts-repos.ts";
import type { Ws } from "./facts-helpers.ts";
import { buildFacts, factsPath } from "../src/facts/store.ts";
import { factLines } from "../src/facts/lines.ts";
import { markFacts } from "../src/hook/reuse.ts";

const SLUG = "request-line-demo";
const FULL_COMMIT = "abcdef0123456789abcdef0123456789abcdef01";
const NOTE = " (another repo, indexed at abcdef01, not checked out here)";
const OUT = "{ id: string }";
const BODY = "{ title: string }";

const TASKS = `// tasks
export function formatTask(task) {
  return { id: task.id };
}
export function listTasks(req, res) {
  res.json(formatTask(req.rows));
}
export function getTask(req, res) {
  res.json(formatTask(req.row));
}
`;
const LINE_LIST = 5;
const LINE_GET = 8;
const place = (n: number) => ({ service: "ext", repo: "ext", file: `src/view${n}.ts`, line: n, loc: `ext/src/view${n}.ts:${n}` });

/** One group of `text` at `n`; `lead: null` is the shape of a row the cloud did not word. */
function group(lead: string | null, type: string, n: number) {
  return { type, symbol: null, home: null, places: [place(n)], total: 1, shown: lead === null ? 0 : 1, lead, more: lead === null ? null : "", tail: lead === null ? null : `, which expects: ${type}`, shortened: false };
}

function row(label: string, name: string, line: number, direction: "request" | "response", groups: unknown[]) {
  return { k: "contract", side: "producer", protocol: "http", label, direction, service: "api", repo: "api", file: "src/tasks.ts", line, name, site: null, locator: "registration", groups, untyped: [], total: groups.length };
}
const response = (label: string, name: string, line: number, n = 4) => row(label, name, line, "response", [group(`Carrick: ${label} is called from `, OUT, n)]);
const request = (label: string, name: string, line: number, groups = [group(`Carrick: ${label} takes a body from `, BODY, 6)]) => row(label, name, line, "request", groups);
const op = (name: string, label: string, line: number) => ({
  k: "op", side: "producer", protocol: "http", label, service: "api", repo: "api", file: "src/tasks.ts", line, name, site: null,
  others: [{ service: "web", repo: "web", file: "src/client.ts", line: 2 }], total: 0, source: "call", text: `op line for ${name}`,
});

async function workspace(t: { after: (fn: () => void) => void }, rows: Array<Record<string, unknown>>): Promise<Ws> {
  const root = fs.realpathSync(fs.mkdtempSync(path.join(os.tmpdir(), "carrick-reqline-")));
  const home = fs.realpathSync(fs.mkdtempSync(path.join(os.tmpdir(), "carrick-reqline-home-")));
  t.after(async () => {
    await drainRunner(root);
    fs.rmSync(root, { recursive: true, force: true });
    fs.rmSync(home, { recursive: true, force: true });
  });
  for (const [rel, text] of Object.entries({ "api/src/tasks.ts": TASKS, "web/src/client.ts": "export const x = 1;\n" })) {
    fs.mkdirSync(path.dirname(path.join(root, rel)), { recursive: true });
    fs.writeFileSync(path.join(root, rel), text);
  }
  fs.mkdirSync(path.join(root, ".carrick", "hosted"), { recursive: true });
  fs.writeFileSync(path.join(root, ".carrick", "hosted", "snapshot.json"), JSON.stringify({ resolution: { repos: [{ project_slug: SLUG }] } }));
  const header = JSON.stringify({
    schema: 1, writer: 9, project_id: "p", project_slug: SLUG, digest: "d1", built_at: "2026-01-01T00:00:00Z",
    repos: [{ repo: "api", service: "api", commit: "1" }, { repo: "web", service: "web", commit: "1" }, { repo: "ext", service: "ext", commit: FULL_COMMIT }],
    counts: { rows: 0, producers: 0, consumers: 0 },
  });
  // The op row of each operation, so an edit of its handler reaches it (tier 1).
  const ops = [...new Map(rows.map((one) => [`${one["name"]}`, op(String(one["name"]), String(one["label"]), Number(one["line"]))])).values()];
  const text = `${[header, ...rows, ...ops].map((one) => (typeof one === "string" ? one : JSON.stringify(one))).join("\n")}\n`;
  const local = ["api", "web"];
  await buildFacts(commitRepos(root, text, local), factsPath(root), local.map((repo) => ({ repo, path: path.join(root, repo) })), new Date().toISOString());
  return { root, home, at: (rel) => path.join(root, rel) };
}

const run = (ws: Ws, snippet: string, session = "s1") =>
  factLines(ws.root, [{ file: ws.at("api/src/tasks.ts"), snippets: [snippet] }], session, { home: ws.home, contract: true });
const IN_LIST = "res.json(formatTask(req.rows));";
const IN_FORMAT = "return { id: task.id };";
const RESPONSE_LIST = `Carrick: GET /tasks is called from ext/src/view4.ts:4${NOTE}, which expects: ${OUT}`;
const REQUEST_LIST = `Carrick: GET /tasks takes a body from ext/src/view6.ts:6${NOTE}, which expects: ${BODY}`;
const KEY = "contract::api::src/tasks.ts::producer::GET /tasks";

test("an edit reaching a route prints its response line, then one request line, under the request key", async (t) => {
  const ws = await workspace(t, [request("GET /tasks", "listTasks", LINE_LIST), response("GET /tasks", "listTasks", LINE_LIST)]);
  const out = await run(ws, IN_LIST);
  assert.deepEqual(out.lines, [RESPONSE_LIST, REQUEST_LIST]);
  assert.deepEqual(out.used, [`${KEY}::0`, `${KEY}::request::0`]);
});

test("a request row with two groups prints one line", async (t) => {
  const two = [group("Carrick: GET /tasks takes a body from ", BODY, 6), group("Carrick: GET /tasks takes a body from ", "{ n: number }", 7)];
  const ws = await workspace(t, [response("GET /tasks", "listTasks", LINE_LIST), request("GET /tasks", "listTasks", LINE_LIST, two)]);
  const out = await run(ws, IN_LIST);
  assert.equal(out.lines.length, 2);
  assert.deepEqual(out.used, [`${KEY}::0`, `${KEY}::request::0`]);
});

test("request lines come after the response lines of every reached operation", async (t) => {
  const rows = [
    request("GET /tasks", "listTasks", LINE_LIST), response("GET /tasks", "listTasks", LINE_LIST),
    request("GET /tasks/:id", "getTask", LINE_GET), response("GET /tasks/:id", "getTask", LINE_GET),
  ];
  const ws = await workspace(t, rows);
  markTierThreeClosed(ws);
  const out = await run(ws, IN_FORMAT);
  assert.deepEqual(out.used, [`${KEY}::0`, `${KEY}::0`.replace("GET /tasks", "GET /tasks/:id"), `${KEY}::request::0`]);
});

test("a request row alone prints under tier 1, and never under tier 3", async (t) => {
  const ws = await workspace(t, [request("GET /tasks", "listTasks", LINE_LIST), request("GET /tasks/:id", "getTask", LINE_GET)]);
  assert.deepEqual((await run(ws, IN_LIST)).used, [`${KEY}::request::0`]);
  // An edit that reaches no operation: the first contract line of the file would be tier 3, which holds response rows only.
  assert.deepEqual((await run(ws, "// tasks", "s2")).lines, []);
});

test("tier 3 lists response rows only, even when request rows sit beside them", async (t) => {
  const ws = await workspace(t, [request("GET /tasks", "listTasks", LINE_LIST), response("GET /tasks", "listTasks", LINE_LIST), request("GET /tasks/:id", "getTask", LINE_GET)]);
  assert.deepEqual((await run(ws, "// tasks")).used, [`${KEY}::0`]);
});

test("a request line said earlier does not close tier 3 for the file's response lines", async (t) => {
  const ws = await workspace(t, [response("GET /tasks", "listTasks", LINE_LIST), request("GET /tasks", "listTasks", LINE_LIST)]);
  markFacts("s1", { [`${KEY}::request::0`]: { at: 1, named: [] } }, ws.home);
  assert.deepEqual((await run(ws, "// tasks")).used, [`${KEY}::0`]);
});

test("a request row without wording (writer 8) changes nothing: the output equals the response lines alone", async (t) => {
  const unworded = request("GET /tasks", "listTasks", LINE_LIST, [group(null, BODY, 6)]);
  const withRequest = await workspace(t, [unworded, response("GET /tasks", "listTasks", LINE_LIST)]);
  const without = await workspace(t, [response("GET /tasks", "listTasks", LINE_LIST)]);
  const a = await run(withRequest, IN_LIST);
  assert.deepEqual(a.lines, [RESPONSE_LIST]);
  const b = await run(without, IN_LIST);
  assert.deepEqual({ lines: a.lines, used: a.used, claims: Object.keys(a.claims) }, { lines: b.lines, used: b.used, claims: Object.keys(b.claims) });
});

/** Say a contract line of this file so tier 3 stays closed. */
function markTierThreeClosed(ws: Ws): void {
  markFacts("s1", { [`${KEY}/x::0`]: { at: 1, named: [] } }, ws.home);
}
