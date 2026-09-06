import test from "node:test";
import assert from "node:assert/strict";
import {
  boundaryFor,
  boundaryLines,
  itemLine,
  renderPostToolUse,
  renderSessionStart,
  serviceLine,
  shortHash,
} from "../src/render.ts";
import { fixture, statusFixture } from "./helpers.ts";

test("the hook context puts locations first and the boundary last", () => {
  const context = renderPostToolUse(fixture("check-mismatch.json"));
  assert.ok(context);
  const lines = context.split("\n");
  assert.match(lines[0] ?? "", /^Carrick checked src\/routes\/users\.ts against the workspace index \(user-service, indexed at 6a1b2c3\)/);
  // Problems first, by line, then the rows that only name a counterpart.
  assert.match(lines[1] ?? "", /^- src\/routes\/users\.ts:42:3/);
  assert.match(lines[2] ?? "", /^- src\/routes\/users\.ts:61:9/);
  assert.match(lines[3] ?? "", /^- src\/routes\/users\.ts:12:3/);
  const boundaryAt = lines.findIndex((line) => line.startsWith("Boundary:"));
  const lastItemAt = lines.reduce((at, line, index) => (line.startsWith("- ") ? index : at), -1);
  assert.ok(boundaryAt > lastItemAt, "the boundary follows every location line");
  assert.equal(lines.at(-1)?.startsWith("  "), true, "the boundary is the last thing rendered");
});

test("every route or call gets exactly one line", () => {
  const context = renderPostToolUse(fixture("check-mismatch.json"));
  const itemLines = (context ?? "").split("\n").filter((line) => line.startsWith("- "));
  assert.equal(itemLines.length, 5);
  for (const line of itemLines) assert.equal(line.includes("\n"), false);
});

test("a candidate row is labelled as one and names what produced it", () => {
  const context = renderPostToolUse(fixture("check-mismatch.json")) ?? "";
  const line = context.split("\n").find((text) => text.includes("/api/orders"));
  assert.ok(line);
  assert.match(line, /\[candidate, model\]/);
  assert.match(line, /method_mismatch \(no type verdict\): order-service serves PUT at this path/);
});

test("consumer sites ride the item line", () => {
  const context = renderPostToolUse(fixture("check-mismatch.json")) ?? "";
  const line = context.split("\n").find((text) => text.includes("/api/users/:id"));
  assert.ok(line);
  assert.match(line, /Consumers: order-service src\/clients\/users\.ts:18/);
  assert.match(line, /billing-service src\/lookup\.ts:7/);
});

test("a peer counterpart gets locations and no producer or consumer word", () => {
  const context = renderPostToolUse(fixture("check-mismatch.json")) ?? "";
  const line = context.split("\n").find((text) => text.includes("example-payments"));
  assert.ok(line);
  assert.match(line, /Same contract in: billing-service src\/charges\.ts:31/);
  assert.equal(/consumer|producer/i.test(line), false);
});

test("a not_checked row prints the state, never the word null", () => {
  const context = renderPostToolUse(fixture("check-mismatch.json")) ?? "";
  const line = context.split("\n").find((text) => text.includes("example-payments"));
  assert.ok(line);
  // result is null wherever the state is the whole statement.
  assert.match(line, /no type verdict: matched to a shared external contract/);
  assert.equal(/\bnull\b/.test(line), false);
  assert.equal(line.includes("not_checked"), false);
});

test("no rendered line anywhere prints the word null", () => {
  const rendered = [
    renderPostToolUse(fixture("check-mismatch.json")) ?? "",
    renderPostToolUse(fixture("check-deleted.json")) ?? "",
    renderPostToolUse(fixture("check-pre-rendered-boundary.json")) ?? "",
    renderSessionStart(statusFixture("status-workspace.json")),
  ].join("\n");
  assert.equal(/\bnull\b/.test(rendered), false);
  assert.equal(rendered.includes("undefined"), false);
});

