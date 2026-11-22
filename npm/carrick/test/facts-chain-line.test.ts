// The chain line: what an edit of a function the cloud walked to from routes'
// handlers prints (`facts/lines.ts`, `k: "chain"`), on by default,
// off with `CARRICK_CHAIN_LINES=0`; it also needs the contract line on. Also
// the store's `chains` table and the refresh's rule for a file without it.
// Every service, file, route and type here is invented. Wire: carrick-cloud
// `docs/internal/reference/edit-facts-wire.md`, section `k: "chain"`.

import test from "node:test";
import assert from "node:assert/strict";
import crypto from "node:crypto";
import fs from "node:fs";
import os from "node:os";
import path from "node:path";
import { drainRunner, fakeEnv, fixturePath, runHook, settleRunner, withoutLineSwitches } from "./helpers.ts";
import { commitRepos } from "./facts-repos.ts";
import { context, lineOf, patchEdit } from "./facts-helpers.ts";
import type { Ws } from "./facts-helpers.ts";
import { buildFacts, factsPath, loadSqlite, openFacts, LOCAL_LAYOUT } from "../src/facts/store.ts";
import { chainLinesEnabled } from "../src/facts/enabled.ts";
import { refreshFacts } from "../src/facts/fetch.ts";
import { factLines, MAX_FACT_LINES } from "../src/facts/lines.ts";
import { markFacts } from "../src/hook/reuse.ts";

const SLUG = "chain-line-demo";
const FULL_COMMIT = "abcdef0123456789abcdef0123456789abcdef01";
const NOTE = " (another repo, indexed at abcdef01, not checked out here)";
const TYPE = "{ id: string }";

// The producer `orders-api`, checked out here: two routes in two files whose
// inline handlers call `formatOrder`, which calls `buildLines`.
const ORDERS = `// orders
import { formatOrder } from "../services/format";
router.get("/orders/:orderId", (req, res) => {
  res.json(formatOrder(req.order));
});
`;
const REFUNDS = `// refunds
import { formatOrder } from "../services/format";
router.post("/refunds", (req, res) => {
  res.json(formatOrder(req.refund));
});
`;
const FORMAT = `import { buildLines } from "./lines";
export function formatOrder(order) {
  return { id: order.id, lines: buildLines(order.items) };
}
`;
const LINES = `export function buildLines(items) {
  return items.map((item) => item.id);
}
export function sumLines(items) {
  return items.length;
}
export function taxLines(items) {
  return items.length * 2;
}
export function roundLines(items) {
  return items.length * 3;
}
`;
const FILES: Record<string, string> = {
  "orders-api/src/routes/orders.ts": ORDERS,
  "orders-api/src/routes/refunds.ts": REFUNDS,
  "orders-api/src/services/format.ts": FORMAT,
  "orders-api/src/services/lines.ts": LINES,
};

const GET = "GET /orders/:orderId";
const POST = "POST /refunds";

/** A producer response contract row of `orders-api`, typed by a consumer in `web-app` (off disk) at `consumerLine`. */
function contractRow(label: string, file: string, line: number, consumerLine: number, site: string | null = null) {
  return {
    k: "contract",
    side: "producer",
    protocol: "http",
    label,
    direction: "response",
    service: "orders-api",
    repo: "orders-api",
    file,
    line,
    name: null,
    site,
    locator: "registration",
    groups: [
      {
        type: TYPE,
        symbol: null,
        home: null,
        places: [{ service: "web-app", repo: "web-app", file: "src/api.ts", line: consumerLine, loc: `web-app/src/api.ts:${consumerLine}` }],
        total: 1,
        shown: 1,
        lead: `Carrick: ${label} is called from `,
        more: "",
        tail: `, which expects: ${TYPE}`,
        shortened: false,
      },
    ],
    untyped: [],
    total: 1,
  };
}

type Hop = { file: string; line: number; name: string; call: number | null };

/** The lead the cloud writes for one route of a chain row (plan section 12, "D6 as ruled"). */
function lead(name: string, file: string, line: number, total: number, label: string, path: Hop[]): string {
  const count = total > 1 ? `, reached by at least ${total} operations` : "";
  const hop = path[0]!;
  const at = hop.call === null ? hop.file : `${hop.file}:${hop.call}`;
  const where = path.length === 1 ? `is called from ${at} in the handler of ${label}` : `is called by ${hop.name} (${at}), which is called by the handler of ${label}`;
  return `Carrick: ${name} (${file}:${line}${count}) ${where}. ${label} is called from `;
}

type RouteSpec = { label: string; file: string; line: number; path: Hop[] };

/** A chain row of `name` declared at `file:line`, over `routes` in order; `total` defaults to their count. */
function chainRow(name: string, file: string, line: number, routes: RouteSpec[], total = routes.length) {
  return {
    k: "chain",
    repo: "orders-api",
    file,
    line,
    name,
    routes: routes.map((route) => ({
      protocol: "http",
      side: "producer",
      label: route.label,
      service: "orders-api",
      repo: "orders-api",
      file: route.file,
      line: route.line,
      depth: route.path.length,
      path: route.path,
      lead: lead(name, file, line, total, route.label, route.path),
    })),
    total,
  };
}

