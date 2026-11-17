// HM-3: the socket send form of a held check. An emit's payload is wrapped, the
// listener's parameter type is held fixed, and the check's own diagnostic is
// found by its callback position (P8). Every name is invented: service
// `orders`, event `order:update`, type `OrderUpdate`.

import test from "node:test";
import assert from "node:assert/strict";
import fs from "node:fs";
import ts from "typescript";
import { arm, attribute, overlay } from "../src/held/place.ts";
import type { CheckInput, Diag, Value } from "../src/held/place.ts";

const dir = new URL("./fixtures/held/", import.meta.url);
const EMITS = fs.readFileSync(new URL("emits.ts", dir), "utf8");
const SENDS = fs.readFileSync(new URL("sends.ts", dir), "utf8");
const RESOLVERS = fs.readFileSync(new URL("resolvers.ts", dir), "utf8");
// The engine's message rule is the compiler engine's (`heldMessageWire`); this stand-in only has to type.
const MSG_WIRE = ["type __carrick_MsgSent<S, E> = S;"];
const GQL_WIRE = [
  "type __carrick_Sent<S, E> = S;",
  "type __carrick_GqlExpected<E> = E;",
  "type __carrick_GqlSent<S, E> = S;",
  "type __carrick_GqlSentS<S, E, R> = S;",
];
const MESSAGE = { kind: "message" } as const;
const group = (type: string): { type: string } => ({ type });

const span = (text: string, needle: string, file: string, after = 0): Value => {
  const start = text.indexOf(needle, after);
  assert.ok(start >= 0, `${needle} is in the text`);
  return { file, start, end: start + needle.length };
};
const PAYLOAD = span(EMITS, '{ id: "o1", status: "paid" }', "emits.ts");
// The payload `update` after the event name (the event's own text holds the word too).
const WITH_ACK = span(EMITS, "update", "emits.ts", EMITS.indexOf('"order:update", update') + '"order:update", '.length);
const ORDER_UPDATE = "{ id: string; status: string }";

function typecheck(text: string): Diag[] {
  const options: ts.CompilerOptions = { strict: true, noEmit: true, target: ts.ScriptTarget.ES2022, types: [] };
  const host = ts.createCompilerHost(options);
  const original = host.getSourceFile.bind(host);
  host.getSourceFile = (name, ...rest) => (name === "/virtual/emits.ts" ? ts.createSourceFile(name, text, ts.ScriptTarget.ES2022, true) : original(name, ...rest));
  const program = ts.createProgram(["/virtual/emits.ts"], options, host);
  const sf = program.getSourceFile("/virtual/emits.ts")!;
  return program.getSemanticDiagnostics(sf).map((d) => ({ start: d.start ?? 0, code: d.code, message: ts.flattenDiagnosticMessageText(d.messageText, "\n") }));
}

test("send form wraps the payload", () => {
  const ov = overlay("emits.ts", EMITS, [{ n: 0, value: PAYLOAD, groups: [group(ORDER_UPDATE)], form: MESSAGE }], MSG_WIRE, "clean");
  assert.deepEqual(ov.unplaced, []);
  assert.ok(ov.text.includes('socket.emit("order:update", __C.send0_0({ id: "o1", status: "paid" }, (o) => o));'));
  assert.ok(ov.text.endsWith(
    `declare namespace __C {\n${MSG_WIRE.map((l) => `  ${l}`).join("\n")}\n` +
      `  type O0_0 = (${ORDER_UPDATE});\n` +
      `  function send0_0<V>(v: V, c: (o: __carrick_MsgSent<V, O0_0>) => O0_0): V;\n}\n`,
  ));
  assert.equal(ov.text.includes("Awaited"), false, "an emit argument is a value: nothing is awaited");
  const c = ov.checks[0]!;
  assert.equal(ov.text.slice(c.callback! - 5, c.callback! + 2), ") => o)");
  // Only the payload is wrapped; an ack callback beside it is left as written.
  const ack = overlay("emits.ts", EMITS, [{ n: 0, value: WITH_ACK, groups: [group(ORDER_UPDATE)], form: MESSAGE }], MSG_WIRE, "clean");
  assert.ok(ack.text.includes('socket.emit("order:update", __C.send0_0(update, (o) => o), (reply) => {'));
});

