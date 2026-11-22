// A file read before the workspace has a fact file is owed its fact lines: the
// first Read, shell read or edit after the file lands says them once, and the
// binary's `check` answer for that file is not asked for or printed again.
// Rows are hand-written in the wire's shape (`facts-helpers.ts`).

import test from "node:test";
import assert from "node:assert/strict";
import fs from "node:fs";
import os from "node:os";
import path from "node:path";
import { renderPostRead } from "../src/render.ts";
import { readSession } from "../src/hook/reuse.ts";
import { fakeEnv, fixture, fixturePath, runHook } from "./helpers.ts";
import { context, landFacts, patchEdit, workspace } from "./facts-helpers.ts";
import type { Ws } from "./facts-helpers.ts";

const ORDERS = ['import { Router } from "express";', "export const router = Router();", "", "export function listOrders(req, res) {", "  return res.json([]);", "}", 'router.get("/orders", listOrders);', ""].join("\n");
const MORE = ['import { Router } from "express";', "export const more = Router();", 'more.get("/more", (req, res) => {', "  res.json([]);", "});", ""].join("\n");
const ORDERS_OP = "GET /orders is called at web/src/client.ts:1";
const MORE_OP = "GET /more is called at web/src/client.ts:1";
const WEB = { service: "web", repo: "web", file: "src/client.ts", line: 1 };
const op = (name: string | null, label: string, text: string, file: string, site: string | null = null) => ({
  k: "op",
  side: "producer",
  protocol: "http",
  label,
  service: "api",
  repo: "api",
  file,
  line: 1,
  name,
  site,
  others: [WEB],
  total: 1,
  source: "route",
  text,
});
const ROWS = [op("listOrders", "GET /orders", ORDERS_OP, "src/orders.ts"), op(null, "GET /more", MORE_OP, "src/more.ts", "/more")];
const FILES = { "api/src/orders.ts": ORDERS, "api/src/more.ts": MORE, "api/src/plain.ts": "export const one = 1;\n", "web/src/client.ts": "export const c = 1;\n" };
const READ_ON = { CARRICK_READ_LINES: "1" };

function logPath(t: { after: (fn: () => void) => void }): string {
  const dir = fs.mkdtempSync(path.join(os.tmpdir(), "carrick-owed-log-"));
  t.after(() => fs.rmSync(dir, { recursive: true, force: true }));
  return path.join(dir, "argv.log");
}

function argvCalls(log: string): number {
  return fs.existsSync(log) ? fs.readFileSync(log, "utf8").split("\n").filter((line) => line.trim() !== "").length : 0;
}

function read(ws: Ws, rel: string, session: string, argvLog: string) {
  const file = ws.at(rel);
  return runHook("post-read.ts", {
    payload: { hook_event_name: "PostToolUse", tool_name: "Read", session_id: session, cwd: ws.root, tool_input: { file_path: file }, tool_response: { file: { filePath: file, content: "x" } } },
    env: fakeEnv({ HOME: ws.home, CLAUDE_PROJECT_DIR: ws.root, CARRICK_FAKE_FIXTURE: fixturePath("check-uses.json"), CARRICK_FAKE_ARGV_LOG: argvLog, ...READ_ON }),
  });
}

const owedOf = (ws: Ws, session: string): string[] => Object.keys(readSession(session, ws.home).owed);
const count = (text: string, needle: string): number => text.split(needle).length - 1;

test("a file read before the fact file lands gets its lines at the next edit, once", async (t) => {
  const ws = await workspace(t, FILES, null);
  const log = logPath(t);
  const session = "owed-edit";
  // No fact file: the Read is answered by the binary alone and the file is owed.
  const first = await read(ws, "api/src/orders.ts", session, log);
  const binary = renderPostRead(fixture("check-uses.json"), "api/src/orders.ts");
  assert.ok(binary, "the fixture has an answer");
  assert.equal(context(first.stdout), binary);
  assert.equal(argvCalls(log), 1);
  assert.deepEqual(owedOf(ws, session), [ws.at("api/src/orders.ts")]);

  await landFacts(ws.root, ROWS);
  // An edit of another file: its own line, then the owed file's.
  const edit = context((await patchEdit(ws, ws.at("api/src/more.ts"), [{ newStart: 3, lines: ['+more.get("/more", (req, res) => {'] }], { session, env: READ_ON })).stdout);
  assert.equal(count(edit, MORE_OP), 1, edit);
  assert.equal(count(edit, ORDERS_OP), 1, edit);
  assert.ok(edit.indexOf(MORE_OP) < edit.indexOf(ORDERS_OP), "the edit's own line comes first");
  assert.equal(count(edit, binary), 0, "the binary's answer for the read file is not printed again");
  assert.deepEqual(owedOf(ws, session), []);

  // Said once: neither a second Read of the file nor a later edit repeats it.
  const again = await read(ws, "api/src/orders.ts", session, log);
  assert.equal(count(context(again.stdout), ORDERS_OP), 0);
  assert.equal(argvCalls(log), 1, "the binary's check is not asked for the file again");
  const later = context((await patchEdit(ws, ws.at("api/src/plain.ts"), [{ newStart: 1, lines: ["+export const one = 1;"] }], { session, env: READ_ON })).stdout);
  assert.equal(count(later, ORDERS_OP), 0);
});

test("a second Read of an owed file says its lines once the fact file has landed", async (t) => {
  const ws = await workspace(t, FILES, null);
  const log = logPath(t);
  const session = "owed-read";
  await read(ws, "api/src/orders.ts", session, log);
  // Still no file: a second Read says nothing and the file stays owed.
  assert.equal((await read(ws, "api/src/orders.ts", session, log)).stdout.trim(), "");
  assert.deepEqual(owedOf(ws, session), [ws.at("api/src/orders.ts")]);

  await landFacts(ws.root, ROWS);
  assert.equal(context((await read(ws, "api/src/orders.ts", session, log)).stdout), ORDERS_OP);
  assert.equal(argvCalls(log), 1, "one binary check for the file, at its first Read");
  assert.equal((await read(ws, "api/src/orders.ts", session, log)).stdout.trim(), "");
  assert.deepEqual(owedOf(ws, session), []);
});

test("a Read of another file carries the owed lines after its own", async (t) => {
  const ws = await workspace(t, FILES, null);
  const log = logPath(t);
  const session = "owed-other";
  await read(ws, "api/src/orders.ts", session, log);
  await landFacts(ws.root, ROWS);
  const out = context((await read(ws, "api/src/more.ts", session, log)).stdout);
  assert.ok(out.startsWith([MORE_OP, ORDERS_OP].join("\n\n")), out);
  assert.equal(count(out, ORDERS_OP), 1);
  assert.equal(count(out, "api/src/orders.ts"), 0, "the binary's answer for the owed file is not printed again");
  assert.deepEqual(owedOf(ws, session), []);
});

test("with a fact file from the start, nothing is owed", async (t) => {
  const ws = await workspace(t, FILES, ROWS);
  const session = "owed-none";
  const out = context((await read(ws, "api/src/orders.ts", session, logPath(t))).stdout);
  assert.equal(count(out, ORDERS_OP), 1, out);
  assert.deepEqual(owedOf(ws, session), []);
});