const GET_ROUTE = { label: GET, file: "src/routes/orders.ts", line: 3 };
const POST_ROUTE = { label: POST, file: "src/routes/refunds.ts", line: 3 };
const viaFormat = (route: { label: string; file: string; line: number }, call = 3): RouteSpec => ({
  ...route,
  path: [
    { file: "src/services/format.ts", line: 2, name: "formatOrder", call },
    { file: route.file, line: route.line, name: route.label.startsWith("GET") ? "get_orderId_handler" : "post_handler", call: 4 },
  ],
});

/** The contract rows of both routes and `buildLines`'s chain row (depth 2 through `formatOrder`). */
function baseRows(): Array<Record<string, unknown>> {
  return [
    contractRow(GET, "src/routes/orders.ts", 3, 4),
    contractRow(POST, "src/routes/refunds.ts", 3, 9),
    chainRow("buildLines", "src/services/lines.ts", 1, [viaFormat(GET_ROUTE), viaFormat(POST_ROUTE)]),
  ];
}

const askFor = (name: string): string => `Open those callers before you change ${name}, because what you add here runs on their calls too.`;
const ASK = askFor("buildLines");
const BUILD_GET = `Carrick: buildLines (src/services/lines.ts:1) runs in the handler of ${GET}, through formatOrder (src/services/format.ts:3) and at least 1 other route. Other services call that route: web-app/src/api.ts:4${NOTE}, which expects: ${TYPE}. ${ASK}`;
const BUILD_POST = `Carrick: buildLines (src/services/lines.ts:1) runs in the handler of ${POST}, through formatOrder (src/services/format.ts:3) and at least 1 other route. Other services call that route: web-app/src/api.ts:9${NOTE}, which expects: ${TYPE}. ${ASK}`;

/** A workspace with `orders-api` committed from `files` and a fact file of `rows` naming `web-app`'s full commit. */
async function workspace(t: { after: (fn: () => void) => void }, rows: Array<Record<string, unknown>>, files: Record<string, string> = FILES): Promise<Ws> {
  const root = fs.realpathSync(fs.mkdtempSync(path.join(os.tmpdir(), "carrick-chain-")));
  const home = fs.realpathSync(fs.mkdtempSync(path.join(os.tmpdir(), "carrick-chain-home-")));
  t.after(async () => {
    await drainRunner(root);
    fs.rmSync(root, { recursive: true, force: true });
    fs.rmSync(home, { recursive: true, force: true });
  });
  for (const [rel, text] of Object.entries(files)) {
    fs.mkdirSync(path.dirname(path.join(root, rel)), { recursive: true });
    fs.writeFileSync(path.join(root, rel), text);
  }
  fs.mkdirSync(path.join(root, ".carrick", "hosted"), { recursive: true });
  fs.writeFileSync(path.join(root, ".carrick", "hosted", "snapshot.json"), JSON.stringify({ resolution: { repos: [{ project_slug: SLUG }] } }));
  const header = JSON.stringify({
    schema: 1,
    writer: 5,
    project_id: "p",
    project_slug: SLUG,
    digest: "d1",
    built_at: "2026-01-01T00:00:00Z",
    repos: [
      { repo: "orders-api", service: "orders-api", commit: "1" },
      { repo: "web-app", service: "web-app", commit: FULL_COMMIT },
    ],
    counts: { rows: 0, producers: 0, consumers: 0 },
  });
  const text = `${[header, ...rows.map((row) => JSON.stringify(row))].join("\n")}\n`;
  const local = ["orders-api"];
  await buildFacts(commitRepos(root, text, local), factsPath(root), local.map((repo) => ({ repo, path: path.join(root, repo) })), new Date().toISOString());
  return { root, home, at: (rel) => path.join(root, rel) };
}

type Edit = { file: string; snippets?: string[]; whole?: boolean; read?: boolean };
const run = (ws: Ws, edits: Edit | Edit[], options: { session?: string; contract?: boolean; chain?: boolean } = {}) =>
  factLines(ws.root, Array.isArray(edits) ? edits : [edits], options.session ?? "s1", { home: ws.home, contract: options.contract ?? true, chain: options.chain ?? true });

const IN_BUILD = "return items.map((item) => item.id);";
const linesEdit = (ws: Ws, snippet = IN_BUILD): Edit => ({ file: ws.at("orders-api/src/services/lines.ts"), snippets: [snippet] });
const ordersEdit = (ws: Ws): Edit => ({ file: ws.at("orders-api/src/routes/orders.ts"), snippets: ["// orders"] });
const groupKey = (label: string, file: string) => `contract::orders-api::${file}::producer::${label}::0`;

test("an edit of a function two calls below a route's handler prints the chain line, with the note", async (t) => {
  const ws = await workspace(t, baseRows());
  const out = await run(ws, linesEdit(ws));
  assert.deepEqual(out.lines, [BUILD_GET]);
  assert.deepEqual(Object.keys(out.claims).sort(), [groupKey(GET, "src/routes/orders.ts"), "chain::orders-api::src/services/lines.ts::buildLines", "orders-api::src/routes/orders.ts::producer::GET /orders/:orderId"].sort());
});

