// Place, arm and attribute (part 3, L1): pure functions that wrap each value a
// producer's reply is sent from in a typed check, map a stored position across
// an edit, decide which diagnostics are the check's own, and gate checks on the
// current text. A value an edit crosses is never placed by a guess.
// Every name is invented.

import test from "node:test";
import assert from "node:assert/strict";
import fs from "node:fs";
import ts from "typescript";
import { hunksOf } from "../src/facts/diff.ts";
import { arm, attribute, checkWire, mapValue, overlay, regionOfEdit, regionsBetween } from "../src/held/place.ts";
import type { Diag, Value } from "../src/held/place.ts";

const dir = new URL("./fixtures/held/", import.meta.url);
const TEXT = fs.readFileSync(new URL("sends.ts", dir), "utf8");
// The wire rule is the compiler engine's (S1); this stand-in only has to type.
const WIRE = ["type __carrick_Sent<S, E> = S;"];
const diag = (start: number, code: number, message: string): Diag => ({ start, code, message });

/** The `[start, end)` of the first `needle` in `text`. */
const span = (text: string, needle: string): Value => {
  const start = text.indexOf(needle);
  assert.ok(start >= 0, `${needle} is in the text`);
  return { file: "sends.ts", start, end: start + needle.length };
};
/** The `inner` text inside the first `outer`. */
const within = (text: string, outer: string, inner: string): Value => {
  const o = span(text, outer);
  const at = o.start + outer.indexOf(inner);
  return { file: "sends.ts", start: at, end: at + inner.length };
};
const group = (type: string): { type: string } => ({ type });

test("lines added above shift every value", () => {
  const value = span(TEXT, "{ id: order.id, total: order.total }");
  const moved = "// a\n// b\n// c\n" + TEXT;
  const region = regionOfEdit(TEXT, moved);
  assert.equal(region.oldEnd, region.oldStart, "an insertion");
  assert.equal(region.newEnd - region.newStart, 15);
  const got = mapValue(value, [region]);
  assert.deepEqual(got, { file: "sends.ts", start: value.start + 15, end: value.end + 15 });
  assert.equal(moved.slice((got as Value).start, (got as Value).end), "{ id: order.id, total: order.total }");
});

test("an edit strictly inside the value keeps its bounds", () => {
  const value = span(TEXT, "{ id: order.id, total: order.total }");
  const edited = TEXT.replace("total: order.total }", "amount: order.total }");
  const got = mapValue(value, regionsBetween(TEXT, edited));
  assert.deepEqual(got, { file: "sends.ts", start: value.start, end: value.end + 1 });
  assert.equal(edited.slice((got as Value).start, (got as Value).end), "{ id: order.id, amount: order.total }");
});

test("an edit across a bound places nothing: the value is crossed, never guessed", () => {
  // The shorthand-expanding rename: the local changes and so does the value's first characters.
  const old = within(TEXT, "c.json(result)", "result");
  const edited = TEXT.replace("const result = load();\n  return c.json(result);", "const found = load();\n  return c.json({ result: found });");
  assert.equal(mapValue(old, regionsBetween(TEXT, edited)), "crossed");
  // A replaced parent too.
  const value = span(TEXT, "{ first: 1 }");
  assert.equal(mapValue(value, regionsBetween(TEXT, TEXT.replace("return res.json({ first: 1 });", "return 1;"))), "crossed");
});

// A handler that logs one object and then replies with another (the stored value), and two edits that do not touch the reply:
// constants above it and a handler below it. One region from the first change to the last would hold the reply.
const HANDLER = [
  "declare const log: { info(fields: object, message: string): void };",
  "declare function save(): Promise<{ id: string; dupe: boolean; name: string }>;",
  "",
  "export async function upload(c: { json(body: unknown): Response }) {",
  "  const saved = await save();",
  '  log.info({ id: saved.id, team: "t1", dupe: saved.dupe }, "saved");',
  "  return c.json({ ok: true, id: saved.id, name: saved.name });",
  "}",
  "",
].join("\n");
const REPLY = "{ ok: true, id: saved.id, name: saved.name }";
const ABOVE = ["const LIMIT = 5;", 'const TEAM_HEADER = "x-team-id";', "", ""].join("\n");
const BELOW = "\nexport const code = (c: { json(body: unknown): Response }) => c.json({ code: String(LIMIT), header: TEAM_HEADER });\n";

