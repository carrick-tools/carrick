// Part 3, L-G1a: the GraphQL send form of a held check. A resolver's returned
// value is wrapped, the caller's expected result type is held fixed, and the
// check's own diagnostic is found by its callback position. Every name is invented.

import test from "node:test";
import assert from "node:assert/strict";
import fs from "node:fs";
import ts from "typescript";
import { arm, attribute, isTopOrBytes, overlay } from "../src/held/place.ts";
import type { CheckInput, Diag, Value } from "../src/held/place.ts";

const dir = new URL("./fixtures/held/", import.meta.url);
const RESOLVERS = fs.readFileSync(new URL("resolvers.ts", dir), "utf8");
const SENDS = fs.readFileSync(new URL("sends.ts", dir), "utf8");
// The engine's GraphQL rules are the compiler engine's; these stand-ins only have to type.
const GQL_WIRE = [
  "type __carrick_Sent<S, E> = S;",
  "type __carrick_GqlExpected<E> = E;",
  "type __carrick_GqlSent<S, E> = S;",
  "type __carrick_GqlSentS<S, E, R> = S;",
];
const group = (type: string): { type: string } => ({ type });
const FORM = { kind: "graphql", leaf: null } as const;
const LEAF = { kind: "graphql", leaf: '{"type":"Order"}' } as const;

const span = (text: string, needle: string, file = "resolvers.ts"): Value => {
  const start = text.indexOf(needle);
  assert.ok(start >= 0, `${needle} is in the text`);
  return { file, start, end: start + needle.length };
};
const ORDER = span(RESOLVERS, "loadOrder(args.id)");
const SHAPE = "{ __typename: 'Order'; id: string; total: number }";

function typecheck(text: string): Diag[] {
  const options: ts.CompilerOptions = { strict: true, noEmit: true, target: ts.ScriptTarget.ES2022, types: [] };
  const host = ts.createCompilerHost(options);
  const original = host.getSourceFile.bind(host);
  host.getSourceFile = (name, ...rest) => (name === "/virtual/resolvers.ts" ? ts.createSourceFile(name, text, ts.ScriptTarget.ES2022, true) : original(name, ...rest));
  const program = ts.createProgram(["/virtual/resolvers.ts"], options, host);
  const sf = program.getSourceFile("/virtual/resolvers.ts")!;
  return program.getSemanticDiagnostics(sf).map((d) => ({ start: d.start ?? 0, code: d.code, message: ts.flattenDiagnosticMessageText(d.messageText, "\n") }));
}

test("the JSON form wraps the value in Awaited and the callback returns the relaxed expected type", () => {
  const ov = overlay("resolvers.ts", RESOLVERS, [{ n: 2, value: ORDER, groups: [group(SHAPE)], form: FORM }], GQL_WIRE, "clean");
  assert.deepEqual(ov.unplaced, []);
  assert.ok(ov.text.includes("return __C.send2_0(loadOrder(args.id), (o) => o);"));
  assert.ok(ov.text.endsWith(
    `declare namespace __C {\n${GQL_WIRE.map((l) => `  ${l}`).join("\n")}\n` +
      `  type O2_0 = (${SHAPE});\n` +
      `  function send2_0<V>(v: V, c: (o: __carrick_GqlSent<Awaited<V>, O2_0>) => __carrick_GqlExpected<O2_0>): V;\n}\n`,
  ));
});

test("the scalar form adds the root's leaf as L<k> and reads it", () => {
  const ov = overlay("resolvers.ts", RESOLVERS, [{ n: 2, value: ORDER, groups: [group(SHAPE)], form: LEAF }], GQL_WIRE, "twin");
  assert.ok(ov.text.includes(`  type O2_0 = (${SHAPE}) & { readonly __carrick_never: true };\n  type L2_0 = {"type":"Order"};\n`));
  assert.ok(ov.text.includes("c: (o: __carrick_GqlSentS<Awaited<V>, O2_0, L2_0>) => __carrick_GqlExpected<O2_0>"));
  const c = ov.checks[0]!;
  assert.ok(ov.text.slice(c.declSpan[0], c.declSpan[1]).includes("type L2_0 ="), "the declaration span holds the leaf line");
});

test("a check records its span and the offset of its callback body", () => {
  const ov = overlay("resolvers.ts", RESOLVERS, [{ n: 0, value: ORDER, groups: [group(SHAPE)], form: FORM }], GQL_WIRE, "clean");
  const c = ov.checks[0]!;
  assert.equal(ov.text.slice(c.span[0], c.span[1]), "__C.send0_0(loadOrder(args.id), (o) => o)");
  assert.equal(ov.text.slice(c.callback!, c.callback! + 2), "o)");
  assert.equal(ov.text.slice(c.callback! - 5, c.callback!), ") => ");
});

test("a diagnostic is a GraphQL check's line only when it starts at its callback body", () => {
  const ov = overlay("resolvers.ts", RESOLVERS, [{ n: 0, value: ORDER, groups: [group(SHAPE)], form: FORM }], GQL_WIRE, "clean");
  const c = ov.checks[0]!;
  // The message does not name O0_0: the compiler printed the expanded type.
  const own: Diag = { start: c.callback!, code: 2322, message: "Type '{ id: string }' is not assignable to type '{ __typename: \"Order\" }'." };
  const inSpan: Diag = { start: c.span[0] + 12, code: 2322, message: "Type 'x' is not assignable to type 'O0_0'." };
  const got = attribute(ov, [own, inSpan]);
  assert.deepEqual(got.lines.get("0_0"), [own]);
  assert.deepEqual(got.outside, [inSpan], "inside the span, not at the callback: no line, even naming the alias");
});

