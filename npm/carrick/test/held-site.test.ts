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

// Probed 15 Nov 2026 (carrick#4556):`find_site` takes the FIRST exact text match on or after the
// OLD line number, so repeated text can name another node. Each case below states the right answer and is marked
// todo until the engine action picks the match nearest the old position; the runtime must not guess around it.
const SEND = "function a() {\n  const result = load();\n  return c.json(result);\n}\n";
const sendStart = SEND.lastIndexOf("result");
const nodeAt = (text: string, at: { start: number } | { reason: string }): string =>
  "reason" in at ? at.reason : `line ${text.slice(0, at.start).split("\n").length}`;

for (const [tag, newText, wanted] of [
  ["a line added above and one below: the send, not the binding of the same name", "// top\n" + SEND + "bottom();\n", "line 4"],
  ["a second handler sending the same name above: this handler's send", "function b() {\n  const result = load();\n  return c.json(result);\n}\n" + SEND, "line 8"],
] as const) {
  test(`find_site on repeated text: ${tag}`, { todo: "find_site picks the first match from the old line (carrick#4556)" }, async (t) => {
    const home = fs.mkdtempSync(path.join(os.tmpdir(), "carrick-held-site-"));
    t.after(() => fs.rmSync(home, { recursive: true, force: true }));
    const found = await engineSiteFinder(ENTRY, process.env, home)({ file: "a.ts", oldText: SEND, newText, start: sendStart, end: sendStart + "result".length });
    assert.equal(nodeAt(newText, found), wanted);
  });
}
