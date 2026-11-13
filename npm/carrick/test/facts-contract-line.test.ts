// The contract line: what an edit of a file that holds contract rows prints
// (`facts/lines.ts`), behind the opt-in switch `CARRICK_CONTRACT_LINES=1`.
// Every service, file and type here is invented. Wire: carrick-cloud
// `docs/internal/reference/edit-facts-wire.md`, section `k: "contract"`.

import test from "node:test";
import assert from "node:assert/strict";
import crypto from "node:crypto";
import fs from "node:fs";
import os from "node:os";
import path from "node:path";
import { drainRunner, fakeEnv, fixturePath, runHook } from "./helpers.ts";
import { commitRepos } from "./facts-repos.ts";
import { context, lineOf, patchEdit } from "./facts-helpers.ts";
import type { Ws } from "./facts-helpers.ts";
import { buildFacts, factsPath } from "../src/facts/store.ts";
import { factLines, MAX_FACT_LINES } from "../src/facts/lines.ts";
import { markFacts } from "../src/hook/reuse.ts";

const SLUG = "contract-line-demo";
const FULL_COMMIT = "abcdef0123456789abcdef0123456789abcdef01";
const NOTE = " (another repo, indexed at abcdef01, not checked out here)";

const TASKS = `// tasks
export function formatTask(task) {
  return { id: task.id };
}
export function listTasks(req, res) {
  res.json(formatTask(req.rows));
}
export function getTask(req, res) {
  res.json(formatTask(req.row));
}
export function otherTask(req, res) {
  res.json({ ok: true });
}
`;
const CLIENT = `export async function loadTasks() {
  return fetch(\`\${BASE}/tasks\`);
}
`;

const place = (repo: string, file: string, line: number) => ({ service: repo, repo, file, line, loc: `${repo}/${file}:${line}` });

/** One printed group: `n` places of `repo` (as many files), its words as the cloud writes them. */
function group(label: string, type: string, places: Array<ReturnType<typeof place>>, side: "producer" | "consumer" = "producer") {
  const shown = Math.min(3, places.length);
  return {
    type,
    symbol: null,
    home: null,
    places,
    total: places.length,
    shown,
    lead: `Carrick: ${label} is ${side === "producer" ? "called from" : "served by"} `,
    more: places.length > shown ? ` (+${places.length - shown} more)` : "",
    tail: `, which ${side === "producer" ? "expects" : "sends"}: ${type}`,
    shortened: false,
  };
}

function contractRow(over: Record<string, unknown>) {
  const groups = (over["groups"] ?? []) as unknown[];
  return {
    k: "contract",
    side: "producer",
    protocol: "http",
    label: "GET /tasks",
    direction: "response",
    service: "api",
    repo: "api",
    file: "src/tasks.ts",
    line: 5,
    name: "listTasks",
    site: null,
    locator: "registration",
    groups: [],
    untyped: [],
    total: groups.length,
    ...over,
  };
}

const extPlaces = (...lines: number[]) => lines.map((line) => place("ext", `src/view${line}.ts`, line));
const A = "{ id: string }";

/** The contract and op rows of the producer's three operations over `places`, one each at its line in TASKS. */
function taskRows(places = extPlaces(4)) {
  const lines = TASKS.split("\n");
  const lineOfText = (needle: string) => lines.findIndex((line) => line.includes(needle)) + 1;
  const ops = [
    { name: "listTasks", label: "GET /tasks" },
    { name: "getTask", label: "GET /tasks/:id" },
    { name: "otherTask", label: "GET /tasks/other" },
  ];
  return ops.flatMap(({ name, label }) => {
    const line = lineOfText(`function ${name}`);
    return [contractRow({ label, name, line, groups: [group(label, A, places)] }), opRow(name, label, "producer", "api", "src/tasks.ts", line)];
  });
}

/** `rows` of one operation only: the other operations of the file would join tier 3. */
const only = (rows: Array<Record<string, unknown>>, name: string) => rows.filter((row) => row["name"] === name);

const opRow = (name: string, label: string, side: "producer" | "consumer" = "producer", repo = "api", file = "src/tasks.ts", line = 5) => ({
  k: "op",
  side,
  protocol: "http",
  label,
  service: repo,
  repo,
  file,
  line,
  name,
  site: null,
  others: [{ service: "web", repo: "web", file: "src/client.ts", line: 2 }],
  total: 0,
  source: "call",
  text: `op line for ${name}`,
});

