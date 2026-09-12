import test from "node:test";
import assert from "node:assert/strict";
import {
  parseCheckResult,
  parseStatusResult,
  problemItems,
  connectedItems,
  isCandidate,
} from "../src/contract.ts";
import { renderSessionStart } from "../src/render.ts";
import { fixture } from "./helpers.ts";

test("a payload with the right schema parses", () => {
  const result = fixture("check-mismatch.json");
  assert.equal(result.repo, "/workspace/user-service");
  assert.match(result.boundary_note ?? "", /^A local index holds/);
  assert.equal(result.service, "user-service");
  assert.equal(result.stale, true);
  assert.equal(result.items?.length, 5);
  assert.equal(result.scanner_version, "0.3.41");
  assert.equal(result.deleted, false);
});

test("anything that is not a carrick.check/0 payload parses to null", () => {
  assert.equal(parseCheckResult(""), null);
  assert.equal(parseCheckResult("not json"), null);
  assert.equal(parseCheckResult("[]"), null);
  assert.equal(parseCheckResult('{"schema":"carrick.check/1"}'), null);
  assert.equal(parseCheckResult('{"items":[]}'), null);
});

test("an error payload parses and carries the reason", () => {
  const result = fixture("check-not-indexed.json");
  assert.equal(result.error, "not_indexed");
  assert.deepEqual(result.items, []);
});

test("a field the plugin does not know is dropped, not fatal", () => {
  const result = parseCheckResult('{"schema":"carrick.check/0","future_field":{"a":1}}');
  assert.ok(result);
  assert.equal("future_field" in result, false);
});

test("problem items are the verdicts that are not compatible", () => {
  const result = fixture("check-mismatch.json");
  const problems = problemItems(result);
  assert.equal(problems.length, 2);
  assert.equal(
    problems.some((item) => item.verdict?.result === "compatible"),
    false,
  );
});

test("connected items are the ones naming another service", () => {
  const result = fixture("check-mismatch.json");
  assert.equal(connectedItems(result).length, 5);
});

test("the result decides what is a problem, not the state", () => {
  const result = fixture("check-mismatch.json");
  const items = result.items ?? [];
  // A routing finding carries `not_checked`, because no TYPE verdict bears on
  // it, and it is still a finding.
  const routing = items.find((item) => item.verdict?.result === "method_mismatch");
  assert.ok(routing);
  assert.equal(routing.verdict?.state, "not_checked");
  assert.equal(problemItems(result).includes(routing), true);
  // A null result claims nothing, in whatever state.
  for (const item of items.filter((entry) => entry.verdict?.result == null)) {
    assert.equal(problemItems(result).includes(item), false);
  }
});

test("boundary_lines is read when the CLI sends it, and only when it holds strings", () => {
  assert.deepEqual(fixture("check-pre-rendered-boundary.json").boundary_lines?.length, 3);
  assert.equal(fixture("check-mismatch.json").boundary_lines, undefined);
  assert.equal(
    parseCheckResult('{"schema":"carrick.check/0","boundary_lines":[1,2]}')?.boundary_lines,
    undefined,
  );
  assert.equal(
    parseCheckResult('{"schema":"carrick.check/0","boundary_lines":"one line"}')?.boundary_lines,
    undefined,
  );
});

test("a candidate row is the one whose source says so", () => {
  const result = fixture("check-mismatch.json");
  const candidates = (result.items ?? []).filter(isCandidate);
  assert.equal(candidates.length, 1);
  assert.equal(candidates[0]?.resolution_source, "model");
});

test("the last paid scan's figures survive the parse, and nothing here prints them", () => {
  // carrick#995: the CLI owns the wording and the two rules behind it (an
  // unpriced run states no dollars; a null amount is "not set"). This package
  // carries the fields so a surface that wants them has them, and a session
  // that starts hours later is not handed a bill.
  const status = parseStatusResult(
    JSON.stringify({
      schema: "carrick.status/0",
      error: "not_indexed",
      services: [],
      last_scan: {
        updated_at: "2026-09-12T21:03:11Z",
        scans: [
          {
            repo: "api",
            spend: {
              schema: "carrick.scan-spend/0",
              scan_id: "scan_01J",
              first_index: true,
              priced: true,
              usd: 4.32,
              monthly_allowance_usd: 10,
              monthly_remaining_usd: 10,
            },
          },
        ],
      },
    }),
  );
  assert.equal(status?.last_scan?.scans[0]?.repo, "api");
  assert.equal(status?.last_scan?.scans[0]?.spend.usd, 4.32);
  assert.equal(status?.last_scan?.updated_at, "2026-09-12T21:03:11Z");
  assert.doesNotMatch(renderSessionStart(status!), /US\$/);

  // A body carrying something that is not a receipt carries no receipt.
  const malformed = parseStatusResult(
    '{"schema":"carrick.status/0","services":[],"last_scan":{"scans":"lots"}}',
  );
  assert.equal(malformed?.last_scan, undefined);
});
