// `engineSiteFinder`: the compiler engine's `find_site` action, through the one engine client.

import test from "node:test";
import assert from "node:assert/strict";
import fs from "node:fs";
import os from "node:os";
import path from "node:path";
import { engineSiteFinder } from "../src/held/shadow.ts";
import { testDir } from "./helpers.ts";

const ENTRY = path.resolve(testDir, "..", "..", "..", "src", "sidecar", "dist", "src", "index.js");

test("a moved value is found again; a value that is not a node, or cannot be wrapped, comes back as a reason", async (t) => {
  assert.ok(fs.existsSync(ENTRY), "the compiler engine is built");
  const home = fs.mkdtempSync(path.join(os.tmpdir(), "carrick-held-site-"));
  t.after(() => fs.rmSync(home, { recursive: true, force: true }));
  const find = engineSiteFinder(ENTRY, process.env, home);
  const oldText = "const a = 1;\nsend({ id, total });\n";
  const start = oldText.indexOf("{ id, total }");
  const end = start + "{ id, total }".length;
  const newText = "const kept = 2;\nconst b = 3;\nsend({ id, total });\n";
  const found = await find({ file: "a.ts", oldText, newText, start, end });
  assert.deepEqual(found, { start: newText.indexOf("{ id, total }"), end: newText.indexOf("{ id, total }") + "{ id, total }".length });
  assert.deepEqual(await find({ file: "a.ts", oldText, newText: "send({ id });\n", start, end }), { reason: "value_not_a_node" });
  const shorthand = "const result = 1;\nconst r = { result };\n";
  const at = shorthand.lastIndexOf("result");
  assert.deepEqual(await find({ file: "a.ts", oldText: shorthand, newText: shorthand, start: at, end: at + "result".length }), { reason: "not_wrappable" });
});