/** A workspace of repos `api` and `web` (committed) and a fact file naming `ext`'s commit as `extCommit` (null: not named). */
async function workspace(t: { after: (fn: () => void) => void }, rows: Array<Record<string, unknown>>, extCommit: string | null = FULL_COMMIT): Promise<Ws> {
  const root = fs.realpathSync(fs.mkdtempSync(path.join(os.tmpdir(), "carrick-cline-")));
  const home = fs.realpathSync(fs.mkdtempSync(path.join(os.tmpdir(), "carrick-cline-home-")));
  t.after(async () => {
    await drainRunner(root);
    fs.rmSync(root, { recursive: true, force: true });
    fs.rmSync(home, { recursive: true, force: true });
  });
  for (const [rel, text] of Object.entries({ "api/src/tasks.ts": TASKS, "web/src/client.ts": CLIENT })) {
    fs.mkdirSync(path.dirname(path.join(root, rel)), { recursive: true });
    fs.writeFileSync(path.join(root, rel), text);
  }
  fs.mkdirSync(path.join(root, ".carrick", "hosted"), { recursive: true });
  fs.writeFileSync(path.join(root, ".carrick", "hosted", "snapshot.json"), JSON.stringify({ resolution: { repos: [{ project_slug: SLUG }] } }));
  const header = JSON.stringify({
    schema: 1,
    writer: 4,
    project_id: "p",
    project_slug: SLUG,
    digest: "d1",
    built_at: "2026-01-01T00:00:00Z",
    repos: [
      { repo: "api", service: "api", commit: "1" },
      { repo: "web", service: "web", commit: "1" },
      ...(extCommit === null ? [] : [{ repo: "ext", service: "ext", commit: extCommit }]),
    ],
    counts: { rows: 0, producers: 0, consumers: 0 },
  });
  const text = `${[header, ...rows.map((row) => JSON.stringify(row))].join("\n")}\n`;
  const local = ["api", "web"];
  await buildFacts(commitRepos(root, text, local), factsPath(root), local.map((repo) => ({ repo, path: path.join(root, repo) })), new Date().toISOString());
  return { root, home, at: (rel) => path.join(root, rel) };
}

const run = (ws: Ws, edit: { file: string; snippets: string[] }, options: { session?: string; contract?: boolean } = {}) =>
  factLines(ws.root, [edit], options.session ?? "s1", { home: ws.home, contract: options.contract ?? true });

const tasksEdit = (ws: Ws, snippet: string) => ({ file: ws.at("api/src/tasks.ts"), snippets: [snippet] });
const IN_LIST = "res.json(formatTask(req.rows));";
const IN_FORMAT = "return { id: task.id };";
const lineFor = (label: string, places: string, more = "") => `Carrick: ${label} is called from ${places}${more}, which expects: ${A}`;

test("a producer edit names the other repo's place with the note and the first 8 characters of its commit", async (t) => {
  const ws = await workspace(t, only(taskRows(), "listTasks"));
  const out = await run(ws, tasksEdit(ws, IN_LIST));
  assert.deepEqual(out.lines, [lineFor("GET /tasks", `ext/src/view4.ts:4${NOTE}`)]);
});

test("a place on disk and unchanged since the index prints without a note", async (t) => {
  const ws = await workspace(t, only(taskRows([place("web", "src/client.ts", 2)]), "listTasks"));
  const out = await run(ws, tasksEdit(ws, IN_LIST));
  assert.deepEqual(out.lines, [lineFor("GET /tasks", "web/src/client.ts:2")]);
});

test("a place on disk whose file changed since the index makes the line silent", async (t) => {
  const ws = await workspace(t, only(taskRows([place("web", "src/client.ts", 2)]), "listTasks"));
  fs.appendFileSync(ws.at("web/src/client.ts"), "// changed after the index\n");
  const out = await run(ws, tasksEdit(ws, IN_LIST));
  assert.deepEqual(out.lines, []);
});

test("a place off disk whose repo has no full commit in the fact file makes the line silent", async (t) => {
  for (const commit of [null, "1", "abcdef0"]) {
    const ws = await workspace(t, only(taskRows(), "listTasks"), commit);
    const out = await run(ws, tasksEdit(ws, IN_LIST));
    // No contract line; the op line, which needs no other repo's commit, is what prints.
    assert.deepEqual(out.lines, ["op line for listTasks"], `commit ${String(commit)}`);
  }
});

test("a consumer edit prints the mirror wording the row carries", async (t) => {
  const rows = [
    contractRow({
      side: "consumer",
      repo: "web",
      service: "web",
      file: "src/client.ts",
      line: 1,
      name: "loadTasks",
      locator: "call",
      groups: [group("GET /tasks", A, [place("api", "src/tasks.ts", 5)], "consumer")],
    }),
  ];
  const ws = await workspace(t, rows);
  const out = await run(ws, { file: ws.at("web/src/client.ts"), snippets: ["return fetch("] });
  assert.deepEqual(out.lines, [`Carrick: GET /tasks is served by api/src/tasks.ts:5, which sends: ${A}`]);
});

