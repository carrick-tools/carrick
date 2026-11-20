// HM-3: the held runtime arms socket emitter rows (the send form) beside HTTP
// and GraphQL rows. A fake compiler engine answers `write_shadow`, and a fake
// held compiler answers diagnostics from the overlay text, so no TypeScript 7
// is needed. The rows come from a fact file built from the wire: the emitter
// row gets its payload position from the listener row's place (`messageSendRows`).
// Every name is invented: service `orders`, event `order:update`.

import test from "node:test";
import assert from "node:assert/strict";
import { execFileSync } from "node:child_process";
import fs from "node:fs";
import os from "node:os";
import path from "node:path";
import ts from "typescript";
import type { ServiceEngine, ServiceInit } from "../src/held/daemon.ts";
import type { Diag, Engine } from "../src/held/engine.ts";
import { earlyResult } from "../src/held/early.ts";
import { placedPath } from "../src/held/paths.ts";
import { createRuntime } from "../src/held/runtime.ts";
import { buildFacts, factsPath } from "../src/facts/store.ts";

const REPO = "shop";
const SERVICE = "orders";
const ORDERS = "src/orders.ts";
const HTTP = "src/http.ts";
const ORDERS_TEXT = [
  "declare const socket: { emit(event: string, payload?: unknown): boolean };",
  "export const resolvers = {",
  '  order: async () => ({ id: "o1", total: 3 }),',
  "};",
  "export function push(): void {",
  '  socket.emit("order:update", { id: "o1", status: "paid" });',
  "}",
  "export function pushAgain(): void {",
  '  socket.emit("order:update", { id: "o2", status: "open" });',
  "}",
  "",
].join("\n");
const HTTP_TEXT = "export const reply = () => ({ ok: true });\n";
const EVENT = "CLIENT->SERVER order:update";
const ORDER_UPDATE = "{ id: string; status: string }";

/** The engine's message rules, cut to what the fake compiler reads. */
const MSG_LINES = ["type __carrick_IsAny<T> = false;", "type __carrick_MsgSent<S, E> = S;"];
const GQL_LINES = ["type __carrick_GqlExpected<E> = E;", "type __carrick_GqlSent<S, E> = S;"];
const TABLE = { Query: { fields: { order: { type: "Order" } } }, Order: { fields: { id: { scalar: "ID" } } } };

/** A fake compiler engine: answers `init` and `write_shadow`; `FAKE_NO_MESSAGE` answers as an engine built before the message rules. */
const FAKE_ENGINE = `
import fs from "node:fs";
import path from "node:path";
const lines = fs.readFileSync(0, "utf8").split("\\n").filter(Boolean).map((l) => JSON.parse(l));
for (const r of lines) {
  if (r.action === "init") { console.log(JSON.stringify({ request_id: r.request_id, status: "ready" })); continue; }
  if (r.action !== "write_shadow") continue;
  fs.mkdirSync(path.join(r.out_dir, "w"), { recursive: true });
  fs.writeFileSync(path.join(r.out_dir, "w", "tsconfig.json"), "{}");
  const fit = { strictNullChecks: true, strictFunctionTypes: true, exactOptionalPropertyTypes: false };
  console.log(JSON.stringify({
    request_id: r.request_id, status: "success", tsconfig: path.join(r.out_dir, "w", "tsconfig.json"),
    own_files: ${JSON.stringify([ORDERS, HTTP])}, native_tsc: "/bin/true",
    wire: ["type __carrick_Wire<S, E> = S;"], tolerances: { unwidened_alias_suffix: "", declarations: ["type __carrick_IsVoid<E> = false;"] },
    options: { service: fit, judge: fit },
    ...(process.env.FAKE_NO_MESSAGE ? {} : { message: ${JSON.stringify(MSG_LINES)} }),
    ...(r.graphql ? { graphql: { lines: ${JSON.stringify(GQL_LINES)}, types_decl: null, leaves: {} } } : {}),
  }));
}
`;

const POST = ", (o) => o)";

