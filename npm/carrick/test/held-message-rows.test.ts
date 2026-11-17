// HM-2 (unwired): `FactStore.messageRows()` and the pure `messageSendRows`
// join. Invented names only: service `orders`, event `order:update`.

import test from "node:test";
import assert from "node:assert/strict";
import fs from "node:fs";
import os from "node:os";
import path from "node:path";
import { fixturePath } from "./helpers.ts";
import { buildFacts, factsPath, loadSqlite, openFacts, type ContractRow } from "../src/facts/store.ts";
import { messageSendRows } from "../src/held/message-rows.ts";

type Place = ContractRow["groups"][number]["places"][number];

const place = (file: string, line: number, span: [number, number] | null): Place => ({
  service: "web", repo: "web", file, line, loc: `web/${file}:${line}`, alias: "a", infer_kind: "call_result", span,
});
const group = (type: string, places: Place[]): ContractRow["groups"][number] => ({
  type, symbol: null, home: null, places, total: places.length, shown: places.length, lead: null, more: null, tail: null, shortened: false,
});
const row = (side: "producer" | "consumer", label: string, file: string, line: number, groups: ContractRow["groups"]): ContractRow => ({
  side, protocol: "socket", label, direction: "response", service: side === "producer" ? "orders" : "web",
  repo: side === "producer" ? "orders" : "web", file, line, name: null, site: null,
  locator: side === "producer" ? "registration" : "call", groups, untyped: [], total: groups.length,
});

const UPDATE = "CLIENT->SERVER order:update";
const CANCEL = "CLIENT->SERVER order:cancel";

test("a socket emitter row gets values from the listener row's place", () => {
  const listener = row("producer", UPDATE, "src/orders.ts", 4, [group("{ id: string }", [place("src/send.ts", 15, [40, 62])])]);
  const emitter = row("consumer", UPDATE, "src/send.ts", 15, [group("OrderUpdate", [])]);
  const got = messageSendRows([listener, emitter]);
  assert.equal(got.length, 1);
  assert.deepEqual(got[0]!.values, [{ file: "src/send.ts", span: [40, 62] }]);
  assert.deepEqual(got[0]!.groups, emitter.groups);
  assert.equal(got[0]!.side, "consumer");
});

test("two events on one line stay apart", () => {
  const listenerA = row("producer", UPDATE, "src/orders.ts", 4, [group("A", [place("src/send.ts", 15, [10, 20])])]);
  const listenerB = row("producer", CANCEL, "src/orders.ts", 9, [group("B", [place("src/send.ts", 15, [30, 44])])]);
  const a = row("consumer", UPDATE, "src/send.ts", 15, [group("OrderUpdate", [])]);
  const b = row("consumer", CANCEL, "src/send.ts", 15, [group("OrderCancel", [])]);
  const got = messageSendRows([listenerA, listenerB, a, b]);
  assert.deepEqual(got.map((r) => r.values), [[{ file: "src/send.ts", span: [10, 20] }], [{ file: "src/send.ts", span: [30, 44] }]]);
});

test("an emitter with no span has no values", () => {
  const listener = row("producer", UPDATE, "src/orders.ts", 4, [group("{ id: string }", [place("src/send.ts", 15, null)])]);
  const emitter = row("consumer", UPDATE, "src/send.ts", 15, [group("OrderUpdate", [])]);
  const got = messageSendRows([listener, emitter]);
  assert.equal(got.length, 1);
  assert.equal("values" in got[0]!, false);
});

test("the same span from two listener rows is kept once", () => {
  const p = place("src/send.ts", 15, [40, 62]);
  const l1 = row("producer", UPDATE, "src/orders.ts", 4, [group("X", [p])]);
  const l2 = row("producer", UPDATE, "src/orders2.ts", 8, [group("X", [p])]);
  const got = messageSendRows([l1, l2, row("consumer", UPDATE, "src/send.ts", 15, [])]);
  assert.equal(got[0]!.values!.length, 1);
});

const SLUG = "demo-shop";
async function workspace(t: { after: (fn: () => void) => void }): Promise<string> {
  const root = fs.realpathSync(fs.mkdtempSync(path.join(os.tmpdir(), "carrick-held-msg-")));
  t.after(() => fs.rmSync(root, { recursive: true, force: true }));
  fs.mkdirSync(path.join(root, ".carrick", "hosted"), { recursive: true });
  fs.writeFileSync(path.join(root, ".carrick", "hosted", "snapshot.json"), JSON.stringify({ resolution: { repos: [{ project_slug: SLUG }] } }));
  await buildFacts(fs.readFileSync(fixturePath("edit-facts.s1.ndjson"), "utf8"), factsPath(root), [], new Date().toISOString());
  return root;
}
const wire = (): Array<Record<string, unknown>> =>
  fs.readFileSync(fixturePath("edit-facts.s1.ndjson"), "utf8").split("\n").filter(Boolean).map((l) => JSON.parse(l) as Record<string, unknown>);

test("messageRows returns the socket response rows in wire order, and no other protocol", async (t) => {
  const expected = wire().filter((r) => r["k"] === "contract" && r["direction"] === "response" && r["protocol"] === "socket");
  assert.ok(expected.length > 0, "the fixture holds socket response rows");
  const store = (await openFacts(await workspace(t)))!;
  assert.deepEqual(store.messageRows(), expected);
  assert.ok(store.messageRows().every((r) => r.protocol === "socket"));
  store.close();
});

test("an old fact file without contracts returns []", async (t) => {
  const root = await workspace(t);
  const sqlite = (await loadSqlite())!;
  const db = new sqlite.DatabaseSync(factsPath(root));
  db.exec("DROP TABLE contracts");
  db.close();
  const store = (await openFacts(root))!;
  assert.deepEqual(store.messageRows(), []);
  store.close();
});

test("producerResponses and graphqlProducerResponses return the same rows as before", async (t) => {
  const rows = wire().filter((r) => r["k"] === "contract" && r["side"] === "producer" && r["direction"] === "response");
  const http = rows.filter((r) => r["protocol"] === "http");
  const gql = rows.filter((r) => r["protocol"] === "graphql");
  assert.ok(http.length > 0 && gql.length > 0);
  const store = (await openFacts(await workspace(t)))!;
  assert.deepEqual(store.producerResponses(), http);
  assert.deepEqual(store.graphqlProducerResponses(), gql);
  store.close();
});