test("a worded request row stored before the response row does not change the chain line", async (t) => {
  const getRequest = { ...contractRow(GET, "src/routes/orders.ts", 3, 4), direction: "request" };
  getRequest.groups = [{ ...getRequest.groups[0]!, lead: `Carrick: ${GET} takes a body from `, tail: ", which sends a request body: { note: string }" }];
  const ws = await workspace(t, [getRequest, ...baseRows()]);
  const out = await run(ws, linesEdit(ws));
  assert.deepEqual(out.lines, [BUILD_GET]);
  assert.deepEqual(out.claims[groupKey(GET, "src/routes/orders.ts")] !== undefined, true);
});

test("the depth 1 wording, and a hop with no call line names the file alone", async (t) => {
  const route: RouteSpec = { ...GET_ROUTE, path: [{ file: "src/routes/orders.ts", line: 3, name: "get_orderId_handler", call: null }] };
  const ws = await workspace(t, [contractRow(GET, "src/routes/orders.ts", 3, 4), chainRow("formatOrder", "src/services/format.ts", 2, [route])]);
  const out = await run(ws, { file: ws.at("orders-api/src/services/format.ts"), snippets: ["return { id: order.id"] });
  assert.deepEqual(out.lines, [`Carrick: formatOrder (src/services/format.ts:2) runs in the handler of ${GET} (src/routes/orders.ts). Other services call that route: web-app/src/api.ts:4${NOTE}, which expects: ${TYPE}. ${askFor("formatOrder")}`]);
});

test("a hop file both routes share, changed since the index, makes the line silent", async (t) => {
  const ws = await workspace(t, baseRows());
  fs.appendFileSync(ws.at("orders-api/src/services/format.ts"), "// changed after the index\n");
  assert.deepEqual((await run(ws, linesEdit(ws))).lines, []);
});

test("only the first route's handler file changed: the second route prints", async (t) => {
  const ws = await workspace(t, baseRows());
  fs.appendFileSync(ws.at("orders-api/src/routes/orders.ts"), "// changed after the index\n");
  const out = await run(ws, linesEdit(ws));
  assert.deepEqual(out.lines, [BUILD_POST]);
});

test("the first route's contract line said earlier in the session: the second route prints, with the function's count unchanged", async (t) => {
  const ws = await workspace(t, baseRows());
  markFacts("s1", { [groupKey(GET, "src/routes/orders.ts")]: { at: 1, named: [] } }, ws.home);
  const out = await run(ws, linesEdit(ws));
  assert.deepEqual(out.lines, [BUILD_POST]);
  assert.ok(out.lines[0]!.includes("and at least 1 other route."));
});

test("every route's contract line said: silent", async (t) => {
  const ws = await workspace(t, baseRows());
  markFacts("s1", { [groupKey(GET, "src/routes/orders.ts")]: { at: 1, named: [] }, [groupKey(POST, "src/routes/refunds.ts")]: { at: 1, named: [] } }, ws.home);
  assert.deepEqual((await run(ws, linesEdit(ws))).lines, []);
});

test("a function's chain line prints once per session", async (t) => {
  const ws = await workspace(t, baseRows());
  const first = await run(ws, linesEdit(ws));
  assert.deepEqual(first.lines, [BUILD_GET]);
  markFacts("s1", first.claims, ws.home);
  assert.deepEqual((await run(ws, linesEdit(ws))).lines, []);
  assert.deepEqual((await run(ws, linesEdit(ws), { session: "s2" })).lines, [BUILD_GET]);
});

test("after a chain line, an edit in the route's file in the same session prints no contract line for that route", async (t) => {
  const ws = await workspace(t, baseRows());
  const said = `${GET} is called from`;
  // Not vacuous: in a fresh session the same edit prints that route's contract line.
  const fresh = await run(ws, ordersEdit(ws), { session: "fresh" });
  assert.ok(fresh.lines.some((line) => line.includes(said)), fresh.lines.join(" | "));
  const first = await run(ws, linesEdit(ws));
  assert.deepEqual(first.lines, [BUILD_GET]);
  markFacts("s1", first.claims, ws.home);
  const after = await run(ws, ordersEdit(ws));
  assert.ok(!after.lines.some((line) => line.includes(said)), after.lines.join(" | "));
});

test("two changed functions reaching one route in one action: the second falls through to its next route", async (t) => {
  const rows = [
    ...baseRows(),
    chainRow("sumLines", "src/services/lines.ts", 4, [viaFormat(GET_ROUTE), viaFormat(POST_ROUTE)]),
  ];
  const ws = await workspace(t, rows);
  const out = await run(ws, linesEdit(ws, "return items.length;"), {});
  // `return items.length;` sits in sumLines alone; buildLines is edited with it below.
  const both = await run(ws, [linesEdit(ws), linesEdit(ws, "return items.length;")], { session: "both" });
  assert.equal(out.lines.length, 1);
  assert.equal(both.lines.length, 2);
  assert.ok(both.lines[0]!.startsWith("Carrick: buildLines") && both.lines[0]!.includes(`handler of ${GET}`), both.lines[0]);
  assert.ok(both.lines[1]!.startsWith("Carrick: sumLines") && both.lines[1]!.includes(`handler of ${POST}`), both.lines[1]);
});

