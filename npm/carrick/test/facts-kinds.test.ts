// Edit-time facts beyond operations: callers of an edited function (`fn`),
// a same-named function elsewhere (`name`), and a package's specs in other
// services (`pkg`). Rows are hand-written in the wire's shape until the cloud
// builder's fixture carries these kinds.

import test from "node:test";
import assert from "node:assert/strict";
import fs from "node:fs";
import os from "node:os";
import path from "node:path";
import { fakeEnv, fixturePath, runHook, testDir } from "./helpers.ts";
import { buildFacts, factsPath, loadSqlite } from "../src/facts/store.ts";
import { refreshFacts } from "../src/facts/fetch.ts";
import { factLines } from "../src/facts/lines.ts";
import { readTimings } from "../src/hook/timings.ts";

const SLUG = "kinds-demo";
const REPOS = ["api", "web"];

type Ws = { root: string; home: string; at: (rel: string) => string };

const header = () =>
  JSON.stringify({
    schema: 1,
    writer: 1,
    project_id: "p",
    project_slug: SLUG,
    digest: "d1",
    built_at: "2026-01-01T00:00:00Z",
    repos: REPOS.map((repo) => ({ repo, service: repo, commit: "1" })),
    counts: { rows: 0, producers: 0, consumers: 0 },
  });

const ndjson = (rows: Array<Record<string, unknown>>) => `${[header(), ...rows.map((row) => JSON.stringify(row))].join("\n")}\n`;

/** A workspace with `files` written and, unless `rows` is null, a fact file built from them. */
async function workspace(
  t: { after: (fn: () => void) => void },
  files: Record<string, string>,
  rows: Array<Record<string, unknown>> | null,
): Promise<Ws> {
  const root = fs.realpathSync(fs.mkdtempSync(path.join(os.tmpdir(), "carrick-kinds-")));
  const home = fs.realpathSync(fs.mkdtempSync(path.join(os.tmpdir(), "carrick-kinds-home-")));
  t.after(() => {
    fs.rmSync(root, { recursive: true, force: true });
    fs.rmSync(home, { recursive: true, force: true });
  });
  for (const [rel, text] of Object.entries(files)) {
    fs.mkdirSync(path.dirname(path.join(root, rel)), { recursive: true });
    fs.writeFileSync(path.join(root, rel), text);
  }
  fs.mkdirSync(path.join(root, ".carrick", "hosted"), { recursive: true });
  fs.writeFileSync(path.join(root, ".carrick", "hosted", "snapshot.json"), JSON.stringify({ resolution: { repos: [{ project_slug: SLUG }] } }));
  if (rows !== null) {
    await buildFacts(ndjson(rows), factsPath(root), REPOS.map((repo) => ({ repo, path: path.join(root, repo) })), new Date().toISOString());
  }
  return { root, home, at: (rel) => path.join(root, rel) };
}

type Hunk = { newStart: number; lines: string[] };

/** Run post-edit for an Edit of `file` whose result carries `hunks`. */
function patchEdit(ws: Ws, file: string, hunks: Hunk[], options: { session?: string; probe?: string; argvLog?: string } = {}) {
  return runHook("post-edit.ts", {
    payload: {
      hook_event_name: "PostToolUse",
      tool_name: "Edit",
      session_id: options.session ?? "kinds",
      cwd: ws.root,
      tool_input: { file_path: file, old_string: "x", new_string: "" },
      tool_response: { structuredPatch: hunks.map((hunk) => ({ oldStart: hunk.newStart, oldLines: 0, newLines: 0, ...hunk })) },
    },
    env: fakeEnv({
      HOME: ws.home,
      CLAUDE_PROJECT_DIR: ws.root,
      CARRICK_FAKE_FIXTURE: fixturePath(options.probe ?? "check-silent.json"),
      ...(options.argvLog ? { CARRICK_FAKE_ARGV_LOG: options.argvLog } : {}),
    }),
  });
}

function context(stdout: string): string {
  if (!stdout.trim()) return "";
  return (JSON.parse(stdout) as { hookSpecificOutput: { additionalContext: string } }).hookSpecificOutput.additionalContext;
}

/** The 1-based line of `file` holding `needle`. */
function lineOf(file: string, needle: string): number {
  return fs.readFileSync(file, "utf8").split("\n").findIndex((line) => line.includes(needle)) + 1;
}