/**
 * A fake held compiler. Per check: a twin speaks at the check's start; a value
 * holding `BROKEN` speaks at the callback body. For a message check the
 * message names the listener's expanded type, never the alias (as the compiler
 * does for 2 of 13 planted breaks, step 0), so only the callback position
 * attributes it. Any other form's check never breaks here.
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
        const message = text.includes(`c: (o: __carrick_MsgSent<V, O${k}>) => O${k}`);
        if (new RegExp(`type O${k} = .*__carrick_never`).test(text)) diags.push({ range: at(m.index), code: 2322, message: `Type '{}' is not assignable to type 'O${k}'.` });
        else if (message && value.includes("BROKEN")) diags.push({ range: at(post + POST.indexOf("=> o") + 3), code: 2322, message: "Type 'string' is not assignable to type '{ id: number; }'." });
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
const lineOf = (text: string, snippet: string): number => text.slice(0, text.indexOf(snippet)).split("\n").length;

type Wire = Record<string, unknown>;

const groupOf = (type: string, places: Wire[]): Wire => ({ type, symbol: null, home: null, places, total: places.length, shown: places.length, lead: "", more: "", tail: "", shortened: false });
const placeOf = (service: string, file: string, line: number, span: [number, number] | null): Wire => ({
  service, repo: REPO, file, line, loc: `${file}:${line}`, alias: "A", infer_kind: "call_result", span, symbol: null, home: null,
});
const common = { direction: "response", repo: REPO, name: null, site: null, untyped: [] };

const PAYLOADS = ['{ id: "o1", status: "paid" }', '{ id: "o2", status: "open" }'];

/** The listener row (another service) and one emitter row per emit; `types` are the emitter rows' groups (one per listener). */
function socketRows(opts: { types?: string[]; spans?: boolean; text?: string; payloads?: string[]; callLine?: (payload: string) => number } = {}): Wire[] {
  const types = opts.types ?? [ORDER_UPDATE];
  const text = opts.text ?? ORDERS_TEXT;
  const payloads = opts.payloads ?? PAYLOADS;
  const callLine = opts.callLine ?? ((p: string) => lineOf(text, p));
  const places = payloads.map((p) => placeOf(SERVICE, ORDERS, callLine(p), opts.spans === false ? null : spanOf(text, p)));
  const listener = { k: "contract", side: "producer", protocol: "socket", label: EVENT, service: "listener", file: "src/listen.ts", line: 3, locator: "registration", ...common, groups: [groupOf("{ id: string; status: string }", places)], total: 1, values: [{ file: "src/listen.ts", span: [10, 20] }] };
  const emitters = payloads.map((p) => ({
    k: "contract", side: "consumer", protocol: "socket", label: EVENT, service: SERVICE, file: ORDERS, line: callLine(p), locator: "call", ...common,
    groups: types.map((type) => groupOf(type, [placeOf("listener", "src/listen.ts", 3, null)])), total: types.length,
  }));
  return [listener, ...emitters];
}

function httpRow(): Wire {
  return { k: "contract", side: "producer", protocol: "http", label: "GET /ok", service: SERVICE, file: HTTP, line: 1, locator: "registration", ...common, groups: [groupOf("{ ok: boolean }", [placeOf("web", "web/ok.ts", 4, null)])], total: 1, values: [{ file: HTTP, span: spanOf(HTTP_TEXT, "{ ok: true }") }] };
}

function graphqlRow(): Wire {
  return {
    k: "contract", side: "producer", protocol: "graphql", label: "QUERY order", service: SERVICE, file: ORDERS, line: 3, locator: "registration", ...common,
    groups: [groupOf("{ order: { id: string } }", [placeOf("web", "web/order.ts", 4, null)])], total: 1,
    values: [{ file: ORDERS, span: spanOf(ORDERS_TEXT, '{ id: "o1", total: 3 }') }], graphql_root: { type: "Query", field: "order" },
  };
}

type Rig = {
  root: string;
  service: ServiceEngine;
  compiler: FakeCompiler;
  placed: () => { armed: Wire[]; unarmed: Wire[] };
  edit: (rel: string, text: string) => Promise<Array<Record<string, unknown>>>;
};

