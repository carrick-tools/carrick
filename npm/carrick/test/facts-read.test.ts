// The Read moment answers from the fact file (T5): a Read, or a shell command that
// only reads, of a TypeScript file in a mapped repo prints the op and fn lines of the
// whole file and starts no carrick process; a file the fact file cannot answer keeps
// today's `check` answer. Rows are hand-written in the wire's shape; peers sit in the
// committed `web` repo (`facts-helpers.ts`).

import test from "node:test";
import assert from "node:assert/strict";
import fs from "node:fs";
import os from "node:os";
import path from "node:path";
import { renderPostRead } from "../src/render.ts";
import { readSession } from "../src/hook/reuse.ts";
import { fakeEnv, fixture, fixturePath, runHook } from "./helpers.ts";
import { context, lineOf, nameRow, nextCarrier, patchEdit, workspace } from "./facts-helpers.ts";
import type { Ws } from "./facts-helpers.ts";

const FN_NAMES = ["listOrders", "createOrder", "cancelOrder", "refundOrder", "shipOrder", "archiveOrder"];
const ORDERS = [
  'import { Router } from "express";',
  "export const router = Router();",
  "",
  "export function listOrders(req, res) {",
  "  return res.json([]);",
  "}",
  'router.get("/orders", listOrders);',
  'router.post("/", (req, res) => {',
  "  res.json({});",
  "});",
  "",
  ...FN_NAMES.slice(1).flatMap((name, i) => [`export function ${name}() {`, `  return ${i};`, "}", ""]),
].join("\n");
const MORE = ['import { Router } from "express";', "export const more = Router();", 'more.get("/more", (req, res) => {', "  res.json([]);", "});", ""].join("\n");
const TYPES = "export type Order = { id: string };\nexport interface Page { n: number }\n";

const fnText = (name: string): string => `${name} is called from web/src/${name}.ts:2`;
const NAMED_OP = "GET /orders is called at web/src/client.ts:1";
const INLINE_OP = "POST / is called at web/src/client.ts:1";
const MORE_OP = "GET /more is called at web/src/client.ts:1";
const WEB = { service: "web", repo: "web", file: "src/client.ts", line: 1 };

const fnRows = FN_NAMES.map((name, i) => ({
  k: "fn",
  repo: "api",
  file: "src/orders.ts",
  line: i * 4 + 4,
  name,
  others: [{ repo: "web", file: `src/${name}.ts`, line: 2, name: `use${name}` }],
  total: 1,
  text: fnText(name),
}));
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
const ROWS = [
  op("listOrders", "GET /orders", NAMED_OP, "src/orders.ts"),
  op(null, "POST /", INLINE_OP, "src/orders.ts", "/"),
  op(null, "GET /more", MORE_OP, "src/more.ts", "/more"),
  ...fnRows,
];
const webFiles = Object.fromEntries(FN_NAMES.map((name) => [`web/src/${name}.ts`, `export function use${name}() {\n  return ${name}();\n}\n`]));
const FILES = { "api/src/orders.ts": ORDERS, "api/src/more.ts": MORE, "api/src/types.ts": TYPES, "web/src/client.ts": "export const c = 1;\n", ...webFiles };

function argvCalls(log: string): number {
  return fs.existsSync(log) ? fs.readFileSync(log, "utf8").split("\n").filter((line) => line.trim() !== "").length : 0;
}

function logPath(t: { after: (fn: () => void) => void }): string {
  const dir = fs.mkdtempSync(path.join(os.tmpdir(), "carrick-read-log-"));
  t.after(() => fs.rmSync(dir, { recursive: true, force: true }));
  return path.join(dir, "argv.log");
}

/** Run post-read for a Read of `rel`; `env` can drop the switch. */
function read(ws: Ws, rel: string, session: string, argvLog: string, env: Record<string, string> = {}) {
  const file = ws.at(rel);
  return runHook("post-read.ts", {
    payload: {
      hook_event_name: "PostToolUse",
      tool_name: "Read",
      session_id: session,
      cwd: ws.root,
      tool_input: { file_path: file },
      tool_response: { file: { filePath: file, content: "x" } },
    },
    env: fakeEnv({ HOME: ws.home, CLAUDE_PROJECT_DIR: ws.root, CARRICK_FAKE_FIXTURE: fixturePath("check-uses.json"), CARRICK_FAKE_ARGV_LOG: argvLog, ...env }),
  });
}