test("two groups print two lines and a third text prints none", async (t) => {
  const types = ["{ id: string }", "{ id: number }", "{ id: boolean }"];
  const rows = [contractRow({ name: "listTasks", line: 5, groups: types.map((type, i) => group("GET /tasks", type, extPlaces(10 + i))) })];
  const ws = await workspace(t, rows);
  const out = await run(ws, tasksEdit(ws, IN_LIST));
  assert.equal(out.lines.length, 2);
  assert.match(out.lines[0]!, /\{ id: string \}$/);
  assert.match(out.lines[1]!, /\{ id: number \}$/);
  assert.ok(!out.lines.some((line) => line.includes("boolean")));
});

test("four places print three and `(+1 more)`", async (t) => {
  const rows = [contractRow({ name: "listTasks", line: 5, groups: [group("GET /tasks", A, extPlaces(1, 2, 3, 4))] })];
  const ws = await workspace(t, rows);
  const out = await run(ws, tasksEdit(ws, IN_LIST));
  assert.deepEqual(out.lines, [lineFor("GET /tasks", [1, 2, 3].map((n) => `ext/src/view${n}.ts:${n}${NOTE}`).join(", "), " (+1 more)")]);
});

test("tier 2: an edit of a helper reaches the routes whose code calls it, and not the one that does not", async (t) => {
  const ws = await workspace(t, taskRows());
  // The file already said a contract line this session: tier 3 is closed.
  markFacts("s1", { [`contract::api::src/tasks.ts::producer::GET /tasks/other::0`]: { at: 1, named: [] } }, ws.home);
  const out = await run(ws, tasksEdit(ws, IN_FORMAT));
  assert.deepEqual(out.lines, [lineFor("GET /tasks", `ext/src/view4.ts:4${NOTE}`), lineFor("GET /tasks/:id", `ext/src/view4.ts:4${NOTE}`)]);
});

test("tier 3: the file's other operations print only on its first contract line of the session, after tiers 1 and 2", async (t) => {
  const ws = await workspace(t, taskRows());
  // A line outside every named thing reaches no operation: tier 3 alone, in line order.
  const first = await run(ws, tasksEdit(ws, "// tasks"));
  assert.equal(first.lines.length, MAX_FACT_LINES);
  assert.deepEqual(
    first.lines.map((line) => line.split(" is called")[0]),
    ["Carrick: GET /tasks", "Carrick: GET /tasks/:id", "Carrick: GET /tasks/other"],
  );
  // Once the file has said a contract line, an edit that reaches no operation prints none.
  markFacts("s1", first.claims, ws.home);
  const second = await run(ws, tasksEdit(ws, "// tasks"));
  assert.deepEqual(second.lines, []);
  // Tiers 1 and 2 come first when they exist: a helper edit says its routes, then the rest.
  const fresh = await run(ws, tasksEdit(ws, IN_FORMAT), { session: "s2" });
  assert.deepEqual(
    fresh.lines.map((line) => line.split(" is called")[0]),
    ["Carrick: GET /tasks", "Carrick: GET /tasks/:id", "Carrick: GET /tasks/other"],
  );
  // A tier 1 edit of the last route comes before the others.
  const own = await run(ws, tasksEdit(ws, "res.json({ ok: true });"), { session: "s3" });
  assert.equal(own.lines[0]!.split(" is called")[0], "Carrick: GET /tasks/other");
});

test("each operation's line prints once per session", async (t) => {
  const ws = await workspace(t, only(taskRows(), "listTasks"));
  const first = await run(ws, tasksEdit(ws, IN_LIST));
  assert.equal(first.lines.length, 1);
  markFacts("s1", first.claims, ws.home);
  assert.deepEqual((await run(ws, tasksEdit(ws, IN_LIST))).lines, []);
  assert.equal((await run(ws, tasksEdit(ws, IN_LIST), { session: "other" })).lines.length, 1);
});

test("the op line of the same row is skipped and claimed when its contract line prints", async (t) => {
  const ws = await workspace(t, only(taskRows(), "listTasks"));
  const on = await run(ws, tasksEdit(ws, IN_LIST));
  assert.deepEqual(on.lines, [lineFor("GET /tasks", `ext/src/view4.ts:4${NOTE}`)]);
  assert.ok("api::listTasks::producer::GET /tasks" in on.claims, "the op line is claimed with it");
  const off = await run(ws, tasksEdit(ws, IN_LIST), { contract: false });
  assert.deepEqual(off.lines, ["op line for listTasks"]);
});

