// A fact line that is dropped saves its reason (carrick#4052): `FactOutcome.stops`
// counts each dropped candidate under one stop word, once per kind and row, and
// the post-edit timing row carries it as `facts_stops`. Nothing is printed for
// a stop; these tests also hold that every case stays silent.

import test from "node:test";
import assert from "node:assert/strict";
import fs from "node:fs";
import os from "node:os";
import path from "node:path";
import { spawnSync } from "node:child_process";
import { fnRow, lineOf, ndjson, workspace } from "./facts-helpers.ts";
import type { Ws } from "./facts-helpers.ts";
import { factLines, pickLines } from "../src/facts/lines.ts";
import type { Candidate } from "../src/facts/lines.ts";
import { buildFacts, factsPath, openFacts } from "../src/facts/store.ts";
import type { FactStore } from "../src/facts/store.ts";
import { readTimings, timed } from "../src/hook/timings.ts";

const ORDERS = [
  "export function loadOrder(id: string) {",
  "  const key = `order:${id}`;",
  "  return cache.get(key);",
  "}",
  "",
].join("\n");
const PAGE = ["export function render() {", "  const id = '1';", "  return loadOrder(id);", "}", ""].join("\n");
const LOAD_LINE = "loadOrder is called from web/src/page.ts:4";
const PEER = { repo: "web", file: "src/page.ts", line: 3, name: "render" };
const FN_PEER = { ...fnRow("loadOrder", LOAD_LINE), others: [PEER] };

/** The edit inside `loadOrder`, as post-edit hands it on. */
const inLoadOrder = (ws: Ws) => {
  const file = ws.at("api/src/orders.ts");
  return { file, lines: [lineOf(file, "const key")], removed: [], removedText: [] };
};

const FILES = { "api/src/orders.ts": ORDERS, "web/src/page.ts": PAGE };

async function rebuild(ws: Ws, rows: Array<Record<string, unknown>>, commits: Record<string, string | null>): Promise<void> {
  const local = ["api", "web"].filter((repo) => fs.existsSync(ws.at(repo)));
  await buildFacts(ndjson(rows, commits), factsPath(ws.root), local.map((repo) => ({ repo, path: ws.at(repo) })), new Date().toISOString());
}

/** A store whose `callers` lookup throws: a rule that fails. */
async function throwingStore(ws: Ws): Promise<FactStore> {
  const real = (await openFacts(ws.root))!;
  return new Proxy(real, {
    get(target, prop) {
      if (prop === "callers") return () => { throw new Error("a field ingest let through"); };
      const value = Reflect.get(target, prop) as unknown;
      return typeof value === "function" ? value.bind(target) : value;
    },
  });
}

test("peer_changed: a peer file edited after the index commit is counted once", async (t) => {
  const ws = await workspace(t, FILES, [FN_PEER]);
  fs.writeFileSync(ws.at("web/src/page.ts"), `// moved down\n${PAGE}`);
  const out = await factLines(ws.root, [inLoadOrder(ws)], undefined);
  assert.deepEqual(out.lines, []);
  assert.deepEqual(out.stops, { peer_changed: 1 });
});

test("a candidate naming two changed peer files counts once, not once per file", async (t) => {
  const two = { ...FN_PEER, others: [PEER, { repo: "web", file: "src/other.ts", line: 1, name: "x" }] };
  const ws = await workspace(t, { ...FILES, "web/src/other.ts": "export function x() {}\n" }, [two]);
  fs.writeFileSync(ws.at("web/src/page.ts"), `// moved down\n${PAGE}`);
  fs.writeFileSync(ws.at("web/src/other.ts"), "export function x() { return 1; }\n");
  const out = await factLines(ws.root, [inLoadOrder(ws)], undefined);
  assert.deepEqual(out.lines, []);
  assert.deepEqual(out.stops, { peer_changed: 1 });
});

test("peer_not_checked_out: a peer repo with no checkout here is counted", async (t) => {
  const ws = await workspace(t, { "api/src/orders.ts": ORDERS }, [FN_PEER]);
  const out = await factLines(ws.root, [inLoadOrder(ws)], undefined);
  assert.deepEqual(out.lines, []);
  assert.deepEqual(out.stops, { peer_not_checked_out: 1 });
});

test("peer_no_index_commit: a repo the fact file holds no commit for is counted", async (t) => {
  const ws = await workspace(t, FILES, [FN_PEER]);
  await rebuild(ws, [FN_PEER], { web: null });
  const out = await factLines(ws.root, [inLoadOrder(ws)], undefined);
  assert.deepEqual(out.lines, []);
  assert.deepEqual(out.stops, { peer_no_index_commit: 1 });
});