test("a verdict that claims nothing says so, and a compiler verdict stands alone", () => {
  const context = renderPostToolUse(fixture("check-mismatch.json")) ?? "";
  // `unresolved` is the type layer's word: a side would not resolve, so there
  // is no result to print and nothing is asserted about the pair.
  const unresolved = context.split("\n").find((text) => text.includes("/api/audit/:id"));
  assert.match(unresolved ?? "", /no usable type on one side, so nothing is claimed: the consumer's expected type resolved to `any`/);
  assert.equal(/type_mismatch/.test(unresolved ?? ""), false);
  const compared = context.split("\n").find((text) => text.includes("/api/users/:id"));
  assert.match(compared ?? "", /type_mismatch: the response no longer carries/);
  assert.equal(compared?.includes("no type verdict"), false);
});

test("evidence rides the item line when the row states it", () => {
  const context = renderPostToolUse(fixture("check-mismatch.json")) ?? "";
  const line = context.split("\n").find((text) => text.includes("/api/users/:id"));
  assert.match(line ?? "", /Read off loader export claimed by a file-route convention/);
});

test("a deleted file says so and keeps its consumers", () => {
  const context = renderPostToolUse(fixture("check-deleted.json")) ?? "";
  assert.match(context, /This file is gone from disk and the index still holds 1 row\(s\) for it, with 1 counterpart\(s\)/);
  assert.match(context, /producer_removed/);
});

test("a stale file says the verdicts describe the indexed version", () => {
  const context = renderPostToolUse(fixture("check-mismatch.json")) ?? "";
  assert.match(context, /This file has changed since the index/);
  assert.match(context, /3 file\(s\) in the workspace have changed since 6a1b2c3/);
});

test("a file with no indexed rows still carries the boundary", () => {
  // The local index holds no bare receiver and no fetch call, so an empty
  // answer without the boundary would read as "nothing crosses a service here".
  const context = renderPostToolUse(fixture("check-clean.json")) ?? "";
  assert.match(context, /^Carrick checked src\/util\/format\.ts/);
  assert.equal(context.split("\n").some((line) => line.startsWith("- ")), false);
  assert.match(context, /^Boundary: A local index holds what the deterministic passes state/m);
});

test("nothing to say prints nothing", () => {
  assert.equal(renderPostToolUse(fixture("check-silent.json")), null);
  assert.equal(renderPostToolUse(fixture("check-not-indexed.json")), null);
});

test("no user-visible line carries an em-dash", () => {
  const rendered = [
    renderPostToolUse(fixture("check-mismatch.json")) ?? "",
    renderPostToolUse(fixture("check-deleted.json")) ?? "",
    renderSessionStart(statusFixture("status-workspace.json")),
    renderSessionStart(statusFixture("status-not-indexed.json")),
  ].join("\n");
  assert.equal(rendered.includes("—"), false);
  assert.equal(rendered.includes("–"), false);
});

test("the boundary keeps the CLI's wording", () => {
  const lines = boundaryLines(fixture("check-mismatch.json").boundary, "user-service");
  assert.equal(lines[0], "user-service at 6a1b2c3: 128 file(s) sent to the analyzer");
  assert.ok(
    lines.includes(
      "  2 file(s) the analyzer never answered for (e.g. src/legacy/huge.ts: analyzer timeout)",
    ),
  );
  assert.ok(
    lines.includes("  1 call site(s) that named a client member and did not resolve to it (usersClient.get)"),
  );
  assert.ok(lines.includes("  4 bare route-literal call site(s) left unclassified"));
  assert.ok(lines.includes("  6 row(s) the model alone states (11 joined a deterministic row)"));
  assert.ok(
    lines.includes("  1 model endpoint(s) dropped in modules a routing convention claims"),
  );
  assert.ok(
    lines.includes("  types captured on a bare checkout: anything through a dependency is `any`"),
  );
  assert.equal(
    lines.some((line) => line.includes("SDK call(s)")),
    false,
    "a zero count prints no line",
  );
});

test("the CLI's own boundary lines are printed as they arrive", () => {
  const result = fixture("check-pre-rendered-boundary.json");
  const context = renderPostToolUse(result) ?? "";
  const lines = context.split("\n");
  const sent = result.boundary_lines ?? [];
  assert.deepEqual(lines.slice(-sent.length), sent);
  assert.equal(
    lines.some((line) => line.startsWith("Boundary: ")),
    false,
    "a label glued to the front would no longer be the CLI's bytes",
  );
  assert.equal(
    context.includes("file(s) sent to the analyzer"),
    false,
    "the counts are not rendered a second time",
  );
  assert.deepEqual(boundaryFor(result), result.boundary_lines);
});

test("without the CLI's lines the note leads the counts, and the block is labelled", () => {
  const result = fixture("check-mismatch.json");
  assert.equal(result.boundary_lines, undefined);
  assert.deepEqual(boundaryFor(result), [
    result.boundary_note,
    ...boundaryLines(result.boundary, result.service),
  ]);
  const context = renderPostToolUse(result) ?? "";
  assert.match(context, /\nBoundary: A local index holds what the deterministic passes state/);
  assert.match(context, /\nuser-service at 6a1b2c3: 128 file\(s\) sent to the analyzer/);
});

test("the session line ends with each service's boundary lines, verbatim", () => {
  const status = statusFixture("status-workspace.json");
  const rendered = renderSessionStart(status).split("\n");
  const expected = status.services.flatMap((service) => service.boundary_lines ?? []);
  assert.deepEqual(rendered.slice(-expected.length), expected);
});

test("a local index dispatches nothing, so the header drops the analyzer count", () => {
  const lines = boundaryLines(
    { commit_hash: "abc1234def", files_attempted: 0, unemitted_literal_candidates: 3 },
    "webapp",
  );
  assert.equal(lines[0], "webapp at abc1234");
  assert.equal(lines[1], "  3 bare route-literal call site(s) left unclassified");
});

test("a boundary with a degraded type stage says which stage", () => {
  const lines = boundaryLines(
    {
      commit_hash: "abc1234def",
      files_attempted: 3,
      sdk_unresolved: { total: 2, reasons: ["@acme/sdk x2: no surface for the member"] },
      types_degraded: { stage: "capture", detail: "the sidecar ran out of memory" },
    },
    "user-service",
  );
  assert.ok(lines.includes("  types degraded at capture: the sidecar ran out of memory"));
  // Two of them, one reason listed, so the reason reads as an example.
  assert.ok(
    lines.includes("  2 SDK call(s) that produced no edge (e.g. @acme/sdk x2: no surface for the member)"),
  );
});

test("the session line is one line per service, with what it holds and how far it has moved", () => {
  const lines = renderSessionStart(statusFixture("status-workspace.json")).split("\n");
  assert.match(
    lines[0] ?? "",
    /^Carrick indexed 3 service\(s\) in \/workspace at 2026-09-06T21:14:03Z, scanner 0\.3\.41\.$/,
  );
  assert.match(
    lines[1] ?? "",
    /^- user-service at 6a1b2c3: 157 route\(s\), 12 call\(s\), changed since index: 7 \(/,
  );
  assert.match(lines[3] ?? "", /^- order-service at 9988776: 40 route\(s\), 61 call\(s\), changed since index: 120 \(/);
});

test("at most five stale files are named, and the rest are counted", () => {
  const lines = renderSessionStart(statusFixture("status-workspace.json")).split("\n");
  const first = lines[1] ?? "";
  assert.match(first, /\(src\/routes\/users\.ts, src\/routes\/orders\.ts, src\/lib\/db\.ts, src\/lib\/http\.ts, src\/util\/format\.ts, \+2 more\)$/);
  const truncated = lines[3] ?? "";
  assert.match(truncated, /, \+115 more\)$/, "the count comes from stale_files_total, not the list");
});

test("services sharing a repo say the changed count once", () => {
  const lines = renderSessionStart(statusFixture("status-workspace.json")).split("\n");
  assert.match(lines[2] ?? "", /^- user-admin at 6a1b2c3: 12 route\(s\), 3 call\(s\)\. Same repo as user-service, so the same 7 changed file\(s\)$/);
  assert.equal(
    (lines[2] ?? "").includes("changed since index:"),
    false,
    "the second service of a repo does not restate the count",
  );
});

test("a service line names its own repo's count when the repo is new", () => {
  const [service] = statusFixture("status-workspace.json").services;
  assert.ok(service);
  assert.match(serviceLine(service, null), /changed since index: 7/);
  assert.match(serviceLine(service, "another"), /Same repo as another, so the same 7 changed file\(s\)$/);
});

test("no index gives one line and the command that builds one", () => {
  const rendered = renderSessionStart(statusFixture("status-not-indexed.json"));
  assert.match(rendered, /^Carrick has no index for this workspace/);
  assert.match(rendered, /`carrick index --workspace <dir>` builds one\.$/);
});

test("a short hash is seven characters, and an absent one says so", () => {
  assert.equal(shortHash("6a1b2c3d4e5f"), "6a1b2c3");
  assert.equal(shortHash(undefined), "an unknown commit");
});

test("an item with no counterparts and no verdict still renders one line", () => {
  const line = itemLine({ kind: "route", method: "GET", path: "/x", line: 3, col: 1 }, "a.ts");
  assert.equal(line, "- a.ts:3:1 route GET /x");
});