async function rig(t: { after: (fn: () => void | Promise<void>) => void }, rows: Wire[], table: unknown = null, extraEnv: Record<string, string> = {}, ordersText: string = ORDERS_TEXT): Promise<Rig> {
  const dir = fs.realpathSync(fs.mkdtempSync(path.join(os.tmpdir(), "held-msg-runtime-")));
  t.after(() => fs.rmSync(dir, { recursive: true, force: true }));
  const root = path.join(dir, "ws");
  const home = path.join(dir, "home");
  fs.mkdirSync(path.join(root, "src"), { recursive: true });
  fs.writeFileSync(path.join(root, ORDERS), ordersText);
  fs.writeFileSync(path.join(root, HTTP), HTTP_TEXT);
  git(root, "init", "-q");
  git(root, "add", "-A");
  git(root, "commit", "-q", "-m", "index");
  const commit = git(root, "rev-parse", "HEAD");
  fs.mkdirSync(path.join(root, ".carrick", "hosted"), { recursive: true });
  fs.writeFileSync(path.join(root, ".carrick", "hosted", "snapshot.json"), JSON.stringify({ resolution: { repos: [{ project_slug: "shop" }] } }));
  const header = { schema: 1, writer: 12, project_id: "p", project_slug: "shop", digest: "d", built_at: "2026-01-01T00:00:00Z", repos: [{ repo: REPO, service: SERVICE, commit }] };
  const lines = [header, ...rows, ...(table === null ? [] : [{ k: "gqltypes", repo: REPO, service: SERVICE, types: table }])];
  await buildFacts(lines.map((l) => JSON.stringify(l)).join("\n") + "\n", factsPath(root), [{ repo: REPO, path: root }], new Date().toISOString());
  const entry = path.join(dir, "engine.mjs");
  fs.writeFileSync(entry, FAKE_ENGINE);
  const compiler = new FakeCompiler();
  const init: ServiceInit = { repo: REPO, service: SERVICE, repo_path: root, sidecar_root: root, tsconfig: null, scan_root: root };
  const runtime = createRuntime({ root, home, entry, env: { ...process.env, ...extraEnv }, site: async () => ({ reason: "value_edited" }), makeEngine: () => compiler });
  const started = await runtime.start(init);
  assert.ok("engine" in started, JSON.stringify(started));
  const service = started.engine;
  t.after(() => service.close());
  const placed = (): { armed: Wire[]; unarmed: Wire[] } => JSON.parse(fs.readFileSync(placedPath(root, SERVICE, home), "utf8")) as { armed: Wire[]; unarmed: Wire[] };
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
  return { root, service, compiler, placed, edit };
}

const shadowText = (r: Rig, rel: string): string => [...r.compiler.texts].find(([file]) => file.endsWith(rel))![1];
const summary = (entries: Wire[]): string[] => entries.map((e) => `${e["protocol"]} ${e["label"]} g${e["group"]}#${e["position"]}${e["reason"] ? `:${e["reason"]}` : ""}`).sort();
const count = (text: string, needle: string): number => text.split(needle).length - 1;

test("send form wraps the payload (runtime: an emitter row placed from the listener's place)", async (t) => {
  const r = await rig(t, [httpRow(), ...socketRows()]);
  assert.deepEqual(summary(r.placed().armed), ["http GET /ok g0#0", `socket ${EVENT} g0#0`, `socket ${EVENT} g0#0`]);
  assert.deepEqual(r.placed().unarmed, []);
  const text = shadowText(r, ORDERS);
  assert.equal(count(text, "__carrick_MsgSent<V, O"), 2, "each emit's payload is a message-form check");
  for (const line of MSG_LINES) assert.equal(count(text, line), 1, `the file appends ${line} once`);
  for (const payload of PAYLOADS) assert.match(text, new RegExp(`socket\\.emit\\("order:update", __C\\.send\\d+_0\\(${payload.replace(/[{}]/g, "\\$&")}, \\(o\\) => o\\)\\);`), "each emit's own payload is wrapped, once");
  assert.equal(text.includes("Awaited"), false);
});

test("clean is silent and the twin speaks (runtime): a break is one line, attributed by its callback", async (t) => {
  const r = await rig(t, socketRows());
  assert.equal(r.placed().armed.length, 2);
  const lines = await r.edit(ORDERS, ORDERS_TEXT.replace('status: "paid"', 'status: "BROKEN"'));
  assert.equal(lines.length, 1, JSON.stringify(lines));
  assert.equal(lines[0]!["label"], EVENT);
  assert.equal(lines[0]!["file"], ORDERS);
  assert.equal(lines[0]!["line"], lineOf(ORDERS_TEXT, PAYLOADS[0]!));
  assert.equal(lines[0]!["expected"], ORDER_UPDATE);
  assert.equal(lines[0]!["side"], "consumer");
  assert.doesNotMatch(String(lines[0]!["message"]), /O\d+_\d+/, "the compiler named the expanded type, not the alias");
  // The early answer states the emitter's break as a call item at the emit's line (HM4 T4).
  const early = earlyResult({ file: path.join(r.root, ORDERS), root: r.root }, lines as never);
  assert.deepEqual(early.items?.map((i) => [i.kind, i.line]), [["call", lineOf(ORDERS_TEXT, PAYLOADS[0]!)]]);
  assert.equal(early.recheck, undefined);
  assert.deepEqual(await r.edit(ORDERS, ORDERS_TEXT), [], "reverted: silent");
});

