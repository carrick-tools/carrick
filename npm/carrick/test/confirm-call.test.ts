// Confirm or correct for an early CALL item (HM4 T2, D1-D3): the consumer-side
// early item joins part 2's call item by the call's place (file, line), method
// and response side, the label only breaking a tie. Hand-built results, no
// runtime: no early call item is produced on main yet, so nothing here arms.
// Every name is invented.

import test from "node:test";
import assert from "node:assert/strict";
import { compareEarly, confirmedResult, earlyRows, withEarlyBreaks } from "../src/hook/confirm.ts";
import type { CheckItem, CheckResult, MovedRow } from "../src/contract.ts";

const SOCKET = "CLIENT->SERVER";
const EVENT = "order:update";
const detail = "'status' has a different type. This code sends '\"BROKEN\"'; the listener expects 'string'.";
const RAN = { ran: "extraction+types" as const, elapsed_ms: 1 };

/** An early call item, shaped by D1: kind call, response side, the producer's type in actual_type only. */
const earlyCall = (line: number, over: Partial<CheckItem> = {}): CheckItem => ({
  kind: "call",
  method: SOCKET,
  path: EVENT,
  line,
  direction: "response",
  verdict: { state: "resolved", result: "type_mismatch", detail },
  actual_type: "{ id: string; status: string }",
  counterparts: [{ role: "producer", service: "listener", file: "src/listen.ts", line: 3 }],
  stated: true,
  ...over,
});
/** Part 2's call item, with both types set as typed_sides sets them. */
const fullCall = (line: number, over: Partial<CheckItem> = {}): CheckItem => ({
  ...earlyCall(line),
  expected_type: "{ id: string; status: \"BROKEN\" }",
  ...over,
});
const result = (items: CheckItem[], moved: MovedRow[] = [], recheck: Partial<NonNullable<CheckResult["recheck"]>> = {}): CheckResult => ({
  schema: "carrick.check/0",
  file: "src/orders.ts",
  items,
  recheck: { ...RAN, ...(moved.length > 0 ? { moved } : {}), ...recheck },
});
const moved = (role: "route" | "call", file: string, line: number, over: Partial<MovedRow> = {}): MovedRow => ({
  file,
  line,
  role,
  method: SOCKET,
  path: EVENT,
  before: "compatible",
  after: "type_mismatch",
  counterparts: [],
  reason: detail,
  not_judged_before: false,
  stated: true,
  ...over,
});
const early = (items: CheckItem[], movedRows: MovedRow[] = []): CheckResult => result(items, movedRows);

test("an early call joins part 2's call at the same line and method: same", () => {
  const compared = compareEarly(early([earlyCall(6)]), result([fullCall(6)]));
  assert.equal(compared.rows[0]?.outcome, "same");
  assert.equal(compared.part2Only.length, 0);
});

test("part 2 calls it compatible: corrected, the label names the call's place", () => {
  const full = result([fullCall(6, { verdict: { state: "resolved", result: "compatible" }, stated: false })]);
  const e = early([earlyCall(6)]);
  const compared = compareEarly(e, full);
  assert.equal(compared.rows[0]?.outcome, "differ");
  assert.equal(compared.rows[0]?.corrected, true);
  const said = new Set(earlyRows(e).map((r) => r.key));
  assert.deepEqual(confirmedResult(full, e, said).corrected, [`${SOCKET} ${EVENT} at src/orders.ts:6`]);
});

test("part 2 holds no call at the line: part3_only absent; unresolved: part3_only unresolved", () => {
  const none = compareEarly(early([earlyCall(6)]), result([fullCall(9)]));
  assert.equal(none.rows[0]?.outcome, "part3_only");
  assert.equal(none.rows[0]?.p2_state, "absent");
  const unresolved = compareEarly(early([earlyCall(6)]), result([fullCall(6, { verdict: { state: "unresolved", result: null }, stated: false })]));
  assert.equal(unresolved.rows[0]?.outcome, "part3_only");
  assert.equal(unresolved.rows[0]?.p2_state, "unresolved");
});

test("two early calls of one event in one file are two rows with two keys, each joined to its own part-2 call", () => {
  const e = early([earlyCall(6), earlyCall(9, { actual_type: "{ id: string }" })]);
  const rows = earlyRows(e);
  assert.equal(new Set(rows.map((r) => r.key)).size, 2);
  // Part 2 lists them in the opposite order: the line, not the order, joins them.
  const second = fullCall(9, { actual_type: "{ id: string }" });
  const first = fullCall(6);
  const full = result([second, first]);
  const compared = compareEarly(e, full);
  assert.deepEqual(compared.rows.map((c) => c.outcome), ["same", "same"]);
  assert.equal(compared.rows[0]?.match, first);
  assert.equal(compared.rows[1]?.match, second);
  assert.equal(compared.part2Only.length, 0);
  // End to end through the confirm layer: both said, both dropped from the full answer.
  const said = new Set(rows.map((r) => r.key));
  assert.deepEqual(confirmedResult(full, e, said).result.items, []);
  // Only the first said: the second stays in part 2's answer.
  assert.deepEqual(confirmedResult(full, e, new Set([rows[0]!.key])).result.items, [second]);
  // One call compatible, one broken: only the compatible one is corrected, at its own line.
  const mixed = result([fullCall(6), fullCall(9, { verdict: { state: "resolved", result: "compatible" }, stated: false })]);
  const out = confirmedResult(mixed, e, said);
  assert.deepEqual(out.corrected, [`${SOCKET} ${EVENT} at src/orders.ts:9`]);
});