test("an edit above a value and an edit below it, in one text, move it by arithmetic; each alone does too", () => {
  const value: Value = { file: "x.ts", ...((at) => ({ start: at, end: at + REPLY.length }))(HANDLER.indexOf(REPLY)) };
  const placedIn = (text: string): string => {
    const got = mapValue(value, regionsBetween(HANDLER, text));
    assert.notEqual(got, "crossed", text);
    return text.slice((got as Value).start, (got as Value).end);
  };
  const both = ABOVE + HANDLER + BELOW;
  assert.equal(regionsBetween(HANDLER, both).length, 2);
  assert.equal(placedIn(both), REPLY);
  assert.equal(placedIn(ABOVE + HANDLER), REPLY, "the edit above alone");
  assert.equal(placedIn(HANDLER + BELOW), REPLY, "the edit below alone");
  // The one-region map the daemon used before: the reply is crossed.
  assert.equal(mapValue(value, [regionOfEdit(HANDLER, both)]), "crossed");
});

test("regionsBetween: equal texts have no region; a change on one line is that line's changed characters", () => {
  assert.deepEqual(regionsBetween(HANDLER, HANDLER), []);
  const edited = HANDLER.replace('team: "t1"', 'team: "t2"');
  const [region, ...rest] = regionsBetween(HANDLER, edited);
  assert.deepEqual(rest, []);
  assert.equal(HANDLER.slice(region!.oldStart, region!.oldEnd), "1");
  assert.equal(edited.slice(region!.newStart, region!.newEnd), "2");
  // Lines removed and lines added at the ends of the text.
  assert.deepEqual(mapValue({ file: "x.ts", start: HANDLER.indexOf(REPLY), end: HANDLER.indexOf(REPLY) + REPLY.length }, regionsBetween(HANDLER, HANDLER.slice(HANDLER.indexOf("\nexport") + 1) + "// end\n")), {
    file: "x.ts",
    start: HANDLER.indexOf(REPLY) - (HANDLER.indexOf("\nexport") + 1),
    end: HANDLER.indexOf(REPLY) - (HANDLER.indexOf("\nexport") + 1) + REPLY.length,
  });
});

test("three runs of changed lines map a value between two of them exactly", () => {
  const old = Array.from({ length: 30 }, (_, i) => `line ${i + 1}`).join("\n") + "\n";
  const lines = old.split("\n");
  lines.splice(2, 0, "added one", "added two"); // after line 2: +2 lines
  lines.splice(14, 1); // the old line 13 (index 12 + 2 added) goes: -1 line
  const edited = lines.join("\n").replace("line 25", "line twenty five"); // a change in place
  const regions = regionsBetween(old, edited);
  assert.equal(regions.length, 3);
  const value: Value = { file: "x.ts", start: old.indexOf("line 20"), end: old.indexOf("line 20") + "line 20".length };
  const got = mapValue(value, regions) as Value;
  assert.equal(edited.slice(got.start, got.end), "line 20");
  // Each hunk's region is its own change, not the whole of its lines.
  assert.equal(edited.slice(regions[0]!.newStart, regions[0]!.newEnd), "added one\nadded two\n");
  assert.equal(old.slice(regions[1]!.oldStart, regions[1]!.oldEnd), "line 13\n");
  assert.equal(edited.slice(regions[2]!.newStart, regions[2]!.newEnd), "twenty five");
});

test("a value file with a byte order mark wraps at the right offsets", () => {
  const bom = "﻿" + TEXT;
  const value = span(TEXT, "{ id: order.id, total: order.total }"); // positions never count the mark
  const ov = overlay("sends.ts", bom, [{ n: 0, value, groups: [group("{ id: string; total: number }")] }], WIRE, "clean");
  assert.ok(ov.text.startsWith("﻿"));
  assert.match(ov.text.slice(ov.checks[0]!.span[0]), /^__C\.send0_0\(\{ id: order\.id, total: order\.total \}, \(o\) => o\)/);
});

