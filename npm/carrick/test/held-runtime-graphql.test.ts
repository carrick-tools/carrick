// L-G1b: the held runtime arms GraphQL resolver rows (the send form) beside
// HTTP rows. A fake compiler engine (a script speaking the engine's stdio
// protocol) answers `write_shadow`, and a fake held compiler answers
// diagnostics from the overlay text, so no TypeScript 7 is needed. The rows and
// the SDL table come from a fact file built from the wire. Every name is invented.

import test from "node:test";
import assert from "node:assert/strict";
import { execFileSync } from "node:child_process";
import fs from "node:fs";
import os from "node:os";
import path from "node:path";
import ts from "typescript";
import type { ServiceEngine, ServiceInit } from "../src/held/daemon.ts";
import type { Diag, Engine } from "../src/held/engine.ts";
import { placedPath } from "../src/held/paths.ts";
import { createRuntime } from "../src/held/runtime.ts";
import { buildFacts, factsPath } from "../src/facts/store.ts";

const REPO = "shop";
const SERVICE = "orders-gql";
const RESOLVERS = "src/resolvers.ts";
const HTTP = "src/http.ts";
const RESOLVERS_TEXT = [
  "export const resolvers = {",
  '  order: async () => ({ id: "o1", total: 3 }),',
  '  createOrder: async () => ({ id: "o2" }),',
  "};",
  "",
].join("\n");
const HTTP_TEXT = "export const reply = () => ({ ok: true });\n";

/** The engine's GraphQL answer, as S-G1 writes it, cut to what the fake compiler reads. */
const GQL_LINES = ["type __carrick_GqlExpected<E> = E;", "type __carrick_GqlSent<S, E> = S;", "type __carrick_GqlSentS<S, E, R> = S;"];
const TYPES_DECL = "type __carrick_GqlTypes = {};";
/** A table in the engine's shape; the fake engine refuses any field leaf that is not. */
const TABLE = { Query: { fields: { order: { type: "Order" } } }, Order: { fields: { id: { non_null: { scalar: "ID" } }, total: { scalar: "Int" } } } };

/** A fake compiler engine: answers `init` and `write_shadow`, and logs each `write_shadow` it is asked. */
const FAKE_ENGINE = `
import fs from "node:fs";
import path from "node:path";
const lines = fs.readFileSync(0, "utf8").split("\\n").filter(Boolean).map((l) => JSON.parse(l));
const shape = (leaf) => leaf && typeof leaf === "object" && Object.keys(leaf).length === 1 && ["scalar", "enum", "type", "list", "non_null"].includes(Object.keys(leaf)[0]);
for (const r of lines) {
  if (r.action === "init") { console.log(JSON.stringify({ request_id: r.request_id, status: "ready" })); continue; }
  if (r.action !== "write_shadow") continue;
  fs.appendFileSync(process.env.FAKE_ENGINE_LOG, JSON.stringify({ graphql: r.graphql ?? null }) + "\\n");
  const bad = r.graphql && Object.values(r.graphql.types).some((t) => Object.values(t.fields ?? {}).some((leaf) => !shape(leaf)));
  if (bad) { console.log(JSON.stringify({ request_id: r.request_id, status: "error", errors: ["Invalid request: graphql.types"] })); continue; }
  fs.mkdirSync(path.join(r.out_dir, "w"), { recursive: true });
  fs.writeFileSync(path.join(r.out_dir, "w", "tsconfig.json"), "{}");
  const fit = { strictNullChecks: true, strictFunctionTypes: true, exactOptionalPropertyTypes: false };
  const leaves = r.graphql ? Object.fromEntries(r.graphql.roots.filter((root) => root.type === "Query").map((root) => [root.type + "." + root.field, { type: "Order" }])) : {};
  console.log(JSON.stringify({
    request_id: r.request_id, status: "success", tsconfig: path.join(r.out_dir, "w", "tsconfig.json"),
    own_files: ${JSON.stringify([RESOLVERS, HTTP])}, native_tsc: "/bin/true",
    wire: ["type __carrick_Wire<S, E> = S;"], tolerances: { unwidened_alias_suffix: "", declarations: ["type __carrick_IsVoid<E> = false;"] },
    options: { service: fit, judge: fit },
    ...(r.graphql ? { graphql: { lines: ${JSON.stringify(GQL_LINES)}, types_decl: ${JSON.stringify(TYPES_DECL)}, leaves } } : {}),
  }));
}
`;

const POST = ", (o) => o)";

/**
 * A fake held compiler. Per check: a twin (its alias intersected with
 * `__carrick_never`) speaks at the check's start; a value holding `BROKEN`
 * speaks at the callback body, naming the alias, as the compiler does for a
 * GraphQL break (P8).
 */
