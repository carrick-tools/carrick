// The held compiler end to end (part 3, T5b): the real compiler engine action
// writes the shadow, a real TypeScript 7 language server holds it, the real
// placer arms two producer rows of a synthetic route service, and the daemon
// answers edits. Fails, never skips, when the native compiler or the built
// engine is missing. Every name here is invented.

import test from "node:test";
import assert from "node:assert/strict";
import fs from "node:fs";
import os from "node:os";
import path from "node:path";
import { HeldDaemon, type HeldAnswer, type ServiceInit } from "../src/held/daemon.ts";
import { createRuntime } from "../src/held/runtime.ts";
import { placedPath } from "../src/held/paths.ts";
import type { ContractRow } from "../src/facts/store.ts";
import { testDir } from "./helpers.ts";

const SIDECAR = path.resolve(testDir, "..", "..", "..", "src", "sidecar");
const FIXTURE = path.join(SIDECAR, "test", "fixtures", "held-node");

const row = (id: number, label: string, site: string, line: number, type: string): ContractRow =>
  ({
    side: "producer",
    protocol: "http",
    label,
    direction: "response",
    service: "orders-api",
    repo: "shop",
    file: "src/orders.ts",
    line,
    name: null,
    site,
    locator: "registration",
    groups: [{ type, places: [{ service: "web", repo: "shop", file: "web/a.ts", line: 3, loc: "web/a.ts:3" }] }],
    untyped: [],
    total: 1,
  }) as unknown as ContractRow;

const ROWS = [
  row(0, "GET /orders", "/orders", 13, '{ total: number; a: "one"; b: "two" }'),
  row(1, "POST /orders", "/orders", 14, "{ created: boolean }"),
];

