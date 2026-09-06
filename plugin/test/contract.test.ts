import test from "node:test";
import assert from "node:assert/strict";
import { parseCheckResult, problemItems, connectedItems, isCandidate } from "../src/contract.ts";
import { fixture } from "./helpers.ts";

test("a payload with the right schema parses", () => {
  const result = fixture("check-mismatch.json");
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
  assert.equal(problems.length, 3);
  assert.equal(
    problems.some((item) => item.verdict?.result === "compatible"),
    false,
  );
});

test("connected items are the ones naming another service", () => {
  const result = fixture("check-mismatch.json");
  assert.equal(connectedItems(result).length, 4);
});

test("a verdict with a null result is not a problem", () => {
  const result = fixture("check-mismatch.json");
  const notChecked = (result.items ?? []).find((item) => item.verdict?.state === "not_checked");
  assert.ok(notChecked);
  assert.equal(problemItems(result).includes(notChecked), false);
});

test("boundary_lines is read when the CLI sends it, and only when it holds strings", () => {
  assert.deepEqual(fixture("check-pre-rendered-boundary.json").boundary_lines?.length, 2);
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
