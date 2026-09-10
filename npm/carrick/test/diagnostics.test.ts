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

// ---------------------------------------------------------------- the budget
//
// One test per rule in carrick#879's acceptance list. The payloads are written
// here rather than as fixtures: a forty-row file is a shape, not an answer key,
// and reading it beside its assertion is the point.

import { capDiagnostics, MAX_PER_CHECK, MAX_PER_FILE } from "../src/diagnostics.ts";
import { DEFAULT_SURFACES } from "../src/surfaces.ts";
import type { CheckItem, CheckResult } from "../src/contract.ts";

/** A client that has somewhere else to show the boundary, as VS Code does. */
const WITH_SURFACE = { ...DEFAULT_SURFACES, boundarySurface: true };

function mismatch(line: number, counterparts: CheckItem["counterparts"] = []): CheckItem {
  return {
    kind: "route",
    method: "GET",
    path: `/api/thing/${line}`,
    line,
    col: 3,
    source: "fact",
    counterparts,
    verdict: { state: "resolved", result: "type_mismatch", detail: "the response lost a field" },
  };
}

function payload(items: CheckItem[]): CheckResult {
  return { schema: "carrick.check/0", file: CHECKED, service: "user-service", items };
}

test("a file the index holds no finding for publishes nothing at all", () => {
  const byFile = toDiagnostics(fixture("check-clean.json"), ROOT, CHECKED, {
    exists,
    surfaces: WITH_SURFACE,
  });
  assert.equal(byFile.size, 0, "not one Information row, none");
});

test("the boundary falls back to a file-level row only for a client with nowhere else", () => {
  const fallback = diagnosticsFor("check-clean.json").get(CHECKED_ABS);
  assert.equal(fallback?.length, 1);
  assert.equal(fallback?.[0]?.code, "boundary");

  const off = toDiagnostics(fixture("check-clean.json"), ROOT, CHECKED, {
    exists,
    surfaces: { ...DEFAULT_SURFACES, boundary: false },
  });
  assert.equal(off.size, 0, "carrick.boundary off removes the fallback");
});

test("each switch turns off its own surface and leaves the other standing", () => {
  const noFindings = toDiagnostics(fixture("check-mismatch.json"), ROOT, CHECKED, {
    exists,
    surfaces: { ...DEFAULT_SURFACES, diagnostics: false },
  });
  assert.deepEqual(
    noFindings.get(CHECKED_ABS)?.map((diagnostic) => diagnostic.code),
    ["boundary"],
    "diagnostics off keeps the boundary",
  );
  assert.equal(noFindings.size, 1, "and mirrors nothing onto a counterpart");

  const noBoundary = toDiagnostics(fixture("check-mismatch.json"), ROOT, CHECKED, {
    exists,
    surfaces: { ...DEFAULT_SURFACES, boundary: false },
  });
  assert.deepEqual(
    noBoundary.get(CHECKED_ABS)?.map((diagnostic) => diagnostic.code),
    ["type_mismatch", "method_mismatch"],
    "boundary off keeps the findings",
  );
});

test("forty findings publish eleven, and the eleventh names the rest and the command", () => {
  const items = Array.from({ length: 40 }, (_, index) => mismatch(index + 1));
  const rows = toDiagnostics(payload(items), ROOT, CHECKED, {
    exists,
    surfaces: WITH_SURFACE,
  }).get(CHECKED_ABS);
  assert.equal(rows?.length, MAX_PER_FILE + 1);
  assert.equal(
    rows?.at(-1)?.message,
    "and 30 more finding(s) in this file, from `carrick check user-service/src/routes/users.ts`.",
  );
  assert.equal(rows?.at(-1)?.code, "capped");
  // Problems first, then by line: the ten that survive are the first ten lines.
  assert.deepEqual(
    rows?.slice(0, MAX_PER_FILE).map((row) => row.range.start.line),
    Array.from({ length: MAX_PER_FILE }, (_, index) => index),
  );
});

