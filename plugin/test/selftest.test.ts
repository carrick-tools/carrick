import test from "node:test";
import assert from "node:assert/strict";
import { run, SHAPES } from "./selftest.ts";

test("every install shape delivers on exactly the channel it should", async () => {
  const { rows, failures } = await run();
  assert.equal(rows.length, SHAPES.length);
  assert.equal(failures, 0, JSON.stringify(rows, null, 2));
  for (const row of rows) {
    const delivered = row.hookContexts + row.diagnosticAttachments;
    if (row.name === "silenced") assert.equal(delivered, 0);
    else assert.equal(delivered, 1, `${row.name} delivered ${delivered} times`);
  }
});