// ---- T1: callers of an edited function (`fn`) ----

const ORDERS = [
  "export function loadOrder(id: string) {",
  "  const key = `order:${id}`;",
  "  return cache.get(key);",
  "}",
  "",
  "export function listOrders() {",
  "  return ids().map((id) => {",
  "    return loadOrder(id);",
  "  });",
  "}",
  "",
].join("\n");

const fnRow = (name: string, text: string, file = "src/orders.ts") => ({
  k: "fn",
  repo: "api",
  file,
  line: 1,
  name,
  others: [{ repo: "web", file: "src/page.ts", line: 4, name: "render" }],
  total: 1,
  text,
});

const LOAD_LINE = "loadOrder is called from web/src/page.ts:4, api/src/jobs.ts:9";
const LIST_LINE = "listOrders is called from web/src/list.ts:2";

test("fn: an edit in a function body names its callers, once per session", async (t) => {
  const ws = await workspace(t, { "api/src/orders.ts": ORDERS }, [fnRow("loadOrder", LOAD_LINE), fnRow("listOrders", LIST_LINE)]);
  const file = ws.at("api/src/orders.ts");
  const at = lineOf(file, "const key");
  assert.equal(context((await patchEdit(ws, file, [{ newStart: at, lines: ["+  const key = `order:${id}`;"] }])).stdout), LOAD_LINE);
  assert.equal(context((await patchEdit(ws, file, [{ newStart: at, lines: ["+  const key = `order:${id}`;"] }])).stdout), "");
});

test("fn: an edit in a nested callback names the enclosing function's callers", async (t) => {
  const ws = await workspace(t, { "api/src/orders.ts": ORDERS }, [fnRow("loadOrder", LOAD_LINE), fnRow("listOrders", LIST_LINE)]);
  const file = ws.at("api/src/orders.ts");
  const at = lineOf(file, "return loadOrder(id);");
  assert.equal(context((await patchEdit(ws, file, [{ newStart: at, lines: ["+    return loadOrder(id);"] }])).stdout), LIST_LINE);
});

test("fn: a row for another file, whose name this file also declares, is silent", async (t) => {
  const ws = await workspace(
    t,
    { "api/src/orders.ts": ORDERS, "api/src/legacy.ts": "export function loadOrder() {}\n" },
    [fnRow("loadOrder", LOAD_LINE, "src/legacy.ts")],
  );
  const file = ws.at("api/src/orders.ts");
  const at = lineOf(file, "const key");
  assert.equal(context((await patchEdit(ws, file, [{ newStart: at, lines: ["+  const key = `order:${id}`;"] }])).stdout), "");
});

test("fn: a name declared twice in the edited file is silent", async (t) => {
  const twice = `${ORDERS}function loadOrder() {}\n`;
  const ws = await workspace(t, { "api/src/orders.ts": twice }, [fnRow("loadOrder", LOAD_LINE)]);
  const file = ws.at("api/src/orders.ts");
  const at = lineOf(file, "const key");
  assert.equal(context((await patchEdit(ws, file, [{ newStart: at, lines: ["+  const key = `order:${id}`;"] }])).stdout), "");
});

const USES_LINE = /Other files use what this file declares/;

test("fn: with a fact file the uses-count line is not printed, and no fallback is recorded", async (t) => {
  const ws = await workspace(t, { "api/src/orders.ts": ORDERS }, [fnRow("loadOrder", LOAD_LINE)]);
  const file = ws.at("api/src/orders.ts");
  const at = lineOf(file, "const key");
  const text = context((await patchEdit(ws, file, [{ newStart: at, lines: ["+  const key = `order:${id}`;"] }], { probe: "check-uses.json" })).stdout);
  assert.doesNotMatch(text, USES_LINE);
  assert.ok(text.endsWith(LOAD_LINE), text);
  const records = readTimings(ws.root).filter((record) => record.hook === "post-edit");
  assert.equal(records.length, 1);
  assert.equal("uses_fallback" in records[0]!, false);
});