test("a finding with four counterparts publishes one row per counterpart file", () => {
  const counterparts = [
    {
      role: "consumer",
      service: "order-service",
      repo: `${ROOT}/order-service`,
      file: "src/clients/users.ts",
      line: 18,
    },
    {
      role: "consumer",
      service: "order-service",
      repo: `${ROOT}/order-service`,
      file: "src/clients/users.ts",
      line: 44,
    },
    {
      role: "consumer",
      service: "order-service",
      repo: `${ROOT}/order-service`,
      file: "src/server.ts",
      line: 120,
    },
    {
      role: "consumer",
      service: "billing-service",
      repo: `${ROOT}/billing-service`,
      file: "src/charges.ts",
      line: 31,
    },
  ];
  const byFile = toDiagnostics(payload([mismatch(42, counterparts)]), ROOT, CHECKED, {
    exists,
    surfaces: WITH_SURFACE,
  });
  const client = byFile.get(path.resolve(ROOT, "order-service/src/clients/users.ts"));
  assert.equal(client?.length, 1, "two call sites in one file are one row");
  assert.match(client?.[0]?.message ?? "", /Also at line 44 in this file\./);
  assert.equal(client?.[0]?.range.start.line, 17, "and it lands on the first of them");
  assert.equal(byFile.size, 4, "the checked file and three counterpart files");
  // Every site is still clickable, whether or not it got a row of its own.
  assert.equal(byFile.get(CHECKED_ABS)?.[0]?.relatedInformation?.length, 4);
});

test("a mirrored row counts against the receiving file's own cap", () => {
  // Twelve findings, every one of them mirrored into the same consumer file.
  const items = Array.from({ length: 12 }, (_, index) =>
    mismatch(index + 1, [
      {
        role: "consumer",
        service: "order-service",
        repo: `${ROOT}/order-service`,
        file: "src/server.ts",
        line: index + 100,
      },
    ]),
  );
  const consumer = toDiagnostics(payload(items), ROOT, CHECKED, {
    exists,
    surfaces: WITH_SURFACE,
  }).get(path.resolve(ROOT, "order-service/src/server.ts"));
  assert.equal(consumer?.length, MAX_PER_FILE + 1);
  assert.match(consumer?.at(-1)?.message ?? "", /^and 2 more finding\(s\) in this file/);
});

test("one check publishes thirty rows at most, and says what it did not show", () => {
  const items = Array.from({ length: 25 }, (_, index) =>
    mismatch(index + 1, [
      {
        role: "consumer",
        service: "order-service",
        repo: `${ROOT}/service-${index}`,
        file: "src/client.ts",
        line: 10,
      },
    ]),
  );
  const byFile = toDiagnostics(payload(items), ROOT, CHECKED, {
    exists: () => true,
    surfaces: WITH_SURFACE,
  });
  const total = [...byFile.values()].reduce((sum, rows) => sum + rows.length, 0);
  assert.equal(total, MAX_PER_CHECK);
  const checked = byFile.get(CHECKED_ABS);
  assert.match(checked?.at(-1)?.message ?? "", /^and \d+ more finding\(s\) elsewhere in this check/);
  // A file the budget dropped publishes an empty list, so its previous rows go.
  assert.ok(
    [...byFile.values()].some((rows) => rows.length === 0),
    "dropped files are cleared rather than left standing",
  );
});

test("no diagnostic Carrick publishes has severity Hint", () => {
  const payloads: CheckResult[] = [
    fixture("check-mismatch.json"),
    fixture("check-clean.json"),
    fixture("check-deleted.json"),
    fixture("check-pre-rendered-boundary.json"),
    payload(Array.from({ length: 40 }, (_, index) => mismatch(index + 1))),
  ];
  for (const one of payloads) {
    for (const surfaces of [DEFAULT_SURFACES, WITH_SURFACE]) {
      for (const rows of toDiagnostics(one, ROOT, CHECKED, { exists, surfaces }).values()) {
        for (const row of rows) assert.notEqual(row.severity, SEVERITY.hint);
      }
    }
  }
});

test("a routing finding is an error only where its other side is on this disk", () => {
  const routing: CheckItem = {
    kind: "call",
    source: "fact",
    verdict: { state: "not_checked", result: "method_mismatch" },
  };
  assert.equal(severityOf(routing, true), SEVERITY.error);
  assert.equal(
    severityOf(routing, false),
    SEVERITY.warning,
    "an error a reader cannot go and look at asserts more than the payload supports",
  );
  // A type verdict is a claim about this file, so it stands on its own.
  assert.equal(severityOf(mismatch(1), false), SEVERITY.error);
});

test("capping an empty check publishes nothing", () => {
  assert.equal(capDiagnostics(new Map(), CHECKED_ABS, CHECKED).size, 0);
});

test("the boundary is the last row on a file, even when the cap trips", () => {
  const items = Array.from({ length: 25 }, (_, index) =>
    mismatch(index + 1, [
      {
        role: "consumer",
        service: "order-service",
        repo: `${ROOT}/service-${index}`,
        file: "src/client.ts",
        line: 10,
      },
    ]),
  );
  const rows = toDiagnostics(
    { ...payload(items), boundary_note: "41 candidate(s) were not classified here." },
    ROOT,
    CHECKED,
    { exists: () => true },
  ).get(CHECKED_ABS);
  assert.equal(rows?.at(-1)?.code, "boundary", "after every overflow row, as the hook renders it");
  assert.match(rows?.at(-2)?.message ?? "", /more finding\(s\) elsewhere in this check/);
});

