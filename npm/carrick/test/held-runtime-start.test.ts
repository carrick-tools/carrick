// N2: the start gate arms each value file on its text as it was when the start
// began, so an edit made while the engine starts is judged by the warm path on
// the next ask, and a break that was on disk before the start stays silent.
// A fake compiler engine and a fake held compiler, as held-runtime-message. Invented names.

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
let scanRoot = "";
for (const r of lines) {
  if (r.action === "init") { scanRoot = r.scan_root; console.log(JSON.stringify({ request_id: r.request_id, status: "ready" })); continue; }
  if (r.action !== "write_shadow") continue;
  if (process.env.FAKE_EDIT_FILE) fs.writeFileSync(process.env.FAKE_EDIT_FILE, process.env.FAKE_EDIT_TEXT);
  fs.mkdirSync(path.join(r.out_dir, "w"), { recursive: true });
  fs.writeFileSync(path.join(r.out_dir, "w", "tsconfig.json"), "{}");
  for (const own of ${JSON.stringify([ORDERS, HTTP])}) {
    fs.mkdirSync(path.dirname(path.join(r.out_dir, "w", own)), { recursive: true });
    fs.copyFileSync(path.join(scanRoot, own), path.join(r.out_dir, "w", own));
  }
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


const BROKEN = ORDERS_TEXT.replace('status: "paid"', 'status: "BROKEN"');

/** Start the runtime. `during`: the text a file gets while the start runs (inside the engine's shadow write); `before`: the text it has before the start. */
async function begin(t: { after: (fn: () => void | Promise<void>) => void }, opts: { before?: string; during?: string }) {
  const dir = fs.realpathSync(fs.mkdtempSync(path.join(os.tmpdir(), "held-start-runtime-")));
  t.after(() => fs.rmSync(dir, { recursive: true, force: true }));
  const root = path.join(dir, "ws");
  const home = path.join(dir, "home");
  fs.mkdirSync(path.join(root, "src"), { recursive: true });
  fs.writeFileSync(path.join(root, ORDERS), ORDERS_TEXT);
  fs.writeFileSync(path.join(root, HTTP), HTTP_TEXT);
  git(root, "init", "-q");
  git(root, "add", "-A");
  git(root, "commit", "-q", "-m", "index");
  const commit = git(root, "rev-parse", "HEAD");
  if (opts.before !== undefined) fs.writeFileSync(path.join(root, ORDERS), opts.before);
  fs.mkdirSync(path.join(root, ".carrick", "hosted"), { recursive: true });
  fs.writeFileSync(path.join(root, ".carrick", "hosted", "snapshot.json"), JSON.stringify({ resolution: { repos: [{ project_slug: "shop" }] } }));
  const header = { schema: 1, writer: 12, project_id: "p", project_slug: "shop", digest: "d", built_at: "2026-01-01T00:00:00Z", repos: [{ repo: REPO, service: SERVICE, commit }] };
  await buildFacts([header, ...socketRows()].map((l) => JSON.stringify(l)).join("\n") + "\n", factsPath(root), [{ repo: REPO, path: root }], new Date().toISOString());
  const entry = path.join(dir, "engine.mjs");
  fs.writeFileSync(entry, FAKE_ENGINE);
  const extra = opts.during === undefined ? {} : { FAKE_EDIT_FILE: path.join(root, ORDERS), FAKE_EDIT_TEXT: opts.during };
  const init: ServiceInit = { repo: REPO, service: SERVICE, repo_path: root, sidecar_root: root, tsconfig: null, scan_root: root };
  const runtime = createRuntime({ root, home, entry, env: { ...process.env, ...extra }, makeEngine: () => new FakeCompiler() });
  const started = await runtime.start(init);
  assert.ok("engine" in started, JSON.stringify(started));
  const service: ServiceEngine = started.engine;
  t.after(() => service.close());
  const placed = (): { armed: Wire[]; unarmed: Wire[] } => JSON.parse(fs.readFileSync(placedPath(root, SERVICE, home), "utf8")) as { armed: Wire[]; unarmed: Wire[] };
  const ask = async (): Promise<Array<Record<string, unknown>>> => {
    const file = path.join(root, ORDERS);
    const answer = await service.answer([file], { session: "s1", files: [file], kind: "edit", edited_at: Date.now() });
    await answer.after?.();
    return answer.lines as unknown as Array<Record<string, unknown>>;
  };
  return { placed, ask };
}

test("the start gate arms on the text at the start, not an edit made during it", async (t) => {
  const r = await begin(t, { during: BROKEN });
  assert.equal(r.placed().armed.length, 2, JSON.stringify(r.placed()));
  assert.deepEqual(r.placed().unarmed, []);
  const lines = await r.ask();
  assert.equal(lines.length, 1, JSON.stringify(lines));
  assert.equal(lines[0]!["label"], EVENT);
  assert.equal(lines[0]!["line"], lineOf(ORDERS_TEXT, PAYLOADS[0]!));
});

test("a break written before the start stays unarmed and silent (no false line)", async (t) => {
  const r = await begin(t, { before: BROKEN });
  assert.deepEqual(r.placed().armed.map((a) => a["line"]), [lineOf(ORDERS_TEXT, PAYLOADS[1]!)], "only the untouched emit arms");
  assert.match(String(r.placed().unarmed[0]!["reason"]), /^clean_check_fails/);
  assert.deepEqual(await r.ask(), []);
});

test("a start with no edit during it places the same rows", async (t) => {
  const r = await begin(t, {});
  assert.equal(r.placed().armed.length, 2);
  assert.deepEqual(r.placed().unarmed, []);
  assert.deepEqual(await r.ask(), []);
});

// A linked worktree's engine started BY an edit (#5038 T2 F1): its first gate judges the edited files at the worktree's HEAD,
// so the edit that started it is judged as an edit and a break in it is stated.

/** Main checkout as `begin` makes it, main's shadow written, then a nested worktree whose ORDERS file is `head` at its HEAD (committed) and `edit` on disk. */
async function beginWorktree(t: { after: (fn: () => void | Promise<void>) => void }, opts: { head?: string; edit: string; edited: boolean }) {
  const dir = fs.realpathSync(fs.mkdtempSync(path.join(os.tmpdir(), "held-start-worktree-")));
  t.after(() => fs.rmSync(dir, { recursive: true, force: true }));
  const root = path.join(dir, "ws");
  const home = path.join(dir, "home");
  fs.mkdirSync(path.join(root, "src"), { recursive: true });
  fs.writeFileSync(path.join(root, ORDERS), ORDERS_TEXT);
  fs.writeFileSync(path.join(root, HTTP), HTTP_TEXT);
  fs.writeFileSync(path.join(root, ".gitignore"), ".claude/worktrees/\n.carrick/\n");
  git(root, "init", "-q", "-b", "main");
  git(root, "add", "-A");
  git(root, "commit", "-q", "-m", "index");
  const commit = git(root, "rev-parse", "HEAD");
  fs.mkdirSync(path.join(root, ".carrick", "hosted"), { recursive: true });
  fs.writeFileSync(path.join(root, ".carrick", "hosted", "snapshot.json"), JSON.stringify({ resolution: { repos: [{ project_slug: "shop" }] } }));
  const header = { schema: 1, writer: 12, project_id: "p", project_slug: "shop", digest: "d", built_at: "2026-01-01T00:00:00Z", repos: [{ repo: REPO, service: SERVICE, commit }] };
  await buildFacts([header, ...socketRows()].map((l) => JSON.stringify(l)).join("\n") + "\n", factsPath(root), [{ repo: REPO, path: root }], new Date().toISOString());
  const entry = path.join(dir, "engine.mjs");
  fs.writeFileSync(entry, FAKE_ENGINE);
  const main: ServiceInit = { repo: REPO, service: SERVICE, repo_path: root, sidecar_root: root, tsconfig: null, scan_root: root };
  const runtime = createRuntime({ root, home, entry, env: { ...process.env }, makeEngine: () => new FakeCompiler() });
  const mainStart = await runtime.start(main);
  assert.ok("engine" in mainStart, JSON.stringify(mainStart));
  t.after(() => mainStart.engine.close());
  git(root, "worktree", "add", "-q", ".claude/worktrees/b", "-b", "b");
  const top = path.join(root, ".claude", "worktrees", "b");
  if (opts.head !== undefined) {
    fs.writeFileSync(path.join(top, ORDERS), opts.head);
    git(top, "commit", "-q", "-am", "branch");
  }
  fs.writeFileSync(path.join(top, ORDERS), opts.edit);
  const file = path.join(top, ORDERS);
  const init: ServiceInit = { ...main, repo_path: top, sidecar_root: top, scan_root: top, checkout: top };
  const started = await runtime.start(init, opts.edited ? { edited: [file] } : {});
  assert.ok("engine" in started, JSON.stringify(started));
  const service: ServiceEngine = started.engine;
  t.after(() => service.close());
  const placed = (): { armed: Wire[]; unarmed: Wire[] } => JSON.parse(fs.readFileSync(placedPath(top, SERVICE, home), "utf8")) as { armed: Wire[]; unarmed: Wire[] };
  const ask = async (): Promise<Array<Record<string, unknown>>> => {
    const answer = await service.answer([file], { session: "s1", files: [file], kind: "edit", edited_at: Date.now() });
    await answer.after?.();
    return answer.lines as unknown as Array<Record<string, unknown>>;
  };
  return { placed, ask, file };
}

test("T2 F1: a break that is a worktree's first edit, and starts its engine, is stated at the first answer", async (t) => {
  const r = await beginWorktree(t, { edit: BROKEN, edited: true });
  assert.equal(r.placed().armed.length, 2, JSON.stringify(r.placed()));
  const lines = await r.ask();
  assert.equal(lines.length, 1, JSON.stringify(lines));
  assert.equal(lines[0]!["row_path"], r.file, "the worktree's file");
});

test("T2 F1: a start that no edit asked for gates the disk text, as before (the break stays unarmed)", async (t) => {
  const r = await beginWorktree(t, { edit: BROKEN, edited: false });
  assert.match(String(r.placed().unarmed[0]?.["reason"]), /^clean_check_fails/);
  assert.deepEqual(await r.ask(), []);
});

test("T2 F1: a row already broken at the worktree's HEAD stays unarmed and says nothing new", async (t) => {
  const r = await beginWorktree(t, { head: BROKEN, edit: `${BROKEN}// another edit\n`, edited: true });
  assert.match(String(r.placed().unarmed[0]?.["reason"]), /^clean_check_fails/);
  assert.deepEqual(await r.ask(), []);
});
