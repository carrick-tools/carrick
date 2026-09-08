import test from "node:test";
import assert from "node:assert/strict";
import path from "node:path";
import { SEVERITY, resolveCounterpart, severityOf, toDiagnostics } from "../src/diagnostics.ts";
import { fixture } from "./helpers.ts";

const ROOT = "/workspace";
const CHECKED = "user-service/src/routes/users.ts";
const CHECKED_ABS = path.resolve(ROOT, CHECKED);

/** `repo` + `file` for every counterpart the fixtures name, as it is on disk. */
const onDisk = new Set(
  [
    "order-service/src/clients/users.ts",
    "order-service/src/server.ts",
    "billing-service/src/lookup.ts",
    "billing-service/src/charges.ts",
    "audit-service/src/routes/audit.ts",
  ].map((file) => path.resolve(ROOT, file)),
);
const exists = (target: string): boolean => onDisk.has(target);

function diagnosticsFor(name: string) {
  return toDiagnostics(fixture(name), ROOT, CHECKED, { exists });
}

test("a finding on a fact row is an error and the same on a candidate is a warning (R1)", () => {
  assert.equal(
    severityOf({
      kind: "route",
      source: "fact",
      verdict: { state: "resolved", result: "type_mismatch" },
    }),
    SEVERITY.error,
  );
  assert.equal(
    severityOf({
      kind: "call",
      source: "candidate",
      verdict: { state: "resolved", result: "type_mismatch" },
    }),
    SEVERITY.warning,
  );
  assert.equal(
    severityOf({
      kind: "call",
      source: "fact",
      verdict: { state: "unresolved", result: null },
    }),
    SEVERITY.warning,
  );
});

test("the checked file gets one diagnostic per problem, then the boundary", () => {
  const edited = diagnosticsFor("check-mismatch.json").get(CHECKED_ABS);
  assert.equal(edited?.length, 3);
  assert.deepEqual(
    edited?.map((diagnostic) => diagnostic.severity),
    [SEVERITY.error, SEVERITY.warning, SEVERITY.information],
  );
  assert.deepEqual(
    edited?.map((diagnostic) => diagnostic.code),
    ["type_mismatch", "method_mismatch", "boundary"],
  );
});

test("a routing finding on a fact row is an error, whatever its type state", () => {
  // `not_checked` says no TYPE verdict bears on the row, not that the row is
  // uncertain: a method mismatch read off two deterministic rows is a fact.
  assert.equal(
    severityOf({
      kind: "call",
      source: "fact",
      verdict: { state: "not_checked", result: "method_mismatch" },
    }),
    SEVERITY.error,
  );
  // `unresolved` claims nothing, so nothing is asserted here either.
  assert.equal(
    severityOf({ kind: "call", source: "fact", verdict: { state: "unresolved", result: null } }),
    SEVERITY.warning,
  );
});

test("the boundary is published even when nothing is wrong", () => {
  const byFile = diagnosticsFor("check-clean.json");
  const edited = byFile.get(CHECKED_ABS);
  assert.equal(edited?.length, 1);
  assert.equal(edited?.[0]?.code, "boundary");
  assert.equal(edited?.[0]?.severity, SEVERITY.information);
  assert.match(edited?.[0]?.message ?? "", /A local index holds what the deterministic passes state/);
});

test("the boundary diagnostic carries the CLI's own lines when they arrive", () => {
  const edited = diagnosticsFor("check-pre-rendered-boundary.json").get(CHECKED_ABS);
  const boundary = edited?.find((diagnostic) => diagnostic.code === "boundary");
  assert.match(boundary?.message ?? "", /boundary \(user-service\): 41 candidates not classified locally/);
  assert.equal(boundary?.message.includes("file(s) sent to the analyzer"), false);
});

test("a payload with no boundary publishes no boundary diagnostic", () => {
  assert.equal(diagnosticsFor("check-silent.json").size, 0);
});

test("the range is zero-based and one character wide", () => {
  const first = diagnosticsFor("check-mismatch.json").get(CHECKED_ABS)?.[0];
  assert.deepEqual(first?.range, {
    start: { line: 41, character: 2 },
    end: { line: 41, character: 3 },
  });
});

test("counterpart sites are in the message text and in relatedInformation (E18)", () => {
  const first = diagnosticsFor("check-mismatch.json").get(CHECKED_ABS)?.[0];
  assert.match(first?.message ?? "", /Counterparts: consumer in order-service, src\/clients\/users\.ts:18/);
  assert.equal(first?.relatedInformation?.length, 1);
  assert.match(
    first?.relatedInformation?.[0]?.location.uri ?? "",
    /file:\/\/\/workspace\/order-service\/src\/clients\/users\.ts$/,
  );
  assert.equal(first?.relatedInformation?.[0]?.location.range.start.line, 17);
});

