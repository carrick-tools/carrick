// The held compiler end to end (part 3, L2): the real compiler engine action
// writes the shadow, a real TypeScript 7 language server holds it, and the
// real placer arms checks at the send positions a producer row stores
// (`values`), in three framework shapes (Hono-, Express- and Lambda-style).
// Fails, never skips, when the native compiler or the built engine is missing.
// Every name here is invented.

import test from "node:test";
import assert from "node:assert/strict";
import { execFileSync } from "node:child_process";
import fs from "node:fs";
import os from "node:os";
import path from "node:path";
import { HeldDaemon, type HeldAnswer, type ServiceInit } from "../src/held/daemon.ts";
import { renderHeldLine } from "../src/held/line.ts";
import { createRuntime } from "../src/held/runtime.ts";
import { logPath, placedPath } from "../src/held/paths.ts";
import type { ContractRow } from "../src/facts/store.ts";
import { testDir } from "./helpers.ts";

const SIDECAR = path.resolve(testDir, "..", "..", "..", "src", "sidecar");
const FIXTURE = path.join(SIDECAR, "test", "fixtures", "held-node");

type Value = { file: string; span: [number, number] };

/** The `[start, end)` of `snippet`'s `nth` occurrence in `text`. */
function spanOf(text: string, snippet: string, nth = 0): [number, number] {
  let at = -1;
  for (let i = 0; i <= nth; i++) at = text.indexOf(snippet, at + 1);
  assert.ok(at >= 0, `no ${snippet} in the text`);
  return [at, at + snippet.length];
}

const lineAt = (text: string, snippet: string): number => text.slice(0, text.indexOf(snippet)).split("\n").length;

function row(label: string, file: string, type: string, values: Value[] | undefined): ContractRow {
  return {
    side: "producer",
    protocol: "http",
    label,
    direction: "response",
    service: "orders-api",
    repo: "shop",
    file,
    line: 1,
    name: null,
    site: null,
    locator: "call",
    ...(values === undefined ? {} : { values }),
    groups: [{ type, symbol: null, places: [{ service: "web", repo: "shop", file: "web/a.ts", line: 3, loc: "web/a.ts:3" }] }],
    untyped: [],
    total: 1,
  } as unknown as ContractRow;
}

type Placed = { armed: Array<{ label: string; file: string; position: number }>; unarmed: Array<{ label: string; reason: string }> };

type Rig = {
  root: string;
  file: (rel: string) => string;
  text: (rel: string) => string;
  placed: () => Placed;
  /** The daemon's request log, one parsed row per line. */
  log: () => Array<Record<string, unknown>>;
  edit: (rel: string, text: string) => Promise<HeldAnswer>;
  ask: (rel: string, kind?: "read" | "edit") => Promise<HeldAnswer>;
};

const git = (cwd: string, ...args: string[]): string =>
  execFileSync("git", ["-c", "user.name=t", "-c", "user.email=t@example.test", "-c", "commit.gpgsign=false", ...args], { cwd, encoding: "utf8" }).trim();

/**
 * A copy of the fixture, committed (the index commit), then `before` changes
 * the files on disk, then the daemon starts over `rowsOf(texts at the commit)`.
 */
