// Code lenses, and the rules that keep them quiet (carrick#880).
//
// Every test here is one of the acceptance rules on the ticket. The payloads
// are written out rather than kept as fixtures where the shape is the point: a
// router file with ten routes of which two have counterparts is a shape.

import test from "node:test";
import assert from "node:assert/strict";
import path from "node:path";
import { SHOW_COUNTERPARTS, toCodeLenses } from "../src/lens.ts";
import { DEFAULT_SURFACES } from "../src/surfaces.ts";
import type { CheckItem, CheckResult } from "../src/contract.ts";
import { fixture } from "./helpers.ts";

const ROOT = "/workspace";
const onDisk = new Set(
  ["order-service/src/clients/users.ts", "order-service/src/server.ts"].map((file) =>
    path.resolve(ROOT, file),
  ),
);
const exists = (target: string): boolean => onDisk.has(target);

function route(line: number, over: Partial<CheckItem> = {}): CheckItem {
  return {
    kind: "route",
    method: "GET",
    path: `/api/thing/${line}`,
    line,
    col: 3,
    source: "fact",
    verdict: { state: "resolved", result: "compatible" },
    ...over,
  };
}

const consumer = {
  role: "consumer",
  service: "order-service",
  repo: `${ROOT}/order-service`,
  file: "src/clients/users.ts",
  line: 18,
};

function payload(items: CheckItem[]): CheckResult {
  return { schema: "carrick.check/0", file: "user-service/src/routes/users.ts", service: "user-service", items };
}

function titles(result: CheckResult): string[] {
  return toCodeLenses(result, { exists }).map((lens) => lens.command?.title ?? "");
}

test("a router file renders a lens only on the rows the index knows something about", () => {
  const items = Array.from({ length: 10 }, (_, index) => route(index + 1));
  items[2] = route(3, { counterparts: [consumer] });
  items[7] = route(8, { counterparts: [consumer, { ...consumer, file: "src/server.ts", line: 120 }] });
  const lenses = toCodeLenses(payload(items), { exists });
  assert.equal(lenses.length, 2, "ten routes, two with counterparts, two lenses");
  assert.deepEqual(
    lenses.map((lens) => lens.range.start.line),
    [2, 7],
  );
});

test("no lens ever says zero, in any form", () => {
  const items = Array.from({ length: 10 }, (_, index) => route(index + 1));
  for (const title of titles(payload(items))) assert.fail(`a bare row rendered ${title}`);
  // And the ones that do render never reach a zero either.
  const rendered = titles(payload([route(1, { counterparts: [consumer] })]));
  assert.deepEqual(rendered, ["1 consumer"]);
  for (const title of rendered) assert.equal(/\b0\b|\bno\b/i.test(title), false);
});

test("a file the index holds nothing for renders no lens and no placeholder", () => {
  assert.deepEqual(toCodeLenses(payload([]), { exists }), []);
  assert.deepEqual(toCodeLenses(fixture("check-not-indexed.json"), { exists }), []);
  assert.deepEqual(toCodeLenses(fixture("check-clean.json"), { exists }), []);
});

test("a mismatch is its own clause, and counts one, never inside the counterpart count", () => {
  assert.deepEqual(
    titles(
      payload([
        route(1, {
          counterparts: [consumer, { ...consumer, file: "src/server.ts", line: 120 }],
          verdict: { state: "resolved", result: "type_mismatch" },
        }),
      ]),
    ),
    ["2 consumers, 1 mismatch"],
  );
  // A mismatch with nobody on the other side still has something to say.
  assert.deepEqual(
    titles(payload([route(1, { verdict: { state: "not_checked", result: "method_mismatch" } })])),
    ["1 mismatch"],
  );
});

test("a candidate row produces no lens at all", () => {
  const candidate = route(1, {
    source: "candidate",
    resolution_source: "model",
    counterparts: [consumer],
    verdict: { state: "not_checked", result: "method_mismatch" },
  });
  assert.deepEqual(toCodeLenses(payload([candidate]), { exists }), []);
  // The fact row beside it still renders, so the rule drops rows and not files.
  assert.equal(toCodeLenses(payload([candidate, route(2, { counterparts: [consumer] })]), { exists }).length, 1);
});

test("a row whose line the payload did not state renders nothing", () => {
  const noLine: CheckItem = {
    kind: "call",
    method: "GET",
    path: "/api/users",
    source: "fact",
    counterparts: [consumer],
    verdict: { state: "resolved", result: "type_mismatch" },
  };
  assert.deepEqual(toCodeLenses(payload([noLine]), { exists }), []);
});

test("one lens per row, and the command carries the sites and the boundary", () => {
  const result = fixture("check-mismatch.json");
  const lenses = toCodeLenses(result, { exists });
  const lines = lenses.map((lens) => lens.range.start.line);
  assert.equal(new Set(lines).size, lines.length, "never two lenses on one row");
  const first = lenses[0];
  assert.equal(first?.command?.command, SHOW_COUNTERPARTS);
  const list = first?.command?.arguments?.[0] as {
    operation: string;
    sites: Array<{ path: string | null; line: number | null }>;
    boundary: string[];
  };
  assert.equal(list.operation, "GET /api/users/:id");
  assert.equal(list.sites.length, 2);
  assert.equal(list.sites[0]?.path, path.resolve(ROOT, "order-service/src/clients/users.ts"));
  assert.equal(list.sites[1]?.path, null, "a counterpart not on this disk is stated as absent");
  assert.match(list.boundary[0] ?? "", /A local index holds what the deterministic passes state/);
});

test("turning carrick.codeLens off removes every lens and changes nothing else", () => {
  const result = fixture("check-mismatch.json");
  assert.ok(toCodeLenses(result, { exists }).length > 0);
  assert.deepEqual(
    toCodeLenses(result, { exists, surfaces: { ...DEFAULT_SURFACES, codeLens: false } }),
    [],
  );
  // And carrick.boundary off takes the boundary off this surface too.
  const noBoundary = toCodeLenses(result, {
    exists,
    surfaces: { ...DEFAULT_SURFACES, boundary: false },
  });
  assert.equal(noBoundary.length, toCodeLenses(result, { exists }).length);
  assert.deepEqual((noBoundary[0]?.command?.arguments?.[0] as { boundary: string[] }).boundary, []);
});
