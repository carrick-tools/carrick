// Edit-time facts: the fact file built from the wire fixture, the declaration
// scanner, and the lines the post-edit and shell hooks print from it.

import test from "node:test";
import assert from "node:assert/strict";
import crypto from "node:crypto";
import fs from "node:fs";
import os from "node:os";
import path from "node:path";
import { spawnSync } from "node:child_process";
import { fakeEnv, fixturePath, runHook, testDir } from "./helpers.ts";
import { buildFacts, factsPath, loadSqlite, SCHEMA } from "../src/facts/store.ts";
import { factLines } from "../src/facts/lines.ts";
import { scanSource } from "../src/facts/match.ts";
import { refreshFacts } from "../src/facts/fetch.ts";
import { readTimings } from "../src/hook/timings.ts";

const WIRE = fixturePath("edit-facts.s1.ndjson");
const REPOS = ["orders-api", "web-app", "billing-worker", "gateway", "billing-api"];

type FactsWorkspace = { root: string; home: string; at: (rel: string) => string };

/** The fixture repos copied into a fresh workspace, with the fact file built from `ndjson`. */
async function factsWorkspace(
  t: { after: (fn: () => void) => void },
  options: { ndjson?: string | null; fetchedAt?: string } = {},
): Promise<FactsWorkspace> {
  const root = fs.realpathSync(fs.mkdtempSync(path.join(os.tmpdir(), "carrick-facts-")));
  const home = fs.realpathSync(fs.mkdtempSync(path.join(os.tmpdir(), "carrick-facts-home-")));
  t.after(() => {
    fs.rmSync(root, { recursive: true, force: true });
    fs.rmSync(home, { recursive: true, force: true });
  });
  fs.cpSync(path.join(testDir, "fixtures", "facts"), root, { recursive: true });
  fs.mkdirSync(path.join(root, ".carrick"), { recursive: true });
  const ndjson = options.ndjson === undefined ? fs.readFileSync(WIRE, "utf8") : options.ndjson;
  if (ndjson !== null) {
    // The workspace is the fact file's project.
    const slug = (JSON.parse(ndjson.slice(0, ndjson.indexOf("\n"))) as { project_slug: string }).project_slug;
    fs.mkdirSync(path.join(root, ".carrick", "hosted"), { recursive: true });
    fs.writeFileSync(path.join(root, ".carrick", "hosted", "snapshot.json"), JSON.stringify({ resolution: { repos: [{ project_slug: slug }] } }));
    await buildFacts(
      ndjson,
      factsPath(root),
      REPOS.map((repo) => ({ repo, path: path.join(root, repo) })),
      options.fetchedAt ?? new Date().toISOString(),
    );
  }
  return { root, home, at: (rel) => path.join(root, rel) };
}

/** Run the post-edit hook for an Edit of `file` that put `newString` in. */
function edit(ws: FactsWorkspace, file: string, newString: string, options: { session?: string; probe?: string; cwd?: string } = {}) {
  return runHook("post-edit.ts", {
    payload: {
      hook_event_name: "PostToolUse",
      tool_name: "Edit",
      session_id: options.session ?? "facts-session",
      cwd: options.cwd ?? ws.root,
      tool_input: { file_path: file, old_string: "x", new_string: newString },
    },
    env: fakeEnv({
      HOME: ws.home,
      CLAUDE_PROJECT_DIR: ws.root,
      CARRICK_FAKE_FIXTURE: fixturePath(options.probe ?? "check-silent.json"),
    }),
  });
}

function context(stdout: string): string {
  if (!stdout.trim()) return "";
  return (JSON.parse(stdout) as { hookSpecificOutput: { additionalContext: string } }).hookSpecificOutput.additionalContext;
}

/** The fact lines for an edit of `rel` at the text `snippet` (whole file when null). */
async function linesFor(ws: FactsWorkspace, rel: string, snippet: string | null): Promise<string[]> {
  return (await factLines(ws.root, [{ file: ws.at(rel), snippets: snippet === null ? null : [snippet] }], undefined)).lines;
}

const header = (repos: string[]) =>
  JSON.stringify({
    schema: 1,
    writer: 1,
    project_id: "p",
    project_slug: "demo",
    digest: "d",
    built_at: "2026-01-01T00:00:00Z",
    repos: repos.map((repo) => ({ repo, service: repo, commit: "1" })),
    counts: { rows: 0, producers: 0, consumers: 0 },
  });

const row = (fields: Record<string, unknown>) =>
  JSON.stringify({ k: "op", service: fields["repo"], others: [], total: 0, source: "handler", ...fields });

test("the wire fixture is the pinned one", () => {
  const pinned = fs.readFileSync(path.join(testDir, "facts-fixture.sha256"), "utf8").split(/\s+/)[0];
  const actual = crypto.createHash("sha256").update(fs.readFileSync(WIRE)).digest("hex");
  assert.equal(actual, pinned);
});

// Naming rule 3 first: the dotted path of keys from a const-bound object.
test("naming rule 3: a resolver in an object map is found by its dotted path", async (t) => {
  const ws = await factsWorkspace(t);
  assert.deepEqual(await linesFor(ws, "orders-api/src/graphql/resolvers.ts", "return db.orders.find(args.id);"), [
    "QUERY order is called at web-app/src/api/order-query.ts:11",
  ]);
  // A method-shorthand member of a nested map, on the consumer side.
  assert.deepEqual(await linesFor(ws, "gateway/src/resolvers.ts", 'return http.post("/events"'), [
    "POST /events {type=refund} is served at billing-api/src/routes/events.ts:30",
  ]);
});

test("naming rule 1: a declared function and a const-bound arrow are found by name", async (t) => {
  const ws = await factsWorkspace(t);
  assert.deepEqual(await linesFor(ws, "orders-api/src/routes/orders.ts", "res.status(204).end();"), [
    "POST /orders/:id/cancel is called at web-app/src/api/order-client.ts:17",
  ]);
  assert.deepEqual(await linesFor(ws, "web-app/src/api/orders.ts", "return response.data as Order;"), [
    "GET /orders/:id is served at orders-api/src/routes/orders.ts:12",
  ]);
});

test("naming rule 2: a class method and a class field holding an arrow are Class.member", async (t) => {
  const ws = await factsWorkspace(t);
  assert.deepEqual(await linesFor(ws, "web-app/src/api/order-client.ts", "await this.http.post("), [
    "POST /orders/:id/cancel is served at orders-api/src/routes/orders.ts:40",
  ]);
  assert.deepEqual(await linesFor(ws, "gateway/src/sockets/chat.ts", 'socket.broadcast.emit("chat:message", payload);'), [
    "CLIENT->SERVER chat:send is called at web-app/src/chat/client.ts:33",
  ]);
});

test("naming rule 4: a call inside an anonymous callback is keyed to its named encloser", async (t) => {
  const ws = await factsWorkspace(t);
  assert.deepEqual(await linesFor(ws, "web-app/src/dashboard/setup.ts", "void api.get(`/orders/${id}`);"), [
    "GET /orders/:id is served at orders-api/src/routes/orders.ts:12",
  ]);
});