test("one armed key per (row, group, position)", async (t) => {
  const r = await rig(t, socketRows({ types: [ORDER_UPDATE, "{ id: string }"] }));
  const armed = summary(r.placed().armed);
  assert.deepEqual(armed, [`socket ${EVENT} g0#0`, `socket ${EVENT} g0#0`, `socket ${EVENT} g1#0`, `socket ${EVENT} g1#0`]);
  assert.deepEqual(new Set(r.placed().armed.map((a) => `${a["file"]}:${a["group"]}:${a["position"]}`)).size, 2, "two groups at one position per row; the two rows differ by line");
  assert.equal(count(shadowText(r, ORDERS), "__carrick_MsgSent<V, O"), 4, "two rows, two groups each");
});

test("a file with a GraphQL and a message check gets each its own wire", async (t) => {
  const mixed = await rig(t, [httpRow(), graphqlRow(), ...socketRows()], TABLE);
  assert.deepEqual(summary(mixed.placed().armed), ["graphql QUERY order g0#0", "http GET /ok g0#0", `socket ${EVENT} g0#0`, `socket ${EVENT} g0#0`]);
  const text = shadowText(mixed, ORDERS);
  for (const line of [...GQL_LINES, ...MSG_LINES]) assert.equal(count(text, line), 1, `the mixed file appends ${line} once`);
  assert.match(text, /__carrick_GqlSent<Awaited<V>, O\d+_0>/, "the GraphQL check keeps its form");
  assert.equal(count(text, "__carrick_MsgSent<V, O"), 2, "the socket checks keep theirs");
  // An HTTP file appends neither, and is the same text with or without socket rows in the service.
  const http = shadowText(mixed, HTTP);
  assert.ok(!http.includes("__carrick_Gql") && !http.includes("__carrick_Msg"));
  const without = await rig(t, [httpRow()]);
  assert.equal(shadowText(without, HTTP), http);
  // A GraphQL-only file appends no message rule.
  const gqlOnly = await rig(t, [graphqlRow()], TABLE);
  assert.ok(!shadowText(gqlOnly, ORDERS).includes("__carrick_Msg"));
});

test("an emitter row with no values places nothing", async (t) => {
  const r = await rig(t, [httpRow(), ...socketRows({ spans: false })]);
  assert.deepEqual(summary(r.placed().armed), ["http GET /ok g0#0"]);
  assert.deepEqual(summary(r.placed().unarmed), [`socket ${EVENT} g0#null:no_values`, `socket ${EVENT} g0#null:no_values`]);
  const orders = [...r.compiler.texts].find(([file]) => file.endsWith(ORDERS));
  assert.ok(orders === undefined || !orders[1].includes("__C."), "no check is placed in the emitter's file");
});

test("a listener typed any places nothing; the position's other group still arms", async (t) => {
  const r = await rig(t, socketRows({ types: ["any", ORDER_UPDATE] }));
  assert.deepEqual(summary(r.placed().unarmed), [`socket ${EVENT} g0#0:top_type`, `socket ${EVENT} g0#0:top_type`]);
  assert.deepEqual(summary(r.placed().armed), [`socket ${EVENT} g1#0`, `socket ${EVENT} g1#0`]);
});

test("an engine without the message rules leaves socket rows unarmed; they never fall to the HTTP form", async (t) => {
  const r = await rig(t, [httpRow(), ...socketRows()], null, { FAKE_NO_MESSAGE: "1" });
  assert.deepEqual(summary(r.placed().armed), ["http GET /ok g0#0"]);
  assert.deepEqual(summary(r.placed().unarmed), [`socket ${EVENT} g0#0:message_rules_missing`, `socket ${EVENT} g0#0:message_rules_missing`]);
  assert.equal(shadowText(r, ORDERS).includes("__C."), false);
});