async function rig(
  t: { after: (fn: () => void | Promise<void>) => void },
  rowsOf: (text: (rel: string) => string) => ContractRow[],
  before: (root: string) => void = () => undefined,
): Promise<Rig> {
  const dir = fs.realpathSync(fs.mkdtempSync(path.join(os.tmpdir(), "held-e2e-")));
  t.after(() => fs.rmSync(dir, { recursive: true, force: true }));
  const root = path.join(dir, "ws");
  const home = path.join(dir, "home");
  fs.cpSync(FIXTURE, root, { recursive: true, dereference: true });
  // `total` takes its type from the helper, so a change to the helper alone reaches the route.
  const orders = path.join(root, "src", "orders.ts");
  fs.writeFileSync(orders, fs.readFileSync(orders, "utf8").replace("const total: number =", "const total ="));
  git(root, "init", "-q");
  git(root, "add", "-A");
  git(root, "commit", "-q", "-m", "index");
  const commit = git(root, "rev-parse", "HEAD");
  const committed = (rel: string): string => fs.readFileSync(path.join(root, rel), "utf8");
  const rows = rowsOf(committed);
  before(root);
  const init: ServiceInit = { repo: "shop", service: "orders-api", repo_path: root, sidecar_root: root, tsconfig: null, scan_root: root };
  const env = { ...process.env, CARRICK_COMPILER_LINES: "1", CARRICK_CONTRACT_LINES: "1" };
  const runtime = createRuntime({
    root,
    home,
    env,
    entry: path.join(SIDECAR, "dist", "src", "index.js"),
    rows: async () => rows,
    commits: async () => ({ shop: commit }),
    stamp: () => "fixed",
  });
  const daemon = new HeldDaemon({ root, home, env, programs: async () => [init], runtime });
  t.after(() => daemon.close());
  let session = 0;
  const file = (rel: string): string => path.join(root, rel);
  const ask = (rel: string, kind: "read" | "edit" = "edit"): Promise<HeldAnswer> => daemon.handle({ session: `s${session++}`, files: [file(rel)], kind, edited_at: Date.now() });
  const edit = async (rel: string, text: string): Promise<HeldAnswer> => {
    fs.writeFileSync(file(rel), text);
    // Bump the mtime past the filesystem's resolution.
    const later = new Date(Date.now() + 5000 * (session + 1));
    fs.utimesSync(file(rel), later, later);
    return ask(rel);
  };
  // The first request starts the engine and answers cold; wait for it to be ready.
  assert.equal((await ask("src/orders.ts", "read")).state, "cold");
  let ready: HeldAnswer | null = null;
  for (let i = 0; i < 200 && ready === null; i++) {
    await new Promise((resolve) => setTimeout(resolve, 500));
    const answer = await ask("src/orders.ts");
    if (answer.state === "ready") ready = answer;
  }
  assert.ok(ready, "the engine never became ready");
  return {
    root,
    file,
    text: (rel) => fs.readFileSync(file(rel), "utf8"),
    placed: () => JSON.parse(fs.readFileSync(placedPath(root, "orders-api", home), "utf8")) as Placed,
    log: () =>
      fs
        .readFileSync(logPath(root, home), "utf8")
        .trim()
        .split("\n")
        .map((l) => JSON.parse(l) as Record<string, unknown>),
    edit,
    ask,
  };
}

const ORDERS = "src/orders.ts";
const EXPRESS = "src/express-orders.ts";
const LAMBDA = "src/lambda.ts";
const SHARED = "src/shared-send.ts";