test("no fact file: post-edit's output is byte-identical to before, and uses_fallback is recorded", async (t) => {
  const ws = await workspace(t, { "api/src/orders.ts": ORDERS }, null);
  const file = ws.at("api/src/orders.ts");
  const at = lineOf(file, "const key");
  const text = context((await patchEdit(ws, file, [{ newStart: at, lines: ["+  const key = `order:${id}`;"] }], { probe: "check-uses.json" })).stdout);
  const snapshot = fs.readFileSync(path.join(testDir, "fixtures", "facts-kinds-no-file.snapshot.txt"), "utf8").replaceAll("<root>", ws.root);
  assert.equal(text, snapshot.trimEnd());
  assert.match(text, USES_LINE);
  const records = readTimings(ws.root).filter((record) => record.hook === "post-edit");
  assert.equal(records.length, 1);
  assert.equal(records[0]!.uses_fallback, true);
});

test("an old local fact file without the new tables is rebuilt, not read", async (t) => {
  const ws = await workspace(t, { "api/src/orders.ts": ORDERS }, null);
  // A file an older package built: `op` rows only, at the layout-less name.
  const sqlite = (await loadSqlite())!;
  const old = path.join(ws.root, ".carrick", "facts", "edit-facts.s1.sqlite");
  fs.mkdirSync(path.dirname(old), { recursive: true });
  const db = new sqlite.DatabaseSync(old);
  db.exec("CREATE TABLE meta (key TEXT PRIMARY KEY, value TEXT NOT NULL); CREATE TABLE repos (repo TEXT, path TEXT); CREATE TABLE rows (id INTEGER PRIMARY KEY, repo TEXT, name TEXT, file TEXT)");
  for (const [key, value] of Object.entries({ project: SLUG, digest: "d1", schema: "1", writer: "1", fetched_at: new Date().toISOString() })) {
    db.prepare("INSERT INTO meta (key, value) VALUES (?, ?)").run(key, value);
  }
  db.close();
  assert.notEqual(factsPath(ws.root), old);
  fs.writeFileSync(path.join(ws.root, ".carrick", "index.json"), JSON.stringify({ repos: REPOS.map((repo) => ({ name: repo, path: ws.at(repo), services: [] })) }));
  const { gzipSync } = await import("node:zlib");
  const bodies: Array<Record<string, unknown>> = [];
  const request = (async (url: string, init?: RequestInit) => {
    if (url.endsWith("/mcp/facts")) {
      const body = JSON.parse(String(init?.body)) as Record<string, unknown>;
      bodies.push(body);
      if (body["known_digest"]) return new Response(JSON.stringify({ unchanged: true }));
      return new Response(JSON.stringify({ staged_url: "https://facts.s3.eu-west-1.amazonaws.com/x?sig=1", digest: "d1", schema: 1, writer: 1, bytes: 1 }));
    }
    return new Response(gzipSync(ndjson([fnRow("loadOrder", LOAD_LINE)])));
  }) as typeof fetch;
  assert.equal(await refreshFacts(ws.root, { request, env: { ...process.env, CARRICK_TOKEN: "t" } }), "written");
  // The old file's digest is never offered, so the cloud sends the whole file.
  assert.equal("known_digest" in bodies[0]!, false);
  const file = ws.at("api/src/orders.ts");
  const at = lineOf(file, "const key");
  assert.equal(context((await patchEdit(ws, file, [{ newStart: at, lines: ["+  const key = `order:${id}`;"] }])).stdout), LOAD_LINE);
});

// ---- T3: a function the edit added, named like one in another file (`name`) ----

const HOOK = ["import fs from \"node:fs\";", "", "export function main() {", "  return 1;", "}", ""].join("\n");
const ADDED = ["export function readStdin() {", "  return fs.readFileSync(0, \"utf8\");", "}", ""];
const nameRow = (name: string, repo: string, file: string) => ({
  k: "name",
  name,
  repo,
  file,
  line: 3,
  text: `a function named ${name} is declared at ${repo}/${file}:3`,
});
const STDIN_LINE = "a function named readStdin is declared at web/src/stdin.ts:3";

/** `HOOK` with `ADDED` (or `lines`) inserted at its end, and the hunk that says so. */
function addFunction(ws: Ws, rel: string, lines = ADDED): { file: string; hunk: Hunk } {
  const file = ws.at(rel);
  const before = fs.readFileSync(file, "utf8").split("\n");
  // The file ends with a newline: insert before the final empty line.
  const at = before.length;
  fs.writeFileSync(file, [...before.slice(0, -1), ...lines.slice(0, -1), ""].join("\n"));
  return { file, hunk: { newStart: at - 1, lines: [` ${before[at - 2]}`, ...lines.slice(0, -1).map((line) => `+${line}`)] } };
}

