// A break is said once, as the edit's that caused it (T1, carrick#4728): the
// held runtime marks a line `standing` when its check spoke the same message at
// the engine's previous edit answer, and the early answer keeps the row with
// that mark. A fake compiler engine answers `write_shadow` and a fake held
// compiler answers diagnostics from the overlay text, so no TypeScript 7 is
// needed. Every name is invented: service `orders`, routes `GET /a`, `GET /b`.

import test from "node:test";
import assert from "node:assert/strict";
import { execFileSync } from "node:child_process";
import fs from "node:fs";
import os from "node:os";
import path from "node:path";
import ts from "typescript";
import type { HeldLine, ServiceInit } from "../src/held/daemon.ts";
import type { Diag, Engine } from "../src/held/engine.ts";
import { earlyResult } from "../src/held/early.ts";
import { createRuntime } from "../src/held/runtime.ts";
import { buildFacts, factsPath } from "../src/facts/store.ts";

const REPO = "shop";
const SERVICE = "orders";
const A = "src/a.ts";
const B = "src/b.ts";
const A_TEXT = 'export const a = () => ({ ok: true });\n';
const B_TEXT = 'export const b = () => ({ ok: true });\n';

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
    own_files: ${JSON.stringify([A, B])}, native_tsc: "/bin/true",
    wire: ["type __carrick_Wire<S, E> = S;"], tolerances: { unwidened_alias_suffix: "", declarations: ["type __carrick_IsVoid<E> = false;"] },
    options: { service: fit, judge: fit },
  }));
}
`;

const POST = ", (o) => o)";

/**
 * A fake held compiler: a twin speaks at the check's start; a value holding
 * `BROKEN` speaks there too, naming the check's alias, with the value's own
 * text in the message, so two breaks of one check can differ.
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
        const value = text.slice(m.index + m[0].length, text.indexOf(POST, m.index));
        const broke = /BROKEN\w*/.exec(value)?.[0];
        if (new RegExp(`type O${k} = .*__carrick_never`).test(text)) diags.push({ range: at(m.index), code: 2322, message: `Type '{}' is not assignable to type 'O${k}'.` });
        else if (broke) diags.push({ range: at(m.index), code: 2322, message: `Type '{ ${broke}: boolean; }' is not assignable to type 'O${k}'.` });
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
const place = (file: string, line: number): Wire => ({ service: "web", repo: REPO, file, line, loc: `${file}:${line}`, alias: "A", infer_kind: "call_result", span: null, symbol: null, home: null });
const group = (places: Wire[]): Wire => ({ type: "{ ok: boolean }", symbol: null, home: null, places, total: places.length, shown: places.length, lead: "", more: "", tail: "", shortened: false });
const route = (label: string, file: string, text: string): Wire => ({
  k: "contract", side: "producer", protocol: "http", label, direction: "response", repo: REPO, service: SERVICE, file, line: 1, name: null, site: null,
  locator: "registration", untyped: [], groups: [group([place(`web/${label.slice(5)}.ts`, 4)])], total: 1, values: [{ file, span: spanOf(text, "{ ok: true }") }],
});

/** `filed: false` is an answer the daemon did not file (a superseded ask): it is never committed. */
type Rig = { root: string; edit: (rel: string, text: string, options?: { filed?: boolean }) => Promise<HeldLine[]> };

async function rig(t: { after: (fn: () => void | Promise<void>) => void }): Promise<Rig> {
  const dir = fs.realpathSync(fs.mkdtempSync(path.join(os.tmpdir(), "held-standing-")));
  t.after(() => fs.rmSync(dir, { recursive: true, force: true }));
  const root = path.join(dir, "ws");
  const home = path.join(dir, "home");
  fs.mkdirSync(path.join(root, "src"), { recursive: true });
  fs.writeFileSync(path.join(root, A), A_TEXT);
  fs.writeFileSync(path.join(root, B), B_TEXT);
  git(root, "init", "-q");
  git(root, "add", "-A");
  git(root, "commit", "-q", "-m", "index");
  const commit = git(root, "rev-parse", "HEAD");
  fs.mkdirSync(path.join(root, ".carrick", "hosted"), { recursive: true });
  fs.writeFileSync(path.join(root, ".carrick", "hosted", "snapshot.json"), JSON.stringify({ resolution: { repos: [{ project_slug: "shop" }] } }));
  const header = { schema: 1, writer: 12, project_id: "p", project_slug: "shop", digest: "d", built_at: "2026-01-01T00:00:00Z", repos: [{ repo: REPO, service: SERVICE, commit }] };
  const lines = [header, route("GET /a", A, A_TEXT), route("GET /b", B, B_TEXT)];
  await buildFacts(lines.map((l) => JSON.stringify(l)).join("\n") + "\n", factsPath(root), [{ repo: REPO, path: root }], new Date().toISOString());
  const entry = path.join(dir, "engine.mjs");
  fs.writeFileSync(entry, FAKE_ENGINE);
  const compiler = new FakeCompiler();
  const init: ServiceInit = { repo: REPO, service: SERVICE, repo_path: root, sidecar_root: root, tsconfig: null, scan_root: root };
  const runtime = createRuntime({ root, home, entry, env: { ...process.env }, site: async () => ({ reason: "value_edited" }), makeEngine: () => compiler });
  const started = await runtime.start(init);
  assert.ok("engine" in started, JSON.stringify(started));
  const service = started.engine;
  t.after(() => service.close());
  let n = 0;
  const edit = async (rel: string, text: string, options: { filed?: boolean } = {}): Promise<HeldLine[]> => {
    const file = path.join(root, rel);
    fs.writeFileSync(file, text);
    const later = new Date(Date.now() + 5000 * ++n);
    fs.utimesSync(file, later, later);
    const answer = await service.answer([file], { session: `s${n}`, files: [file], kind: "edit", edited_at: Date.now() });
    // As the daemon does: what an answer spoke counts only once it is filed.
    if (options.filed !== false) answer.commit?.();
    await answer.after?.();
    return answer.lines;
  };
  return { root, edit };
}

const breakA = A_TEXT.replace("ok: true", "BROKEN: true");
const summary = (lines: HeldLine[]): Array<[string, boolean]> => lines.map((l) => [l.label, l.standing === true]);

test("a break is new at its edit, standing at an unrelated edit, gone when fixed, new again when it breaks again", async (t) => {
  const r = await rig(t);
  const first = await r.edit(A, breakA);
  assert.deepEqual(summary(first), [["GET /a", false]], "this edit's break");
  const second = await r.edit(B, `${B_TEXT}// a comment\n`);
  assert.deepEqual(summary(second), [["GET /a", true]], "an edit of another file lists the break, standing");
  assert.equal(second[0]!.run, first[0]!.run, "one run while it stands");
  const third = await r.edit(B, `${B_TEXT}// another comment\n`);
  assert.deepEqual(summary(third), [["GET /a", true]], "still standing at a further edit");
  assert.deepEqual(await r.edit(A, A_TEXT), [], "fixed: silent");
  const again = await r.edit(A, breakA);
  assert.deepEqual(summary(again), [["GET /a", false]], "broke again: this edit's");
  assert.notEqual(again[0]!.run, first[0]!.run, "a new run");
});

