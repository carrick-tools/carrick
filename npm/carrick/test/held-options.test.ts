// B2: the held check is armed only under the options the judge checks under,
// and states an HTTP response under the judge's reading of an absent body.
// The real compiler engine writes each shadow; no held compiler is ever
// started for a service the gate leaves unarmed. Every name here is invented.

import test from "node:test";
import assert from "node:assert/strict";
import { execFileSync } from "node:child_process";
import fs from "node:fs";
import os from "node:os";
import path from "node:path";
import type { ServiceInit } from "../src/held/daemon.ts";
import { checkWire } from "../src/held/place.ts";
import { placedPath } from "../src/held/paths.ts";
import { createRuntime } from "../src/held/runtime.ts";
import { engineEntry, writeShadow } from "../src/held/shadow.ts";
import type { ContractRow } from "../src/facts/store.ts";

const entry = engineEntry();
assert.ok(entry !== null, "the compiler engine is not built (run `npm run build` in src/sidecar)");

function tree(t: { after: (fn: () => void) => void }, compilerOptions: Record<string, unknown>): { root: string; home: string; init: ServiceInit } {
  const dir = fs.realpathSync(fs.mkdtempSync(path.join(os.tmpdir(), "held-options-")));
  t.after(() => fs.rmSync(dir, { recursive: true, force: true }));
  const root = path.join(dir, "ws");
  fs.mkdirSync(path.join(root, "src"), { recursive: true });
  fs.writeFileSync(path.join(root, "src", "a.ts"), "export const a = 1;\n");
  fs.writeFileSync(path.join(root, "tsconfig.json"), JSON.stringify({ compilerOptions: { noEmit: true, ...compilerOptions }, include: ["src"] }));
  return { root, home: path.join(dir, "home"), init: { repo: "shop", service: "orders-api", repo_path: root, sidecar_root: root, tsconfig: null, scan_root: root } };
}

const row = (label: string): ContractRow =>
  ({
    side: "producer", protocol: "http", label, direction: "response", service: "orders-api", repo: "shop", file: "src/a.ts", line: 1,
    name: null, site: null, locator: "call", values: [{ file: "src/a.ts", span: [0, 5] }],
    groups: [{ type: "{ a: number }", symbol: null, places: [] }], untyped: [], total: 1,
  }) as unknown as ContractRow;

/** The service's rows as the runtime records them: each unarmed row with its reason, and whether a compiler was started. */
async function started(t: { after: (fn: () => void) => void }, compilerOptions: Record<string, unknown>): Promise<{ unarmed: string[]; compilerStarted: boolean }> {
  const { root, home, init } = tree(t, compilerOptions);
  let compilerStarted = false;
  const runtime = createRuntime({
    root, home, entry, env: process.env, rows: async () => [row("GET /a"), row("GET /b")], commits: async () => ({ shop: "x" }), stamp: () => "fixed",
    makeEngine: () => {
      compilerStarted = true;
      throw new Error("a held compiler must not start here");
    },
  });
  try {
    await runtime.start(init);
  } catch (error) {
    assert.ok(compilerStarted, String(error));
  }
  const file = placedPath(root, init.service, home);
  const placed = fs.existsSync(file) ? (JSON.parse(fs.readFileSync(file, "utf8")) as { unarmed: Array<{ label: string; reason: string }> }) : { unarmed: [] };
  return { unarmed: placed.unarmed.map((u) => `${u.label}:${u.reason}`).sort(), compilerStarted };
}

const DIFFER = ["GET /a:options_differ", "GET /b:options_differ"];

test("a service that is not strict is left unarmed, and its compiler is never started", async (t) => {
  assert.deepEqual(await started(t, { strict: false }), { unarmed: DIFFER, compilerStarted: false });
  assert.deepEqual(await started(t, {}), { unarmed: DIFFER, compilerStarted: false });
  assert.deepEqual(await started(t, { strict: true, strictNullChecks: false }), { unarmed: DIFFER, compilerStarted: false });
});

test("a service that turns strictFunctionTypes off, or exactOptionalPropertyTypes on, is left unarmed", async (t) => {
  assert.deepEqual(await started(t, { strict: true, strictFunctionTypes: false }), { unarmed: DIFFER, compilerStarted: false });
  assert.deepEqual(await started(t, { strict: true, exactOptionalPropertyTypes: true }), { unarmed: DIFFER, compilerStarted: false });
});

test("strict alone, or with an option that changes only the type an expression has, is armed as today", async (t) => {
  for (const options of [{ strict: true }, { strict: true, noUncheckedIndexedAccess: true }]) {
    const r = await started(t, options);
    assert.ok(!r.unarmed.some((u) => u.endsWith("options_differ")), JSON.stringify(r));
    assert.equal(r.compilerStarted, true, JSON.stringify(options));
  }
});

test("an HTTP response that can send no body is read as the judge reads it; a real mismatch breaks either way", async (t) => {
  const { root, home, init } = tree(t, { strict: true });
  const shadow = await writeShadow(entry!, init, path.join(home, "s"), process.env, home);
  assert.ok(shadow.native_tsc !== null, "the native compiler is not installed");
  const text = (sent: string, expected: string): string =>
    [...checkWire(shadow.wire, shadow.tolerances.declarations), `declare function send1<V>(v: V, c: (o: __carrick_Sent<V, ${expected}>) => ${expected}): V;`, `declare const v: ${sent};`, "send1(v, (o) => o);"].join("\n") + "\n";
  const errors = (sent: string, expected: string): string[] => {
    const file = path.join(root, "probe.ts");
    fs.writeFileSync(file, text(sent, expected));
    try {
      execFileSync(shadow.native_tsc!, ["--noEmit", "--strict", "--target", "es2022", "--lib", "es2022", "--ignoreConfig", "--pretty", "false", file], { encoding: "utf8", cwd: root });
      return [];
    } catch (error) {
      return String((error as { stdout?: string }).stdout).split("\n").filter((l) => /error TS\d+/.test(l));
    }
  };
  // The judge's absent-body rule reads a request, a socket or a pub/sub payload, never an HTTP response: no drop here.
  assert.ok(errors("{ a: number } | void", "{ a?: number } | undefined").length > 0, "no absent-body drop on a response");
  assert.deepEqual(errors("{ a: number } | undefined", "{ a: number } | undefined"), []);
  assert.ok(errors("{ a: string } | undefined", "{ a: number } | undefined").length > 0, "a real mismatch breaks");
  assert.deepEqual(errors("{ a: number }", "{ a: number }"), []);
});