/** Run post-bash for a shell command with no output. */
function shell(ws: Ws, command: string, session: string, argvLog: string) {
  return runHook("post-bash.ts", {
    payload: { hook_event_name: "PostToolUse", tool_name: "Bash", session_id: session, cwd: ws.root, tool_input: { command }, tool_response: { stdout: "" } },
    env: fakeEnv({ HOME: ws.home, CLAUDE_PROJECT_DIR: ws.root, CARRICK_FAKE_FIXTURE: fixturePath("check-uses.json"), CARRICK_FAKE_ARGV_LOG: argvLog }),
  });
}

test("a Read with a fact file starts no carrick process and names the file's dependents", async (t) => {
  const ws = await workspace(t, FILES, ROWS);
  const log = logPath(t);
  const run = await read(ws, "api/src/orders.ts", "read-one", log);
  assert.equal(context(run.stdout), [NAMED_OP, INLINE_OP, fnText("listOrders")].join("\n\n"));
  assert.equal(argvCalls(log), 0);
});

test("a shell read of two files shares one cap", async (t) => {
  const ws = await workspace(t, FILES, ROWS);
  const log = logPath(t);
  const run = await shell(ws, "cat api/src/orders.ts api/src/more.ts", "read-two", log);
  assert.equal(context(run.stdout), [NAMED_OP, INLINE_OP, MORE_OP].join("\n\n"));
  assert.equal(argvCalls(log), 0);
});

test("a fact said at the Read is not repeated at the edit, and the uses-count line is left out", async (t) => {
  const ws = await workspace(t, FILES, ROWS);
  const log = logPath(t);
  await read(ws, "api/src/orders.ts", "read-edit", log);
  const file = ws.at("api/src/orders.ts");
  const edit = await patchEdit(ws, file, [{ newStart: lineOf(file, "return res.json"), lines: ["+  return res.json([]);"] }], { session: "read-edit" });
  assert.equal(context(edit.stdout), "");
  assert.equal(/Other files use what this file declares/.test(await nextCarrier(ws, "read-edit")), false);
});

test("a Read never prints a same-name line", async (t) => {
  const names = FN_NAMES.map((name) => nameRow(name, "web", `src/${name}.ts`));
  const ws = await workspace(t, FILES, [...ROWS, ...names]);
  const log = logPath(t);
  const run = await read(ws, "api/src/orders.ts", "read-name", log);
  assert.ok(!context(run.stdout).includes("a function named"), context(run.stdout));
  assert.deepEqual(
    Object.keys(readSession("read-name", ws.home).facts).filter((key) => key.startsWith("name::")),
    [],
  );
});

test("a types-only file keeps today's answer", async (t) => {
  const ws = await workspace(t, FILES, ROWS);
  const log = logPath(t);
  const run = await read(ws, "api/src/types.ts", "read-types", log);
  assert.equal(argvCalls(log), 1);
  assert.equal(context(run.stdout), renderPostRead(fixture("check-uses.json"), "api/src/types.ts"));
});

test("with the switch off, a Read is byte-identical to before", async (t) => {
  const ws = await workspace(t, FILES, ROWS);
  const log = logPath(t);
  const env = fakeEnv({ HOME: ws.home, CLAUDE_PROJECT_DIR: ws.root, CARRICK_FAKE_FIXTURE: fixturePath("check-uses.json"), CARRICK_FAKE_ARGV_LOG: log });
  delete env["CARRICK_FACT_LINES"];
  const file = ws.at("api/src/orders.ts");
  const run = await runHook("post-read.ts", {
    payload: { hook_event_name: "PostToolUse", tool_name: "Read", session_id: "read-off", cwd: ws.root, tool_input: { file_path: file }, tool_response: { file: { filePath: file, content: "x" } } },
    env,
  });
  assert.equal(argvCalls(log), 1);
  const snapshot = path.join(import.meta.dirname, "fixtures", "facts-read-no-file.snapshot.txt");
  if (process.env["UPDATE_SNAPSHOTS"] === "1") fs.writeFileSync(snapshot, context(run.stdout));
  assert.equal(context(run.stdout), fs.readFileSync(snapshot, "utf8"));
});

test("a second Read of the same file says nothing", async (t) => {
  const ws = await workspace(t, FILES, ROWS);
  const log = logPath(t);
  assert.notEqual(context((await read(ws, "api/src/orders.ts", "read-twice", log)).stdout), "");
  assert.equal((await read(ws, "api/src/orders.ts", "read-twice", log)).stdout.trim(), "");
});
