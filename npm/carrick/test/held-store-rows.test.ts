// `FactStore.producerResponses()` (part 3, T5b): every HTTP producer response
// row, in wire order, and none when the file has no `contracts` table.

import test from "node:test";
import assert from "node:assert/strict";
import fs from "node:fs";
import os from "node:os";
import path from "node:path";
import { fixturePath } from "./helpers.ts";
import { buildFacts, factsPath, loadSqlite, openFacts } from "../src/facts/store.ts";

const SLUG = "demo-shop";

async function workspace(t: { after: (fn: () => void) => void }): Promise<string> {
  const root = fs.realpathSync(fs.mkdtempSync(path.join(os.tmpdir(), "carrick-held-rows-")));
  t.after(() => fs.rmSync(root, { recursive: true, force: true }));
  fs.mkdirSync(path.join(root, ".carrick", "hosted"), { recursive: true });
  fs.writeFileSync(path.join(root, ".carrick", "hosted", "snapshot.json"), JSON.stringify({ resolution: { repos: [{ project_slug: SLUG }] } }));
  await buildFacts(fs.readFileSync(fixturePath("edit-facts.s1.ndjson"), "utf8"), factsPath(root), [], new Date().toISOString());
  return root;
}

test("producerResponses returns the HTTP producer response rows in wire order", async (t) => {
  const root = await workspace(t);
  const wire = fs
    .readFileSync(fixturePath("edit-facts.s1.ndjson"), "utf8")
    .split("\n")
    .filter(Boolean)
    .slice(1)
    .map((line) => JSON.parse(line) as Record<string, unknown>)
    .filter((r) => r["k"] === "contract" && r["side"] === "producer" && r["direction"] === "response" && r["protocol"] === "http");
  assert.ok(wire.length > 0, "the fixture holds such rows");
  const store = (await openFacts(root))!;
  assert.deepEqual(store.producerResponses(), wire);
  store.close();
});

test("a file built without the contracts table answers no producer rows", async (t) => {
  const root = await workspace(t);
  const sqlite = (await loadSqlite())!;
  const db = new sqlite.DatabaseSync(factsPath(root));
  db.exec("DROP TABLE contracts");
  db.close();
  const store = (await openFacts(root))!;
  assert.deepEqual(store.producerResponses(), []);
  store.close();
});