// ------------------------------------------------------ the span a row marks
//
// carrick#922: the index holds line-only locators, so every row used to carry a
// one-character range and an editor underlined the indent rather than the code.
// The line's own text is the span, and the text of the file the row lands IN.

import { lineSpan, type LineText } from "../src/diagnostics.ts";

const CONSUMER_ABS = path.resolve(ROOT, "order-service/src/clients/users.ts");
/** 14 characters, of which the first two are the indent. */
const CHECKED_LINE_42 = "  send(users);";
/** 24 characters, and a different width from the checked file's line. */
const CONSUMER_LINE_18 = "    await client.get(id);";

/** The lines of named files, 1-based, as the reader `toDiagnostics` takes. */
function linesOf(files: Record<string, Record<number, string>>): LineText {
  return (file, line) => files[file]?.[line] ?? null;
}

const withText = linesOf({
  [CHECKED_ABS]: { 1: "import { send } from './send';", 42: CHECKED_LINE_42, 61: "  post(order);" },
  [CONSUMER_ABS]: { 18: CONSUMER_LINE_18 },
});

test("a finding underlines its line, first non-whitespace to last (carrick#922)", () => {
  const rows = toDiagnostics(fixture("check-mismatch.json"), ROOT, CHECKED, {
    exists,
    lineText: withText,
  }).get(CHECKED_ABS);
  const finding = rows?.[0];
  assert.equal(finding?.code, "type_mismatch");
  assert.deepEqual(finding?.range, {
    start: { line: 41, character: 2 },
    end: { line: 41, character: 14 },
  });
});

test("a mirrored row and its counterpart location read the OTHER file's line", () => {
  const byFile = toDiagnostics(fixture("check-mismatch.json"), ROOT, CHECKED, {
    exists,
    lineText: withText,
  });
  const mirrored = byFile.get(CONSUMER_ABS)?.[0];
  // The counterpart is at line 18 of the consumer, and the span is that line's
  // text: a row that took its width from the file it was found in would mark
  // the wrong columns in every file it is mirrored onto.
  assert.deepEqual(mirrored?.range, {
    start: { line: 17, character: 4 },
    end: { line: 17, character: 25 },
  });
  const related = byFile.get(CHECKED_ABS)?.[0]?.relatedInformation?.[0];
  assert.ok(related?.location.uri.endsWith("order-service/src/clients/users.ts"));
  assert.deepEqual(related?.location.range, mirrored?.range);
});

test("the boundary row underlines the first line of the file it lands on", () => {
  const rows = toDiagnostics(fixture("check-mismatch.json"), ROOT, CHECKED, {
    exists,
    lineText: withText,
  }).get(CHECKED_ABS);
  const boundary = rows?.at(-1);
  assert.equal(boundary?.code, "boundary");
  assert.deepEqual(boundary?.range, {
    start: { line: 0, character: 0 },
    end: { line: 0, character: 30 },
  });
});

test("no text, a blank line, or a line the file does not have keeps the fallback", () => {
  const oneCharacter = { start: { line: 41, character: 2 }, end: { line: 41, character: 3 } };
  // A payload's `col` still decides the fallback: it is the only thing left to
  // place the mark with.
  assert.deepEqual(lineSpan(CHECKED_ABS, 42, 3, undefined), oneCharacter);
  assert.deepEqual(lineSpan(CHECKED_ABS, 42, 3, () => null), oneCharacter);
  assert.deepEqual(lineSpan(CHECKED_ABS, 42, 3, () => "      "), oneCharacter);
  // And with text there is no column to honour: the locator is line-only, so
  // the whole statement is the span.
  assert.deepEqual(lineSpan(CHECKED_ABS, 42, 3, () => CHECKED_LINE_42), {
    start: { line: 41, character: 2 },
    end: { line: 41, character: 14 },
  });
});

test("a span is in UTF-16 code units, so a multi-byte line is not off by its bytes", () => {
  // 16 code units and 19 UTF-8 bytes: `character` in LSP is the former at the
  // default position encoding, and a JavaScript string index already is one.
  // The scanner's own spans are byte offsets, which is the trap carrick#805
  // was, and every ASCII-only fixture is blind to the difference.
  const line = `  ok("café 🎉");`;
  assert.equal(line.length, 16);
  assert.equal(Buffer.byteLength(line, "utf8"), 19);
  assert.deepEqual(lineSpan(CHECKED_ABS, 7, 1, () => line), {
    start: { line: 6, character: 2 },
    end: { line: 6, character: 16 },
  });
});