test("a part-2 call at the same line with another path spelling still joins (GET /orders/:id vs /orders/:orderId)", () => {
  const e = early([earlyCall(12, { method: "GET", path: "/orders/:id" })]);
  const compared = compareEarly(e, result([fullCall(12, { method: "GET", path: "/orders/:orderId" })]));
  assert.equal(compared.rows[0]?.outcome, "same");
});

test("the label breaks a tie between two part-2 calls at one line", () => {
  const other = fullCall(12, { method: "GET", path: "/orders/:orderId" });
  const own = fullCall(12, { method: "GET", path: "/orders/:id" });
  const compared = compareEarly(early([earlyCall(12, { method: "GET", path: "/orders/:id" })]), result([other, own]));
  assert.equal(compared.rows[0]?.match, own);
});

test("a part-2 request-side call at the line is not the same break", () => {
  const request = fullCall(6, { direction: "request" });
  const compared = compareEarly(early([earlyCall(6)]), result([request]));
  assert.equal(compared.rows[0]?.outcome, "part3_only");
  assert.equal(compared.rows[0]?.p2_state, "absent");
  assert.equal(compared.part2Only.length, 1);
});

test("a part-2 call of another method at the line is not the same break", () => {
  const compared = compareEarly(early([earlyCall(6)]), result([fullCall(6, { method: "SERVER->CLIENT" })]));
  assert.equal(compared.rows[0]?.p2_state, "absent");
});

test("part 2's moved route row at the call's producer place is a mirror and is not printed again; of another kind it prints and the early row is differ", () => {
  const e = early([earlyCall(6)]);
  const same = moved("route", "src/listen.ts", 3);
  const mirrored = compareEarly(e, result([fullCall(6)], [same]));
  assert.equal(mirrored.rows[0]?.outcome, "same");
  assert.deepEqual(mirrored.mirrors, [same]);
  assert.equal(mirrored.part2Only.length, 0);
  const full = result([fullCall(6)], [same]);
  const said = new Set(earlyRows(e).map((r) => r.key));
  assert.deepEqual(confirmedResult(full, e, said).result.recheck?.moved, []);
  // Another kind at the producer's place: not a mirror, it prints; the early row, left standing, is differ.
  const other = moved("route", "src/listen.ts", 3, { after: "no_route" });
  const unjudged = compareEarly(e, result([], [other]));
  assert.equal(unjudged.rows[0]?.outcome, "differ");
  assert.deepEqual(unjudged.part2Only, []);
  const printed = compareEarly(e, result([fullCall(6)], [other]));
  assert.equal(printed.rows[0]?.outcome, "same");
  assert.deepEqual(printed.part2Only, [other]);
  // A route row elsewhere is part 2's own.
  const away = moved("route", "src/elsewhere.ts", 40);
  assert.deepEqual(compareEarly(e, result([fullCall(6)], [away])).part2Only, [away]);
});

test("a part-2 route item in the edited file at the call's producer place is a mirror", () => {
  const e = early([earlyCall(6, { counterparts: [{ role: "producer", service: "listener", file: "src/orders.ts", line: 20 }] })]);
  const route: CheckItem = { kind: "route", method: SOCKET, path: EVENT, line: 20, direction: "response", verdict: { state: "resolved", result: "type_mismatch", detail }, stated: true };
  const compared = compareEarly(e, result([fullCall(6), route]));
  assert.deepEqual(compared.mirrors, [route]);
  assert.equal(compared.part2Only.length, 0);
});

test("an early moved call row joins part 2's moved call row by place", () => {
  const own = moved("call", "src/orders.ts", 6, { counterparts: ["listener src/listen.ts:3"] });
  const e = early([], [own]);
  assert.equal(earlyRows(e)[0]?.key, "src/orders.ts\0CLIENT->SERVER order:update\x006");
  const theirs = moved("call", "src/orders.ts", 6);
  const compared = compareEarly(e, result([], [theirs]));
  assert.equal(compared.rows[0]?.outcome, "same");
  assert.equal(compared.rows[0]?.match, theirs);
  // Another line is another call; the early row stands as absent.
  const away = compareEarly(e, result([], [moved("call", "src/orders.ts", 9)]));
  assert.equal(away.rows[0]?.p2_state, "absent");
  // A standing moved call row is counted as a call item.
  const counted = withEarlyBreaks(result([]), e).items ?? [];
  assert.equal(counted[0]?.kind, "call");
});

test("route rows join as before: by label, not by line, and a route mirror rule is unchanged", () => {
  const route = (over: Partial<CheckItem> = {}): CheckItem => ({ kind: "route", method: "GET", path: "/orders/:id", line: 4, direction: "response", verdict: { state: "resolved", result: "type_mismatch", detail }, stated: true, ...over });
  const e = early([route({ counterparts: [{ role: "consumer", service: "web", file: "src/web.ts", line: 8 }] })]);
  assert.equal(earlyRows(e)[0]?.key, "src/orders.ts\0GET /orders/:id");
  const call: CheckItem = { kind: "call", method: "GET", path: "/orders/:id", line: 8, direction: "response", verdict: { state: "resolved", result: "type_mismatch", detail }, stated: true };
  const compared = compareEarly(e, result([route({ line: 40 })], [moved("call", "src/web.ts", 8, { method: "GET", path: "/orders/:id" })]));
  assert.equal(compared.rows[0]?.outcome, "same");
  assert.equal(compared.mirrors.length, 1);
  void call;
});