test("two checks on one value and a check in a check nest into valid syntax", () => {
  const route = span(TEXT, "res.json({ first: 1 })");
  const inner = span(TEXT, "{ first: 1 }");
  const ov = overlay(
    "sends.ts",
    TEXT,
    [
      { n: 0, value: route, groups: [group("unknown"), group("unknown")] },
      { n: 1, value: inner, groups: [group("{ first: number }")] },
    ],
    WIRE,
    "clean",
  );
  assert.deepEqual(ov.unplaced, []);
  assert.equal(ov.checks.length, 3);
  const out = ts.transpileModule(ov.text, { reportDiagnostics: true, fileName: "sends.ts" });
  assert.deepEqual(out.diagnostics ?? [], []);
  const outer = ov.checks.find((x) => x.k === "0_1")!;
  assert.ok(ov.text.slice(outer.span[0], outer.span[1]).includes("__C.send1_0("), "the wider value's later group is outermost");
});

test("the checks type on real TypeScript: the clean one is silent and its twin speaks", () => {
  const cases: Array<[Value, string, string]> = [
    [span(TEXT, "{ id: order.id, total: order.total }"), "{ id: string; total: number }", "express"],
    [within(TEXT, "c.json(result)", "result"), "{ orders: string[] }", "hono"],
    [span(TEXT, "JSON.stringify(payload)"), "string", "lambda"],
  ];
  for (const [value, type, needle] of cases) {
    const clean = overlay("sends.ts", TEXT, [{ n: 0, value, groups: [group(type)] }], WIRE, "clean");
    const twin = overlay("sends.ts", TEXT, [{ n: 0, value, groups: [group(type)] }], WIRE, "twin");
    const c = clean.checks[0]!;
    assert.deepEqual(typecheck(clean.text), [], `${needle}: clean`);
    const bad = typecheck(twin.text);
    assert.ok(bad.some((d) => d.start >= twin.checks[0]!.span[0] && d.start <= twin.checks[0]!.span[1]), `${needle}: the twin speaks inside its check`);
    assert.equal(c.k, "0_0");
  }
});

test("a send that lacks a member the consumer reads is a line, in the value's own check", () => {
  const value = span(TEXT, "{ id: order.id, total: order.total }");
  const ov = overlay("sends.ts", TEXT, [{ n: 4, value, groups: [group("{ id: string; total: number; paid: boolean }")] }], WIRE, "clean");
  const diags = typecheck(ov.text);
  const own = attribute(ov, diags);
  assert.equal(own.lines.get("4_0")?.length, 1, JSON.stringify(diags));
  assert.deepEqual(own.outside, []);
});

test("the half-written file's syntax error inside the span is not a line", () => {
  const value = span(TEXT, "{ id: order.id, total: order.total }");
  const ov = overlay("sends.ts", TEXT, [{ n: 0, value, groups: [group("{ id: string; total: number }")] }], WIRE, "clean");
  const c = ov.checks[0]!;
  const at = ov.text.indexOf("total", c.span[0]);
  assert.ok(at > c.span[0] && at < c.span[1]);
  const got = attribute(ov, [diag(at, 1003, "Identifier expected.")]);
  assert.deepEqual(got.lines.get(c.k) ?? [], []);
  assert.equal(got.outside.length, 1);
});

test("a TS2741 naming the check alias on the appended callback is the line, and another alias is not", () => {
  const value = span(TEXT, "{ id: order.id, total: order.total }");
  const ov = overlay("sends.ts", TEXT, [{ n: 3, value, groups: [group("{ id: string }")] }], WIRE, "clean");
  const c = ov.checks[0]!;
  assert.equal(c.k, "3_0");
  const callback = ov.text.lastIndexOf("(o) => o", c.span[1]);
  assert.ok(callback > ov.text.indexOf("order.total", c.span[0]), "the callback is after the user's value");
  const d = diag(callback, 2741, "Property 'id' is missing in type '{ a: number; }' but required in type 'O3_0'.");
  assert.deepEqual(attribute(ov, [d]).lines.get("3_0"), [d]);
  const other = diag(callback, 2741, "Property 'id' is missing in type 'X' but required in type 'O13_0'.");
  assert.deepEqual(attribute(ov, [other]).lines.get("3_0") ?? [], [], "O13_0 is not O3_0");
});

test("a TS2304 in the declaration span is decl_error, never a line", () => {
  const value = span(TEXT, "{ id: order.id, total: order.total }");
  const ov = overlay("sends.ts", TEXT, [{ n: 0, value, groups: [group("Missing")] }], WIRE, "clean");
  const c = ov.checks[0]!;
  const d = diag(c.declSpan[0] + 4, 2304, "Cannot find name 'Missing'.");
  const got = attribute(ov, [d]);
  assert.ok(got.declErrors.has(c.k));
  assert.deepEqual(got.lines.get(c.k) ?? [], []);
});

