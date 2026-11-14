// Place, arm and attribute (part 3, T4): pure functions that wrap a route
// registration in a typed check, decide which diagnostics are the check's own,
// and gate rows on the current text. Every name here is invented.

import test from "node:test";
import assert from "node:assert/strict";
import fs from "node:fs";
import ts from "typescript";
import { arm, attribute, findSite, overlay } from "../src/held/place.ts";
import type { Diag, PlaceRow } from "../src/held/place.ts";

const dir = new URL("./fixtures/held/", import.meta.url);
const TEXT = fs.readFileSync(new URL("routes.ts", dir), "utf8");
const ROWS = JSON.parse(fs.readFileSync(new URL("rows.json", dir), "utf8")) as PlaceRow[];
const row = (id: number): PlaceRow => structuredClone(ROWS[id]!);
const source = (text: string): ts.SourceFile => ts.createSourceFile("routes.ts", text, ts.ScriptTarget.Latest, true);
const diag = (start: number, code: number, message: string): Diag => ({ start, code, message });

test("a row places on the call that starts on its line", () => {
  const ov = overlay("routes.ts", TEXT, [row(0)], "clean");
  assert.deepEqual(ov.unplaced, []);
  assert.equal(ov.checks.length, 1);
  assert.match(ov.text.slice(ov.checks[0]!.span[0]), /^__C\.reg0_0\(app\.get\("\/orders", listOrders\), \(o\) => o\)/);
});

test("lines moved by three still place through the site-literal fallback", () => {
  const moved = "// a\n// b\n// c\n" + TEXT;
  const found = findSite(source(moved), row(0));
  assert.ok("node" in found, JSON.stringify(found));
  const ov = overlay("routes.ts", moved, [row(0)], "clean");
  assert.equal(ov.checks.length, 1);
  assert.deepEqual(ov.unplaced, []);
});

test("a duplicate site gives site_not_unique", () => {
  const ov = overlay("routes.ts", TEXT, [row(3)], "clean");
  assert.equal(ov.checks.length, 0);
  assert.deepEqual(ov.unplaced, [{ id: 3, reason: "site_not_unique" }]);
});

test("a site that is not in the file gives no_site_found", () => {
  const gone = { ...row(0), site: "/nowhere", label: "GET /nowhere" };
  assert.deepEqual(overlay("routes.ts", TEXT, [gone], "clean").unplaced, [{ id: 0, reason: "no_site_found" }]);
});

test("the chained call wraps the whole chain, and both rows nest into valid syntax", () => {
  const ov = overlay("routes.ts", TEXT, [row(1), row(2)], "clean");
  assert.deepEqual(ov.unplaced, []);
  const post = ov.checks.find((c) => c.id === 2 && c.group === 0)!;
  assert.match(ov.text.slice(post.span[0], post.span[1]), /app\.get\("\/a", readA\)\.post\("\/b", writeB\), \(o\) => o\)$/);
  const get = ov.checks.find((c) => c.id === 1)!;
  assert.ok(ov.text.slice(get.span[0], get.span[1]).includes('app.get("/a", readA)'));
  assert.ok(!ov.text.slice(get.span[0], get.span[1]).includes('post("/b"'));
  const out = ts.transpileModule(ov.text, { reportDiagnostics: true, fileName: "routes.ts" });
  assert.deepEqual(out.diagnostics ?? [], []);
  assert.equal(ov.checks.filter((c) => c.id === 2).length, 2, "one check per group");
});

test("the half-written file's syntax error inside the span is not a line", () => {
  const ov = overlay("routes.ts", TEXT, [row(0)], "clean");
  const c = ov.checks[0]!;
  const inside = ov.text.indexOf("listOrders", c.span[0]);
  assert.ok(inside > c.span[0] && inside < c.span[1]);
  const got = attribute(ov, [diag(inside, 1003, "Identifier expected.")]);
  assert.deepEqual(got.lines.get(c.k) ?? [], []);
  assert.equal(got.outside.length, 1);
});

test("a TS2741 naming the check alias on the appended callback is the row's line", () => {
  const r = { ...row(0), id: 3 };
  const ov3 = overlay("routes.ts", TEXT, [r], "clean");
  const c = ov3.checks[0]!;
  assert.equal(c.k, "3_0");
  const callback = ov3.text.lastIndexOf("(o) => o", c.span[1]);
  assert.ok(callback > ov3.text.indexOf("listOrders", c.span[0]), "the callback is after the user's call");
  const d = diag(callback, 2741, "Property 'orders' is missing in type '{ a: number; }' but required in type 'O3_0'.");
  assert.deepEqual(attribute(ov3, [d]).lines.get("3_0"), [d]);
  const other = diag(callback, 2741, "Property 'orders' is missing in type 'X' but required in type 'O13_0'.");
  assert.deepEqual(attribute(ov3, [other]).lines.get("3_0") ?? [], [], "O13_0 is not O3_0");
});

test("a TS2304 in the declaration span is decl_error, never a line", () => {
  const ov = overlay("routes.ts", TEXT, [row(0)], "clean");
  const c = ov.checks[0]!;
  const d = diag(c.declSpan[0] + 4, 2304, "Cannot find name 'Missing'.");
  const got = attribute(ov, [d]);
  assert.ok(got.declErrors.has(c.k));
  assert.deepEqual(got.lines.get(c.k) ?? [], []);
});

test("rows with no check form are skipped with their reason", () => {
  assert.deepEqual(overlay("routes.js", TEXT, [row(0)], "clean").unplaced, [{ id: 0, reason: "javascript_file" }]);
  assert.deepEqual(overlay("routes.ts", TEXT, [{ ...row(0), locator: "call" }], "clean").unplaced, [{ id: 0, reason: "not_registration" }]);
  assert.deepEqual(overlay("routes.ts", TEXT, [{ ...row(0), label: "GET /orders {action=list}" }], "clean").unplaced, [{ id: 0, reason: "dispatch_case" }]);
});

test("the gate arms a row only when the clean check is silent and its twin speaks", () => {
  const ov = overlay("routes.ts", TEXT, [row(0)], "clean");
  const c = ov.checks[0]!;
  const at = ov.text.lastIndexOf("(o) => o", c.span[1]);
  const twinDiag = diag(at, 2741, "Property '__carrick_never' is missing in type '{ orders: string[] }' but required in type '{ readonly __carrick_never: true }'.");
  assert.deepEqual(arm(ov, [], [twinDiag]), { armed: ["0_0"], unarmed: [] });
  assert.deepEqual(arm(ov, [], []).unarmed, [{ k: "0_0", id: 0, group: 0, reason: "twin_passes" }]);
  assert.equal(arm(ov, [diag(at, 2322, "x O0_0")], [twinDiag]).unarmed[0]!.reason, "clean_check_fails:TS2322");
  assert.equal(arm(ov, [diag(at, 2345, "__carrick_not_a_route_object")], [twinDiag]).unarmed[0]!.reason, "no_hono");
  assert.equal(arm(ov, [diag(c.declSpan[0] + 4, 2304, "Cannot find name 'Q'.")], [twinDiag]).unarmed[0]!.reason, "decl_error");
  assert.deepEqual(arm(ov, [diag(at, 1003, "Identifier expected.")], [twinDiag]).armed, ["0_0"], "a syntax error is not a check failure");
});
