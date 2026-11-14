// Edit-time facts: the fact file built from the wire fixture, the declaration
// scanner, and the lines the post-edit and shell hooks print from it.

import test from "node:test";
import assert from "node:assert/strict";
import crypto from "node:crypto";
import fs from "node:fs";
import os from "node:os";
import path from "node:path";
import { spawn, spawnSync } from "node:child_process";
import { drainRunner, fakeEnv, fixturePath, pluginDir, runHook, testDir, waitForSessionAnswers } from "./helpers.ts";
import { commitAll, commitRepos } from "./facts-repos.ts";
import { buildFacts, factsPath, loadSqlite, SCHEMA } from "../src/facts/store.ts";
import { factLines } from "../src/facts/lines.ts";
import { factsEnabled } from "../src/facts/enabled.ts";
import { scanSource } from "../src/facts/match.ts";
import { FACTS_MARKER, refreshFacts, startFactsRefresh } from "../src/facts/fetch.ts";
import { readTimings } from "../src/hook/timings.ts";
import { newestPerFile, queueDir } from "../src/hook/rechecks.ts";

const WIRE = fixturePath("edit-facts.s1.ndjson");
/** The pinned fixture's line for `getOrder`, the producer of `GET /orders/:id`. */
const GET_ORDER_LINE =
  "GET /orders/:id is called at billing-worker/src/sync/orders.ts:18, gateway/src/resolvers.ts:7, web-app/src/admin/orders.ts:31 (+2 more)";
const REPOS = ["orders-api", "web-app", "billing-worker", "gateway", "billing-api"];

type FactsWorkspace = { root: string; home: string; at: (rel: string) => string };