const rows = (text: (rel: string) => string): ContractRow[] => [
  row("GET /orders", ORDERS, '{ total: number; a: "one"; b: "two" }', [{ file: ORDERS, span: spanOf(text(ORDERS), "{ total, a, b }") }]),
  row("POST /orders", ORDERS, "{ created: boolean }", [{ file: ORDERS, span: spanOf(text(ORDERS), "{ created: true }") }]),
  row("GET /express/orders", EXPRESS, "{ orders: string[]; total: number }", [{ file: EXPRESS, span: spanOf(text(EXPRESS), "{ orders, total }") }]),
  row("GET /express/order", EXPRESS, "{ id: string; settled: boolean; placedAt: string }", [{ file: EXPRESS, span: spanOf(text(EXPRESS), "{ id, settled, placedAt }") }]),
  row("GET /express/pick", EXPRESS, "{ n: number }", [
    { file: EXPRESS, span: spanOf(text(EXPRESS), "{ n: 1 }") },
    { file: EXPRESS, span: spanOf(text(EXPRESS), "{ n: 2 }") },
  ]),
  row("POST /lambda", LAMBDA, "{ ok: boolean; count: number }", [{ file: LAMBDA, span: spanOf(text(LAMBDA), "payload", 1) }]),
  // The send is in another file than the route.
  row("GET /shared", ORDERS, "{ label: string }", [{ file: SHARED, span: spanOf(text(SHARED), "{ label }") }]),
  // A receiver declared void or undefined states no contract (K-a): its check arms (clean passes) and never speaks, so a break of the value it shares with the row above is still one line.
  row("GET /void", ORDERS, "undefined", [{ file: SHARED, span: spanOf(text(SHARED), "{ label }") }]),
  // A request-direction row is never placed, whatever it carries.
  { ...row("POST /request-side", ORDERS, "{ a: number }", [{ file: ORDERS, span: spanOf(text(ORDERS), "{ created: true }") }]), direction: "request" } as ContractRow,
  // A send in a file outside the program the compiler engine loaded (case row 10).
  row("GET /ghost", ORDERS, "{ a: number }", [{ file: "docs/ghost.ts", span: [0, 5] }]),
  // A writer-7 row (no `values`) and one whose scan read none.
  row("GET /legacy", ORDERS, "{ a: number }", undefined),
  row("GET /unread", ORDERS, "{ a: number }", []),
];

const MACHINERY = /__C|\bsend\d|\bO\d+_\d+|__carrick_/;

