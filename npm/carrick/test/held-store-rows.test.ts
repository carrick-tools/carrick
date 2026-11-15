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

// `values` (writer 8): where each reply of a producer row is sent from.

test("the writer-8 fixture's producer rows carry values, and they come back as written", async (t) => {
  const root = await workspace(t);
  const store = (await openFacts(root))!;
  const rows = store.producerResponses();
  store.close();
  const withValues = rows.filter((r) => (r.values?.length ?? 0) > 0);
  assert.ok(withValues.length >= 2, "the fixture holds rows with values");
  assert.ok(withValues.some((r) => r.values!.length === 2), "one has two sends");
  assert.ok(withValues.some((r) => r.values!.some((v) => v.file !== r.file)), "one has a send in another file");
});

test("an absent values and an empty one are read; a malformed values drops that row only", async (t) => {
  const lines = fs.readFileSync(fixturePath("edit-facts.s1.ndjson"), "utf8").split("\n").filter(Boolean);
  const header = lines[0]!;
  const base = lines
    .slice(1)
    .map((line) => JSON.parse(line) as Record<string, unknown>)
    .find((r) => r["k"] === "contract" && r["side"] === "producer" && r["direction"] === "response" && r["protocol"] === "http")!;
  const variant = (label: string, values: unknown): string => {
    const row: Record<string, unknown> = { ...base, label };
    if (values === "absent") delete row["values"];
    else row["values"] = values;
    return JSON.stringify(row);
  };
  const cases: Array<[string, unknown, boolean]> = [
    ["absent", "absent", true],
    ["empty", [], true],
    ["whole", [{ file: "src/a.ts", span: [3, 9] }], true],
    ["not-an-array", "src/a.ts", false],
    ["empty-span", [{ file: "src/a.ts", span: [9, 9] }], false],
    ["negative", [{ file: "src/a.ts", span: [-1, 4] }], false],
    ["fraction", [{ file: "src/a.ts", span: [1.5, 4] }], false],
    ["no-file", [{ span: [1, 4] }], false],
    ["three-numbers", [{ file: "src/a.ts", span: [1, 4, 6] }], false],
  ];
  const root = fs.realpathSync(fs.mkdtempSync(path.join(os.tmpdir(), "carrick-held-values-")));
  t.after(() => fs.rmSync(root, { recursive: true, force: true }));
  fs.mkdirSync(path.join(root, ".carrick", "hosted"), { recursive: true });
  fs.writeFileSync(path.join(root, ".carrick", "hosted", "snapshot.json"), JSON.stringify({ resolution: { repos: [{ project_slug: SLUG }] } }));
  const text = [header, ...cases.map(([label, values]) => variant(label, values))].join("\n") + "\n";
  await buildFacts(text, factsPath(root), [], new Date().toISOString());
  const store = (await openFacts(root))!;
  const kept = store.producerResponses().map((r) => r.label);
  store.close();
  assert.deepEqual(kept, cases.filter(([, , ok]) => ok).map(([label]) => label));
});