test("two emits of one event in one file that break together are two lines", async (t) => {
  const r = await rig(t, socketRows());
  const broken = ORDERS_TEXT.replace('status: "paid"', 'status: "BROKEN"').replace('status: "open"', 'status: "BROKEN"');
  assert.equal((await r.edit(ORDERS, ORDERS_TEXT.replace('status: "paid"', 'status: "BROKEN"'))).length, 1, "the first emit breaks first");
  const lines = await r.edit(ORDERS, broken);
  assert.equal(lines.length, 2, JSON.stringify(lines));
  assert.equal(new Set(lines.map((l) => l["row_key"])).size, 2, "two held keys");
  assert.deepEqual(lines.map((l) => l["row_line"]).sort(), PAYLOADS.map((p) => lineOf(ORDERS_TEXT, p)).sort(), "each carries its own index line");
  assert.deepEqual(lines.map((l) => l["line"]).sort(), PAYLOADS.map((p) => lineOf(ORDERS_TEXT, p)).sort());
});

test("placed rows of two emitters of one event differ by line", async (t) => {
  const r = await rig(t, socketRows({ types: [ORDER_UPDATE, "{ id: string }"] }));
  const armed = r.placed().armed;
  assert.equal(new Set(armed.map((a) => `${a["file"]}:${a["line"]}:${a["group"]}:${a["position"]}`)).size, 4, "file:line:group:position has 4 entries");
  assert.deepEqual(new Set(armed.map((a) => a["line"])), new Set(PAYLOADS.map((p) => lineOf(ORDERS_TEXT, p))));
  const none = await rig(t, [httpRow(), ...socketRows({ spans: false })]);
  const unarmed = none.placed().unarmed.filter((u) => u["protocol"] === "socket");
  assert.ok(unarmed.length > 0);
  for (const u of unarmed) assert.equal(typeof u["line"], "number", "an unarmed consumer row carries its line too");
});

test("an HTTP and a GraphQL placed row keep exactly their keys", async (t) => {
  const r = await rig(t, [httpRow(), graphqlRow(), ...socketRows()], TABLE);
  const placed = r.placed();
  const others = [...placed.armed, ...placed.unarmed].filter((e) => e["protocol"] !== "socket");
  assert.equal(others.length, 2, "one HTTP and one GraphQL placed row");
  for (const e of others) assert.deepEqual(Object.keys(e).sort(), ["file", "group", "label", "position", "protocol"], `${e["protocol"]} row keys`);
  const bare = await rig(t, [{ ...httpRow(), groups: [groupOf("any", [placeOf("web", "web/ok.ts", 4, null)])] }, { ...graphqlRow(), groups: [groupOf("any", [placeOf("web", "web/order.ts", 4, null)])] }], TABLE);
  assert.equal(bare.placed().unarmed.length, 1, "the GraphQL row is unarmed (top type)");
  for (const e of bare.placed().unarmed) assert.deepEqual(Object.keys(e).sort(), ["file", "group", "label", "position", "protocol", "reason"], `${e["protocol"]} unarmed row keys`);
});

// HM4 T4: the early answer states an emitter's break as a consumer-side `call`
// item at the emit's line, joined to the full re-check's call row by place.

const early = (r: Rig, lines: Array<Record<string, unknown>>, rel = ORDERS) => earlyResult({ file: path.join(r.root, rel), root: r.root }, lines as never);
const FIRST = lineOf(ORDERS_TEXT, PAYLOADS[0]!);
const SECOND = lineOf(ORDERS_TEXT, PAYLOADS[1]!);

test("an emitter break is a call item at the emit's line, through earlyResult", async (t) => {
  const r = await rig(t, socketRows());
  const lines = await r.edit(ORDERS, ORDERS_TEXT.replace('status: "paid"', 'status: "BROKEN"'));
  assert.equal(lines[0]!["protocol"], "socket");
  assert.equal(lines[0]!["call_line"], FIRST);
  assert.deepEqual(early(r, lines).items, [
    {
      kind: "call",
      method: "CLIENT->SERVER",
      path: "order:update",
      line: FIRST,
      direction: "response",
      verdict: { state: "resolved", result: "type_mismatch", detail: "This code sends 'string'; the listener expects '{ id: number; }'." },
      expected_type: ORDER_UPDATE,
      sent_by: "consumer",
      counterparts: [{ role: "producer", service: "listener", file: "src/listen.ts", line: 3 }],
      stated: true,
      run: lines[0]!["run"],
      main_commit: lines[0]!["main_commit"],
      named_files: [path.join(r.root, "src/listen.ts")],
    },
  ]);
});