test("three framework shapes arm on real TypeScript 7; a break is one line at the send, a half-written edit none, moved lines stay armed", { timeout: 300_000 }, async (t) => {
  const r = await rig(t, rows);
  const original = Object.fromEntries([ORDERS, EXPRESS, LAMBDA, SHARED].map((rel) => [rel, r.text(rel)]));

  // (a) every row with values arms; the two without say why and stay silent.
  const placed = r.placed();
  assert.deepEqual(
    placed.armed.map((a) => `${a.label}#${a.position}`).sort(),
    ["GET /express/order#0", "GET /express/orders#0", "GET /express/pick#0", "GET /express/pick#1", "GET /orders#0", "GET /shared#0", "GET /void#0", "POST /lambda#0", "POST /orders#0"],
  );
  assert.deepEqual(placed.unarmed.map((u) => `${u.label}:${u.reason}`).sort(), ["GET /ghost:value_outside_service", "GET /legacy:no_values", "GET /unread:no_values"]);
  assert.ok(![...placed.armed, ...placed.unarmed].some((p) => p.label === "POST /request-side"), "a request-side row is ignored");
  assert.deepEqual(placed.armed.find((a) => a.label === "GET /shared")!.file, SHARED);
  assert.deepEqual((await r.ask(ORDERS)).lines, [], "the unedited file says nothing");

  // The log says where each answer's time went: the sweep, the edit's apply, the pull; and the twin pass after.
  const logged = r.log();
  const answered = logged.filter((row) => row["kind"] === "edit" && row["state"] === "ready");
  assert.ok(answered.length > 0, "an answered edit is logged");
  for (const row of answered) {
    const steps = row["steps"] as Record<string, number>;
    for (const name of ["programs", "queue_wait", "engine", "refresh", "sweep"]) assert.equal(typeof steps[name], "number", `${name} in ${JSON.stringify(row)}`);
  }
  assert.ok(answered.some((row) => typeof (row["steps"] as Record<string, number>)["pull"] === "number"), "an edit's row names the pull");
  assert.ok(logged.some((row) => row["kind"] === "after" && typeof (row["steps"] as Record<string, number>)["twin"] === "number"), "the twin pass has a row");

  // (b) removing a required member from each shape: exactly one line, at the send's line.
  const breaks: Array<{ rel: string; from: string; to: string; label: string; at: string }> = [
    { rel: ORDERS, from: "c.json({ total, a, b })", to: "c.json({ a, b })", label: "GET /orders", at: "c.json({ a, b })" },
    { rel: EXPRESS, from: "res.json({ orders, total })", to: "res.json({ orders })", label: "GET /express/orders", at: "res.json({ orders })" },
    { rel: LAMBDA, from: "payload: { ok: boolean; count: number }", to: "payload: { ok: boolean }", label: "POST /lambda", at: "JSON.stringify(payload)" },
    { rel: SHARED, from: "res.json({ label })", to: "res.json({ label: 1 })", label: "GET /shared", at: "res.json({ label: 1 })" },
  ];
  for (const b of breaks) {
    const next = original[b.rel]!.replace(b.from, b.to);
    const answer = await r.edit(b.rel, next);
    assert.equal(answer.state, "ready");
    assert.equal(answer.lines.length, 1, `${b.label}: ${JSON.stringify(answer.lines)}`);
    assert.equal(answer.lines[0]!.label, b.label);
    assert.equal(answer.lines[0]!.file, b.label === "GET /shared" ? SHARED : b.rel);
    assert.equal(answer.lines[0]!.line, lineAt(next, b.at), b.label);
    const said = renderHeldLine(answer.lines[0]!);
    assert.ok(said, `${b.label}: the line is printable: ${answer.lines[0]!.message}`);
    assert.ok(!MACHINERY.test(said.text), said.text);
    assert.deepEqual((await r.edit(b.rel, original[b.rel]!)).lines, [], `${b.label}: restored`);
  }

  // (c) half-written inside a send: a syntax error is never a line.
  const half = await r.edit(EXPRESS, original[EXPRESS]!.replace("{ orders, total }", "{ orders, total: orders. }"));
  assert.equal(half.state, "ready");
  assert.deepEqual(half.lines, []);
  assert.deepEqual((await r.edit(EXPRESS, original[EXPRESS]!)).lines, []);

  // (d) a comment line above shifts every send; they stay armed and a break is still one line.
  const moved = "// moved\n" + original[EXPRESS]!;
  assert.deepEqual((await r.edit(EXPRESS, moved)).lines, []);
  const movedBroken = await r.edit(EXPRESS, moved.replace("res.json({ orders, total })", "res.json({ orders })"));
  assert.equal(movedBroken.lines.length, 1, JSON.stringify(movedBroken.lines));
  assert.equal(movedBroken.lines[0]!.line, lineAt(moved, "res.json({ orders, total })"));
  await r.edit(EXPRESS, original[EXPRESS]!);

  // (e) a rename whose second edit sits inside the value, across its start bound: the engine places the value by its
  // parent slot (the send's argument), so the check stays armed, and a break made in this same edit gives its line.
  const renamed = original[EXPRESS]!.replace("const orders = list", "const kept = list").replace("{ orders, total }", "{ orders: kept, total }");
  assert.deepEqual((await r.edit(EXPRESS, renamed)).lines, []);
  const renamedBroken = original[EXPRESS]!.replace("const orders = list", "const kept = list").replace("{ orders, total }", "{ orders: kept }");
  const brokenAnswer = await r.edit(EXPRESS, renamedBroken);
  assert.deepEqual(brokenAnswer.lines.map((l) => l.label), ["GET /express/orders"], "a break in the same edit as the rename is one line");
  assert.equal(brokenAnswer.lines[0]!.line, lineAt(renamedBroken, "res.json({ orders: kept })"));
  await r.edit(EXPRESS, original[EXPRESS]!);

  // (h) a `Date` value against `string` arms (the wire form); (j) breaking each member prints a line without machinery.
  for (const [from, to] of [["id: string", "id: number"], ["settled: boolean", "settled: string"], ["placedAt: Date", "placedAt: number"]] as const) {
    const answer = await r.edit(EXPRESS, original[EXPRESS]!.replace(from, to));
    assert.equal(answer.lines.length, 1, `${to}: ${JSON.stringify(answer.lines)}`);
    assert.equal(answer.lines[0]!.label, "GET /express/order");
    const said = renderHeldLine(answer.lines[0]!);
    assert.ok(said, `${to}: ${answer.lines[0]!.message}`);
    assert.ok(!MACHINERY.test(said.text), said.text);
    await r.edit(EXPRESS, original[EXPRESS]!);
  }

  // (i) a route with two sends: breaking either gives a line, at that send; both give one line.
  for (const [snippet, other] of [["{ n: 1 }", "{ n: 2 }"], ["{ n: 2 }", "{ n: 1 }"]] as const) {
    const next = original[EXPRESS]!.replace(snippet, '{ n: "x" }');
    const answer = await r.edit(EXPRESS, next);
    assert.equal(answer.lines.length, 1, JSON.stringify(answer.lines));
    assert.equal(answer.lines[0]!.line, lineAt(next, '{ n: "x" }'), other);
    await r.edit(EXPRESS, original[EXPRESS]!);
  }
  // Breaking the second send too, in a second edit, still says one line: the first broken send's.
  const first = original[EXPRESS]!.replace("{ n: 1 }", '{ n: "x" }');
  assert.equal((await r.edit(EXPRESS, first)).lines.length, 1);
  const both = await r.edit(EXPRESS, first.replace("{ n: 2 }", '{ n: "y" }'));
  assert.equal(both.lines.length, 1, JSON.stringify(both.lines));
  assert.equal(both.lines[0]!.line, lineAt(first, '{ n: "x" }'), "the first broken send is named");
  await r.edit(EXPRESS, original[EXPRESS]!);

  // A file the hook never named changes on disk behind the daemon (a checkout, a
  // formatter): it is synced before the answer, and the route it reaches speaks.
  const helper = path.join(r.root, "src", "helper.ts");
  const helperOriginal = fs.readFileSync(helper, "utf8");
  fs.writeFileSync(helper, helperOriginal.replace('Number(process.env.LIMIT ?? "10")', '"ten"'));
  const later = new Date(Date.now() + 90_000);
  fs.utimesSync(helper, later, later);
  const drift = await r.ask(ORDERS);
  assert.equal(drift.lines.length, 1, JSON.stringify(drift.lines));
  assert.equal(drift.lines[0]!.label, "GET /orders");
  fs.writeFileSync(helper, helperOriginal);
  const back = new Date(Date.now() + 180_000);
  fs.utimesSync(helper, back, back);
  assert.deepEqual((await r.edit(EXPRESS, r.text(EXPRESS).replace("res.json({ orders })", "res.json({ orders, total })"))).lines, []);
});