test("naming rule 5: a top-level call with no encloser is found by the operation string in its call", async (t) => {
  // Shaped like the cloud builder's rows: `site` is the string written at the call.
  const ndjson = [
    header(["billing-worker"]),
    row({ side: "consumer", protocol: "http", label: "GET /orders/:id", repo: "billing-worker", file: "src/sync.ts", line: 4, name: null, site: "/orders/:id", source: "call", text: "sync line" }),
  ].join("\n");
  const ws = await factsWorkspace(t, { ndjson: `${ndjson}\n` });
  assert.deepEqual(await linesFor(ws, "billing-worker/src/sync.ts", 'const order = await client.get("/orders/:id"'), ["sync line"]);
  // A changed line outside that call reaches no row.
  assert.deepEqual(await linesFor(ws, "billing-worker/src/sync.ts", "console.log(order.status);"), []);
});

test("a row with no name and no site never prints; the label's path is not a site", async (t) => {
  const ndjson = [
    header(["orders-api"]),
    row({ side: "producer", protocol: "http", label: "POST /orders", repo: "orders-api", file: "src/app.ts", line: 2, name: null, site: null, text: "orders line" }),
  ].join("\n");
  const ws = await factsWorkspace(t, { ndjson: `${ndjson}\n` });
  fs.writeFileSync(ws.at("orders-api/src/app.ts"), 'const col = db.collection("orders");\napp.post(`${base}/orders`, async (req, res) => { res.json(1); });\n');
  assert.deepEqual(await linesFor(ws, "orders-api/src/app.ts", 'const col = db.collection("orders");'), []);
  assert.deepEqual(await linesFor(ws, "orders-api/src/app.ts", "res.json(1);"), []);
});

test("an unnamed HTTP row needs the call's method to be the label's, and the pair unique in the file", async (t) => {
  const ndjson = [
    header(["orders-api"]),
    row({ side: "producer", protocol: "http", label: "GET /orders/:id/notes", repo: "orders-api", file: "src/notes.ts", line: 1, name: null, site: "/:id/notes", text: "notes GET line" }),
    row({ side: "producer", protocol: "http", label: "POST /orders/:id/notes", repo: "orders-api", file: "src/notes.ts", line: 4, name: null, site: "/:id/notes", text: "notes POST line" }),
    row({ side: "producer", protocol: "http", label: "PUT /orders/:id/flags", repo: "orders-api", file: "src/notes.ts", line: 7, name: null, site: "/:id/flags", text: "flags PUT line" }),
    row({ side: "producer", protocol: "http", label: "DELETE /orders/:id/tags", repo: "orders-api", file: "src/notes.ts", line: 10, name: null, site: "/:id/tags", text: "tags line" }),
  ].join("\n");
  const ws = await factsWorkspace(t, { ndjson: `${ndjson}\n` });
  fs.writeFileSync(
    ws.at("orders-api/src/notes.ts"),
    [
      'router.get("/:id/notes", async (req, res) => {',
      "  res.json(await notes(req.params.id, 1));",
      "});",
      'router.post("/:id/notes", async (req, res) => {',
      "  res.json(await notes(req.params.id, 2));",
      "});",
      'router.get("/:id/flags", async (req, res) => {',
      "  res.json(await flags(req.params.id));",
      "});",
      'router.delete("/:id/tags", async (req, res) => {',
      "  res.json(await tags(req.params.id, 1));",
      "});",
      'router.delete("/:id/tags", async (req, res) => {',
      "  res.json(await tags(req.params.id, 2));",
      "});",
      "",
    ].join("\n"),
  );
  assert.deepEqual(await linesFor(ws, "orders-api/src/notes.ts", "res.json(await notes(req.params.id, 1));"), ["notes GET line"]);
  assert.deepEqual(await linesFor(ws, "orders-api/src/notes.ts", "res.json(await notes(req.params.id, 2));"), ["notes POST line"]);
  // The call is `.get(`, the row is a PUT: nothing.
  assert.deepEqual(await linesFor(ws, "orders-api/src/notes.ts", "res.json(await flags(req.params.id));"), []);
  // The (site, method) pair is written twice: nothing.
  assert.deepEqual(await linesFor(ws, "orders-api/src/notes.ts", "res.json(await tags(req.params.id, 1));"), []);
});

test("unnamed socket and pub/sub rows are found by their site in the call around the edit", async (t) => {
  const ndjson = [
    header(["gateway"]),
    row({ side: "producer", protocol: "socket", label: "CLIENT->SERVER chat:send", repo: "gateway", file: "src/chat.ts", line: 4, name: null, site: "chat:send", text: "send line" }),
    row({ side: "consumer", protocol: "socket", label: "SERVER->CLIENT chat:message", repo: "gateway", file: "src/chat.ts", line: 5, name: null, site: "chat:message", text: "message line" }),
    row({ side: "producer", protocol: "pubsub", label: "PUBSUB orders.created", repo: "gateway", file: "src/chat.ts", line: 9, name: null, site: "orders.created", text: "created line" }),
  ].join("\n");
  const ws = await factsWorkspace(t, { ndjson: `${ndjson}\n` });
  fs.writeFileSync(
    ws.at("gateway/src/chat.ts"),
    [
      'io.on("connection", (socket) => {',
      "  const room = rooms.get(socket.id);",
      "  if (!room) return;",
      '  socket.on("chat:send", (payload) => {',
      '    io.emit("chat:message", payload);',
      "    audit(payload);",
      "  });",
      "});",
      'bus.subscribe("orders.created", async (message) => {',
      "  await mirror(message);",
      "});",
      "",
    ].join("\n"),
  );
  assert.deepEqual(await linesFor(ws, "gateway/src/chat.ts", "    audit(payload);"), ["send line"]);
  assert.deepEqual(await linesFor(ws, "gateway/src/chat.ts", "  await mirror(message);"), ["created line"]);
  // Inside `connection` but outside the `chat:send` call: nothing.
  assert.deepEqual(await linesFor(ws, "gateway/src/chat.ts", "  const room = rooms.get(socket.id);"), []);
});

