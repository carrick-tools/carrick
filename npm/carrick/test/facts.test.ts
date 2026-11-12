// Edit-time facts: the fact file built from the wire fixture, the declaration
// scanner, and the lines the post-edit and shell hooks print from it.

import test from "node:test";
import assert from "node:assert/strict";
import crypto from "node:crypto";
import fs from "node:fs";
import os from "node:os";
import path from "node:path";
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
  const ws = await factsWorkspace(t);
  assert.deepEqual(await linesFor(ws, "billing-worker/src/sync.ts", 'const order = await client.get("/orders/:id"'), [
    "GET /orders/:id is served at orders-api/src/routes/orders.ts:12",
  ]);
  // A changed line outside that call reaches no row.
  assert.deepEqual(await linesFor(ws, "billing-worker/src/sync.ts", "console.log(order.status);"), []);
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
  // Once its file no longer declares it, the thing moved here.
  fs.writeFileSync(ws.at("orders-api/src/c.ts"), "export {};\n");
  assert.deepEqual(await linesFor(ws, "orders-api/src/d.ts", "return 5;"), ["GET /c line"]);
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
  // The site literal is written in two calls of the file: nothing.
  assert.deepEqual(await linesFor(ws, "orders-api/src/mounted.ts", "res.json(await notes(req.params.id, 1));"), []);
  // No site: the label's last segment as a string literal in the call the edit reached.
  assert.deepEqual(await linesFor(ws, "orders-api/src/mounted.ts", "relay(payload);"), ["typing line"]);
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
  fs.writeFileSync(path.join(ws.root, ".carrick", "hosted", "snapshot.json"), JSON.stringify({ resolution: { repos: [{ project_slug: "demo" }] } }));
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
      return new Response(JSON.stringify({ staged_url: "https://s3.example/x", digest: "d", schema: 1, writer: 1, bytes: 1 }));
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
  // The same write in the session's own checkout speaks.
  const own = ws.at("orders-api/src/events/publish.ts");
  fs.utimesSync(own, new Date(), new Date());
  assert.equal(
    context((await shell(own, ws.at("orders-api"))).stdout),
    "PUBSUB orders.created is served at billing-worker/src/subscribers/created.ts:7",
  );
});