test("chain lines share the cap of MAX_FACT_LINES", async (t) => {
  const extra = ["/orders/a", "/orders/b", "/orders/c"].map((p, i) => ({ label: `GET ${p}`, file: "src/routes/orders.ts", line: 10 + i }));
  const rows = [
    ...baseRows(),
    ...extra.map((route, i) => contractRow(route.label, route.file, route.line, 20 + i)),
    ...["sumLines", "taxLines", "roundLines"].map((name, i) => chainRow(name, "src/services/lines.ts", 4 + 3 * i, [viaFormat(extra[i]!)])),
  ];
  const ws = await workspace(t, rows);
  const out = await run(ws, { file: ws.at("orders-api/src/services/lines.ts"), whole: true });
  assert.equal(out.lines.length, MAX_FACT_LINES);
  assert.deepEqual(
    out.lines.map((line) => line.split(" (")[0]),
    ["Carrick: buildLines", "Carrick: sumLines", "Carrick: taxLines"],
  );
});

// Tier 2: an edited function the route's own code calls, in the route's file.

const ORDERS_B = `// orders
export function formatOrder(order) {
  return { id: order.id };
}
router.get("/orders/:orderId", (req, res) => {
  res.json(formatOrder(req.order));
});
router.post("/orders", (req, res) => {
  res.json(formatOrder(req.body));
});
`;

test("an edit of a function its routes' own code calls prints their contract lines and no chain line for them", async (t) => {
  const hop = (line: number, name: string, call: number): RouteSpec => ({ label: "", file: "src/routes/orders.ts", line, path: [{ file: "src/routes/orders.ts", line, name, call }] });
  const rows = [
    contractRow(GET, "src/routes/orders.ts", 5, 4, "/orders/:orderId"),
    contractRow("POST /orders", "src/routes/orders.ts", 8, 9, "/orders"),
    chainRow("formatOrder", "src/routes/orders.ts", 2, [
      { ...hop(5, "get_orderId_handler", 6), label: GET },
      { ...hop(8, "post_handler", 9), label: "POST /orders" },
    ]),
  ];
  const ws = await workspace(t, rows, { "orders-api/src/routes/orders.ts": ORDERS_B });
  const out = await run(ws, { file: ws.at("orders-api/src/routes/orders.ts"), snippets: ["return { id: order.id };"] });
  assert.deepEqual(out.lines, [
    `Carrick: ${GET} is called from web-app/src/api.ts:4${NOTE}, which expects: ${TYPE}`,
    `Carrick: POST /orders is called from web-app/src/api.ts:9${NOTE}, which expects: ${TYPE}`,
  ]);
});

// A Read says the chain line too: the best one for the file, once per session.

const READ_LINES = (ws: Ws) => ({ file: ws.at("orders-api/src/services/lines.ts"), read: true });

test("a Read of a function a route's handler reaches prints the chain line and claims it", async (t) => {
  const ws = await workspace(t, baseRows());
  const read = await run(ws, READ_LINES(ws), { session: "read" });
  assert.deepEqual(read.lines, [BUILD_GET]);
  assert.ok("chain::orders-api::src/services/lines.ts::buildLines" in read.claims);
  assert.deepEqual(read.answered, [], "the chain kind alone never answers the file");
});

test("the chain line said at a Read is silent at the Edit and at a second Read of the same session, and says once in another session", async (t) => {
  const ws = await workspace(t, baseRows());
  const read = await run(ws, READ_LINES(ws), { session: "s1" });
  assert.deepEqual(read.lines, [BUILD_GET]);
  markFacts("s1", read.claims, ws.home);
  assert.deepEqual((await run(ws, READ_LINES(ws), { session: "s1" })).lines, []);
  assert.deepEqual((await run(ws, linesEdit(ws), { session: "s1" })).lines, []);
  assert.deepEqual((await run(ws, READ_LINES(ws), { session: "s2" })).lines, [BUILD_GET]);
});

test("a Read says one chain line for the file: the route with the fewest hops, then the most callers", async (t) => {
  const direct: RouteSpec = { ...POST_ROUTE, path: [{ file: "src/routes/refunds.ts", line: 3, name: "post_handler", call: 4 }] };
  const rows = [
    contractRow(GET, "src/routes/orders.ts", 3, 4),
    contractRow(POST, "src/routes/refunds.ts", 3, 9),
    // buildLines reaches GET in two hops and POST in one: POST is said although GET is stored first.
    chainRow("buildLines", "src/services/lines.ts", 1, [viaFormat(GET_ROUTE), direct]),
    chainRow("sumLines", "src/services/lines.ts", 4, [viaFormat(GET_ROUTE)]),
  ];
  const ws = await workspace(t, rows);
  const read = await run(ws, READ_LINES(ws), { session: "rank" });
  assert.equal(read.lines.length, 1);
  assert.ok(read.lines[0]!.startsWith("Carrick: buildLines") && read.lines[0]!.includes(`handler of ${POST}`), read.lines[0]);
});

