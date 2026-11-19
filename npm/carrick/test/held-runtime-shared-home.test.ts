// T3 (#4658): a type both ends import. The check holds the caller's type text
// fixed while the sent value's type is live, so an edit to a declaration both
// ends compile (a field dropped from the type and the value together) moves
// the value and not the text: a break the caller does not have. A group whose
// home is a file of the producer's program that is not the group's own place
// file is unarmed `shared_home`. Real TypeScript 7, as held-e2e. Invented names.

import test from "node:test";
import assert from "node:assert/strict";
import { execFileSync } from "node:child_process";
import fs from "node:fs";
import os from "node:os";
import path from "node:path";
import { HeldDaemon, type HeldAnswer, type ServiceInit } from "../src/held/daemon.ts";
import { Ts7Lsp } from "../src/held/engine.ts";
import { createRuntime } from "../src/held/runtime.ts";
import { placedPath } from "../src/held/paths.ts";
import type { ContractRow } from "../src/facts/store.ts";
import { testDir } from "./helpers.ts";

const SIDECAR = path.resolve(testDir, "..", "..", "..", "src", "sidecar");
const FIXTURE = path.join(SIDECAR, "test", "fixtures", "held-node");

const ORDER = "api/src/shared/order.ts";
const ORDERS = "api/src/orders.ts";
const ORDER_TEXT = "export type Order = { id: string; total: number };\n";
const ORDERS_TEXT = [
  'import { Hono } from "hono";',
  'import type { Order } from "./shared/order.js";',
  'const order: Order = { id: "o1", total: 3 };',
  "const app = new Hono();",
  'app.get("/orders", (c) => c.json(order));',
  "export default app;",
  "",
].join("\n");
const CALLER_TEXT = 'import type { Order } from "../../api/src/shared/order.js";\nexport const o: Order = { id: "o1", total: 3 };\n';
const NARROW_ORDER = "export type Order = { id: string };\n";
const NARROW_ORDERS = ORDERS_TEXT.replace('{ id: "o1", total: 3 }', '{ id: "o1" }');

const git = (cwd: string, ...args: string[]): string =>
  execFileSync("git", ["-c", "user.name=t", "-c", "user.email=t@example.test", "-c", "commit.gpgsign=false", ...args], { cwd, encoding: "utf8" }).trim();

type Placed = { armed: Array<{ label: string; file: string }>; unarmed: Array<{ label: string; reason: string }> };

/** The producer row `GET /orders`; its one group is the caller's `Order`, homed at `home` (null: no home) with its place at `placeFile`. */
function row(home: { file: string; line: number } | null, placeFile: string): ContractRow {
  return {
    side: "producer", protocol: "http", label: "GET /orders", direction: "response", service: "orders-api", repo: "shop", file: ORDERS, line: 5,
    name: null, site: null, locator: "call",
    values: [{ file: "src/orders.ts", span: [ORDERS_TEXT.indexOf("order));") , ORDERS_TEXT.indexOf("order));") + 5] }],
    groups: [{ type: "{ id: string; total: number }", symbol: "Order", home, places: [{ service: "web", repo: "shop", file: placeFile, line: 2, loc: `${placeFile}:2` }], total: 1, shown: 1, lead: "", more: "", tail: "", shortened: false }],
    untyped: [],
    total: 1,
  } as unknown as ContractRow;
}

