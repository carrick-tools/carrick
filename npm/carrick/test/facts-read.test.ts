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
import { FACTS_MARKER } from "../src/facts/fetch.ts";
import { factsPath, FACTS_STALE_MS, loadSqlite, touchFetched } from "../src/facts/store.ts";
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

/** The Read switch on: every test below that expects fact lines at a Read sets it. */
const READ_ON = { CARRICK_READ_LINES: "1" };

/** Run post-read for a Read of `rel`, the Read switch on; `env` can change the switches. */
function read(ws: Ws, rel: string, session: string, argvLog: string, env: Record<string, string> = READ_ON) {
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

/** Run post-bash for a shell command with no output, the Read switch on; `env` can change the switches. */
function shell(ws: Ws, command: string, session: string, argvLog: string, env: Record<string, string> = READ_ON) {
  return runHook("post-bash.ts", {
    payload: { hook_event_name: "PostToolUse", tool_name: "Bash", session_id: session, cwd: ws.root, tool_input: { command }, tool_response: { stdout: "" } },
    env: fakeEnv({ HOME: ws.home, CLAUDE_PROJECT_DIR: ws.root, CARRICK_FAKE_FIXTURE: fixturePath("check-uses.json"), CARRICK_FAKE_ARGV_LOG: argvLog, ...env }),
  });
}

/**
 * Compare `stdout` with the snapshot `name`, written from origin/main's hooks
 * (`UPDATE_SNAPSHOTS=1` with main's `src/` checked out) so equal means what
 * main prints today.
 */
function sameAsMain(stdout: string, name: string): void {
  const snapshot = path.join(import.meta.dirname, "fixtures", name);
  if (process.env["UPDATE_SNAPSHOTS"] === "1") fs.writeFileSync(snapshot, stdout);
  assert.equal(stdout, fs.readFileSync(snapshot, "utf8"));
}

test("with the Read switch unset, a Read prints byte for byte what main prints", async (t) => {
  const ws = await workspace(t, FILES, ROWS);
  const log = logPath(t);
  const run = await read(ws, "api/src/orders.ts", "read-unset", log, {});
  assert.equal(argvCalls(log), 1);
  sameAsMain(run.stdout, "facts-read-unset-read.snapshot.txt");
});

test("with the Read switch unset, a shell read prints byte for byte what main prints", async (t) => {
  const ws = await workspace(t, FILES, ROWS);
  const log = logPath(t);
  const run = await shell(ws, "cat api/src/orders.ts api/src/more.ts", "read-unset-shell", log, {});
  assert.equal(argvCalls(log), 2);
  sameAsMain(run.stdout, "facts-read-unset-shell.snapshot.txt");
});

test("any Read switch value but \"1\" keeps main's answer", async (t) => {
  const ws = await workspace(t, FILES, ROWS);
  for (const value of ["0", "true", ""]) {
    const log = logPath(t);
    const run = await read(ws, "api/src/orders.ts", `read-value-${value}`, log, { CARRICK_READ_LINES: value });
    assert.equal(argvCalls(log), 1, `CARRICK_READ_LINES=${JSON.stringify(value)}`);
    sameAsMain(run.stdout, "facts-read-unset-read.snapshot.txt");
  }
});

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
  // Peers really declare each name on the row's line (3), and the read file's own
  // op and fn rows are left out so the cap is not full; one fn row elsewhere keeps
  // `hasCallers` true. With `whole` in place of `read` these lines would print.
  const names = FN_NAMES.map((name) => nameRow(name, "web", `src/dup/${name}.ts`));
  const dups = Object.fromEntries(FN_NAMES.map((name) => [`web/src/dup/${name}.ts`, `// a\n// b\nexport function ${name}() {\n  return 1;\n}\n`]));
  const elsewhere = { ...fnRows[0], repo: "web", file: "src/client.ts", name: "c", text: "c is called from x" };
  const ws = await workspace(t, { ...FILES, ...dups }, [...names, elsewhere]);
  const log = logPath(t);
  const run = await read(ws, "api/src/orders.ts", "read-name", log);
  assert.ok(!context(run.stdout).includes("a function named"), context(run.stdout));
  assert.deepEqual(
    Object.keys(readSession("read-name", ws.home).facts).filter((key) => key.startsWith("name::")),
    [],
  );
});

test("a throwing fn lookup sends the Read to the binary's check, once (P7)", async (t) => {
  const ws = await workspace(t, FILES, ROWS);
  const sqlite = (await loadSqlite())!;
  const db = new sqlite.DatabaseSync(factsPath(ws.root));
  db.prepare("UPDATE fns SET others = 'not json' WHERE name = 'listOrders'").run();
  db.close();
  const log = logPath(t);
  const run = await read(ws, "api/src/orders.ts", "read-throws", log);
  assert.equal(argvCalls(log), 1);
  const answer = renderPostRead(fixture("check-uses.json"), "api/src/orders.ts");
  assert.ok(answer, "the fixture has an answer");
  assert.ok(context(run.stdout).includes(answer), context(run.stdout));
});

test("a stale fact file starts the refresh at a Read, a fresh one does not", async (t) => {
  const refresh = { ...READ_ON, CARRICK_NO_FACTS_REFRESH: "" };
  const marker = (ws: Ws) => path.join(ws.root, ".carrick", FACTS_MARKER);
  const fresh = await workspace(t, FILES, ROWS);
  await read(fresh, "api/src/orders.ts", "read-fresh", logPath(t), refresh);
  assert.equal(fs.existsSync(marker(fresh)), false, "a fresh file starts no refresh");
  const old = await workspace(t, FILES, ROWS);
  await touchFetched(old.root, new Date(Date.now() - 2 * FACTS_STALE_MS).toISOString());
  await read(old, "api/src/orders.ts", "read-stale", logPath(t), refresh);
  assert.equal(fs.existsSync(marker(old)), true, "a stale file starts the refresh");
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
  const env = fakeEnv({ HOME: ws.home, CLAUDE_PROJECT_DIR: ws.root, CARRICK_FAKE_FIXTURE: fixturePath("check-uses.json"), CARRICK_FAKE_ARGV_LOG: log, CARRICK_FACT_LINES: "0" });
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