test("naming rule 6: one name in two files is told apart by the edited file", async (t) => {
  const ndjson = [
    header(["orders-api"]),
    row({ side: "producer", protocol: "http", label: "GET /a", repo: "orders-api", file: "src/a.ts", line: 1, name: "handle", text: "GET /a line" }),
    row({ side: "producer", protocol: "http", label: "GET /b", repo: "orders-api", file: "src/b.ts", line: 1, name: "handle", text: "GET /b line" }),
    row({ side: "producer", protocol: "http", label: "GET /c", repo: "orders-api", file: "src/c.ts", line: 1, name: "only", text: "GET /c line" }),
  ].join("\n");
  const ws = await factsWorkspace(t, { ndjson: `${ndjson}\n` });
  for (const [file, body] of [
    ["src/a.ts", "export function handle() { return 1; }\n"],
    ["src/b.ts", "export function handle() { return 2; }\n"],
    ["src/c.ts", "export function only() { return 3; }\n"],
    ["src/d.ts", "export function handle() { return 4; }\nexport function only() { return 5; }\n"],
  ] as const) {
    fs.writeFileSync(ws.at(`orders-api/${file}`), body);
  }
  assert.deepEqual(await linesFor(ws, "orders-api/src/a.ts", "return 1;"), ["GET /a line"]);
  assert.deepEqual(await linesFor(ws, "orders-api/src/b.ts", "return 2;"), ["GET /b line"]);
  // Two files hold `handle` and neither is this one: no guess.
  assert.deepEqual(await linesFor(ws, "orders-api/src/d.ts", "return 4;"), []);
  // `only` is still declared in its own file, so this `only` is another thing.
  assert.deepEqual(await linesFor(ws, "orders-api/src/d.ts", "return 5;"), []);
  // A row file that exists but no longer declares the name is not proof the thing moved here.
  fs.writeFileSync(ws.at("orders-api/src/c.ts"), "export {};\n");
  assert.deepEqual(await linesFor(ws, "orders-api/src/d.ts", "return 5;"), []);
  // Once its file is gone, the one declaration of the name here is that thing, moved.
  fs.rmSync(ws.at("orders-api/src/c.ts"));
  assert.deepEqual(await linesFor(ws, "orders-api/src/d.ts", "return 5;"), ["GET /c line"]);
  // Not when the name is declared twice here.
  fs.writeFileSync(ws.at("orders-api/src/d.ts"), "export function only() { return 5; }\nclass K { m() { function only() { return 6; } } }\n");
  assert.deepEqual(await linesFor(ws, "orders-api/src/d.ts", "return 5;"), []);
});

test("a row in another file whose name the edited file also declares is another thing", async (t) => {
  const ndjson = [
    header(["orders-api", "web-app"]),
    row({ side: "consumer", protocol: "http", label: "GET /orders", repo: "orders-api", file: "src/client.ts", line: 2, name: "fetchAll", text: "fetchAll line" }),
    row({ side: "consumer", protocol: "http", label: "GET /orders", repo: "web-app", file: "src/Page.tsx", line: 3, name: "load", text: "load line" }),
  ].join("\n");
  const ws = await factsWorkspace(t, { ndjson: `${ndjson}\n` });
  fs.writeFileSync(ws.at("orders-api/src/client.ts"), 'export async function fetchAll<T>(url: string): Promise<T> {\n  return http.get("/orders");\n}\n');
  fs.writeFileSync(ws.at("orders-api/src/jobs.ts"), "export function fetchAll() {\n  return queue.drain();\n}\n");
  assert.deepEqual(await linesFor(ws, "orders-api/src/jobs.ts", "return queue.drain();"), []);
  // The row's file does not read cleanly: never "not declared there".
  fs.writeFileSync(ws.at("web-app/src/Page.tsx"), 'const pick = <T,>(a: T) => a;\nexport function load() {\n  return api.get("/orders"\n}\n');
  fs.writeFileSync(ws.at("web-app/src/cron.ts"), "export function load() {\n  return cache.warm();\n}\n");
  assert.deepEqual(await linesFor(ws, "web-app/src/cron.ts", "return cache.warm();"), []);
});

test("a name declared twice in the edited file prints nothing for that name", async (t) => {
  const ndjson = [
    header(["web-app"]),
    row({ side: "consumer", protocol: "socket", label: "CLIENT->SERVER chat:send", repo: "web-app", file: "src/chat.tsx", line: 3, name: "onMessage", site: "chat:send", text: "onMessage line" }),
  ].join("\n");
  const ws = await factsWorkspace(t, { ndjson: `${ndjson}\n` });
  fs.writeFileSync(
    ws.at("web-app/src/chat.tsx"),
    [
      "export function Inbox() {",
      "  const onMessage = (text: string) => {",
      '    socket.emit("chat:send", text);',
      "  };",
      "  return <List onMessage={onMessage} />;",
      "}",
      "export function Thread() {",
      "  const onMessage = (text: string) => {",
      "    log(text);",
      "  };",
      "  return <List onMessage={onMessage} />;",
      "}",
      "",
    ].join("\n"),
  );
  assert.deepEqual(await linesFor(ws, "web-app/src/chat.tsx", "    log(text);"), []);
  assert.deepEqual(await linesFor(ws, "web-app/src/chat.tsx", '    socket.emit("chat:send", text);'), []);
});

test("an inline handler is found by the operation string written at its site, and only when it is unique", async (t) => {
  const ndjson = [
    header(["orders-api"]),
    row({ side: "producer", protocol: "http", label: "GET /orders/:id/items", repo: "orders-api", file: "src/mounted.ts", line: 3, name: null, site: "/:id/items", text: "items line" }),
    row({ side: "producer", protocol: "http", label: "GET /orders/:id/notes", repo: "orders-api", file: "src/mounted.ts", line: 6, name: null, site: "/:id/notes", text: "notes GET line" }),
    row({ side: "producer", protocol: "http", label: "POST /orders/:id/notes", repo: "orders-api", file: "src/mounted.ts", line: 9, name: null, site: "/:id/notes", text: "notes POST line" }),
    row({ side: "producer", protocol: "socket", label: "CLIENT->SERVER chat:typing", repo: "orders-api", file: "src/mounted.ts", line: 13, name: null, site: null, text: "typing line" }),
  ].join("\n");
  const ws = await factsWorkspace(t, { ndjson: `${ndjson}\n` });
  fs.writeFileSync(
    ws.at("orders-api/src/mounted.ts"),
    [
      'const router = Router();',
      '',
      'router.get("/:id/items", async (req, res) => {',
      "  res.json(await items(req.params.id));",
      "});",
      'router.get("/:id/notes", async (req, res) => {',
      "  res.json(await notes(req.params.id, 1));",
      "});",
      'router.post("/:id/notes", async (req, res) => {',
      "  res.json(await notes(req.params.id, 2));",
      "});",
      'io.on("connection", (socket) => {',
      '  socket.on("chat:typing", (payload) => {',
      "    relay(payload);",
      "  });",
      "});",
      "",
    ].join("\n"),
  );
  assert.deepEqual(await linesFor(ws, "orders-api/src/mounted.ts", "res.json(await items(req.params.id));"), ["items line"]);
  // The same site under two methods: each call is told apart by its method.
  assert.deepEqual(await linesFor(ws, "orders-api/src/mounted.ts", "res.json(await notes(req.params.id, 1));"), ["notes GET line"]);
  // No site: nothing, whatever the label says.
  assert.deepEqual(await linesFor(ws, "orders-api/src/mounted.ts", "relay(payload);"), []);
});

test("text the edit put in that the file holds twice reaches nothing", async (t) => {
  const ws = await factsWorkspace(t);
  // `return;` is now in both handlers: the text cannot say which one the edit wrote.
  const file = ws.at("orders-api/src/routes/orders.ts");
  fs.writeFileSync(file, fs.readFileSync(file, "utf8").replace("res.status(204).end();", "res.status(204).end();\n  return;"));
  assert.deepEqual(await linesFor(ws, "orders-api/src/routes/orders.ts", "  return;"), []);
});