class FakeCompiler implements Engine {
  readonly texts = new Map<string, string>();
  set(file: string, text: string): void {
    this.texts.set(file, text);
  }
  async pull(files: string[]): Promise<Map<string, Diag[]>> {
    const out = new Map<string, Diag[]>();
    for (const file of files) {
      const text = this.texts.get(file) ?? "";
      const sf = ts.createSourceFile("x.ts", text, ts.ScriptTarget.Latest, false);
      const at = (offset: number): Diag["range"] => {
        const p = sf.getLineAndCharacterOfPosition(offset);
        return { start: p, end: p };
      };
      const diags: Diag[] = [];
      for (const m of text.matchAll(/__C\.send(\d+_\d+)\(/g)) {
        const k = m[1]!;
        const post = text.indexOf(POST, m.index);
        const value = text.slice(m.index + m[0].length, post);
        if (new RegExp(`type O${k} = .*__carrick_never`).test(text)) diags.push({ range: at(m.index), code: 2322, message: `Type '{}' is not assignable to type 'O${k}'.` });
        else if (value.includes("BROKEN")) diags.push({ range: at(post + POST.indexOf("=> o") + 3), code: 2322, message: `Type 'string' is not assignable to type 'O${k}'.` });
      }
      out.set(file, diags);
    }
    return out;
  }
  rss(): number | null {
    return 1;
  }
  async close(): Promise<void> {}
}

const git = (cwd: string, ...args: string[]): string =>
  execFileSync("git", ["-c", "user.name=t", "-c", "user.email=t@example.test", "-c", "commit.gpgsign=false", ...args], { cwd, encoding: "utf8" }).trim();

function spanOf(text: string, snippet: string): [number, number] {
  const at = text.indexOf(snippet);
  assert.ok(at >= 0, snippet);
  return [at, at + snippet.length];
}

type Wire = Record<string, unknown>;

function contract(label: string, protocol: string, file: string, type: string, span: [number, number], extra: Wire = {}): Wire {
  return {
    k: "contract", side: "producer", protocol, label, direction: "response", service: SERVICE, repo: REPO, file, line: 2,
    name: null, site: null, locator: "registration",
    groups: [{ type, symbol: null, home: null, places: [{ service: "web", repo: REPO, file: "web/order.ts", line: 4, loc: "web/order.ts:4", alias: "A", infer_kind: "call_result", span: null, symbol: null, home: null }], total: 1, shown: 1, lead: "", more: "", tail: "", shortened: false }],
    untyped: [], total: 1, values: [{ file, span }], ...extra,
  };
}

const ORDER = "{ order: { id: string; total: number } | null }";
const rowsOf = (gql: boolean, orderType = ORDER): Wire[] => [
  contract("GET /ok", "http", HTTP, "{ ok: boolean }", spanOf(HTTP_TEXT, "{ ok: true }")),
  ...(gql
    ? [
        contract("QUERY order", "graphql", RESOLVERS, orderType, spanOf(RESOLVERS_TEXT, '{ id: "o1", total: 3 }'), { graphql_root: { type: "Query", field: "order" } }),
        contract("MUTATION createOrder", "graphql", RESOLVERS, "{ createOrder: { id: string } }", spanOf(RESOLVERS_TEXT, '{ id: "o2" }'), { graphql_root: { type: "Mutation", field: "createOrder" } }),
      ]
    : []),
];

type Rig = {
  root: string;
  service: ServiceEngine;
  compiler: FakeCompiler;
  /** Each `write_shadow` the fake engine was asked, with the GraphQL it was sent. */
  asked: () => Array<{ graphql: unknown }>;
  placed: () => { armed: Array<Wire>; unarmed: Array<Wire> };
  edit: (rel: string, text: string) => Promise<Array<Record<string, unknown>>>;
  /** Write a new fact file (the daemon reads it on the next request). */
  facts: (rows: Wire[], table: unknown) => Promise<void>;
};

async function rig(t: { after: (fn: () => void | Promise<void>) => void }, rows: Wire[], table: unknown, startCompiler = true): Promise<Rig | { placed: Rig["placed"]; asked: Rig["asked"] }> {
  const dir = fs.realpathSync(fs.mkdtempSync(path.join(os.tmpdir(), "held-gql-runtime-")));
  t.after(() => fs.rmSync(dir, { recursive: true, force: true }));
  const root = path.join(dir, "ws");
  const home = path.join(dir, "home");
  fs.mkdirSync(path.join(root, "src"), { recursive: true });
  fs.writeFileSync(path.join(root, RESOLVERS), RESOLVERS_TEXT);
  fs.writeFileSync(path.join(root, HTTP), HTTP_TEXT);
  git(root, "init", "-q");
  git(root, "add", "-A");
  git(root, "commit", "-q", "-m", "index");
  const commit = git(root, "rev-parse", "HEAD");
  // The workspace's project, so the fact file is read (as `held-store-graphql.test.ts` does).
  fs.mkdirSync(path.join(root, ".carrick", "hosted"), { recursive: true });
  fs.writeFileSync(path.join(root, ".carrick", "hosted", "snapshot.json"), JSON.stringify({ resolution: { repos: [{ project_slug: "shop" }] } }));
  const facts =async (wireRows: Wire[], gqlTable: unknown): Promise<void> => {
    const header = { schema: 1, writer: 12, project_id: "p", project_slug: "shop", digest: "d", built_at: "2026-01-01T00:00:00Z", repos: [{ repo: REPO, service: SERVICE, commit }] };
    const lines = [header, ...wireRows, ...(gqlTable === null ? [] : [{ k: "gqltypes", repo: REPO, service: SERVICE, types: gqlTable }])];
    await buildFacts(lines.map((l) => JSON.stringify(l)).join("\n") + "\n", factsPath(root), [{ repo: REPO, path: root }], new Date().toISOString());
  };
  await facts(rows, table);
  const entry = path.join(dir, "engine.mjs");
  fs.writeFileSync(entry, FAKE_ENGINE);
  const log = path.join(dir, "engine.log");
  fs.writeFileSync(log, "");
  const compiler = new FakeCompiler();
  const init: ServiceInit = { repo: REPO, service: SERVICE, repo_path: root, sidecar_root: root, tsconfig: null, scan_root: root };
  const runtime = createRuntime({
    root, home, entry, env: { ...process.env, FAKE_ENGINE_LOG: log },
    site: async () => ({ reason: "value_edited" }),
    makeEngine: () => {
      assert.ok(startCompiler, "no held compiler may start here");
      return compiler;
    },
  });
  const started = await runtime.start(init);
  const asked = (): Array<{ graphql: unknown }> => fs.readFileSync(log, "utf8").split("\n").filter(Boolean).map((l) => JSON.parse(l) as { graphql: unknown });
  const placed = (): { armed: Wire[]; unarmed: Wire[] } => JSON.parse(fs.readFileSync(placedPath(root, SERVICE, home), "utf8")) as { armed: Wire[]; unarmed: Wire[] };
  if (!startCompiler) return { placed, asked };
  assert.ok("engine" in started, JSON.stringify(started));
  const service = started.engine;
  t.after(() => service.close());
  let n = 0;
  const edit = async (rel: string, text: string): Promise<Array<Record<string, unknown>>> => {
    const file = path.join(root, rel);
    fs.writeFileSync(file, text);
    const later = new Date(Date.now() + 5000 * ++n);
    fs.utimesSync(file, later, later);
    const answer = await service.answer([file], { session: `s${n}`, files: [file], kind: "edit", edited_at: Date.now() });
    await answer.after?.();
    return answer.lines as unknown as Array<Record<string, unknown>>;
  };
  return { root, service, compiler, asked, placed, edit, facts };
}

const shadowText = (r: Rig, rel: string): string => [...r.compiler.texts].find(([file]) => file.endsWith(rel))![1];
const summary = (entries: Wire[]): string[] => entries.map((e) => `${e["protocol"]} ${e["label"]}#${e["position"]}${e["reason"] ? `:${e["reason"]}` : ""}`).sort();

test("GraphQL rows arm beside an HTTP row: the scalar form where the engine gave a leaf, the JSON form where it gave none; a break is one line", async (t) => {
  const r = (await rig(t, rowsOf(true), TABLE)) as Rig;
  // The engine was asked once, with the table and both roots.
  assert.deepEqual(r.asked(), [{ graphql: { types: TABLE, roots: [{ type: "Mutation", field: "createOrder" }, { type: "Query", field: "order" }] } }]);
  assert.deepEqual(summary(r.placed().armed), ["graphql MUTATION createOrder#0", "graphql QUERY order#0", "http GET /ok#0"]);
  assert.deepEqual(r.placed().unarmed, []);
  const gql = shadowText(r, RESOLVERS);
  for (const line of [...GQL_LINES, TYPES_DECL]) assert.ok(gql.includes(line), `the GraphQL file appends ${line}`);
  assert.match(gql, /type L\d+_0 = \{"type":"Order"\};/, "the leaf the engine gave Query.order");
  assert.match(gql, /__carrick_GqlSentS<Awaited<V>, O\d+_0, L\d+_0>/, "Query.order: the scalar form");
  assert.match(gql, /__carrick_GqlSent<Awaited<V>, O\d+_0>/, "Mutation.createOrder: no leaf, the JSON form");
  assert.ok(!shadowText(r, HTTP).includes("__carrick_Gql"), "an HTTP file appends no GraphQL rule");

  const lines = await r.edit(RESOLVERS, RESOLVERS_TEXT.replace('"o1"', '"BROKEN"'));
  assert.equal(lines.length, 1, JSON.stringify(lines));
  assert.equal(lines[0]!["label"], "QUERY order");
  assert.equal(lines[0]!["expected"], ORDER);
  assert.equal(lines[0]!["file"], RESOLVERS);
  assert.deepEqual(await r.edit(RESOLVERS, RESOLVERS_TEXT), [], "reverted: silent");
});

test("an HTTP file's overlay text is the same with or without GraphQL rows in the service (P18)", async (t) => {
  const withGql = (await rig(t, rowsOf(true), TABLE)) as Rig;
  const without = (await rig(t, rowsOf(false), null)) as Rig;
  assert.equal(shadowText(withGql, HTTP), shadowText(without, HTTP));
  assert.deepEqual(without.asked(), [{ graphql: null }], "a service with no GraphQL row sends no table");
});

test("a GraphQL group whose expected type is a top type or bytes is unarmed top_type; the position's other group still arms", async (t) => {
  const rows = rowsOf(true);
  const order = rows.find((row) => row["label"] === "QUERY order")!;
  const groups = order["groups"] as Wire[];
  order["groups"] = [{ ...groups[0]!, type: "any" }, { ...groups[0]!, type: ORDER }];
  const r = (await rig(t, rows, TABLE)) as Rig;
  const placed = r.placed();
  assert.deepEqual(placed.unarmed.map((u) => `${u["label"]} g${u["group"]}:${u["reason"]}`), ["QUERY order g0:top_type"]);
  assert.ok(placed.armed.some((a) => a["label"] === "QUERY order" && a["group"] === 1), JSON.stringify(placed.armed));
});

test("a table the engine refuses, or none, leaves the GraphQL rows unarmed with one reason; the HTTP row arms as before", async (t) => {
  const bad = { Order: { fields: { id: { type: "ID", list: false, nullable: true } } } };
  for (const [name, table, asks] of [["refused", bad, 2], ["missing", null, 1]] as const) {
    const r = (await rig(t, rowsOf(true), table)) as Rig;
    assert.deepEqual(summary(r.placed().armed), ["http GET /ok#0"], name);
    assert.deepEqual(summary(r.placed().unarmed), ["graphql MUTATION createOrder#0:graphql_table_unusable", "graphql QUERY order#0:graphql_table_unusable"], name);
    assert.equal(r.asked().length, asks, `${name}: one engine run, and one more without the table when it refuses it`);
    assert.equal(r.asked().at(-1)!.graphql, null, name);
    assert.ok(!shadowText(r, RESOLVERS).includes("__C."), `${name}: no check is placed in the resolver file`);
    assert.deepEqual(await r.edit(RESOLVERS, RESOLVERS_TEXT.replace('"o1"', '"BROKEN"')), [], `${name}: silent, and silence states nothing`);
  }
});

test("a service with only GraphQL rows and a refused table starts no held compiler", async (t) => {
  const rows = rowsOf(true).filter((row) => row["protocol"] === "graphql");
  const r = await rig(t, rows, { Order: { fields: { id: "ID" } } }, false);
  assert.deepEqual(summary(r.placed().unarmed), ["graphql MUTATION createOrder#null:graphql_table_unusable", "graphql QUERY order#null:graphql_table_unusable"]);
});

test("a changed table unarms every GraphQL check with graphql_table_changed until the next start; HTTP keeps its arm (P15)", async (t) => {
  const r = (await rig(t, rowsOf(true), TABLE)) as Rig;
  await r.facts(rowsOf(true), { ...TABLE, Extra: { fields: { n: { scalar: "Int" } } } });
  const lines = await r.edit(RESOLVERS, RESOLVERS_TEXT.replace('"o1"', '"BROKEN"'));
  assert.deepEqual(lines, [], "the table changed: no GraphQL line");
  assert.deepEqual(summary(r.placed().armed), ["http GET /ok#0"]);
  assert.deepEqual(summary(r.placed().unarmed), ["graphql MUTATION createOrder#0:graphql_table_changed", "graphql QUERY order#0:graphql_table_changed"]);
  assert.equal(r.asked().length, 1, "no engine run per refresh");
});