test("a check that speaks another message at the next edit is that edit's break", async (t) => {
  const r = await rig(t);
  await r.edit(A, breakA);
  const other = await r.edit(A, A_TEXT.replace("ok: true", "BROKEN_TWICE: true"));
  assert.deepEqual(summary(other), [["GET /a", false]]);
});

test("an answer that was not filed (a superseded ask) leaves the last filed one: the break is new at the next edit", async (t) => {
  const r = await rig(t);
  await r.edit(A, breakA, { filed: false });
  const next = await r.edit(B, `${B_TEXT}// a comment\n`);
  assert.deepEqual(summary(next), [["GET /a", false]], "never filed, so never standing");
  const after = await r.edit(B, `${B_TEXT}// another comment\n`);
  assert.deepEqual(summary(after), [["GET /a", true]]);
});

test("the early answer keeps a standing break, marked, beside this edit's own", async (t) => {
  const r = await rig(t);
  await r.edit(A, breakA);
  const lines = await r.edit(B, B_TEXT.replace("ok: true", "BROKEN: true"));
  const early = earlyResult({ file: path.join(r.root, B), root: r.root }, lines);
  assert.deepEqual(early.items?.map((i) => [i.path, i.standing === true]), [["/b", false]], "B's own break is this edit's");
  assert.deepEqual(early.recheck?.moved?.map((m) => [m.path, m.standing === true, m.stated]), [["/a", true, true]], "A's break stays listed, standing and stated");
});