test("an Edit's patch hunks place the change by line, deletions included", async (t) => {
  const ws = await factsWorkspace(t);
  const file = ws.at("orders-api/src/routes/orders.ts");
  const lineOf = (needle: string) => fs.readFileSync(file, "utf8").split("\n").findIndex((line) => line.includes(needle)) + 1;
  const at = lineOf("res.status(204).end();");
  const run = await runHook("post-edit.ts", {
    payload: {
      hook_event_name: "PostToolUse",
      tool_name: "Edit",
      session_id: "patch",
      cwd: ws.root,
      // A deletion just above the line: the new text is empty, so only the hunk places it.
      tool_input: { file_path: file, old_string: "  // gone\n", new_string: "" },
      tool_response: { structuredPatch: [{ oldStart: at - 1, oldLines: 3, newStart: at - 1, newLines: 2, lines: [" ctx", "-  // gone", " ctx"] }] },
    },
    env: fakeEnv({ HOME: ws.home, CLAUDE_PROJECT_DIR: ws.root, CARRICK_FAKE_FIXTURE: fixturePath("check-silent.json") }),
  });
  assert.equal(context(run.stdout), "POST /orders/:id/cancel is called at web-app/src/api/order-client.ts:17");
});

/** Run the post-edit hook for an Edit of `file` whose result carries `hunks`. */
function patchEdit(ws: FactsWorkspace, file: string, hunks: Array<{ newStart: number; lines: string[] }>, session = "patch") {
  return runHook("post-edit.ts", {
    payload: {
      hook_event_name: "PostToolUse",
      tool_name: "Edit",
      session_id: session,
      cwd: ws.root,
      tool_input: { file_path: file, old_string: "x", new_string: "" },
      tool_response: { structuredPatch: hunks.map((hunk) => ({ oldStart: hunk.newStart, oldLines: 0, newLines: 0, ...hunk })) },
    },
    env: fakeEnv({ HOME: ws.home, CLAUDE_PROJECT_DIR: ws.root, CARRICK_FAKE_FIXTURE: fixturePath("check-silent.json") }),
  });
}

test("a removed line touches a thing only when it was removed strictly inside it", async (t) => {
  const ndjson = [
    header(["orders-api"]),
    row({ side: "consumer", protocol: "http", label: "POST /g", repo: "orders-api", file: "src/g.ts", line: 3, name: "g", text: "g line" }),
  ].join("\n");
  const ws = await factsWorkspace(t, { ndjson: `${ndjson}\n` });
  const file = ws.at("orders-api/src/g.ts");
  // `function f() {...}` was deleted from above g: g now starts at line 1.
  fs.writeFileSync(file, 'function g() {\n  log("g");\n  return api.post("/g");\n}\n\nconst after = 1;\n');
  const above = await patchEdit(ws, file, [{ newStart: 1, lines: ["-function f() {", '-  api.get("/f");', "-}", " function g() {"] }]);
  assert.equal(context(above.stdout), "");
  // Removed just below g's closing brace: not inside g.
  const below = await patchEdit(ws, file, [{ newStart: 4, lines: [" }", "-// note", " "] }]);
  assert.equal(context(below.stdout), "");
  // Removed between two lines of g's body: g.
  const inside = await patchEdit(ws, file, [{ newStart: 2, lines: ['   log("g");', "-  trace();", '   return api.post("/g");'] }]);
  assert.equal(context(inside.stdout), "g line");
});

test("a thing's span starts at export, default, async and decorators: its header line is its own", async (t) => {
  assert.deepEqual(spansOf("export async function g() { return 1 }\nexport default function h() {}\nexport const k = () => 1;\n"), [
    "g: export async function g() { return 1 }",
    "h: export default function h() {}",
    "k: export const k = () => 1",
  ]);
  const ndjson = [
    header(["orders-api"]),
    row({ side: "consumer", protocol: "http", label: "POST /g", repo: "orders-api", file: "src/g.ts", line: 2, name: "g", text: "g line" }),
  ].join("\n");
  const ws = await factsWorkspace(t, { ndjson: `${ndjson}\n` });
  const file = ws.at("orders-api/src/g.ts");
  fs.writeFileSync(file, 'export async function g(id: string) {\n  return api.post("/g");\n}\n');
  const run = await patchEdit(ws, file, [{ newStart: 1, lines: ["-export async function g() {", "+export async function g(id: string) {", '   return api.post("/g");'] }]);
  assert.equal(context(run.stdout), "g line");
});

/** Run the post-edit hook with a whole payload (a Write, or Codex's `apply_patch`). */
function payloadEdit(ws: FactsWorkspace, payload: Record<string, unknown>, session = "whole") {
  return runHook("post-edit.ts", {
    payload: { hook_event_name: "PostToolUse", session_id: session, cwd: ws.root, ...payload },
    env: fakeEnv({ HOME: ws.home, CLAUDE_PROJECT_DIR: ws.root, CARRICK_FAKE_FIXTURE: fixturePath("check-silent.json") }),
  });
}

/** Make `repo` in the workspace a git repository with its files committed. */
function gitRepo(ws: FactsWorkspace, repo: string): void {
  const dir = ws.at(repo);
  const git = (...args: string[]) => {
    const run = spawnSync("git", ["-C", dir, "-c", "user.email=t@example.com", "-c", "user.name=t", ...args], { encoding: "utf8" });
    assert.equal(run.status, 0, run.stderr);
  };
  git("init", "-q");
  git("add", ".");
  git("commit", "-q", "-m", "init");
}

test("a Write over an existing file, whose changed lines are unknown, prints nothing", async (t) => {
  const ws = await factsWorkspace(t);
  const file = ws.at("orders-api/src/routes/orders.ts");
  const run = await payloadEdit(ws, { tool_name: "Write", tool_input: { file_path: file, content: fs.readFileSync(file, "utf8") }, tool_response: { type: "update" } });
  assert.equal(context(run.stdout), "");
});

test("a file this Write created, untracked in git, counts every named thing in it", async (t) => {
  const ws = await factsWorkspace(t);
  gitRepo(ws, "orders-api");
  const file = ws.at("orders-api/src/routes/fresh.ts");
  const text = fs.readFileSync(ws.at("orders-api/src/routes/orders.ts"), "utf8");
  fs.writeFileSync(file, text);
  const payload = { tool_name: "Write", tool_input: { file_path: file, content: text }, tool_response: { type: "create", filePath: file } };
  // Its rows are another file's, still on disk: nothing, by the named-row rule.
  assert.equal(context((await payloadEdit(ws, payload)).stdout), "");
  // A tracked file is never "created by this call".
  fs.rmSync(file);
  const tracked = ws.at("orders-api/src/routes/orders.ts");
  const again = await payloadEdit(ws, { ...payload, tool_input: { file_path: tracked, content: text }, tool_response: { type: "create", filePath: tracked } }, "tracked");
  assert.equal(context(again.stdout), "");
  // The repo's own file, deleted and written again by this call: every thing counts.
  fs.rmSync(tracked);
  spawnSync("git", ["-C", ws.at("orders-api"), "rm", "-q", "--cached", "src/routes/orders.ts"]);
  fs.writeFileSync(tracked, text);
  const created = await payloadEdit(ws, { ...payload, tool_input: { file_path: tracked, content: text }, tool_response: { type: "create", filePath: tracked } }, "created");
  assert.match(context(created.stdout), /^GET \/orders\/:id is called at /);
});

