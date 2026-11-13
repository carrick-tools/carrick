// The cap of fact lines per action (T2): which lines win, how a second call in
// the same action shares the cap (`limit`/`exclude`/`used`), the bound on failed
// peer proofs, and held lines left unclaimed. Rows are hand-written in the wire's shape.

import test from "node:test";
import assert from "node:assert/strict";
import fs from "node:fs";
import { factLines } from "../src/facts/lines.ts";
import { markFacts, readSession } from "../src/hook/reuse.ts";
import { context, countingGit, ELSEWHERE, lineOf, nextCarrier, patchEdit, workspace } from "./facts-helpers.ts";
import type { Ws } from "./facts-helpers.ts";

const NAMES = ["alpha", "beta", "gamma", "delta", "echo"];
const ORDERS = NAMES.flatMap((name, i) => [`export function ${name}() {`, `  return ${i};`, "}", ""]).join("\n");
const ROUTES = ["export function listRoutes() {", "  return [];", "}", ""].join("\n");
const fnText = (name: string): string => `${name} is called from web/src/${name}.ts:2`;
const fnKey = (name: string): string => `fn::api::src/orders.ts::${name}`;

/** An fn row per function of `orders.ts`, its caller in its own committed web file. */
const fnRows = NAMES.map((name, i) => ({
  k: "fn",
  repo: "api",
  file: "src/orders.ts",
  line: i * 4 + 1,
  name,
  others: [{ repo: "web", file: `src/${name}.ts`, line: 2, name: `use${name}` }],
  total: 1,
  text: fnText(name),
}));
const webFiles = Object.fromEntries(NAMES.map((name) => [`web/src/${name}.ts`, `export function use${name}() {\n  return ${name}();\n}\n`]));

const opRow = (name: string, label: string, text: string, file = "src/routes.ts") => ({
  k: "op",
  side: "consumer",
  protocol: "http",
  label,
  service: "api",
  repo: "api",
  file,
  line: 1,
  name,
  site: null,
  others: [ELSEWHERE],
  total: 0,
  source: "call",
  text,
});
const ROUTE_LINE = "GET /routes is called at api/src/peer.ts:1";
const ROUTE_ROW = opRow("listRoutes", "GET /routes", ROUTE_LINE);

const FILES = { "api/src/orders.ts": ORDERS, "api/src/routes.ts": ROUTES, ...webFiles };
const whole = (ws: Ws, rel: string) => ({ file: ws.at(rel), whole: true });

test("limit and exclude share one cap across two calls", async (t) => {
  const ws = await workspace(t, FILES, fnRows);
  const outcome = await factLines(ws.root, [whole(ws, "api/src/orders.ts")], undefined, { limit: 1, exclude: new Set([fnKey("alpha")]) });
  assert.deepEqual(outcome.lines, [fnText("beta")]);
  assert.deepEqual(outcome.used, [fnKey("beta")]);
});

test("used names every key printed, so a second call can pass them as exclude", async (t) => {
  const ws = await workspace(t, FILES, fnRows);
  const first = await factLines(ws.root, [whole(ws, "api/src/orders.ts")], undefined, { limit: 2 });
  assert.deepEqual(first.lines, [fnText("alpha"), fnText("beta")]);
  assert.deepEqual(first.used, [fnKey("alpha"), fnKey("beta")]);
  const second = await factLines(ws.root, [whole(ws, "api/src/orders.ts")], undefined, { limit: 3 - first.used.length, exclude: new Set(first.used) });
  assert.deepEqual(second.lines, [fnText("gamma")]);
  assert.equal(first.lines.length + second.lines.length, 3);
});

test("limit never exceeds the cap of three, and a non-finite limit means three", async (t) => {
  const ws = await workspace(t, FILES, fnRows);
  const files = [whole(ws, "api/src/orders.ts")];
  const five = await factLines(ws.root, files, undefined, { limit: 5 });
  assert.equal(five.lines.length, 3);
  assert.equal(five.used.length, 3);
  const nan = await factLines(ws.root, files, undefined, { limit: Number.NaN });
  assert.equal(nan.lines.length, 3);
  for (const limit of [0, -1]) {
    const none = await factLines(ws.root, files, undefined, { limit });
    assert.deepEqual(none.lines, []);
  }
});