test("an uncommitted change made before the engine starts still places (the hunk map); a row without values is silent", { timeout: 300_000 }, async (t) => {
  const r = await rig(t, rows, (root) => {
    // Lines added above both sends and a rename, none committed: the stored spans are the committed text's.
    const file = path.join(root, EXPRESS);
    fs.writeFileSync(file, "// one\n// two\n" + fs.readFileSync(file, "utf8").replace("const orders = list", "const orders = [...list]"));
  });
  const armed = r.placed().armed.map((a) => a.label);
  assert.ok(armed.includes("GET /express/orders"), armed.join(", "));
  assert.ok(armed.includes("GET /express/pick"), armed.join(", "));
  const next = r.text(EXPRESS).replace("res.json({ orders, total })", "res.json({ orders })");
  const answer = await r.edit(EXPRESS, next);
  assert.equal(answer.lines.length, 1, JSON.stringify(answer.lines));
  assert.equal(answer.lines[0]!.line, lineAt(next, "res.json({ orders })"));
  // (g) the writer-7 row.
  assert.ok(r.placed().unarmed.some((u) => u.label === "GET /legacy" && u.reason === "no_values"));
  assert.deepEqual((await r.edit(EXPRESS, r.text(EXPRESS).replace("res.json({ orders })", "res.json({ orders, total })"))).lines, []);
});