test("Codex apply_patch: its hunks place the change; a hunk that cannot be placed prints nothing", async (t) => {
  const ws = await factsWorkspace(t);
  const patch = (body: string[]) => ["*** Begin Patch", "*** Update File: orders-api/src/routes/orders.ts", ...body, "*** End Patch", ""].join("\n");
  const file = ws.at("orders-api/src/routes/orders.ts");
  fs.writeFileSync(file, fs.readFileSync(file, "utf8").replace("  res.status(204).end();", "  res.status(205).end();"));
  const placed = await payloadEdit(ws, {
    tool_name: "apply_patch",
    tool_input: { command: patch(["@@ export async function cancelOrder", '   await db.orders.cancel(req.params["id"]);', "-  res.status(204).end();", "+  res.status(205).end();"]) },
  });
  assert.equal(context(placed.stdout), "POST /orders/:id/cancel is called at web-app/src/api/order-client.ts:17");
  // The hunk's new text is nowhere in the file: nothing.
  const lost = await payloadEdit(ws, { tool_name: "apply_patch", tool_input: { command: patch(["@@", "-  res.status(204).end();", "+  res.status(299).end();"]) } }, "lost");
  assert.equal(context(lost.stdout), "");
  // `*** Add File` of a file git does not track: every thing in it counts.
  gitRepo(ws, "orders-api");
  spawnSync("git", ["-C", ws.at("orders-api"), "rm", "-q", "--cached", "src/routes/orders.ts"]);
  const text = fs.readFileSync(file, "utf8");
  const add = ["*** Begin Patch", "*** Add File: orders-api/src/routes/orders.ts", ...text.trimEnd().split("\n").map((line) => `+${line}`), "*** End Patch", ""].join("\n");
  const added = await payloadEdit(ws, { tool_name: "apply_patch", tool_input: { command: add } }, "added");
  assert.match(context(added.stdout), /^GET \/orders\/:id is called at /);
});

test("a file over 512 KB is skipped: a 2 MB generated file returns in under 50 ms", async (t) => {
  const ws = await factsWorkspace(t);
  // Warm `node:sqlite` so the timing is the file's.
  await linesFor(ws, "orders-api/src/routes/orders.ts", "res.status(204).end();");
  const file = ws.at("orders-api/src/routes/generated.ts");
  const parts: string[] = [];
  let size = 0;
  for (let i = 0; size < 2_000_000; i += 1) {
    const part = `export function getOrder${i}(req, res) {\n  return api.get("/orders/${i}");\n}\n`;
    parts.push(part);
    size += part.length;
  }
  fs.writeFileSync(file, parts.join(""));
  const outcome = await factLines(ws.root, [{ file, whole: true }], undefined);
  assert.deepEqual(outcome.lines, []);
  assert.ok(outcome.ms < 50, `took ${outcome.ms} ms`);
});

test("a fact file's repo maps to a checkout only by its exact remote, else its exact name", async () => {
  const { matchRepos } = await import("../src/facts/fetch.ts");
  const local = [
    { name: "web", path: "/w/web", remote: "git@github.com:acme/web.git" },
    { name: "api", path: "/w/api", remote: "https://github.com/acme/api-server" },
    { name: "jobs", path: "/w/jobs-1", remote: null },
    { name: "jobs", path: "/w/jobs-2", remote: null },
  ];
  // The remote's last segment is not the repo.
  assert.deepEqual(matchRepos([{ repo: "api" }], local), [{ repo: "api", path: "/w/api" }]);
  assert.deepEqual(matchRepos([{ repo: "api-server" }], local), []);
  // An exact owner/name remote, in either spelling git accepts.
  assert.deepEqual(matchRepos([{ repo: "frontend", remote: "acme/web" }], local), [{ repo: "frontend", path: "/w/web" }]);
  // A header remote that matches no checkout is not rescued by the name.
  assert.deepEqual(matchRepos([{ repo: "web", remote: "other/web" }], local), []);
  // Two checkouts of one name: ambiguous, not read.
  assert.deepEqual(matchRepos([{ repo: "jobs" }], local), []);
});

test("a fact file for another project is not read", async (t) => {
  const ws = await factsWorkspace(t);
  const edited = { file: ws.at("orders-api/src/routes/orders.ts"), snippets: ["res.status(204).end();"] };
  assert.equal((await factLines(ws.root, [edited], undefined)).lines.length, 1);
  fs.writeFileSync(path.join(ws.root, ".carrick", "hosted", "snapshot.json"), JSON.stringify({ resolution: { repos: [{ project_slug: "another" }] } }));
  assert.deepEqual((await factLines(ws.root, [edited], undefined)).lines, []);
  fs.rmSync(path.join(ws.root, ".carrick", "hosted", "snapshot.json"));
  assert.deepEqual((await factLines(ws.root, [edited], undefined)).lines, []);
});

test("the refresh sends the local file's digest and writer", async (t) => {
  const ws = await factsWorkspace(t);
  const bodies: Array<Record<string, unknown>> = [];
  const request = (async (_url: string, init?: RequestInit) => {
    bodies.push(JSON.parse(String(init?.body)) as Record<string, unknown>);
    return new Response(JSON.stringify({ unchanged: true }));
  }) as typeof fetch;
  const env = { ...process.env, CARRICK_TOKEN: "t" };
  assert.equal(await refreshFacts(ws.root, { request, env }), "unchanged");
  const wire = JSON.parse(fs.readFileSync(WIRE, "utf8").split("\n")[0]!) as { digest: string; writer: number };
  assert.equal(bodies[0]!["known_digest"], wire.digest);
  assert.equal(bodies[0]!["known_writer"], wire.writer);
  // With no local file, neither is sent.
  fs.rmSync(factsPath(ws.root));
  const building = (async (_url: string, init?: RequestInit) => {
    bodies.push(JSON.parse(String(init?.body)) as Record<string, unknown>);
    return new Response(JSON.stringify({ success: false, error: { code: "facts_building" } }));
  }) as typeof fetch;
  await refreshFacts(ws.root, { request: building, env });
  assert.equal("known_digest" in bodies[1]!, false);
  assert.equal("known_writer" in bodies[1]!, false);
});

test("facts_unavailable is silent and does not schedule an early retry", async (t) => {
  const { runFactsRefresh, startFactsRefresh, FACTS_BUILDING_RETRY_MS } = await import("../src/facts/fetch.ts");
  const ws = await factsWorkspace(t, { ndjson: null });
  fs.mkdirSync(path.join(ws.root, ".carrick", "hosted"), { recursive: true });
  fs.writeFileSync(path.join(ws.root, ".carrick", "hosted", "snapshot.json"), JSON.stringify({ resolution: { repos: [{ project_slug: "demo-shop" }] } }));
  const env = { ...process.env, CARRICK_TOKEN: "t", CARRICK_NO_FACTS_REFRESH: "" };
  const now = Date.parse("2026-01-01T12:00:00Z");
  const start = () => undefined;
  assert.equal(startFactsRefresh(ws.root, { env, now, start }), true);
  const answer = (code: string) =>
    (async () => new Response(JSON.stringify({ success: false, error: { code, message: "", retriable: code === "facts_building" } }))) as typeof fetch;
  assert.equal(await runFactsRefresh(ws.root, { request: answer("facts_unavailable"), env, now }), "none");
  assert.equal(fs.existsSync(factsPath(ws.root)), false);
  assert.equal(startFactsRefresh(ws.root, { env, now: now + FACTS_BUILDING_RETRY_MS + 1_000, start }), false, "no early retry");
  // A building answer does move the next ask a minute on.
  assert.equal(await runFactsRefresh(ws.root, { request: answer("facts_building"), env, now }), "building");
  assert.equal(startFactsRefresh(ws.root, { env, now: now + FACTS_BUILDING_RETRY_MS + 1_000, start }), true);
});