async function rig(t: { after: (fn: () => void | Promise<void>) => void }, makeRow: () => ContractRow): Promise<{ placed: () => Placed; edit: (rel: string, text: string) => Promise<HeldAnswer> }> {
  const dir = fs.realpathSync(fs.mkdtempSync(path.join(os.tmpdir(), "held-shared-home-")));
  t.after(() => fs.rmSync(dir, { recursive: true, force: true }));
  const root = path.join(dir, "ws");
  const home = path.join(dir, "home");
  const api = path.join(root, "api");
  fs.mkdirSync(path.join(api, "src", "shared"), { recursive: true });
  fs.mkdirSync(path.join(root, "web", "src"), { recursive: true });
  fs.cpSync(path.join(FIXTURE, "node_modules"), path.join(api, "node_modules"), { recursive: true, dereference: true });
  fs.writeFileSync(path.join(api, "package.json"), '{ "name": "orders-api", "private": true, "type": "module" }\n');
  fs.writeFileSync(path.join(api, "tsconfig.json"), '{ "compilerOptions": { "module": "NodeNext", "moduleResolution": "NodeNext", "target": "ES2022", "strict": true, "noEmit": true, "skipLibCheck": true }, "include": ["src"] }\n');
  fs.writeFileSync(path.join(root, ORDER), ORDER_TEXT);
  fs.writeFileSync(path.join(root, ORDERS), ORDERS_TEXT);
  fs.writeFileSync(path.join(root, "web", "src", "a.ts"), CALLER_TEXT);
  git(root, "init", "-q");
  git(root, "add", "-A");
  git(root, "commit", "-q", "-m", "index");
  const commit = git(root, "rev-parse", "HEAD");
  const rows = [makeRow()];
  const init: ServiceInit = { repo: "shop", service: "orders-api", repo_path: root, sidecar_root: api, tsconfig: null, scan_root: api };
  const env = { ...process.env, CARRICK_COMPILER_LINES: "1", CARRICK_CONTRACT_LINES: "1" };
  const runtime = createRuntime({
    root, home, env, entry: path.join(SIDECAR, "dist", "src", "index.js"),
    rows: async () => rows, commits: async () => ({ shop: commit }), stamp: () => "fixed",
    makeEngine: (command, cwd) => new Ts7Lsp(command, cwd),
  });
  const daemon = new HeldDaemon({ root, home, env, programs: async () => [init], runtime });
  t.after(() => daemon.close());
  let n = 0;
  const ask = (rel: string, kind: "read" | "edit"): Promise<HeldAnswer> => daemon.handle({ session: `s${n++}`, files: [path.join(root, rel)], kind, edited_at: Date.now() });
  const edit = async (rel: string, text: string): Promise<HeldAnswer> => {
    fs.writeFileSync(path.join(root, rel), text);
    const later = new Date(Date.now() + 5000 * (n + 1));
    fs.utimesSync(path.join(root, rel), later, later);
    return ask(rel, "edit");
  };
  assert.equal((await ask(ORDERS, "read")).state, "cold");
  let ready = false;
  for (let i = 0; i < 200 && !ready; i++) {
    await new Promise((resolve) => setTimeout(resolve, 500));
    ready = (await ask(ORDERS, "edit")).state === "ready";
  }
  assert.ok(ready, "the engine never became ready");
  return { placed: () => JSON.parse(fs.readFileSync(placedPath(root, "orders-api", home), "utf8")) as Placed, edit };
}

test("a type both ends import: the group is unarmed shared_home, and an edit to the shared declaration says nothing", { timeout: 300_000 }, async (t) => {
  const r = await rig(t, () => row({ file: ORDER, line: 1 }, "web/src/a.ts"));
  assert.deepEqual(r.placed().unarmed.map((u) => `${u.label}:${u.reason}`), ["GET /orders:shared_home"]);
  assert.deepEqual(r.placed().armed, []);
  await r.edit(ORDERS, NARROW_ORDERS);
  const answer = await r.edit(ORDER, NARROW_ORDER);
  assert.equal(answer.state, "ready");
  assert.deepEqual(answer.lines, [], JSON.stringify(answer.lines));
});

test("a group whose home is the caller's own file still states a producer break", { timeout: 300_000 }, async (t) => {
  const r = await rig(t, () => row({ file: "web/src/a.ts", line: 2 }, "web/src/a.ts"));
  assert.deepEqual(r.placed().armed.map((a) => a.label), ["GET /orders"]);
  const answer = await r.edit(ORDERS, ORDERS_TEXT.replace('{ id: "o1", total: 3 }', '{ id: "o1" }').replace("const order: Order", "const order: { id: string }"));
  assert.equal(answer.lines.length, 1, JSON.stringify(answer.lines));
  assert.equal(answer.lines[0]!.label, "GET /orders");
});

test("a group with no home arms as before", { timeout: 300_000 }, async (t) => {
  const r = await rig(t, () => row(null, "web/src/a.ts"));
  assert.deepEqual(r.placed().armed.map((a) => a.label), ["GET /orders"]);
});
