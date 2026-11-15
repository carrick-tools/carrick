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
  assert.deepEqual(await find({ file: "a.ts", oldText, newText: "send();\n", start, end }), { reason: "value_not_a_node" });
  assert.deepEqual(await find({ file: "a.ts", oldText, newText: "send({ id });\n", start, end }), { start: 5, end: 11 }, "an edited value keeps its slot");
  const shorthand = "const result = 1;\nconst r = { result };\n";
  const at = shorthand.lastIndexOf("result");
  assert.deepEqual(await find({ file: "a.ts", oldText: shorthand, newText: shorthand, start: at, end: at + "result".length }), { reason: "not_wrappable" });
});

// carrick#4556: repeated text must not name another node. `find_site` places by the value's parent slot,
// the one nearest the old position carried through the edit, never by the old text alone.
const SEND = "function a() {\n  const result = load();\n  return c.json(result);\n}\n";
const sendStart = SEND.lastIndexOf("result");
const nodeAt = (text: string, at: { start: number } | { reason: string }): string =>
  "reason" in at ? at.reason : `line ${text.slice(0, at.start).split("\n").length}`;

for (const [tag, newText, wanted] of [
  ["a line added above and one below: the send, not the binding of the same name", "// top\n" + SEND + "bottom();\n", "line 4"],
  ["a second handler sending the same name above: this handler's send", "function b() {\n  const result = load();\n  return c.json(result);\n}\n" + SEND, "line 7"],
] as const) {
  test(`find_site on repeated text: ${tag}`, async (t) => {
    const home = fs.mkdtempSync(path.join(os.tmpdir(), "carrick-held-site-"));
    t.after(() => fs.rmSync(home, { recursive: true, force: true }));
    const found = await engineSiteFinder(ENTRY, process.env, home)({ file: "a.ts", oldText: SEND, newText, start: sendStart, end: sendStart + "result".length });
    assert.equal(nodeAt(newText, found), wanted);
  });
}

test("find_site: an edit that changes the value's own text and head still places it (case e); a slot nothing fills is a reason", async (t) => {
  const home = fs.mkdtempSync(path.join(os.tmpdir(), "carrick-held-site-"));
  t.after(() => fs.rmSync(home, { recursive: true, force: true }));
  const find = engineSiteFinder(ENTRY, process.env, home);
  const oldText = "const orders = list();\nfunction h(res: R) {\n  res.json({ orders, total });\n}\n";
  const start = oldText.indexOf("{ orders, total }");
  const end = start + "{ orders, total }".length;
  // A rename of the binding and of the value's head in one edit: the old text is gone from the new text.
  const renamed = "const kept = list();\nfunction h(res: R) {\n  res.json({ orders: kept, total });\n}\n";
  const at = renamed.indexOf("{ orders: kept, total }");
  assert.deepEqual(await find({ file: "a.ts", oldText, newText: renamed, start, end }), { start: at, end: at + "{ orders: kept, total }".length });
  // Another handler above sends the very same text: this handler's send is the answer.
  const twin = "function g(res: R) {\n  res.json({ orders, total });\n}\n" + oldText;
  const own = twin.lastIndexOf("{ orders, total }");
  assert.deepEqual(await find({ file: "a.ts", oldText, newText: twin, start, end }), { start: own, end: own + "{ orders, total }".length });
  // The send was deleted: no slot fits, though the text appears in a binding.
  assert.deepEqual(await find({ file: "a.ts", oldText, newText: "const x = { orders, total };\n", start, end }), { reason: "value_not_a_node" });
});