test("a staged URL is fetched only over https from an amazonaws.com host", async (t) => {
  const ws = await factsWorkspace(t, { ndjson: null });
  fs.mkdirSync(path.join(ws.root, ".carrick", "hosted"), { recursive: true });
  fs.writeFileSync(path.join(ws.root, ".carrick", "hosted", "snapshot.json"), JSON.stringify({ resolution: { repos: [{ project_slug: "demo-shop" }] } }));
  const env = { ...process.env, CARRICK_TOKEN: "t" };
  for (const staged of ["https://s3.example/x", "http://facts.s3.amazonaws.com/x", "https://evil.example/.amazonaws.com/x", "https://amazonaws.com.evil.example/x", "not a url"]) {
    const fetched: string[] = [];
    const request = (async (url: string) => {
      fetched.push(url);
      if (url.endsWith("/mcp/facts")) return new Response(JSON.stringify({ staged_url: staged, digest: "d", schema: 1, writer: 1, bytes: 1 }));
      return new Response(fs.readFileSync(WIRE));
    }) as typeof fetch;
    assert.equal(await refreshFacts(ws.root, { request, env }), "none", staged);
    assert.equal(fetched.length, 1, `${staged} is not fetched`);
    assert.equal(fs.existsSync(factsPath(ws.root)), false);
  }
});

test("a failed build leaves no temp file behind", async (t) => {
  const ws = await factsWorkspace(t, { ndjson: null });
  const dest = factsPath(ws.root);
  const bad = `${header(["orders-api"])}\n{"k":"op", not json\n`;
  await assert.rejects(buildFacts(bad, dest, [], new Date().toISOString()));
  assert.deepEqual(fs.readdirSync(path.dirname(dest)), []);
});

test("every database handle is closed, whichever way the read ends", async (t) => {
  const sqlite = (await loadSqlite())!;
  const proto = sqlite.DatabaseSync.prototype as { close: () => void };
  const close = proto.close;
  let closed = 0;
  proto.close = function (this: unknown) {
    closed += 1;
    return close.call(this);
  };
  t.after(() => {
    proto.close = close;
  });
  const { openFacts, touchFetched } = await import("../src/facts/store.ts");
  const ws = await factsWorkspace(t, { ndjson: null });
  fs.mkdirSync(path.join(ws.root, ".carrick", "hosted"), { recursive: true });
  fs.writeFileSync(path.join(ws.root, ".carrick", "hosted", "snapshot.json"), JSON.stringify({ resolution: { repos: [{ project_slug: "demo" }] } }));
  fs.mkdirSync(path.dirname(factsPath(ws.root)), { recursive: true });
  // A file with a meta table this package reads but no other table.
  const db = new sqlite.DatabaseSync(factsPath(ws.root));
  db.exec("CREATE TABLE meta (key TEXT PRIMARY KEY, value TEXT NOT NULL); INSERT INTO meta VALUES ('schema', '1'), ('project', 'demo');");
  db.close();
  closed = 0;
  assert.equal(await openFacts(ws.root), null);
  assert.equal(closed, 1, "openFacts closes the file it could not read");
  // A file with no meta table at all.
  fs.rmSync(factsPath(ws.root));
  new sqlite.DatabaseSync(factsPath(ws.root)).close();
  closed = 0;
  await touchFetched(ws.root, new Date().toISOString());
  assert.equal(closed, 1, "touchFetched closes the file it could not stamp");
  closed = 0;
  const request = (async () => new Response(JSON.stringify({ success: false, error: { code: "facts_building" } }))) as typeof fetch;
  await refreshFacts(ws.root, { request, env: { ...process.env, CARRICK_TOKEN: "t" } });
  assert.equal(closed, 1, "the digest read closes the file it could not read");
});

test("a component file with markup is read: the call in its effect is found by the component's name", async (t) => {
  const ndjson = [
    header(["web-app"]),
    row({ side: "consumer", protocol: "http", label: "GET /orders/:id", repo: "web-app", file: "src/pages/order.tsx", line: 4, name: "OrderPage", text: "page line" }),
  ].join("\n");
  const ws = await factsWorkspace(t, { ndjson: `${ndjson}\n` });
  fs.writeFileSync(
    ws.at("web-app/src/pages/order.tsx"),
    [
      "export function OrderPage({ id }: { id: string }) {",
      "  const [order, setOrder] = useState<Order | null>(null);",
      "  useEffect(() => {",
      '    void api.get("/orders/" + id).then(setOrder);',
      "  }, [id]);",
      "  return (",
      '    <div className="order">',
      "      <h1>{order?.title}</h1>",
      "      <p>Don't wait</p>",
      "      <Item value={order} onPick={() => setOrder(null)} />",
      "    </div>",
      "  );",
      "}",
      "",
    ].join("\n"),
  );
  assert.deepEqual(await linesFor(ws, "web-app/src/pages/order.tsx", '    void api.get("/orders/" + id)'), ["page line"]);
});

test("the refresh starts once per cooldown, sooner after a building answer, and never outside a workspace", async (t) => {
  const { startFactsRefresh, allowEarlyRetry, FACTS_BUILDING_RETRY_MS } = await import("../src/facts/fetch.ts");
  const ws = await factsWorkspace(t, { ndjson: null });
  const env = { ...process.env, CARRICK_NO_FACTS_REFRESH: "" };
  const started: string[][] = [];
  const start = (_command: string, args: string[]) => void started.push(args);
  const now = Date.parse("2026-01-01T12:00:00Z");
  assert.equal(startFactsRefresh(ws.root, { env, now, start }), true);
  assert.match(started[0]![0]!, /refresh-run\.ts$/);
  assert.equal(startFactsRefresh(ws.root, { env, now: now + 30_000, start }), false, "inside the cooldown");
  allowEarlyRetry(ws.root, env, now);
  assert.equal(startFactsRefresh(ws.root, { env, now: now + FACTS_BUILDING_RETRY_MS - 1_000, start }), false);
  assert.equal(startFactsRefresh(ws.root, { env, now: now + FACTS_BUILDING_RETRY_MS + 1_000, start }), true);
  // A folder Carrick was never set up in: nothing starts and no directory appears.
  const bare = fs.realpathSync(fs.mkdtempSync(path.join(os.tmpdir(), "carrick-bare-")));
  t.after(() => fs.rmSync(bare, { recursive: true, force: true }));
  assert.equal(startFactsRefresh(bare, { env, now, start }), false);
  assert.equal(fs.existsSync(path.join(bare, ".carrick")), false);
  assert.equal(started.length, 2);
});