test("a Read skips a route the read file registers itself, and one whose caller is in its own service", async (t) => {
  const hop = (line: number, name: string, call: number): RouteSpec => ({ label: GET, file: "src/routes/orders.ts", line, path: [{ file: "src/routes/orders.ts", line, name, call }] });
  const rows = [contractRow(GET, "src/routes/orders.ts", 5, 4, "/orders/:orderId"), chainRow("formatOrder", "src/routes/orders.ts", 2, [hop(5, "get_orderId_handler", 6)])];
  const ws = await workspace(t, rows, { "orders-api/src/routes/orders.ts": ORDERS_B });
  const read = await run(ws, { file: ws.at("orders-api/src/routes/orders.ts"), read: true }, { session: "own" });
  assert.deepEqual(read.lines, []);
  // Not vacuous: an Edit of the same function prints the route's contract line.
  const edit = await run(ws, { file: ws.at("orders-api/src/routes/orders.ts"), snippets: ["return { id: order.id };"] }, { session: "own-edit" });
  assert.ok(edit.lines.length > 0);
});

test("chainOnly: the Read prints the chain line and no op, fn or name line, and answers no file", async (t) => {
  const ws = await workspace(t, baseRows());
  const out = await factLines(ws.root, [READ_LINES(ws)], "only", { home: ws.home, contract: true, chain: true, chainOnly: true });
  assert.deepEqual(out.lines, [BUILD_GET]);
  assert.deepEqual(out.answered, []);
});

test("a Read with the chain kind off prints no chain line", async (t) => {
  const ws = await workspace(t, baseRows());
  assert.deepEqual((await run(ws, READ_LINES(ws), { session: "off", chain: false })).lines, []);
});

const readHook = (ws: Ws, session: string, env: Record<string, string>) =>
  runHook("post-read.ts", {
    payload: {
      hook_event_name: "PostToolUse",
      tool_name: "Read",
      session_id: session,
      cwd: ws.root,
      tool_input: { file_path: ws.at("orders-api/src/services/lines.ts") },
      tool_response: { file: { filePath: ws.at("orders-api/src/services/lines.ts"), content: LINES } },
    },
    env: fakeEnv({ HOME: ws.home, CLAUDE_PROJECT_DIR: ws.root, CARRICK_FAKE_FIXTURE: fixturePath("check-silent.json"), ...env }),
  });

test("the Read hook prints the chain line with the Read switch off and with it on, and not with the chain switch off", async (t) => {
  const ws = await workspace(t, baseRows());
  const cases: Array<[Record<string, string>, boolean]> = [
    [{}, true],
    [{ CARRICK_READ_LINES: "1" }, true],
    [{ CARRICK_CHAIN_LINES: "0" }, false],
    [{ CARRICK_READ_LINES: "1", CARRICK_CHAIN_LINES: "0" }, false],
    [{ CARRICK_FACT_LINES: "0" }, false],
  ];
  for (const [i, [env, prints]] of cases.entries()) {
    const out = await readHook(ws, `read-hook-${i}`, env);
    assert.equal(out.code, 0);
    assert.equal(out.stdout.includes("buildLines (src/services/lines.ts:1"), prints, `${JSON.stringify(env)}: ${out.stdout}`);
  }
});

// The switch, through the post-edit hook.

const hookEdit = (ws: Ws, session: string, env: Record<string, string>) => {
  const file = ws.at("orders-api/src/services/lines.ts");
  return patchEdit(ws, file, [{ newStart: lineOf(file, IN_BUILD), lines: [`+  ${IN_BUILD}`] }], { session, env });
};

test("the chain switch is on by default: off only for exactly \"0\", with the contract switch off, or with fact lines off", () => {
  assert.equal(chainLinesEnabled({}), true, "unset");
  assert.equal(chainLinesEnabled({ CARRICK_CHAIN_LINES: "" }), true, "empty");
  assert.equal(chainLinesEnabled({ CARRICK_CHAIN_LINES: "0" }), false, "0");
  assert.equal(chainLinesEnabled({ CARRICK_CHAIN_LINES: "1" }), true, "1");
  assert.equal(chainLinesEnabled({ CARRICK_CHAIN_LINES: "true" }), true, "true");
  assert.equal(chainLinesEnabled({ CARRICK_CONTRACT_LINES: "0" }), false, "contract off");
  assert.equal(chainLinesEnabled({ CARRICK_CHAIN_LINES: "1", CARRICK_FACT_LINES: "0" }), false, "facts off");
});