test("an excluded key is not a failed proof: a fourth fn line still prints", async (t) => {
  const ws = await workspace(t, FILES, fnRows);
  const exclude = new Set(["alpha", "beta", "gamma"].map(fnKey));
  const outcome = await factLines(ws.root, [whole(ws, "api/src/orders.ts")], undefined, { exclude });
  assert.deepEqual(outcome.lines, [fnText("delta"), fnText("echo")]);
});

test("a line said earlier in the session and a line in exclude both stay out of one action", async (t) => {
  const ws = await workspace(t, FILES, fnRows);
  const first = await factLines(ws.root, [whole(ws, "api/src/orders.ts")], "said-excl", { home: ws.home, limit: 1 });
  markFacts("said-excl", first.claims, ws.home);
  const second = await factLines(ws.root, [whole(ws, "api/src/orders.ts")], "said-excl", { home: ws.home, exclude: new Set([fnKey("beta")]) });
  assert.deepEqual(second.lines, [fnText("gamma"), fnText("delta"), fnText("echo")]);
});

test("failed proofs stop at three per action", async (t) => {
  const ws = await workspace(t, FILES, fnRows);
  // Every peer file changed after the committed index commit: every proof fails.
  for (const name of NAMES) fs.appendFileSync(ws.at(`web/src/${name}.ts`), "// edited\n");
  const git = countingGit(t);
  const before = process.env["PATH"];
  process.env["PATH"] = git.path;
  try {
    const outcome = await factLines(ws.root, [whole(ws, "api/src/orders.ts")], undefined);
    assert.deepEqual(outcome.lines, []);
  } finally {
    process.env["PATH"] = before;
  }
  assert.equal(git.calls(/ls-tree/), 3);
});

/** One Edit whose snippet spans all of `orders.ts`. */
function editAll(ws: Ws, session: string, probe?: string) {
  const file = ws.at("api/src/orders.ts");
  return patchEdit(ws, file, [{ newStart: 1, lines: ORDERS.split("\n").slice(0, -1).map((line) => `+${line}`) }], { session, ...(probe ? { probe } : {}) });
}

test("five touched functions print three lines, in the order the edit reached them", async (t) => {
  const ws = await workspace(t, FILES, fnRows);
  const run = await editAll(ws, "five");
  assert.equal(context(run.stdout), [fnText("alpha"), fnText("beta"), fnText("gamma")].join("\n\n"));
});

test("a held line prints at the next action that touches its thing", async (t) => {
  const ws = await workspace(t, FILES, fnRows);
  await editAll(ws, "held-next");
  const file = ws.at("api/src/orders.ts");
  const run = await patchEdit(ws, file, [{ newStart: lineOf(file, "return 3"), lines: ["+  return 3;"] }], { session: "held-next" });
  assert.equal(context(run.stdout), fnText("delta"));
});

test("held lines are not claimed", async (t) => {
  const ws = await workspace(t, FILES, fnRows);
  await editAll(ws, "unclaimed");
  assert.deepEqual(Object.keys(readSession("unclaimed", ws.home).facts).sort(), [fnKey("alpha"), fnKey("beta"), fnKey("gamma")]);
});

test("kind order holds across the files of one action", async (t) => {
  const ws = await workspace(t, FILES, [...fnRows, ROUTE_ROW]);
  const outcome = await factLines(ws.root, [whole(ws, "api/src/orders.ts"), whole(ws, "api/src/routes.ts")], undefined);
  assert.deepEqual(outcome.lines, [ROUTE_LINE, fnText("alpha"), fnText("beta")]);
});

test("an fn line the cap held back leaves the uses-count line in", async (t) => {
  const ops = ["a", "b", "c"].map((n) => opRow("alpha", `GET /${n}`, `op line ${n}`, "src/orders.ts"));
  const rows = [
    { ...ops[0]!, protocol: "http" },
    { ...ops[1]!, side: "producer", protocol: "socket", label: "SOCKET b" },
    { ...ops[2]!, side: "producer", protocol: "pubsub", label: "PUBSUB c" },
    fnRows[0]!,
  ];
  const ws = await workspace(t, FILES, rows);
  const file = ws.at("api/src/orders.ts");
  const run = await patchEdit(ws, file, [{ newStart: lineOf(file, "return 0"), lines: ["+  return 0;"] }], { session: "uses-in", probe: "check-uses.json" });
  const text = context(run.stdout);
  assert.deepEqual(text.split("\n\n"), ["op line a", "op line b", "op line c"]);
  assert.ok(!text.includes(fnText("alpha")), text);
  assert.match(await nextCarrier(ws, "uses-in"), /Other files use what this file declares/);
});
