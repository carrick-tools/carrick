// `FactStore.graphqlProducerResponses()` and `graphqlTypes()` (part 3, L-G1a):
// the GraphQL send form's rows and the service's SDL table, from the wire's
// `graphql_root` and `gqltypes` kinds. The HTTP query stays HTTP-only.

import test from "node:test";
import assert from "node:assert/strict";
import fs from "node:fs";
import os from "node:os";
import path from "node:path";
import { fixturePath } from "./helpers.ts";
import { buildFacts, factsPath, loadSqlite, openFacts } from "../src/facts/store.ts";

const SLUG = "demo-shop";
type Wire = Record<string, unknown>;
const wire = (): Wire[] => fs.readFileSync(fixturePath("edit-facts.s1.ndjson"), "utf8").split("\n").filter(Boolean).map((l) => JSON.parse(l) as Wire);
const isGql = (r: Wire): boolean => r["k"] === "contract" && r["side"] === "producer" && r["direction"] === "response" && r["protocol"] === "graphql";

async function workspace(t: { after: (fn: () => void) => void }, rows: Wire[] = wire()): Promise<string> {
  const root = fs.realpathSync(fs.mkdtempSync(path.join(os.tmpdir(), "carrick-held-gql-")));
  t.after(() => fs.rmSync(root, { recursive: true, force: true }));
  fs.mkdirSync(path.join(root, ".carrick", "hosted"), { recursive: true });
  fs.writeFileSync(path.join(root, ".carrick", "hosted", "snapshot.json"), JSON.stringify({ resolution: { repos: [{ project_slug: SLUG }] } }));
  await buildFacts(rows.map((r) => JSON.stringify(r)).join("\n") + "\n", factsPath(root), [], new Date().toISOString());
  return root;
}

test("graphqlProducerResponses returns the GraphQL producer response rows with their root, and no HTTP row", async (t) => {
  const root = await workspace(t);
  const expected = wire().filter(isGql);
  assert.ok(expected.length > 0, "the fixture holds a GraphQL resolver row");
  assert.deepEqual(expected[0]!["graphql_root"], { type: "Query", field: "order" });
  const store = (await openFacts(root))!;
  assert.deepEqual(store.graphqlProducerResponses(), expected);
  assert.ok(store.producerResponses().length > 0 && store.producerResponses().every((r) => r.protocol === "http"), "the HTTP query is unchanged");
  store.close();
});

test("graphqlTypes returns the service's table, and null for a service without one", async (t) => {
  const root = await workspace(t);
  const row = wire().find((r) => r["k"] === "gqltypes")!;
  const store = (await openFacts(root))!;
  assert.deepEqual(store.graphqlTypes(String(row["repo"]), String(row["service"])), row["types"]);
  assert.equal(store.graphqlTypes("billing-api", "billing-api"), null);
  store.close();
});

test("a malformed gqltypes row or a malformed graphql_root drops that row only", async (t) => {
  const rows = wire();
  const keep = rows.length;
  const bad = structuredClone(rows.find(isGql)!);
  bad["graphql_root"] = { type: "Query" };
  rows.push(bad, { k: "gqltypes", repo: "gateway", service: "orders", types: "nope" });
  const store = (await openFacts(await workspace(t, rows)))!;
  assert.equal(store.graphqlProducerResponses().length, wire().filter(isGql).length, "the half row is not stored");
  assert.equal(store.graphqlTypes("gateway", "orders"), null);
  assert.ok(rows.length > keep);
  store.close();
});

test("a file built without the gqltypes or contracts table answers no rows and no table", async (t) => {
  const root = await workspace(t);
  const sqlite = (await loadSqlite())!;
  const db = new sqlite.DatabaseSync(factsPath(root));
  db.exec("DROP TABLE contracts; DROP TABLE gqltypes");
  db.close();
  const store = (await openFacts(root))!;
  assert.deepEqual(store.graphqlProducerResponses(), []);
  assert.equal(store.graphqlTypes("gateway", "gateway"), null);
  store.close();
});

test("a producer row without graphql_root (an older file's) is read as it was", async (t) => {
  const rows = wire().map((r) => {
    if (!isGql(r)) return r;
    const { graphql_root: _root, ...rest } = r;
    return rest;
  });
  const store = (await openFacts(await workspace(t, rows)))!;
  const got = store.graphqlProducerResponses();
  assert.ok(got.length > 0);
  assert.equal(got[0]!.graphql_root, undefined);
  store.close();
});