test("a diagnostic never prints the word null, and qualifies a finding with no type verdict", () => {
  const edited = diagnosticsFor("check-mismatch.json").get(CHECKED_ABS) ?? [];
  const messages = edited.map((diagnostic) => diagnostic.message).join("\n");
  assert.equal(/\bnull\b/.test(messages), false);
  const routing = edited.find((diagnostic) => diagnostic.code === "method_mismatch");
  assert.match(routing?.message ?? "", /method_mismatch \(no type verdict\)/);
  assert.equal(
    edited.some((diagnostic) => diagnostic.message.includes("/api/audit/:id")),
    false,
    "a row that claims nothing publishes nothing",
  );
});

test("a candidate diagnostic says it is a reading of the code", () => {
  const candidate = diagnosticsFor("check-mismatch.json")
    .get(CHECKED_ABS)
    ?.find((diagnostic) => diagnostic.code === "method_mismatch");
  assert.match(candidate?.message ?? "", /Candidate row \(model\)/);
});

test("evidence is carried into the diagnostic", () => {
  const first = diagnosticsFor("check-mismatch.json").get(CHECKED_ABS)?.[0];
  assert.match(first?.message ?? "", /Read off loader export claimed by a file-route convention\./);
});

test("the same finding is published at each counterpart site", () => {
  const byFile = diagnosticsFor("check-mismatch.json");
  const consumer = byFile.get(path.resolve(ROOT, "order-service/src/clients/users.ts"));
  assert.equal(consumer?.length, 1);
  assert.match(consumer?.[0]?.message ?? "", /^consumer in order-service of src\/routes\/users\.ts\./);
  assert.equal(consumer?.[0]?.range.start.line, 17);
  assert.equal(byFile.has(path.resolve(ROOT, "order-service/src/server.ts")), true);
});

test("a counterpart path that is not on this disk gets no URI", () => {
  const byFile = toDiagnostics(fixture("check-mismatch.json"), ROOT, CHECKED, {
    exists: () => false,
  });
  const first = byFile.get(CHECKED_ABS)?.[0];
  assert.equal(first?.relatedInformation, undefined);
  assert.match(first?.message ?? "", /src\/clients\/users\.ts:18/);
  assert.equal(byFile.size, 1, "only the checked file, and no guessed location");
});

test("a counterpart resolves through its own repo, and nothing is guessed", () => {
  assert.equal(
    resolveCounterpart(
      { role: "consumer", service: "order-service", repo: "/workspace/order-service", file: "src/server.ts" },
      exists,
    ),
    path.resolve(ROOT, "order-service/src/server.ts"),
  );
  // The index no longer holds the repo: there is no path to give, and the
  // workspace root is not a substitute for one.
  assert.equal(
    resolveCounterpart({ role: "consumer", service: "order-service", repo: null, file: "src/server.ts" }, exists),
    null,
  );
  assert.equal(
    resolveCounterpart(
      { role: "consumer", service: "order-service", repo: "/elsewhere", file: "src/server.ts" },
      exists,
    ),
    null,
  );
});

test("a counterpart whose repo the index lost stays in the text with no URI", () => {
  const byFile = diagnosticsFor("check-mismatch.json");
  const first = byFile.get(CHECKED_ABS)?.[0];
  // Two consumers on that row, one of them with repo: null.
  assert.equal(first?.relatedInformation?.length, 1);
  assert.match(first?.message ?? "", /consumer in billing-service, src\/lookup\.ts:7/);
  assert.equal(byFile.has(path.resolve(ROOT, "billing-service/src/lookup.ts")), false);
});

test("a deleted file is surfaced as producer removed", () => {
  const byFile = toDiagnostics(fixture("check-deleted.json"), ROOT, CHECKED, { exists });
  const edited = byFile.get(CHECKED_ABS);
  const removed = edited?.filter((diagnostic) => diagnostic.code === "producer_removed");
  assert.equal(removed?.length, 2, "the verdict on the route and the file-level line");
  assert.match(
    removed?.[1]?.message ?? "",
    /the index holds 1 row\(s\) for this file and it is no longer on disk\. 1 counterpart\(s\) still name it\./,
  );
});

test("an error payload publishes nothing", () => {
  assert.equal(diagnosticsFor("check-not-indexed.json").size, 0);
});