test("a JavaScript value file is skipped with its reason", () => {
  const value = span(TEXT, "{ id: order.id, total: order.total }");
  const ov = overlay("sends.js", TEXT, [{ n: 7, value, groups: [group("unknown")] }], WIRE, "clean");
  assert.deepEqual(ov.unplaced, [{ n: 7, reason: "javascript_file" }]);
  assert.equal(ov.checks.length, 0);
  assert.equal(ov.text, TEXT + "\n" + "declare namespace __C {\n  type __carrick_Sent<S, E> = S;\n}\n");
});

test("the gate arms a check only when the clean form is silent and its twin speaks", () => {
  const value = span(TEXT, "{ id: order.id, total: order.total }");
  const ov = overlay("sends.ts", TEXT, [{ n: 0, value, groups: [group("{ id: string }")] }], WIRE, "clean");
  const c = ov.checks[0]!;
  const at = ov.text.lastIndexOf("(o) => o", c.span[1]);
  const twinDiag = diag(at, 2741, "Property '__carrick_never' is missing in type '{ id: string }' but required in type '{ readonly __carrick_never: true }'.");
  assert.deepEqual(arm(ov, [], [twinDiag]), { armed: ["0_0"], unarmed: [] });
  assert.deepEqual(arm(ov, [], []).unarmed, [{ k: "0_0", n: 0, group: 0, reason: "twin_passes" }]);
  assert.equal(arm(ov, [diag(at, 2322, "x O0_0")], [twinDiag]).unarmed[0]!.reason, "clean_check_fails:TS2322");
  assert.equal(arm(ov, [diag(c.declSpan[0] + 4, 2304, "Cannot find name 'Q'.")], [twinDiag]).unarmed[0]!.reason, "decl_error");
  assert.deepEqual(arm(ov, [diag(at, 1003, "Identifier expected.")], [twinDiag]).armed, ["0_0"], "a syntax error is not a check failure");
});

/** Semantic diagnostics of one in-memory file, under the real standard library. */
function typecheck(text: string): Array<{ start: number; code: number; message: string }> {
  const options: ts.CompilerOptions = { strict: true, noEmit: true, target: ts.ScriptTarget.ES2022, types: [] };
  const host = ts.createCompilerHost(options);
  const original = host.getSourceFile.bind(host);
  host.getSourceFile = (name, ...rest) => (name === "/virtual/sends.ts" ? ts.createSourceFile(name, text, ts.ScriptTarget.ES2022, true) : original(name, ...rest));
  const program = ts.createProgram(["/virtual/sends.ts"], options, host);
  const sf = program.getSourceFile("/virtual/sends.ts")!;
  return program.getSemanticDiagnostics(sf).map((d) => ({
    start: d.start ?? 0,
    code: d.code,
    message: ts.flattenDiagnosticMessageText(d.messageText, "\n"),
  }));
}

test("hunksOf reads every `-U0` header, with the counts git omits as 1 (part 1's diff)", () => {
  const diff = "diff --git a/f b/f\n@@ -3 +3,2 @@\n-a\n+b\n+c\n@@ -10,0 +12,4 @@\n+x\n@@ -20,2 +22 @@\n";
  assert.deepEqual(hunksOf(diff), [
    { oldStart: 3, oldCount: 1, newStart: 3, newCount: 2 },
    { oldStart: 10, oldCount: 0, newStart: 12, newCount: 4 },
    { oldStart: 20, oldCount: 2, newStart: 22, newCount: 1 },
  ]);
  assert.deepEqual(hunksOf("no headers"), []);
});

test("checkWire composes the engine's declarations and tolerance helpers into the one type a check receives", () => {
  const text = checkWire(["W1"], ["T1", "T2"]);
  assert.deepEqual(text.slice(0, 3), ["W1", "T1", "T2"]);
  assert.match(text[3]!, /^type __carrick_Sent<S, E> = __carrick_IsVoid<E> extends true \? E : __carrick_Wire<S, E>;$/);
  assert.ok(!text.join("\n").includes("AbsentBody<S, E>, E>"), "an HTTP response gets no absent-body drop");
});
