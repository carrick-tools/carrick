import test from "node:test";
import assert from "node:assert/strict";
import {
  boundaryLines,
  itemLine,
  renderPostToolUse,
  renderSessionStart,
  shortHash,
} from "../src/render.ts";
import { fixture } from "./helpers.ts";

test("the hook context puts locations first and the boundary last", () => {
  const context = renderPostToolUse(fixture("check-mismatch.json"));
  assert.ok(context);
  const lines = context.split("\n");
  assert.match(lines[0] ?? "", /^Carrick checked src\/routes\/users\.ts against the workspace index \(user-service, indexed at 6a1b2c3\)/);
  // Problems first, by line, then the rows that only name a counterpart.
  assert.match(lines[1] ?? "", /^- src\/routes\/users\.ts:42:3/);
  assert.match(lines[4] ?? "", /^- src\/routes\/users\.ts:12:3/);
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
  assert.match(line, /method_mismatch: order-service serves PUT at this path/);
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
  const line = context.split("\n").find((text) => text.includes("stripe"));
  assert.ok(line);
  assert.match(line, /Same contract in: billing-service src\/charges\.ts:31/);
  assert.equal(/consumer|producer/i.test(line), false);
});

test("a row nothing was compared for is not reported as a problem", () => {
  const context = renderPostToolUse(fixture("check-mismatch.json")) ?? "";
  const line = context.split("\n").find((text) => text.includes("stripe"));
  // not_checked with a null result: listed because it names a counterpart,
  // after the three real problems, and never as a verdict of its own.
  assert.ok(line);
  assert.equal(line.includes("not_checked"), false);
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
  assert.match(context, /41 candidate\(s\) not classified locally: no model runs on this machine/);
});

test("nothing to say prints nothing", () => {
  assert.equal(renderPostToolUse(fixture("check-silent.json")), null);
  assert.equal(renderPostToolUse(fixture("check-not-indexed.json")), null);
});

test("no user-visible line carries an em-dash", () => {
  const rendered = [
    renderPostToolUse(fixture("check-mismatch.json")) ?? "",
    renderPostToolUse(fixture("check-deleted.json")) ?? "",
    renderSessionStart(fixture("touch-workspace.json")),
    renderSessionStart(fixture("check-not-indexed.json")),
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
  assert.ok(
    lines.includes("  41 candidate(s) not classified locally: no model runs on this machine"),
  );
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

test("a boundary with a degraded type stage says which stage", () => {
  const lines = boundaryLines(fixture("touch-workspace.json").boundary, "user-service");
  assert.ok(lines.includes("  types degraded at capture: the sidecar ran out of memory"));
  // Two of them, one reason listed, so the reason reads as an example.
  assert.ok(
    lines.includes("  2 SDK call(s) that produced no edge (e.g. @acme/sdk x2: no surface for the member)"),
  );
});

test("the session line states the index, the drift and the boundary", () => {
  const rendered = renderSessionStart(fixture("touch-workspace.json"));
  const lines = rendered.split("\n");
  assert.match(lines[0] ?? "", /^Carrick indexed user-service at 6a1b2c3\. 7 file\(s\) have changed/);
  assert.ok(lines.some((line) => line.startsWith("Boundary:")));
});

test("no index gives one line and the command that builds one", () => {
  const rendered = renderSessionStart(fixture("check-not-indexed.json"));
  assert.match(rendered, /^Carrick has no index for this workspace/);
  assert.match(rendered, /`carrick index` builds one\.$/);
});

test("a short hash is seven characters, and an absent one says so", () => {
  assert.equal(shortHash("6a1b2c3d4e5f"), "6a1b2c3");
  assert.equal(shortHash(undefined), "an unknown commit");
});

test("an item with no counterparts and no verdict still renders one line", () => {
  const line = itemLine({ kind: "route", method: "GET", path: "/x", line: 3, col: 1 }, "a.ts");
  assert.equal(line, "- a.ts:3:1 route GET /x");
});