test("a planted break on real TypeScript is one line at the callback; the unbroken value is silent", () => {
  const input = (type: string): CheckInput[] => [{ n: 0, value: ORDER, groups: [group(type)], form: FORM }];
  const same = overlay("resolvers.ts", RESOLVERS, input("{ id: string; total: number }"), GQL_WIRE, "clean");
  assert.deepEqual(typecheck(same.text), []);
  const broken = overlay("resolvers.ts", RESOLVERS, input("{ id: string; total: number; paid: boolean }"), GQL_WIRE, "clean");
  const got = attribute(broken, typecheck(broken.text));
  assert.equal(got.lines.get("0_0")?.length, 1, JSON.stringify(typecheck(broken.text)));
  assert.deepEqual(got.outside, []);
});

test("arm: a GraphQL check arms when its clean form is silent and its twin speaks", () => {
  const input: CheckInput[] = [{ n: 0, value: ORDER, groups: [group("{ id: string; total: number }")], form: FORM }];
  const clean = overlay("resolvers.ts", RESOLVERS, input, GQL_WIRE, "clean");
  const twin = overlay("resolvers.ts", RESOLVERS, input, GQL_WIRE, "twin");
  assert.deepEqual(arm(clean, typecheck(clean.text), typecheck(twin.text)).armed, ["0_0"]);
});

test("top types stay unarmed: an expected any, unknown, never or bytes places no check, and a sent any arms none", () => {
  for (const type of ["any", "unknown", "never", "Uint8Array", "(Blob | null)", "ReadableStream<Uint8Array> | undefined"]) {
    assert.equal(isTopOrBytes(type), true, type);
    const ov = overlay("resolvers.ts", RESOLVERS, [{ n: 0, value: ORDER, groups: [group(type)], form: FORM }], GQL_WIRE, "clean");
    assert.deepEqual(ov.checks, [], type);
    assert.deepEqual(ov.unplaced, [{ n: 0, reason: "top_type" }], type);
    assert.equal(ov.text.includes("__C.send"), false, type);
  }
  for (const type of ["{ id: string }", "string | null", "{ data: unknown }", "Order[]"]) assert.equal(isTopOrBytes(type), false, type);
  // A value of type any: the clean form is silent and so is the twin, so it never arms.
  const sent = span(RESOLVERS, "payload", "resolvers.ts");
  const at = RESOLVERS.indexOf("return payload;") + "return ".length;
  const raw: CheckInput[] = [{ n: 1, value: { ...sent, start: at, end: at + "payload".length }, groups: [group("{ id: string }")], form: FORM }];
  const clean = overlay("resolvers.ts", RESOLVERS, raw, GQL_WIRE, "clean");
  const twin = overlay("resolvers.ts", RESOLVERS, raw, GQL_WIRE, "twin");
  const result = arm(clean, typecheck(clean.text), typecheck(twin.text));
  assert.deepEqual(result.armed, []);
  assert.deepEqual(result.unarmed.map((u) => u.reason), ["twin_passes"]);
  // The same expected type on an HTTP check is not touched by the rule.
  const http = overlay("sends.ts", SENDS, [{ n: 0, value: span(SENDS, "{ id: order.id, total: order.total }", "sends.ts"), groups: [group("unknown")] }], GQL_WIRE, "clean");
  assert.equal(http.checks.length, 1);
});

test("an HTTP overlay is byte for byte the same with and without a GraphQL input beside it", () => {
  const value = span(SENDS, "{ id: order.id, total: order.total }", "sends.ts");
  const http: CheckInput[] = [{ n: 0, value, groups: [group("{ id: string; total: number }")] }];
  const alone = overlay("sends.ts", SENDS, http, GQL_WIRE, "clean");
  overlay("resolvers.ts", RESOLVERS, [{ n: 1, value: ORDER, groups: [group(SHAPE)], form: LEAF }], GQL_WIRE, "clean");
  const again = overlay("sends.ts", SENDS, http, GQL_WIRE, "clean");
  assert.equal(again.text, alone.text);
  assert.ok(alone.text.includes("  function send0_0<V>(v: V, c: (o: __carrick_Sent<V, O0_0>) => O0_0): V;\n"));
  assert.equal(alone.checks[0]!.callback, undefined, "an HTTP check records no callback offset");
  // And an HTTP check keeps R3 beside a GraphQL one in one file: only the alias-naming diagnostic is its line.
  const mixed = overlay("sends.ts", SENDS, [...http, { n: 1, value: span(SENDS, "{ first: 1 }", "sends.ts"), groups: [group("{ first: string }")], form: FORM }], GQL_WIRE, "clean");
  const h = mixed.checks.find((c) => c.k === "0_0")!;
  const d: Diag = { start: h.span[1] - 2, code: 2741, message: "missing in type 'O0_0'" };
  assert.deepEqual(attribute(mixed, [d]).lines.get("0_0"), [d]);
});