const STDIN_FILES = { "api/src/hook.ts": HOOK, "web/src/stdin.ts": "// stdin\n\nexport function readStdin() {\n  return \"\";\n}\n" };

test("name: an Edit that adds a function named like one in another file names that one, once per session", async (t) => {
  const ws = await workspace(t, STDIN_FILES, [nameRow("readStdin", "web", "src/stdin.ts")]);
  const { file, hunk } = addFunction(ws, "api/src/hook.ts");
  assert.equal(context((await patchEdit(ws, file, [hunk])).stdout), STDIN_LINE);
  assert.equal(context((await patchEdit(ws, file, [hunk])).stdout), "");
});

test("name: a file the call created counts every declaration in it", async (t) => {
  const ws = await workspace(t, { ...STDIN_FILES, "api/src/fresh.ts": ADDED.join("\n") }, [nameRow("readStdin", "web", "src/stdin.ts")]);
  const outcome = await factLines(ws.root, [{ file: ws.at("api/src/fresh.ts"), whole: true }], undefined);
  assert.deepEqual(outcome.lines, [STDIN_LINE]);
});

test("name: more than one other declaration prints the first by (repo, file) only", async (t) => {
  const ws = await workspace(t, { ...STDIN_FILES, "api/src/io.ts": "export function readStdin() {}\n" }, [
    nameRow("readStdin", "web", "src/stdin.ts"),
    nameRow("readStdin", "api", "src/io.ts"),
  ]);
  const { file, hunk } = addFunction(ws, "api/src/hook.ts");
  assert.equal(context((await patchEdit(ws, file, [hunk])).stdout), "a function named readStdin is declared at api/src/io.ts:3");
});

test("name: silent when the name was declared before the edit", async (t) => {
  const ws = await workspace(t, { ...STDIN_FILES, "api/src/hook.ts": [HOOK.trimEnd(), ...ADDED].join("\n") }, [nameRow("readStdin", "web", "src/stdin.ts")]);
  const file = ws.at("api/src/hook.ts");
  const at = lineOf(file, "export function readStdin");
  // The declaration line itself was replaced.
  fs.writeFileSync(file, fs.readFileSync(file, "utf8").replace("export function readStdin() {", "export function readStdin(fd = 0) {"));
  const hunk = { newStart: at, lines: ["-export function readStdin() {", "+export function readStdin(fd = 0) {"] };
  assert.equal(context((await patchEdit(ws, file, [hunk])).stdout), "");
});

test("name: silent when the only declaration is this file's own", async (t) => {
  const ws = await workspace(t, STDIN_FILES, [nameRow("readStdin", "api", "src/hook.ts")]);
  const { file, hunk } = addFunction(ws, "api/src/hook.ts");
  assert.equal(context((await patchEdit(ws, file, [hunk])).stdout), "");
});

test("name: silent when main already declares the name in this file, even beside another file's", async (t) => {
  const ws = await workspace(t, STDIN_FILES, [nameRow("readStdin", "api", "src/hook.ts"), nameRow("readStdin", "web", "src/stdin.ts")]);
  const { file, hunk } = addFunction(ws, "api/src/hook.ts");
  assert.equal(context((await patchEdit(ws, file, [hunk])).stdout), "");
});

test("name: silent for a name shorter than 4 characters", async (t) => {
  const ws = await workspace(t, { ...STDIN_FILES, "web/src/run.ts": "export function run() {}\n" }, [nameRow("run", "web", "src/run.ts")]);
  const { file, hunk } = addFunction(ws, "api/src/hook.ts", ["export function run() {", "  return 2;", "}", ""]);
  assert.equal(context((await patchEdit(ws, file, [hunk])).stdout), "");
});

test("name: silent when the other file is gone from disk (the function may have moved here)", async (t) => {
  const ws = await workspace(t, STDIN_FILES, [nameRow("readStdin", "web", "src/stdin.ts")]);
  fs.rmSync(ws.at("web/src/stdin.ts"));
  const { file, hunk } = addFunction(ws, "api/src/hook.ts");
  assert.equal(context((await patchEdit(ws, file, [hunk])).stdout), "");
});

test("name: silent when the patch does not read the same as the file (the file before is unknown)", async (t) => {
  const ws = await workspace(t, STDIN_FILES, [nameRow("readStdin", "web", "src/stdin.ts")]);
  const { file, hunk } = addFunction(ws, "api/src/hook.ts");
  assert.equal(context((await patchEdit(ws, file, [{ ...hunk, lines: [" not this line", ...hunk.lines.slice(1)] }])).stdout), "");
});