test("an edit inside a handler prints where it is called, once per session", async (t) => {
  const ws = await factsWorkspace(t);
  const file = ws.at("orders-api/src/routes/orders.ts");
  const first = await edit(ws, file, 'const order = await db.orders.find(req.params["id"]);');
  assert.equal(first.code, 0);
  // Some Node 24 releases warn when `node:sqlite` loads; a hook's stderr stays clean.
  assert.doesNotMatch(first.stderr, /ExperimentalWarning/);
  assert.equal(
    context(first.stdout),
    "GET /orders/:id is called at web-app/src/api/orders.ts:8, web-app/src/dashboard/setup.ts:21, billing-worker/src/sync.ts:5",
  );
  const second = await edit(ws, file, 'const order = await db.orders.find(req.params["id"]);');
  assert.equal(context(second.stdout), "");
  // Another session hears it again.
  const other = await edit(ws, file, 'const order = await db.orders.find(req.params["id"]);', { session: "another" });
  assert.match(context(other.stdout), /^GET \/orders\/:id is called at /);
});

test("an edit at a consumer call prints where it is served", async (t) => {
  const ws = await factsWorkspace(t);
  const run = await edit(ws, ws.at("web-app/src/chat/client.ts"), 'socket.emit("chat:send", { text });');
  assert.equal(context(run.stdout), "CLIENT->SERVER chat:send is served at gateway/src/sockets/chat.ts:14");
});

test("the handler moved 30 lines on the branch: the line still prints, found by name", async (t) => {
  const ws = await factsWorkspace(t);
  const file = ws.at("orders-api/src/routes/orders.ts");
  fs.writeFileSync(file, `${"// moved\n".repeat(30)}${fs.readFileSync(file, "utf8")}`);
  const run = await edit(ws, file, 'const order = await db.orders.find(req.params["id"]);');
  assert.match(context(run.stdout), /^GET \/orders\/:id is called at web-app\/src\/api\/orders\.ts:8/);
});

test("the handler was renamed on the branch: nothing prints", async (t) => {
  const ws = await factsWorkspace(t);
  const file = ws.at("orders-api/src/routes/orders.ts");
  fs.writeFileSync(file, fs.readFileSync(file, "utf8").replaceAll("getOrder", "fetchOrderById"));
  const run = await edit(ws, file, 'const order = await db.orders.find(req.params["id"]);');
  assert.equal(run.code, 0);
  assert.equal(context(run.stdout), "");
});

test("a file with another schema prints nothing and asks for schema 1", async (t) => {
  const ws = await factsWorkspace(t);
  const sqlite = await loadSqlite();
  const db = new sqlite!.DatabaseSync(factsPath(ws.root));
  db.prepare("UPDATE meta SET value = '2' WHERE key = 'schema'").run();
  db.close();
  const run = await edit(ws, ws.at("orders-api/src/routes/orders.ts"), 'const order = await db.orders.find(req.params["id"]);');
  assert.equal(context(run.stdout), "");
  const outcome = await factLines(ws.root, [{ file: ws.at("orders-api/src/routes/orders.ts"), snippets: null }], undefined);
  assert.deepEqual(outcome.lines, []);
  assert.equal(outcome.stale, true, "an unreadable file asks for a refresh");
  // The refresh asks for this package's schema.
  fs.mkdirSync(path.join(ws.root, ".carrick", "hosted"), { recursive: true });
  fs.writeFileSync(
    path.join(ws.root, ".carrick", "hosted", "snapshot.json"),
    JSON.stringify({ resolution: { repos: [{ project_slug: "demo" }] } }),
  );
  const bodies: unknown[] = [];
  const request = (async (_url: string, init?: RequestInit) => {
    bodies.push(JSON.parse(String(init?.body)));
    return new Response(JSON.stringify({ success: false, error: { code: "facts_building", message: "", retriable: true } }), { status: 200 });
  }) as typeof fetch;
  assert.equal(await refreshFacts(ws.root, { request, env: { ...process.env, CARRICK_TOKEN: "t" } }), "building");
  assert.equal((bodies[0] as { schema: number }).schema, SCHEMA);
});

test("a 404 or a building answer leaves no fact file", async (t) => {
  const ws = await factsWorkspace(t, { ndjson: null });
  fs.mkdirSync(path.join(ws.root, ".carrick", "hosted"), { recursive: true });
  fs.writeFileSync(path.join(ws.root, ".carrick", "hosted", "snapshot.json"), JSON.stringify({ resolution: { repos: [{ project_slug: "demo" }] } }));
  const env = { ...process.env, CARRICK_TOKEN: "t" };
  const notFound = (async () => new Response("", { status: 404 })) as typeof fetch;
  assert.equal(await refreshFacts(ws.root, { request: notFound, env }), "none");
  assert.equal(fs.existsSync(factsPath(ws.root)), false);
});

test("a fresh digest is fetched, unzipped and built; an unchanged one is one read", async (t) => {
  const ws = await factsWorkspace(t, { ndjson: null });
  fs.mkdirSync(path.join(ws.root, ".carrick", "hosted"), { recursive: true });
  fs.writeFileSync(path.join(ws.root, ".carrick", "hosted", "snapshot.json"), JSON.stringify({ resolution: { repos: [{ project_slug: "demo-shop" }] } }));
  fs.writeFileSync(
    path.join(ws.root, ".carrick", "index.json"),
    JSON.stringify({ repos: REPOS.map((repo) => ({ name: repo, path: ws.at(repo), services: [] })) }),
  );
  const { gzipSync } = await import("node:zlib");
  const calls: Array<{ url: string; auth: string | null }> = [];
  const request = (async (url: string, init?: RequestInit) => {
    const auth = new Headers(init?.headers).get("authorization");
    calls.push({ url, auth });
    if (url.endsWith("/mcp/facts")) {
      const body = JSON.parse(String(init?.body)) as { known_digest?: string };
      if (body.known_digest) return new Response(JSON.stringify({ unchanged: true }));
      return new Response(JSON.stringify({ staged_url: "https://facts.s3.eu-west-1.amazonaws.com/x?sig=1", digest: "d", schema: 1, writer: 1, bytes: 1 }));
    }
    return new Response(gzipSync(fs.readFileSync(WIRE)));
  }) as typeof fetch;
  const env = { ...process.env, CARRICK_TOKEN: "t" };
  assert.equal(await refreshFacts(ws.root, { request, env }), "written");
  assert.equal(calls[1]!.auth, null, "the staged URL is fetched without the credential");
  assert.deepEqual(await linesFor(ws, "orders-api/src/events/publish.ts", 'await bus.publish("orders.created"'), [
    "PUBSUB orders.created is served at billing-worker/src/subscribers/created.ts:7",
  ]);
  assert.equal(await refreshFacts(ws.root, { request, env }), "unchanged");
  assert.equal(calls.length, 3);
});

test("no fact file: post-edit prints what it printed before fact files", async (t) => {
  const ws = await factsWorkspace(t, { ndjson: null });
  const run = await edit(ws, ws.at("orders-api/src/routes/orders.ts"), 'const order = await db.orders.find(req.params["id"]);', {
    probe: "check-counterpart.json",
  });
  const snapshot = fs
    .readFileSync(path.join(testDir, "fixtures", "facts-no-file.snapshot.txt"), "utf8")
    .replaceAll("<root>", ws.root);
  assert.equal(context(run.stdout), snapshot.trimEnd());
});