test("clean is silent and the twin speaks", () => {
  const input = (type: string, value: Value = PAYLOAD): CheckInput[] => [{ n: 0, value, groups: [group(type)], form: MESSAGE }];
  for (const value of [PAYLOAD, WITH_ACK]) {
    const clean = overlay("emits.ts", EMITS, input(ORDER_UPDATE, value), MSG_WIRE, "clean");
    const twin = overlay("emits.ts", EMITS, input(ORDER_UPDATE, value), MSG_WIRE, "twin");
    assert.deepEqual(typecheck(clean.text), []);
    assert.deepEqual(arm(clean, typecheck(clean.text), typecheck(twin.text)).armed, ["0_0"]);
  }
  // A planted break is one line at the callback, also where the message names the expanded type, never the alias.
  for (const type of ["{ id: string; status: string; total: number }", "string"]) {
    const broken = overlay("emits.ts", EMITS, input(type), MSG_WIRE, "clean");
    const diags = typecheck(broken.text);
    const got = attribute(broken, diags);
    assert.equal(got.lines.get("0_0")?.length, 1, JSON.stringify(diags));
    assert.deepEqual(got.outside, []);
  }
  assert.doesNotMatch(typecheck(overlay("emits.ts", EMITS, input("string"), MSG_WIRE, "clean").text)[0]!.message, /O0_0/);
});

test("one armed key per (row, group, position)", () => {
  const inputs: CheckInput[] = [
    { n: 0, value: PAYLOAD, groups: [group(ORDER_UPDATE), group("{ id: string }")], form: MESSAGE },
    { n: 1, value: WITH_ACK, groups: [group(ORDER_UPDATE), group("{ status: string }")], form: MESSAGE },
  ];
  const clean = overlay("emits.ts", EMITS, inputs, MSG_WIRE, "clean");
  const twin = overlay("emits.ts", EMITS, inputs, MSG_WIRE, "twin");
  const { armed, unarmed } = arm(clean, typecheck(clean.text), typecheck(twin.text));
  assert.deepEqual(armed.sort(), ["0_0", "0_1", "1_0", "1_1"]);
  assert.deepEqual(unarmed, []);
  assert.deepEqual(clean.checks.map((c) => `${c.n}:${c.group}`), ["0:0", "0:1", "1:0", "1:1"]);
});

test("a file with a GraphQL and a message check gets each its own form", () => {
  const ov = overlay("emits.ts", EMITS, [
    { n: 0, value: PAYLOAD, groups: [group(ORDER_UPDATE)], form: MESSAGE },
    { n: 1, value: WITH_ACK, groups: [group(ORDER_UPDATE)], form: { kind: "graphql", leaf: null } },
  ], [...GQL_WIRE, ...MSG_WIRE], "clean");
  assert.ok(ov.text.includes("  function send0_0<V>(v: V, c: (o: __carrick_MsgSent<V, O0_0>) => O0_0): V;\n"));
  assert.ok(ov.text.includes("  function send1_0<V>(v: V, c: (o: __carrick_GqlSent<Awaited<V>, O1_0>) => __carrick_GqlExpected<O1_0>): V;\n"));
  assert.ok(ov.checks.every((c) => c.callback !== undefined));
});

test("a listener typed any places nothing", () => {
  for (const type of ["any", "unknown", "never", "Buffer", "(Uint8Array | null)"]) {
    const ov = overlay("emits.ts", EMITS, [{ n: 0, value: PAYLOAD, groups: [group(type)], form: MESSAGE }], MSG_WIRE, "clean");
    assert.deepEqual(ov.checks, [], type);
    assert.deepEqual(ov.unplaced, [{ n: 0, reason: "top_type" }], type);
    assert.equal(ov.text.includes("__C.send"), false, type);
  }
});

test("the overlay text for a file with no message check is byte for byte the text before the message form", () => {
  // The golden was written by origin/main's place.ts before the message form existed (HM-3).
  const golden = JSON.parse(fs.readFileSync(new URL("overlay-no-message.golden.json", dir), "utf8")) as Record<string, unknown>;
  const http = overlay("sends.ts", SENDS, [
    { n: 0, value: span(SENDS, "{ id: order.id, total: order.total }", "sends.ts"), groups: [group("{ id: string; total: number }")] },
    { n: 1, value: span(SENDS, "{ first: 1 }", "sends.ts"), groups: [group("{ first: number }"), group("any")] },
  ], GQL_WIRE, "clean");
  const gql = overlay("resolvers.ts", RESOLVERS, [
    { n: 0, value: span(RESOLVERS, "loadOrder(args.id)", "resolvers.ts"), groups: [group("{ id: string }")], form: { kind: "graphql", leaf: null } },
    { n: 1, value: span(RESOLVERS, '{ edges: [{ id: "1", total: 2 }], totalCount: 1 }', "resolvers.ts"), groups: [group("{ totalCount: number }"), group("unknown")], form: { kind: "graphql", leaf: '{"type":"Orders"}' } },
  ], GQL_WIRE, "twin");
  assert.deepEqual(JSON.parse(JSON.stringify({ http, gql })), golden);
});