test("fn: a row whose file is gone is silent, even for a name declared once here", async (t) => {
  const ws = await workspace(t, { "api/src/orders.ts": ORDERS }, [fnRow("loadOrder", LOAD_LINE, "src/old-orders.ts")]);
  const file = ws.at("api/src/orders.ts");
  const at = lineOf(file, "const key");
  assert.equal(context((await patchEdit(ws, file, [{ newStart: at, lines: ["+  const key = `order:${id}`;"] }])).stdout), "");
});

// ---- T4: a dependency's specs in other services (`pkg`) ----

const MANIFEST = (deps: string[], extra = "") =>
  ["{", '  "name": "api",', '  "version": "1.0.0",', `  "dependencies": {`, ...deps, "  }," + extra, '  "scripts": {', '    "zod": "^3.0.0"', "  }", "}", ""].join("\n");
const pkgRow = (name: string, specs: Array<[string, string, string]>) => ({
  k: "pkg",
  package: name,
  services: specs.map(([repo, spec, manifest]) => ({ repo, service: repo, spec, manifest })),
  text: `${name} is ${specs.map(([repo, spec]) => `${spec} in ${repo}`).join(", ")}`,
});
const ZOD = pkgRow("zod", [
  ["api", "^3.22.0", "package.json"],
  ["web", "^3.23.8", "package.json"],
]);

/** Write `next` over the manifest and return the hunk from `before` to it (whole file, one hunk). */
function manifestHunk(file: string, before: string, next: string): Hunk {
  fs.writeFileSync(file, next);
  const a = before.split("\n");
  const b = next.split("\n");
  let head = 0;
  while (head < a.length && a[head] === b[head]) head += 1;
  let tail = 0;
  while (tail < a.length - head && tail < b.length - head && a[a.length - 1 - tail] === b[b.length - 1 - tail]) tail += 1;
  const lines = [
    ...a.slice(head, a.length - tail).map((line) => `-${line}`),
    ...b.slice(head, b.length - tail).map((line) => `+${line}`),
  ];
  return { newStart: head + 1, lines };
}

async function bump(t: { after: (fn: () => void) => void }, before: string, next: string, rows = [ZOD], options: { argvLog?: string; session?: string } = {}) {
  const ws = await workspace(t, { "api/package.json": before, "web/package.json": "{}\n", "api/src/orders.ts": ORDERS }, rows);
  const file = ws.at("api/package.json");
  const run = await patchEdit(ws, file, [manifestHunk(file, before, next)], options);
  return { ws, text: context(run.stdout) };
}

test("pkg: a bumped dependency names the other services' specs, with no probe, queue or recorded edit", async (t) => {
  const argvLog = path.join(os.tmpdir(), `carrick-kinds-argv-${process.pid}.log`);
  t.after(() => fs.rmSync(argvLog, { force: true }));
  const { ws, text } = await bump(t, MANIFEST(['    "zod": "^3.22.0"']), MANIFEST(['    "zod": "^3.24.0"']), [ZOD], { argvLog });
  assert.equal(text, ZOD.text);
  assert.equal(fs.existsSync(argvLog), false, "the binary was not called");
  assert.equal(fs.existsSync(path.join(ws.root, ".carrick", "rechecks")), false, "nothing was queued");
  const sessions = path.join(ws.home, ".carrick", "sessions");
  const records = fs.existsSync(sessions) ? fs.readdirSync(sessions).filter((name) => name.endsWith(".json")) : [];
  assert.ok(records.length > 0, "the fact claim was recorded");
  for (const name of records) {
    const record = JSON.parse(fs.readFileSync(path.join(sessions, name), "utf8")) as { files?: object; edited?: object };
    assert.deepEqual(Object.keys(record.files ?? {}), [], "no edited file was recorded");
    assert.deepEqual(Object.keys(record.edited ?? {}), [], "the workspace was not stamped as edited");
  }
});