test("a line inserted above the emit moves call_line with it", async (t) => {
  const r = await rig(t, socketRows());
  const shifted = ORDERS_TEXT.replace("export function push(): void {\n", "export function push(): void {\n  void 0;\n");
  assert.deepEqual(await r.edit(ORDERS, shifted), [], "the inserted line breaks nothing");
  const lines = await r.edit(ORDERS, shifted.replace('status: "paid"', 'status: "BROKEN"'));
  assert.equal(lines.length, 1, JSON.stringify(lines));
  assert.equal(lines[0]!["row_line"], FIRST, "the held key keeps the index line");
  assert.equal(lines[0]!["call_line"], FIRST + 1, "the call is one line lower now");
  assert.equal(early(r, lines).items?.[0]?.line, FIRST + 1);
});

test("a multi-line emit's call_line is the call's first line, not the payload's last", async (t) => {
  const payload = '{\n    id: "o3",\n    status: "long",\n  }';
  const text = `${ORDERS_TEXT}export function pushLong(): void {\n  socket.emit(\n    "order:update",\n    ${payload},\n  );\n}\n`;
  const call = lineOf(text, "socket.emit(\n");
  const rows = socketRows({ text, payloads: [payload], callLine: () => call });
  const r = await rig(t, rows, null, {}, text);
  const lines = await r.edit(ORDERS, text.replace('status: "long"', 'status: "BROKEN"'));
  assert.equal(lines.length, 1, JSON.stringify(lines));
  assert.ok((lines[0]!["line"] as number) > call + 2, "the compiler speaks past the payload's first line");
  assert.equal(lines[0]!["call_line"], call);
  assert.equal(early(r, lines).items?.[0]?.line, call);
});

test("a type edit in another file gives a moved call row at the emit", async (t) => {
  const r = await rig(t, socketRows());
  // The emit's payload breaks behind the edit (as a type it reads would), and the edit is to another file.
  const orders = path.join(r.root, ORDERS);
  fs.writeFileSync(orders, ORDERS_TEXT.replace('status: "paid"', 'status: "BROKEN"'));
  const later = new Date(Date.now() + 60_000);
  fs.utimesSync(orders, later, later);
  const lines = await r.edit(HTTP, `${HTTP_TEXT}// edited\n`);
  assert.equal(lines.length, 1, JSON.stringify(lines));
  const answer = early(r, lines, HTTP);
  assert.deepEqual(answer.items, []);
  assert.deepEqual(answer.recheck?.moved, [
    {
      file: ORDERS,
      line: FIRST,
      role: "call",
      method: "CLIENT->SERVER",
      path: "order:update",
      before: "compatible",
      after: "type_mismatch",
      counterparts: ["listener src/listen.ts:3"],
      reason: "This code sends 'string'; the listener expects '{ id: number; }'.",
      not_judged_before: false,
      stated: true,
      run: lines[0]!["run"],
      main_commit: lines[0]!["main_commit"],
      named_files: [path.join(r.root, "src/listen.ts")],
    },
  ]);
});

test("a socket emit line is not printed when the engine gave no message rules", async (t) => {
  const r = await rig(t, socketRows(), null, { FAKE_NO_MESSAGE: "1" });
  const lines = await r.edit(ORDERS, ORDERS_TEXT.replace('status: "paid"', 'status: "BROKEN"'));
  assert.deepEqual(lines, []);
  assert.deepEqual(early(r, lines).items, []);
});

test("a consumer line with no call line is not printed (part 2 could not join it)", () => {
  const root = fs.realpathSync(os.tmpdir());
  const line = { row_key: "k", group: 0, label: EVENT, consumer: "listener", file: ORDERS, line: 6, message: "Type 'string' is not assignable to type '{ id: number; }'.", row_file: ORDERS, row_line: 6, row_path: path.join(root, ORDERS), places: [{ service: "listener", file: "src/listen.ts", line: 3 }], expected: ORDER_UPDATE, main_commit: null, named_files: [], side: "consumer" as const, protocol: "socket" };
  for (const call_line of [undefined, null]) assert.deepEqual(earlyResult({ file: path.join(root, ORDERS), root }, [{ ...line, call_line }]).items, [], String(call_line));
  assert.equal(earlyResult({ file: path.join(root, ORDERS), root }, [{ ...line, call_line: 6 }]).items?.length, 1);
});
