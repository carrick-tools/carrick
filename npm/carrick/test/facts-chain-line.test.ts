// The chain line: what an edit of a function the cloud walked to from routes'
// handlers prints (`facts/lines.ts`, `k: "chain"`), behind the opt-in switch
// `CARRICK_CHAIN_LINES=1`, which also needs `CARRICK_CONTRACT_LINES=1`. Also
// the store's `chains` table and the refresh's rule for a file without it.
// Every service, file, route and type here is invented. Wire: carrick-cloud
// `docs/internal/reference/edit-facts-wire.md`, section `k: "chain"`.

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

const BUILD_GET = `Carrick: buildLines (src/services/lines.ts:1, reached by at least 2 operations) is called by formatOrder (src/services/format.ts:3), which is called by the handler of ${GET}. ${GET} is called from web-app/src/api.ts:4${NOTE}, which expects: ${TYPE}`;
const BUILD_POST = `Carrick: buildLines (src/services/lines.ts:1, reached by at least 2 operations) is called by formatOrder (src/services/format.ts:3), which is called by the handler of ${POST}. ${POST} is called from web-app/src/api.ts:9${NOTE}, which expects: ${TYPE}`;

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

test("the depth 1 wording, and a hop with no call line names the file alone", async (t) => {
  const route: RouteSpec = { ...GET_ROUTE, path: [{ file: "src/routes/orders.ts", line: 3, name: "get_orderId_handler", call: null }] };
  const ws = await workspace(t, [contractRow(GET, "src/routes/orders.ts", 3, 4), chainRow("formatOrder", "src/services/format.ts", 2, [route])]);
  const out = await run(ws, { file: ws.at("orders-api/src/services/format.ts"), snippets: ["return { id: order.id"] });
  assert.deepEqual(out.lines, [`Carrick: formatOrder (src/services/format.ts:2) is called from src/routes/orders.ts in the handler of ${GET}. ${GET} is called from web-app/src/api.ts:4${NOTE}, which expects: ${TYPE}`]);
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
  assert.ok(out.lines[0]!.includes("reached by at least 2 operations"));
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
  assert.ok(both.lines[0]!.startsWith("Carrick: buildLines") && both.lines[0]!.includes(`handler of ${GET}.`), both.lines[0]);
  assert.ok(both.lines[1]!.startsWith("Carrick: sumLines") && both.lines[1]!.includes(`handler of ${POST}.`), both.lines[1]);
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

// Reads never say a chain line.

test("a Read prints no chain line even when the caller asks for the contract and chain kinds", async (t) => {
  const ws = await workspace(t, baseRows());
  const read = await run(ws, { file: ws.at("orders-api/src/services/lines.ts"), read: true }, { session: "read" });
  assert.deepEqual(read.lines, []);
  assert.deepEqual(Object.keys(read.claims), []);
  // Not vacuous: an edit of the same file prints the chain line.
  const edit = await run(ws, { file: ws.at("orders-api/src/services/lines.ts"), whole: true }, { session: "edit" });
  assert.deepEqual(edit.lines, [BUILD_GET]);
});

test("with every switch on, the Read hook prints no chain line", async (t) => {
  const ws = await workspace(t, baseRows());
  const read = await runHook("post-read.ts", {
    payload: {
      hook_event_name: "PostToolUse",
      tool_name: "Read",
      session_id: "hook-read-all-on",
      cwd: ws.root,
      tool_input: { file_path: ws.at("orders-api/src/services/lines.ts") },
      tool_response: { file: { filePath: ws.at("orders-api/src/services/lines.ts"), content: LINES } },
    },
    env: fakeEnv({ HOME: ws.home, CLAUDE_PROJECT_DIR: ws.root, CARRICK_READ_LINES: "1", CARRICK_CONTRACT_LINES: "1", CARRICK_CHAIN_LINES: "1", CARRICK_FAKE_FIXTURE: fixturePath("check-silent.json") }),
  });
  assert.equal(read.code, 0);
  assert.ok(!read.stdout.includes("buildLines ("), read.stdout);
});

// The switch, through the post-edit hook.

const hookEdit = (ws: Ws, session: string, env: Record<string, string>) => {
  const file = ws.at("orders-api/src/services/lines.ts");
  return patchEdit(ws, file, [{ newStart: lineOf(file, IN_BUILD), lines: [`+  ${IN_BUILD}`] }], { session, env });
};

test("the chain switch is opt-in: exactly \"1\", with the contract switch on and fact lines on", () => {
  assert.equal(chainLinesEnabled({}), false, "unset");
  assert.equal(chainLinesEnabled({ CARRICK_CHAIN_LINES: "1" }), false, "chain alone");
  assert.equal(chainLinesEnabled({ CARRICK_CHAIN_LINES: "true", CARRICK_CONTRACT_LINES: "1" }), false, "true");
  assert.equal(chainLinesEnabled({ CARRICK_CHAIN_LINES: "1", CARRICK_CONTRACT_LINES: "1" }), true, "both");
  assert.equal(chainLinesEnabled({ CARRICK_CHAIN_LINES: "1", CARRICK_CONTRACT_LINES: "1", CARRICK_FACT_LINES: "0" }), false, "facts off");
});

test("the hook prints the chain line only with both switches on, and never with fact lines off", async (t) => {
  const ws = await workspace(t, baseRows());
  const cases: Array<[Record<string, string>, boolean]> = [
    [{ CARRICK_CHAIN_LINES: "1" }, false],
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

test("with CARRICK_CHAIN_LINES unset or 0, the hook's stdout, exit code and files written are the same whether or not the fact file holds chain rows", async (t) => {
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
  const envs: Array<Record<string, string>> = [{}, { CARRICK_CONTRACT_LINES: "1" }, { CARRICK_CONTRACT_LINES: "1", CARRICK_CHAIN_LINES: "0" }];
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
  const on = await observe(rows, { CARRICK_CONTRACT_LINES: "1", CARRICK_CHAIN_LINES: "1" });
  assert.equal(context(on.stdout), `${BUILD_GET}\n\nfn line for buildLines`);
});

// The store and the refresh.

/** Drop the `chains` table from a built file: what an older package built. */
async function dropChains(root: string): Promise<void> {
  const sqlite = (await loadSqlite())!;
  const db = new sqlite.DatabaseSync(factsPath(root));
  db.exec("DROP TABLE chains");
  db.close();
}

test("the local layout is unchanged by the chain kind", () => {
  assert.equal(LOCAL_LAYOUT, 3);
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

test("with the chain switch on, a file without the chains table is not known; otherwise it is", async (t) => {
  const bodies: Array<Record<string, unknown>> = [];
  const request = (async (_url: string, init?: RequestInit) => {
    bodies.push(JSON.parse(String(init?.body)) as Record<string, unknown>);
    return new Response(JSON.stringify({ success: false, error: { code: "facts_building" } }));
  }) as typeof fetch;
  const env = { ...process.env, CARRICK_TOKEN: "t" };
  const ws = await workspace(t, baseRows());
  await dropChains(ws.root);
  const known = (i: number) => bodies[i]!["known_digest"] !== undefined;
  await refreshFacts(ws.root, { request, env });
  assert.equal(known(0), true, "switches off: known");
  await refreshFacts(ws.root, { request, env: { ...env, CARRICK_CONTRACT_LINES: "1" } });
  assert.equal(known(1), true, "contract on, chain off: known");
  await refreshFacts(ws.root, { request, env: { ...env, CARRICK_CHAIN_LINES: "1" } });
  assert.equal(known(2), true, "chain alone (contract off): known");
  await refreshFacts(ws.root, { request, env: { ...env, CARRICK_CONTRACT_LINES: "1", CARRICK_CHAIN_LINES: "1" } });
  assert.equal(known(3), false, "chain on, no table: not known");
  const withTable = await workspace(t, baseRows());
  await refreshFacts(withTable.root, { request, env: { ...env, CARRICK_CONTRACT_LINES: "1", CARRICK_CHAIN_LINES: "1" } });
  assert.equal(known(4), true, "chain on, table present: known");
});