test("pkg: an added dependency (a comma on the line above) names only the added one", async (t) => {
  const rows = [ZOD, pkgRow("react", [["web", "^18.0.0", "package.json"]]), pkgRow("express", [["web", "^4.0.0", "package.json"], ["api", "^4.19.0", "package.json"]])];
  const { text } = await bump(t, MANIFEST(['    "express": "^4.19.0"']), MANIFEST(['    "express": "^4.19.0",', '    "react": "^18.2.0"']), rows);
  assert.equal(text, rows[1]!.text);
});

test("pkg: silent when every other service already has the new spec", async (t) => {
  const { text } = await bump(t, MANIFEST(['    "zod": "^3.22.0"']), MANIFEST(['    "zod": "^3.23.8"']));
  assert.equal(text, "");
});

test("pkg: silent for a pair outside the dependency sections", async (t) => {
  const before = MANIFEST(['    "left-pad": "^1.0.0"']);
  const { text } = await bump(t, before, before.replace('"zod": "^3.0.0"', '"zod": "^3.1.0"'));
  assert.equal(text, "");
});

test("pkg: silent when the file does not parse", async (t) => {
  const { text } = await bump(t, MANIFEST(['    "zod": "^3.22.0"']), MANIFEST(['    "zod": "^3.24.0",']));
  assert.equal(text, "");
});

test("pkg: silent for an unchanged dependency and a package with no row", async (t) => {
  const { text } = await bump(t, MANIFEST(['    "zod": "^3.22.0"']), MANIFEST(['    "zod": "^3.22.0"'], ' '));
  assert.equal(text, "");
  const { text: none } = await bump(t, MANIFEST(['    "chalk": "^4.0.0"']), MANIFEST(['    "chalk": "^5.0.0"']));
  assert.equal(none, "");
});

test("pkg: a Codex patch over a source file and a package.json probes only the source file", async (t) => {
  const before = MANIFEST(['    "zod": "^3.22.0"']);
  const ws = await workspace(t, { "api/package.json": before, "web/package.json": "{}\n", "api/src/orders.ts": ORDERS }, [ZOD]);
  fs.writeFileSync(ws.at("api/package.json"), before.replace('"zod": "^3.22.0"', '"zod": "^3.24.0"'));
  const argvLog = path.join(os.tmpdir(), `carrick-kinds-mixed-${process.pid}.log`);
  t.after(() => fs.rmSync(argvLog, { force: true }));
  const patch = [
    "*** Begin Patch",
    "*** Update File: api/package.json",
    "@@",
    '-    "zod": "^3.22.0"',
    '+    "zod": "^3.24.0"',
    "*** Update File: api/src/orders.ts",
    "@@",
    "   const key = `order:${id}`;",
    "*** End Patch",
  ].join("\n");
  const run = await runHook("post-edit.ts", {
    payload: { hook_event_name: "PostToolUse", tool_name: "apply_patch", session_id: "mixed", cwd: ws.root, tool_input: { command: patch } },
    env: fakeEnv({ HOME: ws.home, CLAUDE_PROJECT_DIR: ws.root, CARRICK_FAKE_FIXTURE: fixturePath("check-silent.json"), CARRICK_FAKE_ARGV_LOG: argvLog }),
  });
  assert.equal(context(run.stdout), ZOD.text);
  const calls = fs.readFileSync(argvLog, "utf8");
  assert.match(calls, /api\/src\/orders\.ts/);
  assert.doesNotMatch(calls, /package\.json/);
});

// ---- The cap across kinds ----

test("at most 3 fact lines per action, in the order op, fn, name", async (t) => {
  const src = [
    "export function loadOrder(id: string) {",
    "  return api.get(\"/orders\");",
    "}",
    "export function readStdin() {",
    "  return 1;",
    "}",
    "",
  ].join("\n");
  const op = (label: string, text: string) => ({ k: "op", side: "consumer", protocol: "http", label, service: "api", repo: "api", file: "src/orders.ts", line: 2, name: "loadOrder", site: null, others: [], total: 0, source: "call", text });
  const ws = await workspace(t, { "api/src/orders.ts": src, "web/src/stdin.ts": "export function readStdin() {}\n" }, [
    op("GET /orders", "op line 1"),
    op("POST /orders", "op line 2"),
    fnRow("loadOrder", LOAD_LINE),
    nameRow("readStdin", "web", "src/stdin.ts"),
  ]);
  const outcome = await factLines(ws.root, [{ file: ws.at("api/src/orders.ts"), whole: true }], undefined);
  assert.deepEqual(outcome.lines, ["op line 1", "op line 2", LOAD_LINE]);
});