test("a contract line that is silent leaves the op line to print", async (t) => {
  const ws = await workspace(t, only(taskRows(), "listTasks"), null);
  const out = await run(ws, tasksEdit(ws, IN_LIST));
  assert.deepEqual(out.lines, ["op line for listTasks"]);
});

test("the contract kind comes before the op kind and shares the cap of MAX_FACT_LINES", async (t) => {
  const rows = [...taskRows(), opRow("formatTask", "GET /format", "producer", "api", "src/tasks.ts", 2)];
  const ws = await workspace(t, rows);
  // Tier 3 fills the cap with contract lines: the op line is the one pushed out.
  const out = await run(ws, tasksEdit(ws, IN_FORMAT));
  assert.equal(out.lines.length, MAX_FACT_LINES);
  assert.ok(out.lines.every((line) => line.startsWith("Carrick: ")));
});

// The post-edit hook, end to end.

const hookEdit = (ws: Ws, session: string, env: Record<string, string>) =>
  patchEdit(ws, ws.at("api/src/tasks.ts"), [{ newStart: lineOf(ws.at("api/src/tasks.ts"), IN_LIST), lines: [`+  ${IN_LIST}`] }], { session, env });

test("with CARRICK_CONTRACT_LINES=1 an edit prints the contract line; a Read prints none", async (t) => {
  const ws = await workspace(t, only(taskRows(), "listTasks"));
  const out = await hookEdit(ws, "hook-on", { CARRICK_CONTRACT_LINES: "1" });
  assert.equal(out.code, 0);
  assert.ok(context(out.stdout).includes(lineFor("GET /tasks", `ext/src/view4.ts:4${NOTE}`)), context(out.stdout));
  const read = await runHook("post-read.ts", {
    payload: {
      hook_event_name: "PostToolUse",
      tool_name: "Read",
      session_id: "hook-read",
      cwd: ws.root,
      tool_input: { file_path: ws.at("api/src/tasks.ts") },
      tool_response: { file: { filePath: ws.at("api/src/tasks.ts"), content: TASKS } },
    },
    env: fakeEnv({ HOME: ws.home, CLAUDE_PROJECT_DIR: ws.root, CARRICK_CONTRACT_LINES: "1", CARRICK_FAKE_FIXTURE: fixturePath("check-silent.json") }),
  });
  assert.ok(!read.stdout.includes("is called from"), read.stdout);
});

/** Every file under `dir` by relative path, with its hash. */
function snapshot(dir: string): Map<string, string> {
  const seen = new Map<string, string>();
  const walk = (current: string) => {
    for (const entry of fs.readdirSync(current, { withFileTypes: true })) {
      const full = path.join(current, entry.name);
      if (entry.isDirectory()) walk(full);
      else seen.set(path.relative(dir, full), crypto.createHash("sha256").update(fs.readFileSync(full)).digest("hex"));
    }
  };
  walk(dir);
  return seen;
}

test("with CARRICK_CONTRACT_LINES unset or 0, the hook's stdout, exit code and files written are the same whether or not the fact file holds contract rows", async (t) => {
  const observe = async (rows: Array<Record<string, unknown>>, env: Record<string, string>) => {
    const ws = await workspace(t, rows);
    const before = [snapshot(ws.root), snapshot(ws.home)];
    const result = await hookEdit(ws, "hook-off", env);
    const after = [snapshot(ws.root), snapshot(ws.home)];
    // The fact file itself differs by its rows; every other file written is compared.
    const written = after.flatMap((map, i) =>
      [...map]
        .filter(([rel, hash]) => before[i]!.get(rel) !== hash && !rel.endsWith(".sqlite"))
        .map(([rel]) => `${i}:${rel.replace(/\.\d+-\d+-\d+\./, ".<id>.").replace(/\/\d+-\d+-\d+\.json$/, "/<id>.json")}`),
    );
    return { code: result.code, stdout: result.stdout, written: written.sort() };
  };
  const rows = only(taskRows(), "listTasks");
  const op = rows.find((row) => row["k"] === "op")!;
  for (const env of [{} as Record<string, string>, { CARRICK_CONTRACT_LINES: "0" }]) {
    const withRows = await observe(rows, env);
    const without = await observe([op], env);
    assert.equal(withRows.code, 0);
    assert.equal(withRows.stdout, without.stdout);
    assert.equal(context(withRows.stdout), "op line for listTasks");
    assert.deepEqual(withRows.written, without.written);
  }
  // The proof is not vacuous: with the switch on the same file prints the contract line instead.
  const on = await observe(rows, { CARRICK_CONTRACT_LINES: "1" });
  assert.notEqual(context(on.stdout), "op line for listTasks");
});