test("with a fact file, a problem line from the probe still prints, beside the op line", async (t) => {
  const ws = await factsWorkspace(t);
  const run = await edit(ws, ws.at("orders-api/src/routes/orders.ts"), 'const order = await db.orders.find(req.params["id"]);', {
    probe: "check-counterpart.json",
  });
  const text = context(run.stdout);
  assert.match(text, /contract problem/);
  assert.match(text, /type_mismatch/);
  assert.match(text, /\n\nGET \/orders\/:id is called at web-app\/src\/api\/orders\.ts:8/);
});

test("the timing record carries facts_ms", async (t) => {
  const ws = await factsWorkspace(t);
  await edit(ws, ws.at("orders-api/src/routes/orders.ts"), 'const order = await db.orders.find(req.params["id"]);');
  const records = readTimings(ws.root).filter((record) => record.hook === "post-edit");
  assert.equal(records.length, 1);
  assert.equal(typeof records[0]!.facts_ms, "number");
});

/** The spans `scanSource` finds, as `name: text`; null when the file did not read. */
function spansOf(src: string, jsx = false): string[] | null {
  const scan = scanSource(src, jsx);
  return scan.ok ? scan.spans.map((span) => `${span.name}: ${src.slice(span.start, span.end)}`) : null;
}

test("generic functions, methods and arrows get their own spans", () => {
  assert.deepEqual(spansOf('function f<T>(a: T) { api.get("/f") }'), ['f: function f<T>(a: T) { api.get("/f") }']);
  assert.deepEqual(spansOf('class A { async find<T>(id: string) { return api.get("/a") } }'), [
    'A.find: async find<T>(id: string) { return api.get("/a") }',
  ]);
  assert.deepEqual(spansOf('class B extends Base<{ id: string }> { find() { return api.get("/b") } }'), [
    'B.find: find() { return api.get("/b") }',
  ]);
  assert.deepEqual(spansOf('const o = { order<T>() { api.get("/o") } }'), ['o.order: order<T>() { api.get("/o") }']);
  assert.deepEqual(spansOf('const f = <T>(a: T): T => { api.get("/g"); return a }'), [
    'f: const f = <T>(a: T): T => { api.get("/g"); return a }',
  ]);
  assert.deepEqual(spansOf('const g = async <T extends object>(a: T) => api.get("/g")'), [
    'g: const g = async <T extends object>(a: T) => api.get("/g")',
  ]);
  // `<T,>` in a component file is a type parameter, not an element.
  assert.deepEqual(spansOf("const pick = <T,>(a: T) => a;\nfunction load() {\n  return <div>{1}</div>;\n}\n", true), [
    "pick: const pick = <T,>(a: T) => a",
    "load: function load() {\n  return <div>{1}</div>;\n}",
  ]);
  // Overloads, declarations and function types still read.
  assert.notEqual(spansOf("declare function d(a: string): void;\nexport function o(a: string): void;\nexport function o(a: any) { return a }\ntype F = <T>(a: T) => T;\n"), null);
});

test("a declaration the scanner cannot place makes the file unread, never its outer function", () => {
  assert.equal(spansOf("function outer() {\n  function inner<T(a) { api.get('/x') }\n}\n"), null);
  assert.equal(spansOf("class A { find<T(a) { return 1 } }"), null);
});

test("an edit inside a generic arrow nested in a function is the arrow's, not the outer function's", async (t) => {
  const ndjson = [
    header(["orders-api"]),
    row({ side: "consumer", protocol: "http", label: "POST /audit", repo: "orders-api", file: "src/a.ts", line: 2, name: "outer", text: "outer line" }),
  ].join("\n");
  const ws = await factsWorkspace(t, { ndjson: `${ndjson}\n` });
  fs.writeFileSync(
    ws.at("orders-api/src/a.ts"),
    'export function outer() {\n  api.post("/audit");\n  const inner = <T>(x: T) => {\n    return cache.set(x);\n  };\n}\n',
  );
  assert.deepEqual(await linesFor(ws, "orders-api/src/a.ts", "return cache.set(x);"), []);
  assert.deepEqual(await linesFor(ws, "orders-api/src/a.ts", 'api.post("/audit");'), ["outer line"]);
});

/** A linked worktree of `repo` at `<repo>/.claude/worktrees/<branch>`, holding a copy of the repo's files. */
function worktree(ws: FactsWorkspace, repo: string, branch: string): string {
  const main = ws.at(repo);
  if (!fs.existsSync(path.join(main, ".git"))) fs.mkdirSync(path.join(main, ".git", "worktrees"), { recursive: true });
  const top = path.join(main, ".claude", "worktrees", branch);
  fs.mkdirSync(top, { recursive: true });
  fs.cpSync(path.join(main, "src"), path.join(top, "src"), { recursive: true });
  const gitdir = path.join(main, ".git", "worktrees", branch);
  fs.mkdirSync(gitdir, { recursive: true });
  fs.writeFileSync(path.join(gitdir, "commondir"), "../..\n");
  fs.writeFileSync(path.join(top, ".git"), `gitdir: ${gitdir}\n`);
  return top;
}

test("an edit in a worktree path maps to its repo's rows", async (t) => {
  const ws = await factsWorkspace(t);
  const top = worktree(ws, "orders-api", "my-branch");
  const run = await edit(ws, path.join(top, "src", "routes", "orders.ts"), "res.status(204).end();", { cwd: ws.at("orders-api") });
  assert.equal(context(run.stdout), "POST /orders/:id/cancel is called at web-app/src/api/order-client.ts:17");
});

test("a file another session wrote in another worktree prints nothing here", async (t) => {
  const ws = await factsWorkspace(t);
  fs.mkdirSync(ws.at("web-app/.git"), { recursive: true });
  const top = worktree(ws, "orders-api", "their-branch");
  const shell = (file: string, cwd: string) =>
    runHook("post-bash.ts", {
      payload: {
        hook_event_name: "PostToolUse",
        tool_name: "Bash",
        session_id: `bash-${path.basename(cwd)}`,
        cwd,
        tool_input: { command: `sed -i 's/204/205/' ${file}` },
        tool_response: { stdout: "" },
      },
      env: fakeEnv({ HOME: ws.home, CLAUDE_PROJECT_DIR: ws.root, CARRICK_FAKE_FIXTURE: fixturePath("check-silent.json") }),
    });
  // Written just now in another session's worktree, named by this session's command.
  const theirs = path.join(top, "src", "events", "publish.ts");
  fs.utimesSync(theirs, new Date(), new Date());
  assert.equal(context((await shell(theirs, ws.at("web-app"))).stdout), "");
  // The same write in the session's own checkout: a shell write names no
  // changed lines, so it prints nothing either.
  const own = ws.at("orders-api/src/events/publish.ts");
  fs.utimesSync(own, new Date(), new Date());
  assert.equal(context((await shell(own, ws.at("orders-api"))).stdout), "");
});