test("peer_git_failed: a commit the clone lacks is counted", async (t) => {
  const ws = await workspace(t, FILES, [FN_PEER]);
  await rebuild(ws, [FN_PEER], { web: "a".repeat(40) });
  const out = await factLines(ws.root, [inLoadOrder(ws)], undefined);
  assert.deepEqual(out.lines, []);
  assert.deepEqual(out.stops, { peer_git_failed: 1 });
});

test("peer_git_failed: git slower than its budget is counted", async (t) => {
  const ws = await workspace(t, FILES, [FN_PEER]);
  const shim = fs.mkdtempSync(path.join(os.tmpdir(), "carrick-stops-git-"));
  t.after(() => fs.rmSync(shim, { recursive: true, force: true }));
  const real = spawnSync("sh", ["-c", "command -v git"], { encoding: "utf8" }).stdout.trim();
  fs.writeFileSync(path.join(shim, "git"), `#!/bin/sh\nfor arg in "$@"; do [ "$arg" = ls-tree ] && sleep 2; done\nexec ${real} "$@"\n`, { mode: 0o755 });
  const env = { ...process.env, PATH: `${shim}${path.delimiter}${process.env["PATH"] ?? ""}`, CARRICK_FACTS_DIFF_BUDGET_MS: "300" };
  const out = await factLines(ws.root, [inLoadOrder(ws)], undefined, { env });
  assert.deepEqual(out.lines, []);
  assert.deepEqual(out.stops, { peer_git_failed: 1 });
});

test("no_changed_lines: an edit whose changed lines are unknown is counted", async (t) => {
  const ws = await workspace(t, FILES, [FN_PEER]);
  const out = await factLines(ws.root, [{ file: ws.at("api/src/orders.ts") }], undefined);
  assert.deepEqual(out.lines, []);
  assert.deepEqual(out.stops, { no_changed_lines: 1 });
});

test("not_read_cleanly: a file that does not scan cleanly is counted", async (t) => {
  const ws = await workspace(t, FILES, [FN_PEER]);
  fs.writeFileSync(ws.at("api/src/orders.ts"), "export function loadOrder(id: string) {\n  return cache.get(\n}\n");
  const out = await factLines(ws.root, [{ file: ws.at("api/src/orders.ts"), snippets: ["return cache.get("] }], undefined);
  assert.deepEqual(out.lines, []);
  assert.deepEqual(out.stops, { not_read_cleanly: 1 });
});

test("rule_failed: a lookup that throws is counted", async (t) => {
  const ws = await workspace(t, FILES, [FN_PEER]);
  const out = await factLines(ws.root, [inLoadOrder(ws)], undefined, { store: await throwingStore(ws) });
  assert.deepEqual(out.lines, []);
  assert.deepEqual(out.stops, { rule_failed: 1 });
});

test("rule_failed: a candidate's check that throws is counted, once per kind and row", () => {
  const boom: Candidate = { key: "fn::a", text: "a", named: [], holds: () => { throw new Error("x"); } };
  const stops: Record<string, number> = {};
  const out = pickLines({ fn: [boom, boom], op: [], name: [], pkg: [] }, new Set(), {}, 0, { stops });
  assert.deepEqual(out.lines, []);
  assert.deepEqual(stops, { rule_failed: 1 });
});

test("a line that prints counts no stop", async (t) => {
  const ws = await workspace(t, FILES, [FN_PEER]);
  const out = await factLines(ws.root, [inLoadOrder(ws)], undefined);
  assert.deepEqual(out.lines, [LOAD_LINE]);
  assert.deepEqual(out.stops, {});
});

test("the post-edit timing row carries the stops as facts_stops, and writes nothing for none", async (t) => {
  const ws = await workspace(t, FILES, [FN_PEER]);
  fs.mkdirSync(path.join(ws.root, ".carrick"), { recursive: true });
  await timed("post-edit", async (timing) => {
    timing.root = ws.root;
    timing.factsMs = 1;
    timing.factsStops = { peer_changed: 2, rule_failed: 1 };
  });
  await timed("post-edit", async (timing) => {
    timing.root = ws.root;
    timing.factsMs = 1;
    timing.factsStops = {};
  });
  const rows = readTimings(ws.root);
  assert.equal(rows.length, 2);
  assert.deepEqual(rows[0]!.facts_stops, { peer_changed: 2, rule_failed: 1 });
  assert.equal("facts_stops" in rows[1]!, false);
});