test("the hook prints the chain line unless a switch is \"0\", and never with fact lines off", async (t) => {
  const ws = await workspace(t, baseRows());
  const cases: Array<[Record<string, string>, boolean]> = [
    [{ CARRICK_CHAIN_LINES: "0" }, false],
    [{ CARRICK_CONTRACT_LINES: "0" }, false],
    [{}, true],
    [{ CARRICK_CHAIN_LINES: "1", CARRICK_CONTRACT_LINES: "1" }, true],
    [{ CARRICK_CHAIN_LINES: "1", CARRICK_CONTRACT_LINES: "1", CARRICK_FACT_LINES: "0" }, false],
  ];
  for (const [i, [env, prints]] of cases.entries()) {
    const out = await hookEdit(ws, `switch-${i}`, env);
    assert.equal(out.code, 0);
    assert.equal(context(out.stdout).includes(BUILD_GET), prints, `${JSON.stringify(env)}: ${out.stdout}`);
  }
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

test("with CARRICK_CHAIN_LINES=0 or CARRICK_CONTRACT_LINES=0, the hook's stdout, exit code and files written are the same whether or not the fact file holds chain rows", async (t) => {
  const observe = async (rows: Array<Record<string, unknown>>, env: Record<string, string>) => {
    const ws = await workspace(t, rows);
    const before = [snapshot(ws.root), snapshot(ws.home)];
    const result = await hookEdit(ws, "hook-off", env);
    await settleRunner(ws.root);
    const after = [snapshot(ws.root), snapshot(ws.home)];
    // The fact file itself differs by its rows; every other file written is compared.
    const written = after.flatMap((map, i) =>
      [...map]
        .filter(([rel, hash]) => before[i]!.get(rel) !== hash && !rel.endsWith(".sqlite"))
        .map(([rel]) => `${i}:${rel.replace(/\.\d+-\d+-\d+\./, ".<id>.").replace(/\/\d+-\d+-\d+\.json$/, "/<id>.json")}`),
    );
    return { code: result.code, stdout: result.stdout, written: written.sort() };
  };
  // An fn row for the edited function, so every arm prints a line and the comparison is not of two empty outputs.
  const fn = {
    k: "fn",
    repo: "orders-api",
    file: "src/services/lines.ts",
    line: 1,
    name: "buildLines",
    others: [{ repo: "orders-api", file: "src/services/format.ts", line: 3, name: "formatOrder" }],
    total: 1,
    text: "fn line for buildLines",
  };
  const rows = [...baseRows(), fn];
  const withoutChains = rows.filter((row) => row["k"] !== "chain");
  const envs: Array<Record<string, string>> = [{ CARRICK_CHAIN_LINES: "0" }, { CARRICK_CONTRACT_LINES: "0" }, { CARRICK_CONTRACT_LINES: "1", CARRICK_CHAIN_LINES: "0" }];
  for (const env of envs) {
    const withRows = await observe(rows, env);
    const without = await observe(withoutChains, env);
    assert.equal(withRows.code, 0, JSON.stringify(env));
    assert.equal(withRows.code, without.code, JSON.stringify(env));
    assert.equal(withRows.stdout, without.stdout, JSON.stringify(env));
    assert.equal(context(withRows.stdout), "fn line for buildLines", JSON.stringify(env));
    assert.deepEqual(withRows.written, without.written, JSON.stringify(env));
  }
  // Not vacuous: with both switches on the same file prints the chain line, then the fn line.
  for (const env of [{}, { CARRICK_CONTRACT_LINES: "1", CARRICK_CHAIN_LINES: "1" }] as Array<Record<string, string>>) {
    const on = await observe(rows, env);
    assert.equal(context(on.stdout), `${BUILD_GET}\n\nfn line for buildLines`, JSON.stringify(env));
  }
});

// The store and the refresh.

/** Drop the `chains` table from a built file: what an older package built. */
async function dropChains(root: string): Promise<void> {
  const sqlite = (await loadSqlite())!;
  const db = new sqlite.DatabaseSync(factsPath(root));
  db.exec("DROP TABLE chains");
  db.close();
}

test("the local layout is 5: the matched kind added a table (chain)", () => {
  assert.equal(LOCAL_LAYOUT, 5);
});

test("a file built without the chains table opens and prints today's lines, with every switch on", async (t) => {
  const ws = await workspace(t, baseRows());
  await dropChains(ws.root);
  const store = await openFacts(ws.root);
  assert.ok(store, "opens");
  assert.deepEqual(store.chains("orders-api", "src/services/lines.ts", "buildLines"), []);
  store.close();
  assert.deepEqual((await run(ws, linesEdit(ws))).lines, []);
  // Today's lines: the route file's contract line (tier 3) prints as before.
  const today = await run(ws, ordersEdit(ws));
  assert.ok(today.lines.some((line) => line.startsWith(`Carrick: ${GET} is called from`)), today.lines.join(" | "));
});

test("a chain row is stored only when whole", async (t) => {
  const good = chainRow("buildLines", "src/services/lines.ts", 1, [viaFormat(GET_ROUTE)]);
  const route = good.routes[0]!;
  const bad: Array<Record<string, unknown>> = [
    { ...good, routes: [] },
    { ...good, routes: undefined },
    { ...good, routes: [{ ...route, depth: 3 }] },
    { ...good, routes: [{ ...route, side: "consumer" }] },
    { ...good, routes: [{ ...route, path: [] }] },
    { ...good, routes: [{ ...route, path: [{ ...route.path[0]!, call: "3" }] }] },
    { ...good, routes: [{ ...route, lead: "" }] },
    { ...good, total: 0 },
    { ...good, name: "" },
    { ...good, line: null },
  ];
  // Each bad row under its own name, so a stored one would answer.
  const ws = await workspace(t, [good, ...bad.map((row, i) => ({ ...row, name: row["name"] === "" ? "" : `bad${i}` }))]);
  const store = (await openFacts(ws.root))!;
  assert.equal(store.chains("orders-api", "src/services/lines.ts", "buildLines").length, 1);
  for (let i = 0; i < bad.length; i += 1) assert.deepEqual(store.chains("orders-api", "src/services/lines.ts", `bad${i}`), [], `bad${i}`);
  const sqlite = (await loadSqlite())!;
  const db = new sqlite.DatabaseSync(factsPath(ws.root), { readOnly: true });
  assert.equal(Number(db.prepare("SELECT COUNT(*) AS n FROM chains").get()!["n"]), 1);
  db.close();
  store.close();
});

test("every chain row of the pinned fixture is ingested and answered by function", async (t) => {
  const wire = fs.readFileSync(fixturePath("edit-facts.s1.ndjson"), "utf8").split("\n").filter(Boolean);
  const chains = wire.slice(1).map((line) => JSON.parse(line) as Record<string, any>).filter((row) => row["k"] === "chain");
  assert.ok(chains.length > 0, "the fixture holds chain rows");
  const root = fs.realpathSync(fs.mkdtempSync(path.join(os.tmpdir(), "carrick-chain-wire-")));
  t.after(() => fs.rmSync(root, { recursive: true, force: true }));
  fs.mkdirSync(path.join(root, ".carrick", "hosted"), { recursive: true });
  fs.writeFileSync(path.join(root, ".carrick", "hosted", "snapshot.json"), JSON.stringify({ resolution: { repos: [{ project_slug: "demo-shop" }] } }));
  await buildFacts(wire.join("\n"), factsPath(root), [], new Date().toISOString());
  const store = (await openFacts(root))!;
  for (const row of chains) assert.deepEqual(store.chains(row["repo"], row["file"], row["name"]), [row], row["name"]);
  // Every kept route joins exactly one printable producer response contract row, as the line looks it up.
  for (const row of chains) {
    for (const route of row["routes"]) {
      const joined = store
        .contracts(route["repo"], route["file"])
        .filter((one) => one.side === route["side"] && one.label === route["label"] && one.line === route["line"] && one.direction === "response" && typeof one.groups[0]?.lead === "string");
      assert.equal(joined.length, 1, `${row["name"]} -> ${route["label"]}`);
    }
  }
  store.close();
});

test("with the chain switch on (the default), a file without the chains table is not known; otherwise it is", async (t) => {
  const bodies: Array<Record<string, unknown>> = [];
  const request = (async (_url: string, init?: RequestInit) => {
    bodies.push(JSON.parse(String(init?.body)) as Record<string, unknown>);
    return new Response(JSON.stringify({ success: false, error: { code: "facts_building" } }));
  }) as typeof fetch;
  const env = { ...withoutLineSwitches(), CARRICK_TOKEN: "t" };
  const ws = await workspace(t, baseRows());
  await dropChains(ws.root);
  const known = (i: number) => bodies[i]!["known_digest"] !== undefined;
  await refreshFacts(ws.root, { request, env: { ...env, CARRICK_CONTRACT_LINES: "0", CARRICK_CHAIN_LINES: "0" } });
  assert.equal(known(0), true, "switches off: known");
  await refreshFacts(ws.root, { request, env: { ...env, CARRICK_CHAIN_LINES: "0" } });
  assert.equal(known(1), true, "contract on, chain off: known");
  await refreshFacts(ws.root, { request, env: { ...env, CARRICK_CONTRACT_LINES: "0" } });
  assert.equal(known(2), true, "chain on but contract off: known");
  await refreshFacts(ws.root, { request, env: { ...env, CARRICK_CONTRACT_LINES: "1", CARRICK_CHAIN_LINES: "1" } });
  assert.equal(known(3), false, "chain on, no table: not known");
  const withTable = await workspace(t, baseRows());
  await refreshFacts(withTable.root, { request, env: { ...env, CARRICK_CONTRACT_LINES: "1", CARRICK_CHAIN_LINES: "1" } });
  assert.equal(known(4), true, "chain on, table present: known");
});

// The callers a chain line names (request and response rows together) and the
// next named thing out when the edited one has no chain row.

const INTAKE_ROUTE = `import { InvoiceIntake } from "../services/intake";
router.post("/intake/upload", async (req, res) => {
  res.json(await intake.accept(req.file));
});
`;
const INTAKE_SERVICE = `export class InvoiceIntake {
  accept(file) {
    const finish = () => {
      return this.notify(file.id);
    };
    return this.store(file).andThen(finish);
  }
  store(file) {
    return file;
  }
}
`;
const INTAKE_FILES: Record<string, string> = {
  "orders-api/src/routes/intake.ts": INTAKE_ROUTE,
  "orders-api/src/services/intake.ts": INTAKE_SERVICE,
};
const UPLOAD = "POST /intake/upload";
const place = (file: string, line: number) => ({ service: "web-app", repo: "web-app", file, line, loc: `web-app/${file}:${line}` });
const group = (places: Array<ReturnType<typeof place>>, lead: string, tail: string) => ({ type: TYPE, symbol: null, home: null, places, total: places.length, shown: places.length, lead, more: "", tail, shortened: false });

/** The response row types only IntakePage; UploadDialog reads `.ok` alone, so it is untyped there. The request row types both. */
function intakeRows(extra: Array<Record<string, unknown>> = []): Array<Record<string, unknown>> {
  const base = contractRow(UPLOAD, "src/routes/intake.ts", 2, 118);
  const response = {
    ...base,
    groups: [group([place("src/IntakePage.tsx", 118)], `Carrick: ${UPLOAD} is called from `, `, which expects: ${TYPE}`)],
    untyped: [{ service: "web-app", repo: "web-app", file: "src/UploadDialog.tsx", line: 90, reason: "no_text" }],
    total: 2,
  };
  const request = {
    ...base,
    direction: "request",
    groups: [group([place("src/UploadDialog.tsx", 90), place("src/IntakePage.tsx", 118)], `Carrick: ${UPLOAD} takes a body from `, ", which sends a request body: { file: string }")],
    total: 2,
  };
  const path: Hop[] = [{ file: "src/routes/intake.ts", line: 2, name: "post_handler", call: 3 }];
  return [response, request, chainRow("InvoiceIntake.accept", "src/services/intake.ts", 2, [{ label: UPLOAD, file: "src/routes/intake.ts", line: 2, path }]), ...extra];
}
const OFF = (file: string, line: number) => `web-app/${file}:${line}${NOTE}`;
const INTAKE_LINE = `Carrick: InvoiceIntake.accept (src/services/intake.ts:2) runs in the handler of ${UPLOAD} (src/routes/intake.ts:3). Other services call that route: ${OFF("src/IntakePage.tsx", 118)}, which expects: ${TYPE}; ${OFF("src/UploadDialog.tsx", 90)}. Open those callers before you change InvoiceIntake.accept, because what you add here runs on their calls too.`;
const inAccept = (ws: Ws): Edit => ({ file: ws.at("orders-api/src/services/intake.ts"), snippets: ["return this.store(file).andThen(finish);"] });
const inCallback = (ws: Ws): Edit => ({ file: ws.at("orders-api/src/services/intake.ts"), snippets: ["return this.notify(file.id);"] });

test("the chain line names every other-service caller of the route: one typed on the reply, one that reads only .ok, both on the request row", async (t) => {
  const ws = await workspace(t, intakeRows(), INTAKE_FILES);
  assert.deepEqual((await run(ws, inAccept(ws))).lines, [INTAKE_LINE]);
});

test("a caller in the route's own service is not named", async (t) => {
  const rows = intakeRows();
  for (const row of rows) {
    if (row["k"] !== "contract") continue;
    for (const g of row["groups"] as Array<{ places: Array<{ service: string; file: string }> }>) for (const one of g.places) if (one.file.includes("UploadDialog")) one.service = "orders-api";
    for (const u of row["untyped"] as Array<{ service: string }>) u.service = "orders-api";
  }
  const ws = await workspace(t, rows, INTAKE_FILES);
  const [line] = (await run(ws, inAccept(ws))).lines;
  assert.ok(line !== undefined && !line.includes("UploadDialog") && line.includes("call that route: web-app/src/IntakePage.tsx:118"), String(line));
});

test("more than three callers: the first three, then a count of the rest", async (t) => {
  const rows = intakeRows();
  const many = ["A", "B", "C", "D", "E"].map((n, i) => place(`src/Page${n}.tsx`, 10 + i));
  (rows[1] as { groups: unknown[] }).groups = [group(many, `Carrick: ${UPLOAD} takes a body from `, ", which sends a request body: { file: string }")];
  const ws = await workspace(t, rows, INTAKE_FILES);
  const [line] = (await run(ws, inAccept(ws))).lines;
  assert.ok(line !== undefined && line.includes("PageB.tsx:11") && !line.includes("PageC.tsx") && line.includes(" and 4 more."), String(line));
});

test("an edit inside a named inner function of the method, which has no chain row of its own, prints the method's chain line", async (t) => {
  const ws = await workspace(t, intakeRows(), INTAKE_FILES);
  assert.deepEqual((await run(ws, inCallback(ws))).lines, [INTAKE_LINE]);
});

test("a function reached from 60 operations (a floor: the row carries no exact flag) says one route and \"at least\" 59 other routes, at an Edit and at a Read; one route says no count", async (t) => {
  const many = [...baseRows().slice(0, 2), chainRow("buildLines", "src/services/lines.ts", 1, [viaFormat(GET_ROUTE)], 60)];
  const ws = await workspace(t, many);
  const edit = await run(ws, linesEdit(ws), { session: "many-edit" });
  const read = await run(ws, READ_LINES(ws), { session: "many-read" });
  for (const line of [edit.lines[0], read.lines[0]]) {
    assert.ok(line !== undefined && line.includes(`through formatOrder (src/services/format.ts:3) and at least 59 other routes. Other services`), String(line));
    assert.ok(!line.includes(POST), "no list of the other routes");
  }
  const one = await workspace(t, [...baseRows().slice(0, 1), chainRow("buildLines", "src/services/lines.ts", 1, [viaFormat(GET_ROUTE)])]);
  for (const out of [await run(one, linesEdit(one), { session: "one-edit" }), await run(one, READ_LINES(one), { session: "one-read" })]) {
    assert.ok(out.lines[0] !== undefined && !out.lines[0].includes("other route"), String(out.lines[0]));
  }
});

test("a chain row that states its total exact says \"and N other routes\" without \"at least\", at an Edit and at a Read", async (t) => {
  const exact = { ...chainRow("buildLines", "src/services/lines.ts", 1, [viaFormat(GET_ROUTE)], 3), total_exact: true };
  const ws = await workspace(t, [...baseRows().slice(0, 2), exact]);
  const edit = await run(ws, linesEdit(ws), { session: "exact-edit" });
  const read = await run(ws, READ_LINES(ws), { session: "exact-read" });
  for (const line of [edit.lines[0], read.lines[0]]) {
    assert.ok(line !== undefined && line.includes("(src/services/format.ts:3) and 2 other routes. Other services"), String(line));
  }
});
