// `askEngine`: the one client of the compiler engine's stdio protocol.

import test from "node:test";
import assert from "node:assert/strict";
import fs from "node:fs";
import os from "node:os";
import path from "node:path";
import { askEngine } from "../src/engine-ask.ts";

function fakeEngine(t: { after: (fn: () => void) => void }, body: string): string {
  const dir = fs.mkdtempSync(path.join(os.tmpdir(), "carrick-engine-ask-"));
  t.after(() => fs.rmSync(dir, { recursive: true, force: true }));
  const entry = path.join(dir, "engine.mjs");
  fs.writeFileSync(entry, body);
  return entry;
}

const ECHO = `
import readline from "node:readline";
const rl = readline.createInterface({ input: process.stdin });
rl.on("line", (line) => {
  const req = JSON.parse(line);
  process.stdout.write(JSON.stringify({ request_id: req.request_id, status: "progress" }) + "\\n");
  process.stdout.write("not a protocol line\\n");
  process.stdout.write(JSON.stringify({ request_id: req.request_id, status: "success", action: req.action }) + "\\n");
});
`;

test("each request's last non-progress frame comes back by request_id, and a shutdown follows", async (t) => {
  const entry = fakeEngine(t, ECHO);
  const frames = await askEngine(entry, [{ action: "init", request_id: "a" }, { action: "find_site", request_id: "b" }], { timeoutMs: 10_000 });
  assert.deepEqual([...frames.keys()].sort(), ["a", "b", "shutdown"]);
  assert.equal(frames.get("a")!.status, "success");
  assert.equal(frames.get("b")!["action"], "find_site");
  assert.equal(frames.get("shutdown")!["action"], "shutdown");
});

test("an engine that answers nothing rejects", async (t) => {
  const entry = fakeEngine(t, "process.stdin.resume(); process.stdin.on('end', () => process.exit(0));");
  await assert.rejects(askEngine(entry, [{ action: "init", request_id: "a" }], { timeoutMs: 10_000 }), /no answer/);
});