test("two rows arm on real TypeScript 7; a break is one line, a half-written edit none, moved lines stay armed", { timeout: 240_000 }, async (t) => {
  const dir = fs.realpathSync(fs.mkdtempSync(path.join(os.tmpdir(), "held-e2e-")));
  t.after(() => fs.rmSync(dir, { recursive: true, force: true }));
  const root = path.join(dir, "ws");
  const home = path.join(dir, "home");
  fs.cpSync(FIXTURE, root, { recursive: true, dereference: true });
  const orders = path.join(root, "src", "orders.ts");
  // `total` takes its type from the helper, so a change to the helper alone reaches the route.
  fs.writeFileSync(orders, fs.readFileSync(orders, "utf8").replace("const total: number =", "const total ="));
  const helper = path.join(root, "src", "helper.ts");
  const helperOriginal = fs.readFileSync(helper, "utf8");
  const original = fs.readFileSync(orders, "utf8");
  const init: ServiceInit = { repo: "shop", service: "orders-api", repo_path: root, sidecar_root: root, tsconfig: null, scan_root: root };
  const env = { ...process.env, CARRICK_COMPILER_LINES: "1", CARRICK_CONTRACT_LINES: "1" };
  let current = ROWS;
  let stampValue = "fixed";
  const runtime = createRuntime({
    root,
    home,
    env,
    entry: path.join(SIDECAR, "dist", "src", "index.js"),
    rows: async () => current,
    stamp: () => stampValue,
  });
  const daemon = new HeldDaemon({ root, home, env, programs: async () => [init], runtime });
  t.after(() => daemon.close());

  let session = 0;
  const ask = (kind: "read" | "edit"): Promise<HeldAnswer> =>
    daemon.handle({ session: `s${session++}`, files: [orders], kind, edited_at: Date.now() });
  const edit = async (text: string): Promise<HeldAnswer> => {
    fs.writeFileSync(orders, text);
    // Bump the mtime past the filesystem's resolution.
    const later = new Date(Date.now() + 5000 * (session + 1));
    fs.utimesSync(orders, later, later);
    return ask("edit");
  };

  // The first request starts the engine and answers cold; wait for it to be ready.
  assert.equal((await ask("read")).state, "cold");
  let ready: HeldAnswer | null = null;
  for (let i = 0; i < 200 && ready === null; i++) {
    await new Promise((resolve) => setTimeout(resolve, 500));
    const answer = await ask("edit");
    if (answer.state === "ready") ready = answer;
  }
  assert.ok(ready, "the engine never became ready");
  assert.deepEqual(ready.lines, [], "the unedited file says nothing");

  // Both rows armed: the gate's record names them.
  const placed = JSON.parse(fs.readFileSync(placedPath(root, "orders-api", home), "utf8")) as { armed: Array<{ label: string }>; unarmed: unknown[] };
  assert.deepEqual(placed.armed.map((a) => a.label).sort(), ["GET /orders", "POST /orders"]);
  assert.deepEqual(placed.unarmed, []);

  // A member the row's type requires is gone: exactly one line, naming the route.
  const broken = await edit(original.replace("c.json({ total, a, b })", "c.json({ a, b })"));
  assert.equal(broken.state, "ready");
  assert.equal(broken.lines.length, 1, JSON.stringify(broken.lines));
  assert.equal(broken.lines[0]!.label, "GET /orders");
  assert.equal(broken.lines[0]!.file, "src/orders.ts");

  // Restored: silent again (the twin pass of the last answer left clean text).
  assert.deepEqual((await edit(original)).lines, []);

  // Half-written inside the handler: a syntax error is never a line.
  const half = await edit(original.replace("c.json({ total, a, b })", "c.json({ total: orders., a, b })"));
  assert.equal(half.state, "ready");
  assert.deepEqual(half.lines, []);

  // A comment line above both routes moves them: both stay armed (site-literal fallback).
  const moved = "// moved\n" + original;
  assert.deepEqual((await edit(moved)).lines, []);
  const movedBroken = await edit(moved.replace("c.json({ total, a, b })", "c.json({ a, b })"));
  assert.equal(movedBroken.lines.length, 1, JSON.stringify(movedBroken.lines));
  assert.equal(movedBroken.lines[0]!.label, "GET /orders");

  // A file the hook never named changes on disk behind the daemon (a checkout, a
  // formatter): it is synced before the answer, and the route it reaches speaks.
  fs.writeFileSync(orders, original);
  fs.writeFileSync(helper, helperOriginal.replace('Number(process.env.LIMIT ?? "10")', '"ten"'));
  const later = new Date(Date.now() + 60_000);
  fs.utimesSync(helper, later, later);
  const drift = await ask("edit");
  assert.equal(drift.lines.length, 1, JSON.stringify(drift.lines));
  assert.equal(drift.lines[0]!.label, "GET /orders");
  fs.writeFileSync(helper, helperOriginal);
  const back = new Date(Date.now() + 120_000);
  fs.utimesSync(helper, back, back);
  assert.deepEqual((await ask("edit")).lines, []);

  // The fact file moves and a row's stated type changes to one the route does
  // not have: the row is gated again (in the background) and is unarmed, with
  // its reason saved, while the other row stays armed.
  fs.writeFileSync(orders, original);
  current = [ROWS[0]!, row(1, "POST /orders", "/orders", 14, "{ created: string }")];
  stampValue = "moved";
  type Placed = { armed: Array<{ label: string }>; unarmed: Array<{ label: string; reason: string }> };
  let regated: Placed | null = null;
  for (let i = 0; i < 60 && regated === null; i++) {
    await ask("edit");
    await new Promise((resolve) => setTimeout(resolve, 500));
    const now: Placed = JSON.parse(fs.readFileSync(placedPath(root, "orders-api", home), "utf8"));
    if (now.unarmed.length > 0) regated = now;
  }
  assert.ok(regated, "the changed row was never gated again");
  assert.deepEqual(regated.armed.map((a) => a.label), ["GET /orders"]);
  assert.match(regated.unarmed[0]!.reason, /^clean_check_fails:TS\d+$/);
});