/** The fixture repos copied into a fresh workspace, with the fact file built from `ndjson`. */
async function factsWorkspace(
  t: { after: (fn: () => void) => void },
  options: { ndjson?: string | null; fetchedAt?: string } = {},
): Promise<FactsWorkspace> {
  const root = fs.realpathSync(fs.mkdtempSync(path.join(os.tmpdir(), "carrick-facts-")));
  const home = fs.realpathSync(fs.mkdtempSync(path.join(os.tmpdir(), "carrick-facts-home-")));
  t.after(async () => {
    // An edit with a fact file queues its check: no runner outlives the test.
    await drainRunner(root);
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
    // Each fixture repo committed, and the header naming its commit. A repo
    // the fixture has no folder for is not checked out here, so not mapped.
    const local = REPOS.filter((repo) => fs.existsSync(path.join(root, repo)));
    await buildFacts(
      commitRepos(root, ndjson, local),
      factsPath(root),
      local.map((repo) => ({ repo, path: path.join(root, repo) })),
      options.fetchedAt ?? new Date().toISOString(),
    );
  }
  return { root, home, at: (rel) => path.join(root, rel) };
}

/** Run the post-edit hook for an Edit of `file` that put `newString` in. */
function edit(
  ws: FactsWorkspace,
  file: string,
  newString: string,
  options: { session?: string; probe?: string; cwd?: string; env?: Record<string, string>; agent?: string } = {},
) {
  return runHook("post-edit.ts", {
    payload: {
      hook_event_name: "PostToolUse",
      tool_name: "Edit",
      session_id: options.session ?? "facts-session",
      ...(options.agent ? { agent_id: options.agent } : {}),
      cwd: options.cwd ?? ws.root,
      tool_input: { file_path: file, old_string: "x", new_string: newString },
    },
    env: fakeEnv({
      HOME: ws.home,
      CLAUDE_PROJECT_DIR: ws.root,
      CARRICK_FAKE_FIXTURE: fixturePath(options.probe ?? "check-silent.json"),
      ...options.env,
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
  // A peer in the row's own repo, a committed fixture checkout (`commitRepos`
  // writes the file): ingest skips an op row with `others: []`.
  JSON.stringify({ k: "op", service: fields["repo"], others: [{ service: fields["repo"], repo: fields["repo"], file: "src/peer.ts", line: 1 }], total: 0, source: "handler", ...fields });

test("the wire fixture is the pinned one", () => {
  const pinned = fs.readFileSync(path.join(testDir, "facts-fixture.sha256"), "utf8").split(/\s+/)[0];
  const actual = crypto.createHash("sha256").update(fs.readFileSync(WIRE)).digest("hex");
  assert.equal(actual, pinned);
});

test("every row of the pinned fixture is ingested", async (t) => {
  const ws = await factsWorkspace(t);
  const wire = fs.readFileSync(WIRE, "utf8").split("\n").filter(Boolean).slice(1).map((line) => (JSON.parse(line) as { k: string }).k);
  const tables: Record<string, string> = { op: "rows", fn: "fns", name: "names", pkg: "pkgs", chain: "chains" };
  const sqlite = (await loadSqlite())!;
  const db = new sqlite.DatabaseSync(factsPath(ws.root), { readOnly: true });
  for (const [kind, table] of Object.entries(tables)) {
    const stored = Number(db.prepare(`SELECT COUNT(*) AS n FROM ${table}`).get()!["n"]);
    assert.equal(stored, wire.filter((k) => k === kind).length, kind);
  }
  db.close();
});

// The pinned fixture's fn, name and pkg rows (writer 2), on the workspace's own files.

test("the pinned fixture's fn row: an edit in formatPrice names its callers", async (t) => {
  const ws = await factsWorkspace(t);
  assert.deepEqual(await linesFor(ws, "web-app/src/lib/format.ts", "const whole = Math.floor(cents / 100);"), [
    "formatPrice is called from web-app/src/cart/total.ts:4, web-app/src/checkout/summary.ts:14",
  ]);
});

test("the pinned fixture's name rows: a new file declaring slugify names the first other declaration", async (t) => {
  const ws = await factsWorkspace(t);
  const file = ws.at("gateway/src/slug.ts");
  fs.writeFileSync(file, "export function slugify(text: string): string {\n  return text;\n}\n");
  assert.deepEqual((await factLines(ws.root, [{ file, whole: true }], undefined)).lines, [
    "a function named slugify is declared at orders-api/src/util/slug.ts:1",
  ]);
});

test("the pinned fixture's pkg row: a service bumping its own spec is told only the others'", async (t) => {
  const ws = await factsWorkspace(t);
  const file = ws.at("orders-api/package.json");
  const before = fs.readFileSync(file, "utf8").split("\n");
  const at = before.findIndex((line) => line.includes("@demo/http-kit")) + 1;
  fs.writeFileSync(file, before.map((line, i) => (i === at - 1 ? line.replace("^4.18.2", "^4.20.0") : line)).join("\n"));
  const lines = (await factLines(ws.root, [{ file, lines: [at], removed: [at], removedText: [before[at - 1]!] }], undefined)).lines;
  assert.deepEqual(lines, ["@demo/http-kit is ^4.19.0 in web-app"]);
});

// Naming rule 3 first: the dotted path of keys from a const-bound object.
test("naming rule 3: a resolver in an object map is found by its dotted path", async (t) => {
  const ws = await factsWorkspace(t);
  // A method-shorthand resolver that both resolves a query and calls a route: both its rows.
  assert.deepEqual(await linesFor(ws, "gateway/src/resolvers.ts", "return http.get(`${ORDERS_URL}/orders/${args.id}`);"), [
    "GET /orders/:id is served at orders-api/src/routes/orders.ts:12",
    "QUERY order is requested at web-app/src/graphql/order.ts:4",
  ]);
});

test("naming rule 1: a declared function and a const-bound arrow are found by name", async (t) => {
  const ws = await factsWorkspace(t);
  assert.deepEqual(await linesFor(ws, "orders-api/src/routes/orders.ts", "res.status(404).json({ error: \"not found\" });"), [GET_ORDER_LINE]);
  assert.deepEqual(await linesFor(ws, "web-app/src/api/orders.ts", "return response.data as Order;"), [
    "GET /orders/:id is served at orders-api/src/routes/orders.ts:12",
  ]);
});

test("naming rule 2: a class method and a class field holding an arrow are Class.member", async (t) => {
  const ws = await factsWorkspace(t);
  // The producer sits in billing-api, which this workspace does not check out:
  // silent (R40.2). The method is still named Class.member.
  assert.deepEqual(await linesFor(ws, "web-app/src/billing/client.ts", "await this.http.post("), []);
  const billing = scanSource(fs.readFileSync(ws.at("web-app/src/billing/client.ts"), "utf8"));
  assert.ok(billing.spans.some((span) => span.name === "BillingClient.refund"));
  assert.deepEqual(await linesFor(ws, "web-app/src/chat/client.ts", 'this.socket.emit("chat:send", { text });'), [
    "CLIENT->SERVER chat:send is handled at gateway/src/chat.ts:4",
  ]);
});

test("the pinned fixture's inline handlers are found by the site written in their call", async (t) => {
  const ws = await factsWorkspace(t);
  // An HTTP handler written inline: `router.post("/", ...)` against `POST /orders`, site "/".
  assert.deepEqual(await linesFor(ws, "orders-api/src/routes/orders.ts", "res.status(201).json(created);"), ["POST /orders is called at web-app/src/boot.ts:4"]);
  // A socket handler inside the connection handler. The emit inside it is an
  // unnamed consumer row, silent until the row says the callee's name (R24.3).
  assert.deepEqual(await linesFor(ws, "gateway/src/chat.ts", 'io.emit("chat:message", payload);'), [
    "CLIENT->SERVER chat:send is emitted at web-app/src/chat/client.ts:15",
  ]);
  // A pub/sub subscriber written inline.
  assert.deepEqual(await linesFor(ws, "billing-worker/src/subscribers.ts", "await mirror(message.orderId);"), [
    "PUBSUB orders.created is published at orders-api/src/events/publish.ts:9",
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

test("R24.1: several rows of one kind under one name print only by their own reached site", async (t) => {
  const handlers = [1, 2, 3, 4, 5].map((n) =>
    row({ side: "producer", protocol: "http", label: `GET /r${n}`, repo: "orders-api", file: "src/register.ts", line: n + 1, name: "registerRoutes", site: `/r${n}`, text: `r${n} line` }),
  );
  const ndjson = [
    header(["orders-api"]),
    ...handlers,
    // A consumer call inside the /r1 handler, also named by the encloser.
    row({ side: "consumer", protocol: "http", label: "POST /audit", repo: "orders-api", file: "src/register.ts", line: 3, name: "registerRoutes", site: "/audit", text: "audit line" }),
    row({ side: "consumer", protocol: "http", label: "GET /orders/:id", repo: "orders-api", file: "src/api.ts", line: 3, name: "createApi", site: "/orders/1", text: "get line" }),
    row({ side: "consumer", protocol: "http", label: "POST /orders", repo: "orders-api", file: "src/api.ts", line: 7, name: "createApi", site: "/orders", text: "post line" }),
  ].join("\n");
  const ws = await factsWorkspace(t, { ndjson: `${ndjson}\n` });
  fs.writeFileSync(
    ws.at("orders-api/src/register.ts"),
    [
      "export function registerRoutes(app) {",
      "  app.get('/r1', (req, res) => {",
      "    http.post('/audit', req.body);",
      "    res.json(1);",
      "  });",
      ...[2, 3, 4, 5].flatMap((n) => [`  app.get('/r${n}', (req, res) => {`, `    res.json(${n});`, "  });"]),
      "  log('routes');",
      "}",
      "",
    ].join("\n"),
  );
  assert.deepEqual(await linesFor(ws, "orders-api/src/register.ts", "    res.json(5);"), ["r5 line"]);
  assert.deepEqual(await linesFor(ws, "orders-api/src/register.ts", "    http.post('/audit', req.body);"), ["r1 line", "audit line"]);
  assert.deepEqual(await linesFor(ws, "orders-api/src/register.ts", "  log('routes');"), []);
  fs.writeFileSync(
    ws.at("orders-api/src/api.ts"),
    [
      "export function createApi(http) {",
      "  return {",
      "    getOrder() {",
      "      trace('get');",
      "      return http.get('/orders/1');",
      "    },",
      "    createOrder(body) {",
      "      return http.post('/orders', body);",
      "    },",
      "  };",
      "}",
      "",
    ].join("\n"),
  );
  assert.deepEqual(await linesFor(ws, "orders-api/src/api.ts", "      trace('get');"), []);
  assert.deepEqual(await linesFor(ws, "orders-api/src/api.ts", "      return http.post('/orders', body);"), ["post line"]);
});

test("R24.3: an unnamed listener row needs a function in its call; an unnamed emit row is silent", async (t) => {
  const ndjson = [
    header(["gateway"]),
    row({ side: "producer", protocol: "socket", label: "CLIENT->SERVER chat:send", repo: "gateway", file: "src/hub.ts", line: 2, name: null, site: "chat:send", text: "send line" }),
    row({ side: "consumer", protocol: "socket", label: "SERVER->CLIENT chat:sent", repo: "gateway", file: "src/hub.ts", line: 4, name: null, site: "chat:sent", text: "sent line" }),
  ].join("\n");
  const ws = await factsWorkspace(t, { ndjson: `${ndjson}\n` });
  fs.writeFileSync(
    ws.at("gateway/src/hub.ts"),
    ['io.on(EV.SEND, (m) => {', "  relay(m);", "});", 'io.emit("chat:sent", 1);', 'metrics.count("chat:send");', ""].join("\n"),
  );
  assert.deepEqual(await linesFor(ws, "gateway/src/hub.ts", 'metrics.count("chat:send");'), []);
  assert.deepEqual(await linesFor(ws, "gateway/src/hub.ts", 'io.emit("chat:sent", 1);'), []);
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
  // Nor is its file being gone: a named row prints only in its own file.
  fs.rmSync(ws.at("orders-api/src/c.ts"));
  assert.deepEqual(await linesFor(ws, "orders-api/src/d.ts", "return 5;"), []);
});

test("R24.6: a new function whose only row sits in a file gone from the checkout prints nothing", async (t) => {
  const ndjson = [
    header(["web-app"]),
    row({ side: "consumer", protocol: "http", label: "GET /orders/:id", repo: "web-app", file: "src/old/orders.ts", line: 3, name: "fetchOrder", text: "fetchOrder line" }),
  ].join("\n");
  const ws = await factsWorkspace(t, { ndjson: `${ndjson}\n` });
  fs.writeFileSync(ws.at("web-app/src/fresh.ts"), "export async function fetchOrder(id: string) {\n  return cache.read(id);\n}\n");
  assert.deepEqual(await linesFor(ws, "web-app/src/fresh.ts", "return cache.read(id);"), []);
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
  fs.writeFileSync(file, fs.readFileSync(file, "utf8").replace("res.status(201).json(created);", "res.status(201).json(created);\n  return;"));
  assert.deepEqual(await linesFor(ws, "orders-api/src/routes/orders.ts", "  return;"), []);
});

test("an Edit's patch hunks place the change by line, deletions included", async (t) => {
  const ws = await factsWorkspace(t);
  const file = ws.at("orders-api/src/routes/orders.ts");
  const lineOf = (needle: string) => fs.readFileSync(file, "utf8").split("\n").findIndex((line) => line.includes(needle)) + 1;
  const at = lineOf("res.status(201).json(created);");
  const text = fs.readFileSync(file, "utf8").split("\n");
  // A deletion just above the line: the new text is empty, so only the hunk
  // places it. Its context lines are the file's lines around the gap.
  const hunk = { oldStart: at - 1, oldLines: 3, newStart: at - 1, newLines: 2, lines: [` ${text[at - 2]}`, "-  // gone", ` ${text[at - 1]}`] };
  const removal = (session: string) =>
    runHook("post-edit.ts", {
      payload: {
        hook_event_name: "PostToolUse",
        tool_name: "Edit",
        session_id: session,
        cwd: ws.root,
        tool_input: { file_path: file, old_string: "  // gone\n", new_string: "" },
        tool_response: { structuredPatch: [hunk] },
      },
      env: fakeEnv({ HOME: ws.home, CLAUDE_PROJECT_DIR: ws.root, CARRICK_FAKE_FIXTURE: fixturePath("check-silent.json") }),
    });
  assert.equal(context((await removal("patch")).stdout), "POST /orders is called at web-app/src/boot.ts:4");
});

test("a removal-only hunk that went stale is unplaced", async (t) => {
  const ws = await factsWorkspace(t);
  const file = ws.at("orders-api/src/routes/orders.ts");
  const text = fs.readFileSync(file, "utf8").split("\n");
  const at = text.findIndex((line) => line.includes("res.status(201).json(created);")) + 1;
  const hunk = { oldStart: at - 1, oldLines: 3, newStart: at - 1, newLines: 2, lines: [` ${text[at - 2]}`, "-  // gone", ` ${text[at - 1]}`] };
  // Ten lines were put on top after the Edit: the hunk's gap now sits inside getOrder.
  fs.writeFileSync(file, `${"// added\n".repeat(10)}${text.join("\n")}`);
  const run = await runHook("post-edit.ts", {
    payload: {
      hook_event_name: "PostToolUse",
      tool_name: "Edit",
      session_id: "stale",
      cwd: ws.root,
      tool_input: { file_path: file, old_string: "  // gone\n", new_string: "" },
      tool_response: { structuredPatch: [hunk] },
    },
    env: fakeEnv({ HOME: ws.home, CLAUDE_PROJECT_DIR: ws.root, CARRICK_FAKE_FIXTURE: fixturePath("check-silent.json") }),
  });
  assert.equal(context(run.stdout), "");
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

test("a replaced header line of a nested function is that function's, not its encloser's", async (t) => {
  const ndjson = [
    header(["orders-api"]),
    row({ side: "consumer", protocol: "http", label: "POST /audit", repo: "orders-api", file: "src/a.ts", line: 2, name: "outer", text: "outer line" }),
  ].join("\n");
  const ws = await factsWorkspace(t, { ndjson: `${ndjson}\n` });
  const file = ws.at("orders-api/src/a.ts");
  fs.writeFileSync(file, 'export function outer() {\n  api.post("/audit");\n  const inner = <T>(x: T) => {\n    return cache.set(x);\n  };\n}\n');
  const run = await patchEdit(ws, file, [{ newStart: 2, lines: ['   api.post("/audit");', "-  const inner = (x) => {", "+  const inner = <T>(x: T) => {", "     return cache.set(x);"] }]);
  assert.equal(context(run.stdout), "");
});

test("R24.5: hunks whose added lines are not the file's lines at those numbers are unplaced", async (t) => {
  const ndjson = [
    header(["orders-api"]),
    row({ side: "consumer", protocol: "http", label: "POST /a", repo: "orders-api", file: "src/ab.ts", line: 5, name: "a", text: "a line" }),
    row({ side: "consumer", protocol: "http", label: "POST /b", repo: "orders-api", file: "src/ab.ts", line: 8, name: "b", text: "b line" }),
  ].join("\n");
  const ws = await factsWorkspace(t, { ndjson: `${ndjson}\n` });
  const file = ws.at("orders-api/src/ab.ts");
  // The tool added `trace("b")` at line 5, inside b; three header lines were added above it afterwards.
  fs.writeFileSync(file, '// one\n// two\n// three\nfunction a() {\n  return api.post("/a");\n}\nfunction b() {\n  trace("b");\n  return api.post("/b");\n}\n');
  const stale = await patchEdit(ws, file, [{ newStart: 4, lines: [" function b() {", '+  trace("b");', '   return api.post("/b");'] }]);
  assert.equal(context(stale.stdout), "");
  // The same hunk at its true numbers places the line in b.
  const placed = await patchEdit(ws, file, [{ newStart: 7, lines: [" function b() {", '+  trace("b");', '   return api.post("/b");'] }], "placed");
  assert.equal(context(placed.stdout), "b line");
});

test("A-5: an Edit with no hunks reads only the lines its old and new text do not share", async (t) => {
  const ndjson = [
    header(["orders-api"]),
    row({ side: "consumer", protocol: "http", label: "POST /a", repo: "orders-api", file: "src/ab.ts", line: 3, name: "a", text: "a line" }),
    row({ side: "consumer", protocol: "http", label: "POST /b", repo: "orders-api", file: "src/ab.ts", line: 7, name: "b", text: "b line" }),
  ].join("\n");
  const ws = await factsWorkspace(t, { ndjson: `${ndjson}\n` });
  const file = ws.at("orders-api/src/ab.ts");
  const after = 'function a() {\n  trace("a", 2);\n  return api.post("/a");\n}\nfunction b() {\n  return api.post("/b");\n}\n';
  fs.writeFileSync(file, after);
  const run = (oldString: string, newString: string, session: string) =>
    payloadEdit(ws, { tool_name: "Edit", tool_input: { file_path: file, old_string: oldString, new_string: newString } }, session);
  const before = after.replace('trace("a", 2)', 'trace("a", 1)');
  assert.equal(context((await run(before, after, "middle")).stdout), "a line");
  // Nothing differs: nothing is said.
  assert.equal(context((await run(after, after, "same")).stdout), "");
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
  commitAll(ws.at(repo));
}

test("a Write over an existing file, whose changed lines are unknown, prints nothing", async (t) => {
  const ws = await factsWorkspace(t);
  const file = ws.at("orders-api/src/routes/orders.ts");
  const run = await payloadEdit(ws, { tool_name: "Write", tool_input: { file_path: file, content: fs.readFileSync(file, "utf8") }, tool_response: { type: "update" } });
  assert.equal(context(run.stdout), "");
  // A Write's result also carries hunks (`structuredPatch`); a Write over a file still says nothing.
  const at = fs.readFileSync(file, "utf8").split("\n").findIndex((line) => line.includes("res.status(404).json(")) + 1;
  const withHunks = await payloadEdit(
    ws,
    {
      tool_name: "Write",
      tool_input: { file_path: file, content: fs.readFileSync(file, "utf8") },
      tool_response: {
        type: "update",
        filePath: file,
        structuredPatch: [{ oldStart: at, oldLines: 1, newStart: at, newLines: 1, lines: ['-    res.status(400).json({ error: "not found" });', '+    res.status(404).json({ error: "not found" });'] }],
      },
    },
    "write-hunks",
  );
  assert.equal(context(withHunks.stdout), "");
});

test("a file this Write created, untracked in git, counts every named thing in it", async (t) => {
  const ws = await factsWorkspace(t);
  gitRepo(ws, "orders-api");
  const file = ws.at("orders-api/src/routes/fresh.ts");
  const text = fs.readFileSync(ws.at("orders-api/src/routes/orders.ts"), "utf8");
  fs.writeFileSync(file, text);
  const payload = { tool_name: "Write", tool_input: { file_path: file, content: text }, tool_response: { type: "create", filePath: file } };
  // Its op rows are another file's, still on disk: none print, by the named-row
  // rule. The name kind's line for getOrder would state orders.ts:12, but this
  // fixture copy declares getOrder at line 11: unproven on disk, so silent (R33.3).
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
  fs.writeFileSync(file, fs.readFileSync(file, "utf8").replace('    res.status(404).json({ error: "not found" });', '    res.status(410).json({ error: "not found" });'));
  const placed = await payloadEdit(ws, {
    tool_name: "apply_patch",
    tool_input: { command: patch(["@@ export async function getOrder", "   if (!order) {", '-    res.status(404).json({ error: "not found" });', '+    res.status(410).json({ error: "not found" });'] ) },
  });
  assert.equal(context(placed.stdout), GET_ORDER_LINE);
  // The hunk's new text is nowhere in the file, and the file reads as HEAD
  // (the diff fallback has nothing either): nothing.
  gitRepo(ws, "orders-api");
  const lost = await payloadEdit(ws, { tool_name: "apply_patch", tool_input: { command: patch(["@@", "-  res.status(201).json(created);", "+  res.status(299).json(created);"]) } }, "lost");
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
  await linesFor(ws, "orders-api/src/routes/orders.ts", "res.status(201).json(created);");
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
  assert.deepEqual(matchRepos(["api"], local), [{ repo: "api", path: "/w/api" }]);
  assert.deepEqual(matchRepos(["api-server"], local), []);
  // An exact owner/name remote, in either spelling git accepts.
  assert.deepEqual(matchRepos(["acme/web", "acme/api-server"], local), [
    { repo: "acme/web", path: "/w/web" },
    { repo: "acme/api-server", path: "/w/api" },
  ]);
  // An owner/name that matches no checkout's origin is not rescued by the name.
  assert.deepEqual(matchRepos(["other/web"], local), []);
  // Two checkouts of one name: ambiguous, not read.
  assert.deepEqual(matchRepos(["jobs"], local), []);
});

test("R24.7: two header repos that map to one checkout are neither mapped", async () => {
  const { matchRepos } = await import("../src/facts/fetch.ts");
  const { locate } = await import("../src/facts/lines.ts");
  const local = [{ name: "api", path: "/w/api", remote: "https://github.com/zeta/api" }];
  assert.deepEqual(matchRepos(["api", "zeta/api"], local), []);
  assert.deepEqual(matchRepos(["zeta/api"], local), [{ repo: "zeta/api", path: "/w/api" }]);
  // A fact file built before this rule may still hold both: the edit is placed in neither.
  const both = [{ repo: "api", path: "/w/api" }, { repo: "zeta/api", path: "/w/api" }];
  assert.equal(locate("/w/api/src/a.ts", both), null);
  assert.deepEqual(locate("/w/api/src/a.ts", [both[1]!]), { repo: "zeta/api", rel: "src/a.ts", top: "/w/api" });
});

test("a fact file for another project is not read", async (t) => {
  const ws = await factsWorkspace(t);
  const edited = { file: ws.at("orders-api/src/routes/orders.ts"), snippets: ["res.status(201).json(created);"] };
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
  const { runFactsRefresh, FACTS_BUILDING_RETRY_MS } = await import("../src/facts/fetch.ts");
  const ws = await factsWorkspace(t, { ndjson: null });
  fs.mkdirSync(path.join(ws.root, ".carrick", "hosted"), { recursive: true });
  fs.writeFileSync(path.join(ws.root, ".carrick", "hosted", "snapshot.json"), JSON.stringify({ resolution: { repos: [{ project_slug: "demo-shop" }] } }));
  const env = { ...process.env, CARRICK_TOKEN: "t", CARRICK_NO_FACTS_REFRESH: "" };
  const now = Date.parse("2026-01-01T12:00:00Z");
  const start = () => undefined;
  assert.equal(startFactsRefresh(ws.root, { env, now, start }), true);
  const answer = (code: string, retriable: boolean, status = 200) =>
    (async () => new Response(JSON.stringify({ success: false, error: { code, message: "", retriable } }), { status })) as typeof fetch;
  // Not retriable: no facts, no early retry.
  assert.equal(await runFactsRefresh(ws.root, { request: answer("facts_unavailable", false), env, now }), "none");
  assert.equal(fs.existsSync(factsPath(ws.root)), false);
  assert.equal(startFactsRefresh(ws.root, { env, now: now + FACTS_BUILDING_RETRY_MS + 1_000, start }), false, "no early retry");
  // The same code, retriable (a build in flight, HTTP 409), moves the next ask a minute on: the flag decides, not the code.
  assert.equal(await runFactsRefresh(ws.root, { request: answer("facts_unavailable", true, 409), env, now }), "retry");
  assert.equal(startFactsRefresh(ws.root, { env, now: now + FACTS_BUILDING_RETRY_MS + 1_000, start }), true);
  // And `facts_building` marked not retriable does not.
  const later = now + 3_600_000;
  assert.equal(startFactsRefresh(ws.root, { env, now: later, start }), true);
  assert.equal(await runFactsRefresh(ws.root, { request: answer("facts_building", false), env, now: later }), "none");
  assert.equal(startFactsRefresh(ws.root, { env, now: later + FACTS_BUILDING_RETRY_MS + 1_000, start }), false);
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
  const { allowEarlyRetry, FACTS_BUILDING_RETRY_MS } = await import("../src/facts/fetch.ts");
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

test("factsEnabled is off only for exactly \"0\"", () => {
  assert.equal(factsEnabled({}), true, "unset");
  assert.equal(factsEnabled({ CARRICK_FACT_LINES: "" }), true, "empty");
  assert.equal(factsEnabled({ CARRICK_FACT_LINES: "0" }), false, "0");
  assert.equal(factsEnabled({ CARRICK_FACT_LINES: "1" }), true, "1");
  assert.equal(factsEnabled({ CARRICK_FACT_LINES: "true" }), true, "true");
});

test("with CARRICK_FACT_LINES unset, an edit that matches a row prints the stored fact line", async (t) => {
  const ws = await factsWorkspace(t);
  const env = fakeEnv({ HOME: ws.home, CLAUDE_PROJECT_DIR: ws.root, CARRICK_FAKE_FIXTURE: fixturePath("check-silent.json") });
  delete env["CARRICK_FACT_LINES"];
  const run = await runHook("post-edit.ts", {
    payload: {
      hook_event_name: "PostToolUse",
      tool_name: "Edit",
      session_id: "facts-default",
      cwd: ws.root,
      tool_input: { file_path: ws.at("orders-api/src/routes/orders.ts"), old_string: "x", new_string: 'const order = await db.orders.find(req.params["id"]);' },
    },
    env,
  });
  assert.equal(run.code, 0);
  assert.equal(context(run.stdout), GET_ORDER_LINE);
});

/** Every file under `dir` with its size, keyed by relative path. */
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

test("with CARRICK_FACT_LINES=0 and a fact file, the hook's stdout, exit code and files written equal a run with no fact file", async (t) => {
  const run = async (ndjson: string | null) => {
    const ws = await factsWorkspace(t, { ndjson });
    const env = fakeEnv({ HOME: ws.home, CLAUDE_PROJECT_DIR: ws.root, CARRICK_FAKE_FIXTURE: fixturePath("check-silent.json"), CARRICK_FACT_LINES: "0" });
    const before = [snapshot(ws.root), snapshot(ws.home)];
    const result = await runHook("post-edit.ts", {
      payload: {
        hook_event_name: "PostToolUse",
        tool_name: "Edit",
        session_id: "facts-zero",
        cwd: ws.root,
        tool_input: { file_path: ws.at("orders-api/src/routes/orders.ts"), old_string: "x", new_string: 'const order = await db.orders.find(req.params["id"]);' },
      },
      env,
    });
    const after = [snapshot(ws.root), snapshot(ws.home)];
    // A file written or changed by the hook, by relative path.
    const written = after.flatMap((map, i) => [...map].filter(([rel, hash]) => before[i]!.get(rel) !== hash).map(([rel]) => `${i}:${rel.replace(/\.\d+-\d+-\d+\./, ".<id>.")}`));
    return { code: result.code, stdout: result.stdout, written: written.sort() };
  };
  const withFile = await run(fs.readFileSync(WIRE, "utf8"));
  const without = await run(null);
  assert.equal(withFile.code, 0);
  assert.equal(withFile.stdout, without.stdout);
  assert.equal(context(withFile.stdout), "");
  assert.deepEqual(withFile.written, without.written);
});

test("with CARRICK_FACT_LINES=0, an edit that matches a row prints nothing and starts no refresh", async (t) => {
  const ws = await factsWorkspace(t);
  const env = fakeEnv({ HOME: ws.home, CLAUDE_PROJECT_DIR: ws.root, CARRICK_FAKE_FIXTURE: fixturePath("check-silent.json"), CARRICK_FACT_LINES: "0" });
  const run = await runHook("post-edit.ts", {
    payload: {
      hook_event_name: "PostToolUse",
      tool_name: "Edit",
      session_id: "facts-off",
      cwd: ws.root,
      tool_input: { file_path: ws.at("orders-api/src/routes/orders.ts"), old_string: "x", new_string: 'const order = await db.orders.find(req.params["id"]);' },
    },
    env,
  });
  assert.equal(run.code, 0);
  assert.equal(context(run.stdout), "");
  assert.equal(fs.existsSync(path.join(ws.root, ".carrick", FACTS_MARKER)), false, "no refresh claimed");
  const started: string[] = [];
  const noEnv = { ...env, CARRICK_NO_FACTS_REFRESH: "" };
  assert.equal(startFactsRefresh(ws.root, { env: noEnv, start: () => void started.push("x") }), false);
  assert.deepEqual(started, []);
  const edited = { file: ws.at("orders-api/src/routes/orders.ts"), snippets: ["res.status(201).json(created);"] };
  const off = await factLines(ws.root, [edited], undefined, { env: noEnv });
  assert.deepEqual(off.lines, []);
  assert.equal(off.stale, false);
  assert.equal(await refreshFacts(ws.root, { env: { ...noEnv, CARRICK_TOKEN: "t" }, request: () => Promise.reject(new Error("fetched")) }), "none");
});

test("with CARRICK_FACT_LINES=1 an edit that matches a row prints it", async (t) => {
  const ws = await factsWorkspace(t);
  const run = await edit(ws, ws.at("orders-api/src/routes/orders.ts"), 'const order = await db.orders.find(req.params["id"]);', { session: "facts-on" });
  assert.equal(context(run.stdout), GET_ORDER_LINE);
});

test("an edit inside a handler prints where it is called, once per session", async (t) => {
  const ws = await factsWorkspace(t);
  const file = ws.at("orders-api/src/routes/orders.ts");
  const first = await edit(ws, file, 'const order = await db.orders.find(req.params["id"]);');
  assert.equal(first.code, 0);
  // Some Node 24 releases warn when `node:sqlite` loads; a hook's stderr stays clean.
  assert.doesNotMatch(first.stderr, /ExperimentalWarning/);
  assert.equal(context(first.stdout), GET_ORDER_LINE);
  const second = await edit(ws, file, 'const order = await db.orders.find(req.params["id"]);');
  assert.equal(context(second.stdout), "");
  // Another session hears it again.
  const other = await edit(ws, file, 'const order = await db.orders.find(req.params["id"]);', { session: "another" });
  assert.match(context(other.stdout), /^GET \/orders\/:id is called at /);
});

test("an edit at a consumer call prints where it is served", async (t) => {
  const ws = await factsWorkspace(t);
  const run = await edit(ws, ws.at("web-app/src/chat/client.ts"), 'this.socket.emit("chat:send", { text });');
  assert.equal(context(run.stdout), "CLIENT->SERVER chat:send is handled at gateway/src/chat.ts:4");
});

test("the handler moved 30 lines on the branch: the line still prints, found by name", async (t) => {
  const ws = await factsWorkspace(t);
  const file = ws.at("orders-api/src/routes/orders.ts");
  fs.writeFileSync(file, `${"// moved\n".repeat(30)}${fs.readFileSync(file, "utf8")}`);
  const run = await edit(ws, file, 'const order = await db.orders.find(req.params["id"]);');
  assert.equal(context(run.stdout), GET_ORDER_LINE);
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
  assert.equal(await refreshFacts(ws.root, { request, env: { ...process.env, CARRICK_TOKEN: "t" } }), "retry");
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
    JSON.stringify({ repos: REPOS.filter((repo) => fs.existsSync(ws.at(repo))).map((repo) => ({ name: repo, path: ws.at(repo), services: [] })) }),
  );
  // The fixture repos committed; the served file's header names those commits.
  const wire = commitRepos(ws.root, fs.readFileSync(WIRE, "utf8"), REPOS.filter((repo) => fs.existsSync(ws.at(repo))));
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
    return new Response(gzipSync(wire));
  }) as typeof fetch;
  const env = { ...process.env, CARRICK_TOKEN: "t" };
  assert.equal(await refreshFacts(ws.root, { request, env }), "written");
  assert.equal(calls[1]!.auth, null, "the staged URL is fetched without the credential");
  assert.deepEqual(await linesFor(ws, "orders-api/src/events/publish.ts", 'await bus.publish("orders.created"'), [
    "PUBSUB orders.created is handled at billing-worker/src/subscribers.ts:5",
  ]);
  assert.equal(await refreshFacts(ws.root, { request, env }), "unchanged");
  assert.equal(calls.length, 3);
});

test("no fact file: post-edit prints what it printed before fact files", async (t) => {
  const ws = await factsWorkspace(t, { ndjson: null });
  const argvLog = path.join(ws.home, "argv.log");
  const run = await edit(ws, ws.at("orders-api/src/routes/orders.ts"), 'const order = await db.orders.find(req.params["id"]);', {
    probe: "check-counterpart.json",
    env: { CARRICK_FAKE_ARGV_LOG: argvLog },
  });
  const snapshot = fs
    .readFileSync(path.join(testDir, "fixtures", "facts-no-file.snapshot.txt"), "utf8")
    .replaceAll("<root>", ws.root);
  assert.equal(context(run.stdout), snapshot.trimEnd());
  assert.equal(probeCalls(argvLog), 1);
});

/** The calls the fake binary logged that carry `--no-rescan`: the post-edit probe's. */
function probeCalls(argvLog: string): number {
  if (!fs.existsSync(argvLog)) return 0;
  return fs
    .readFileSync(argvLog, "utf8")
    .split("\n")
    .filter(Boolean)
    .filter((line) => (JSON.parse(line) as { argv: string[] }).argv.includes("--no-rescan")).length;
}

const waitForAnswer = (ws: FactsWorkspace, session: string) => waitForSessionAnswers(ws.home, session);

test("with CARRICK_FACT_LINES=0 and a fact file on disk, an Edit, a Write that creates a file and a shell write probe as before", async (t) => {
  const ws = await factsWorkspace(t);
  const argvLog = path.join(ws.home, "argv.log");
  const env = fakeEnv({
    HOME: ws.home,
    CLAUDE_PROJECT_DIR: ws.root,
    CARRICK_FAKE_FIXTURE: fixturePath("check-counterpart.json"),
    CARRICK_FAKE_ARGV_LOG: argvLog,
    CARRICK_FACT_LINES: "0",
  });
  const hook = (script: string, payload: Record<string, unknown>) =>
    runHook(script, { payload: { hook_event_name: "PostToolUse", session_id: "switch-off", cwd: ws.root, ...payload }, env });
  const said: Record<string, string> = {};
  const probes: number[] = [];
  const counted = () => {
    probes.push(probeCalls(argvLog) - probes.reduce((sum, n) => sum + n, 0));
  };
  const edited = ws.at("orders-api/src/routes/orders.ts");
  said["edit"] = context(
    (await hook("post-edit.ts", { tool_name: "Edit", tool_input: { file_path: edited, old_string: "x", new_string: 'const order = await db.orders.find(req.params["id"]);' } })).stdout,
  );
  counted();
  const created = ws.at("orders-api/src/routes/refunds.ts");
  const body = "export function refund(id: string) {\n  return id;\n}\n";
  fs.writeFileSync(created, body);
  said["write"] = context((await hook("post-edit.ts", { tool_name: "Write", tool_input: { file_path: created, content: body }, tool_response: { type: "create" } })).stdout);
  counted();
  const shelled = ws.at("orders-api/src/events/publish.ts");
  fs.utimesSync(shelled, new Date(), new Date());
  said["shell"] = context((await hook("post-bash.ts", { tool_name: "Bash", tool_input: { command: `sed -i 's/204/205/' ${shelled}` }, tool_response: { stdout: "" } })).stdout);
  counted();
  // Each call's output as this base printed it (the Edit's is the no-fact-file snapshot).
  const snapshot = (name: string) => fs.readFileSync(path.join(testDir, "fixtures", name), "utf8").replaceAll("<root>", ws.root).trimEnd();
  assert.deepEqual(said, {
    edit: snapshot("facts-no-file.snapshot.txt"),
    write: snapshot("facts-switch-off-write.snapshot.txt"),
    shell: snapshot("facts-switch-off-shell.snapshot.txt"),
  });
  assert.deepEqual(probes, [1, 1, 1], "one probe per call");
  const queue = queueDir(ws.root);
  assert.deepEqual(fs.existsSync(queue) ? fs.readdirSync(queue) : [], [], "nothing queued");
  assert.equal(fs.existsSync(path.join(path.dirname(queue), "runner.lock")), false, "no runner started");
});

test("with a fact file, the edit starts no probe", async (t) => {
  const ws = await factsWorkspace(t);
  const argvLog = path.join(ws.home, "argv.log");
  const file = ws.at("orders-api/src/routes/orders.ts");
  const run = await edit(ws, file, 'const order = await db.orders.find(req.params["id"]);', {
    session: "no-probe",
    env: { CARRICK_FAKE_ARGV_LOG: argvLog },
  });
  assert.equal(context(run.stdout), GET_ORDER_LINE);
  assert.equal(probeCalls(argvLog), 0);
  // Queued for the runner instead, and recorded for Stop.
  assert.ok(newestPerFile("no-probe", ws.home).has(file));
});

test("with a fact file and an edit that touches no row, no probe either", async (t) => {
  const ws = await factsWorkspace(t);
  const argvLog = path.join(ws.home, "argv.log");
  const file = ws.at("orders-api/src/routes/orders.ts");
  const run = await edit(ws, file, "text this file does not hold", { session: "no-row", env: { CARRICK_FAKE_ARGV_LOG: argvLog } });
  assert.equal(context(run.stdout), "");
  assert.equal(probeCalls(argvLog), 0);
  assert.ok(newestPerFile("no-row", ws.home).has(file));
});

test("with a fact file, the probe's problem line arrives on the next carrier", async (t) => {
  const ws = await factsWorkspace(t);
  const run = await edit(ws, ws.at("orders-api/src/routes/orders.ts"), 'const order = await db.orders.find(req.params["id"]);', {
    session: "late-problem",
    probe: "check-counterpart.json",
  });
  assert.equal(context(run.stdout), GET_ORDER_LINE);
  await waitForAnswer(ws, "late-problem");
  const next = await edit(ws, ws.at("web-app/src/api/orders.ts"), "text this file does not hold", {
    session: "late-problem",
    probe: "check-counterpart.json",
  });
  const text = context(next.stdout);
  assert.match(text, /contract problem/);
  assert.match(text, /type_mismatch/);
  assert.match(text, /orders-api\/src\/routes\/orders\.ts after your earlier edit to it/);
});

test("a fact-file edit whose runner answer does not answer the edit still delivers its uses line late", async (t) => {
  const ws = await factsWorkspace(t);
  // A file no fact line covers: the uses-count line is its callers' answer.
  const file = ws.at("orders-api/src/routes/orders.ts");
  const first = await edit(ws, file, "text this file does not hold", { session: "late-uses", probe: "check-stale-uses.json" });
  assert.equal(context(first.stdout), "");
  await waitForAnswer(ws, "late-uses");
  const next = await edit(ws, ws.at("web-app/src/api/orders.ts"), "text this file does not hold", {
    session: "late-uses",
    probe: "check-silent.json",
  });
  const expected = fs
    .readFileSync(path.join(testDir, "fixtures", "facts-stale-uses-late.snapshot.txt"), "utf8")
    .replaceAll("<root>", ws.root)
    .trimEnd();
  assert.equal(context(next.stdout), expected);
});

/** Run the Stop hook for `session` in the workspace, as a settings entry does. */
function stop(ws: FactsWorkspace, session: string, env: Record<string, string> = {}): Promise<string> {
  return new Promise((resolve) => {
    const child = spawn(process.execPath, [path.join(pluginDir, "src", "hook", "stop.ts")], {
      env: fakeEnv({ HOME: ws.home, CLAUDE_PROJECT_DIR: ws.root, ...env }),
      stdio: ["pipe", "pipe", "pipe"],
    });
    let stdout = "";
    child.stdout.on("data", (chunk: Buffer) => (stdout += chunk.toString("utf8")));
    child.on("close", () => resolve(stdout));
    child.stdin.end(JSON.stringify({ hook_event_name: "Stop", session_id: session, cwd: ws.root }));
  });
}

const PROBLEM_HEAD = "Carrick re-checked orders-api/src/routes/orders.ts after this edit: 1 contract problem.";

// R46.1: a file the queue did not take is probed in hand and printed as
// before fact files: a queue folder that cannot be made, a session id no file
// name can carry.
test("with a fact file, a file the queue refuses is probed in hand and printed as before", async (t) => {
  const ws = await factsWorkspace(t);
  const file = ws.at("orders-api/src/routes/orders.ts");
  const text = 'const order = await db.orders.find(req.params["id"]);';
  // A plain file where the queue folder goes.
  fs.writeFileSync(path.join(ws.root, ".carrick", "rechecks"), "");
  t.after(() => fs.rmSync(path.join(ws.root, ".carrick", "rechecks"), { force: true }));
  const refused = context((await edit(ws, file, text, { session: "refused", probe: "check-counterpart.json" })).stdout);
  assert.ok(refused.startsWith(`${PROBLEM_HEAD}\n- `), refused);
  assert.ok(refused.endsWith(`\n\n${GET_ORDER_LINE}`), refused);
  // What this Stop said with CARRICK_FACT_LINES unset (review run, S3).
  assert.equal(
    await stop(ws, "refused"),
    JSON.stringify({
      hookSpecificOutput: {
        hookEventName: "Stop",
        additionalContext: "Carrick checked the 1 file this session edited (order-service): 1 contract problem.",
      },
    }),
  );
  fs.rmSync(path.join(ws.root, ".carrick", "rechecks"));
  const unsafe = context((await edit(ws, file, text, { session: "bad:id", probe: "check-counterpart.json" })).stdout);
  assert.ok(unsafe.startsWith(`${PROBLEM_HEAD}\n- `), unsafe);
});

// R46.2 end to end (the review's S4): a deferred answer is named at its first
// Stop as with the switch off, once, and its lead is not lost: Stop leaves the
// answer for the next carrier, which prints the lead.
test("with a fact file, a deferred answer is named at Stop as before, once, and its lead prints on the next carrier", async (t) => {
  const ws = await factsWorkspace(t);
  const file = ws.at("orders-api/src/routes/orders.ts");
  // The `check --changed` the first Stop queues answers late, whichever runner
  // takes it (the edit's or Stop's): a second Stop that finds its clean answer
  // states the clean check itself (carrick#4085), and the next carrier then
  // has nothing to say about it. Held back, the second Stop always runs first.
  const slowCheck = { CARRICK_FAKE_FULL_DELAY_MS: "2000" };
  const run = await edit(ws, file, 'const order = await db.orders.find(req.params["id"]);', {
    session: "lead",
    probe: "check-deferred-lead.json",
    env: slowCheck,
  });
  assert.equal(context(run.stdout), GET_ORDER_LINE);
  await waitForAnswer(ws, "lead");
  // What this Stop said with CARRICK_FACT_LINES unset (review run 2, S4).
  assert.equal(
    await stop(ws, "lead", slowCheck),
    JSON.stringify({
      hookSpecificOutput: {
        hookEventName: "Stop",
        additionalContext:
          "Carrick did not check the whole change: orders-api/src/routes/orders.ts: this edit needs a re-scan, which this call was asked not to start. A background `carrick check --changed` is queued; its answer comes with the next Carrick line.",
      },
    }),
  );
  // The file is named once: this Stop does not name it again. On a slow
  // machine the queued check can answer before this Stop runs in spite of the
  // hold; Stop then states its clean line itself (carrick#4085), and that is
  // the one time it is said.
  const lead = "Carrick: `slugify` looks like `toSlug` at shared-lib/src/text/slug.ts:4: reuse or extend it if it does the same job.";
  const clean = "Carrick checked orders-api/src/routes/orders.ts and found no contract problem.";
  const second = context(await stop(ws, "lead"));
  assert.ok(second === "" || second === clean, `named once: ${second}`);
  // The `check --changed` Stop queued has answered (the fake binary: clean).
  await waitForSessionAnswers(ws.home, "lead", 2);
  const next = await edit(ws, ws.at("web-app/src/api/orders.ts"), "text this file does not hold", { session: "lead", probe: "check-silent.json" });
  // The lead always prints here; the clean line prints here unless the second Stop said it.
  assert.equal(context(next.stdout), second === "" ? `${lead}\n\n${clean}` : lead);
});

// R46.3: a sub-agent's edit keeps the probe; nothing reads its late answers.
test("with a fact file, a sub-agent's edit is probed in hand and queues nothing in place of the probe", async (t) => {
  const ws = await factsWorkspace(t);
  const argvLog = path.join(ws.home, "argv.log");
  const file = ws.at("orders-api/src/routes/orders.ts");
  const run = await edit(ws, file, 'const order = await db.orders.find(req.params["id"]);', {
    session: "parent",
    agent: "sub1",
    probe: "check-counterpart.json",
    env: { CARRICK_FAKE_ARGV_LOG: argvLog },
  });
  const said = context(run.stdout);
  assert.ok(said.startsWith(`${PROBLEM_HEAD}\n- `), said);
  assert.equal(probeCalls(argvLog), 1);
  for (const { entry } of newestPerFile("parent.sub1", ws.home).values()) assert.equal(entry.instead_of_probe, undefined);
});

// R46.3 on the shell path: a sub-agent's shell write is probed in hand too.
test("with a fact file, a sub-agent's shell write is probed in hand and queues nothing in place of the probe", async (t) => {
  const ws = await factsWorkspace(t);
  const argvLog = path.join(ws.home, "argv.log");
  const file = ws.at("orders-api/src/events/publish.ts");
  fs.utimesSync(file, new Date(), new Date());
  const run = await runHook("post-bash.ts", {
    payload: {
      hook_event_name: "PostToolUse",
      tool_name: "Bash",
      session_id: "parent",
      agent_id: "sub1",
      cwd: ws.root,
      tool_input: { command: `sed -i 's/204/205/' ${file}` },
      tool_response: { stdout: "" },
    },
    env: fakeEnv({
      HOME: ws.home,
      CLAUDE_PROJECT_DIR: ws.root,
      CARRICK_FAKE_FIXTURE: fixturePath("check-counterpart.json"),
      CARRICK_FAKE_ARGV_LOG: argvLog,
    }),
  });
  assert.equal(probeCalls(argvLog), 1, `probed in hand: ${run.stdout}`);
  for (const { entry } of newestPerFile("parent.sub1", ws.home).values()) assert.equal(entry.instead_of_probe, undefined);
});

// R46.5: an answer the binary refused (no index) states nothing at Stop, as
// the probe's refusal did: no "did not check", no `check --changed`.
test("with a fact file, an error answer queued in place of the probe is not stated at Stop", async (t) => {
  const ws = await factsWorkspace(t);
  const file = ws.at("orders-api/src/routes/orders.ts");
  const run = await edit(ws, file, 'const order = await db.orders.find(req.params["id"]);', { session: "err", probe: "check-not-indexed.json" });
  assert.equal(context(run.stdout), GET_ORDER_LINE);
  await waitForAnswer(ws, "err");
  assert.equal(await stop(ws, "err"), "");
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

test("an element written as an attribute's value reads", () => {
  const src = 'export function Banner() {\n  return <Card badge={<Badge count={2} />} onPress={() => api.post("/banner")} />;\n}\n';
  assert.deepEqual(spansOf(src, true), [`Banner: ${src.trimEnd()}`]);
  // A regex can start a container too.
  const check = "function Check({ s }) {\n  return <Flag on={/it's/.test(s)} />;\n}\n";
  assert.deepEqual(spansOf(check, true), [`Check: ${check.trimEnd()}`]);
});

test("a comment between an element's attributes reads", () => {
  const src = "function Form() {\n  return (\n    <Field\n      // the user's name\n      name=\"title\"\n      /* it's required */\n      required\n    />\n  );\n}\n";
  assert.deepEqual(spansOf(src, true), [`Form: ${src.trimEnd()}`]);
});

test("a generic method named like a modifier is a method", () => {
  assert.deepEqual(spansOf('class Store {\n  get<V>(id: string) { return api.get("/s") }\n  set<V>(id: string, value: V) {}\n}\n'), [
    'Store.get: get<V>(id: string) { return api.get("/s") }',
    "Store.set: set<V>(id: string, value: V) {}",
  ]);
  assert.deepEqual(spansOf('const box = {\n  get<T>(key: string) { return api.get("/b") },\n}\n'), [
    'box.get: get<T>(key: string) { return api.get("/b") }',
  ]);
});

test("a JSX container's first `(` is never a call of what came before the container", () => {
  const src = "function Row({ title }) {\n  return <Line label={title} render={(x) => <Cell value={x} />} />;\n}\n";
  const scan = scanSource(src, true);
  assert.equal(scan.ok, true);
  assert.deepEqual(scan.calls.map((call) => src.slice(call.from, call.start)), ["Row"]);
});

test("a declaration the scanner cannot place makes the file unread, never its outer function", () => {
  assert.equal(spansOf("function outer() {\n  function inner<T(a) { api.get('/x') }\n}\n"), null);
  assert.equal(spansOf("class A { find<T(a) { return 1 } }"), null);
});

test("R24.4: the scan's work grows with the file, and past its step budget the file is not read", async () => {
  const { SCAN_STEP_BUDGET } = await import("../src/facts/match.ts");
  // Semicolon-free code, where every `a < b` once rescanned to the end of its block.
  const flat = Array.from({ length: 10_000 }, (_, i) => `export const f${i} = (a, b) => a < b`).join("\n");
  const object = `export const o = {\n${Array.from({ length: 10_000 }, (_, i) => `  f${i}: (a, b) => a < b,`).join("\n")}\n}\n`;
  for (const [name, text] of [["flat", flat], ["object", object]] as const) {
    const scan = scanSource(text, false, Number.POSITIVE_INFINITY);
    assert.equal(scan.ok, true, name);
    assert.ok(scan.steps < SCAN_STEP_BUDGET, `${name}: ${scan.steps} steps`);
  }
  // A normal 2,000-line file reads well inside the budget.
  const normal = Array.from({ length: 400 }, (_, i) => `export function f${i}(id: string) {\n  const order = load<Order>(id);\n  return api.get(\`/orders/\${id}\`);\n}\n`).join("");
  assert.equal(scanSource(normal).ok, true);
  // Past the budget: not read.
  assert.equal(scanSource(normal, false, 1_000).ok, false);
});

test("R24.2: a type-predicate return type is read as a type, and one that does not read makes the file unread", async (t) => {
  assert.deepEqual(spansOf("function outer() {\n  function isX(v): v is { id: string } { return check(v); }\n}\n"), [
    "outer: function outer() {\n  function isX(v): v is { id: string } { return check(v); }\n}",
    "isX: function isX(v): v is { id: string } { return check(v); }",
  ]);
  assert.deepEqual(spansOf("function outer() {\n  function must(v): asserts v is { id: string } { check(v); }\n}\n"), [
    "outer: function outer() {\n  function must(v): asserts v is { id: string } { check(v); }\n}",
    "must: function must(v): asserts v is { id: string } { check(v); }",
  ]);
  assert.deepEqual(spansOf("function pick<T>(v: T): T extends string ? { s: T } : { n: T } { return wrap(v); }\n"), [
    "pick: function pick<T>(v: T): T extends string ? { s: T } : { n: T } { return wrap(v); }",
  ]);
  // A return type the scanner cannot skip: the file is not read.
  assert.equal(spansOf("function outer() {\n  function f(v): Box = { id: string } { return v; }\n}\n"), null);
  const ndjson = [
    header(["orders-api"]),
    row({ side: "consumer", protocol: "http", label: "POST /audit", repo: "orders-api", file: "src/p.ts", line: 2, name: "outer", text: "outer line" }),
  ].join("\n");
  const ws = await factsWorkspace(t, { ndjson: `${ndjson}\n` });
  fs.writeFileSync(ws.at("orders-api/src/p.ts"), 'export function outer() {\n  api.post("/audit");\n  function isX(v): v is { id: string } {\n    return check(v);\n  }\n}\n');
  assert.deepEqual(await linesFor(ws, "orders-api/src/p.ts", "return check(v);"), []);
});

test("R24.8: class expressions and functions assigned to members get their own spans", async (t) => {
  assert.deepEqual(spansOf("function outer() {\n  const C = class { m() { a(); } };\n}\n"), [
    "outer: function outer() {\n  const C = class { m() { a(); } };\n}",
    "C: const C = class { m() { a(); } }",
    "C.m: m() { a(); }",
  ]);
  assert.deepEqual(spansOf("function outer() {\n  obj.h = function () { a(); };\n  Foo.prototype.bar = () => { b(); };\n}\n"), [
    "outer: function outer() {\n  obj.h = function () { a(); };\n  Foo.prototype.bar = () => { b(); };\n}",
    "h: obj.h = function () { a(); }",
    "bar: Foo.prototype.bar = () => { b(); }",
  ]);
  const ndjson = [
    header(["orders-api"]),
    row({ side: "consumer", protocol: "http", label: "POST /audit", repo: "orders-api", file: "src/m.ts", line: 2, name: "outer", text: "outer line" }),
  ].join("\n");
  const ws = await factsWorkspace(t, { ndjson: `${ndjson}\n` });
  fs.writeFileSync(
    ws.at("orders-api/src/m.ts"),
    'export function outer() {\n  api.post("/audit");\n  const C = class {\n    m() {\n      one();\n    }\n  };\n  obj.h = function () {\n    two();\n  };\n  Foo.prototype.bar = () => {\n    three();\n  };\n}\n',
  );
  for (const inner of ["one();", "two();", "three();"]) assert.deepEqual(await linesFor(ws, "orders-api/src/m.ts", inner), [], inner);
  assert.deepEqual(await linesFor(ws, "orders-api/src/m.ts", 'api.post("/audit");'), ["outer line"]);
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

test("R24.9: a malformed edits list exits 0 with no output", async (t) => {
  const ws = await factsWorkspace(t);
  const file = ws.at("orders-api/src/routes/orders.ts");
  for (const edits of [[null], {}]) {
    const run = await payloadEdit(ws, { tool_name: "MultiEdit", tool_input: { file_path: file, edits } }, `bad-${JSON.stringify(edits)}`);
    assert.equal(run.code, 0, run.stderr);
    assert.equal(run.stdout, "");
  }
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
  const run = await edit(ws, path.join(top, "src", "routes", "orders.ts"), "res.status(201).json(created);", { cwd: ws.at("orders-api") });
  assert.equal(context(run.stdout), "POST /orders is called at web-app/src/boot.ts:4");
});

test("R24.7: a worktree of a checkout two fact-file repos map to is placed in neither", async (t) => {
  const { locate } = await import("../src/facts/lines.ts");
  const ws = await factsWorkspace(t);
  const top = worktree(ws, "orders-api", "dup-branch");
  const file = path.join(top, "src", "routes", "orders.ts");
  const one = [{ repo: "orders-api", path: ws.at("orders-api") }];
  assert.equal(locate(file, one)?.repo, "orders-api");
  assert.equal(locate(file, [...one, { repo: "acme/orders-api", path: ws.at("orders-api") }]), null);
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

test("a regex holding a backtick inside a template substitution is read as a regex, not as the template's end", () => {
  const src = 'function render(line) {\n  const p = `<p>${line.replace(/`([^`]+)`/g, "<code>$1</code>")}</p>`;\n  return api.get("/x");\n}\n';
  const spans = spansOf(src);
  assert.notEqual(spans, null);
  assert.equal(spans![0], `render: ${src.trimEnd()}`);
});

test("a template inside a substitution inside a template, with a regex holding } and a backtick, still reads", () => {
  const src = 'function f(s) {\n  return `a ${`b ${s.replace(/[}`]|\\}`/g, "")} c`} d`;\n}\n';
  assert.deepEqual(spansOf(src), [`f: ${src.trimEnd()}`]);
});

test("a division inside a template substitution is still division", () => {
  const src = "function f(a, b, c) {\n  return `${a / b / c}`;\n}\n";
  assert.deepEqual(spansOf(src), [`f: ${src.trimEnd()}`]);
  // `/` after `)` and after `a++` is division too; the backtick after it ends nothing early.
  const src2 = "function g(a, b) {\n  return `${(a) / b} ${a++ / b}`;\n}\n";
  assert.deepEqual(spansOf(src2), [`g: ${src2.trimEnd()}`]);
});

test("a file with an unbalanced brace is still unread when it holds a template substitution", () => {
  assert.equal(spansOf("function f(a) {\n  return `${a / 2}`;\n"), null);
  assert.equal(spansOf("function f(a) {\n  return `${a / 2`;\n}\n"), null);
});
