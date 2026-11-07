import test from "node:test";
import assert from "node:assert/strict";
import {
  BOUNDARY_POINTER,
  LISTED_SERVICES,
  MAX_EDIT_LINE_CHARS,
  CHECK_BEFORE_DONE,
  answersTheEdit,
  notRecheckedLine,
  pendingLine,
  boundaryFor,
  boundaryLines,
  itemLine,
  renderPostToolUse,
  renderSessionStart,
  runningScanLine,
  serviceLine,
  hasSomethingToDo,
  shortHash,
  typedMismatchClause,
} from "../src/render.ts";
import type { CheckItem, CheckResult } from "../src/contract.ts";
import { fixture, statusFixture } from "./helpers.ts";

// What an edit is told (the post-edit hook): only what this edit changed or
// what needs action, a few lines at most. Never the index's stored verdict
// list: on a large repo that was 8.7 KB of rows the edit did not touch.

/** The mismatch fixture as the re-check answers it from the working tree. */
function rechecked(): CheckResult {
  return { ...fixture("check-mismatch.json"), recheck: { ran: "extraction+types", elapsed_ms: 900 } };
}

/** `count` routes that name a consumer and claim nothing: the bulk of a stale answer. */
function connectedRows(count: number): CheckItem[] {
  return Array.from({ length: count }, (_, n) => ({
    kind: "route",
    method: "GET",
    path: `/api/things/${n}`,
    line: 100 + n,
    col: 1,
    source: "fact",
    counterparts: [{ role: "consumer", service: "web", file: "src/client.ts", line: n + 1 }],
    verdict: { state: "unresolved", result: null, detail: "a side did not resolve" },
  }));
}

function expectedNotRechecked(reason?: string): string {
  return `Carrick: src/routes/users.ts was not re-checked: ${reason ?? "no re-check of it ran"}. If this change touches a route, a call to another service, an event or a type another service reads, run \`carrick check --changed\` before you finish.`;
}

test("an edit the hook could not re-check lists none of the index's verdicts", () => {
  const stale = fixture("check-mismatch.json");
  stale.items = [...(stale.items ?? []), ...connectedRows(40)];
  for (const recheck of [
    undefined,
    {
      ran: "none" as const,
      elapsed_ms: 10004,
      stale_since: "2026-09-13T22:26:26.123456789Z",
      reason: "the re-scan ran past the budget",
    },
    { ran: "none" as const, elapsed_ms: 40, known_miss: true },
  ]) {
    assert.equal(
      renderPostToolUse({ ...stale, recheck }),
      expectedNotRechecked(recheck?.reason),
    );
  }
});

test("an edit the scan reads the same gets no not-re-checked line", () => {
  const same = fixture("check-mismatch.json");
  same.stale = false;
  same.recheck = {
    ran: "none",
    elapsed_ms: 0,
    reason: "the edit changed nothing the scan reads (comments or layout); verdicts are the index's",
  };
  assert.equal(answersTheEdit(same), true);
  assert.doesNotMatch(renderPostToolUse(same) ?? "", /not re-checked/);
});

test("a file left un-checked says why, and the pending and late lines read as pinned", () => {
  assert.equal(
    notRecheckedLine("src/a.ts", "an earlier re-check of this repo ran past its budget"),
    "Carrick: src/a.ts was not re-checked: an earlier re-check of this repo ran past its budget. If this change touches a route, a call to another service, an event or a type another service reads, run `carrick check --changed` before you finish.",
  );
  // A reason that already names the command is not told to run it twice.
  assert.equal(
    notRecheckedLine("src/a.ts", "too big; run `carrick check --changed`"),
    "Carrick: src/a.ts was not re-checked: too big; run `carrick check --changed`.",
  );
  assert.equal(
    pendingLine("src/a.ts", "user-service"),
    "Carrick is re-checking src/a.ts in the background (re-reading user-service). Its answer comes with the next Carrick line, and before you finish.",
  );
  assert.equal(
    pendingLine("src/a.ts"),
    "Carrick is re-checking src/a.ts in the background. Its answer comes with the next Carrick line, and before you finish.",
  );
  const late = renderPostToolUse(rechecked(), "src/a.ts", { late: true }) ?? "";
  assert.match(late, /^Carrick re-checked src\/a\.ts after your earlier edit to it: 2 contract problems\.\n/);
});

test("a re-checked edit lists its problems and nothing that is fine", () => {
  const context = renderPostToolUse(rechecked()) ?? "";
  const lines = context.split("\n");
  assert.equal(lines[0], "Carrick re-checked src/routes/users.ts after this edit: 2 contract problems.");
  assert.match(lines[1] ?? "", /^- src\/routes\/users\.ts:42:3 route GET \/api\/users\/:id .*type_mismatch/);
  assert.match(lines[2] ?? "", /^- src\/routes\/users\.ts:61:9 call POST \/api\/orders .*method_mismatch/);
  assert.equal(lines.length, 3, context);
  // The compatible row, the row that claims nothing and the peer row are not problems.
  for (const quiet of [":12:3", ":80:5", ":95:7", "compatible"]) {
    assert.equal(context.includes(quiet), false, quiet);
  }
});

test("a re-checked edit with many problems prints three and counts the rest", () => {
  const result = rechecked();
  const mismatch = result.items![0]!;
  result.items = Array.from({ length: 7 }, (_, n) => ({ ...mismatch, line: n + 1 }));
  const lines = (renderPostToolUse(result) ?? "").split("\n");
  assert.equal(lines[0], "Carrick re-checked src/routes/users.ts after this edit: 7 contract problems.");
  assert.equal(lines.filter((line) => line.startsWith("- src/")).length, 3);
  assert.equal(lines[4], "- and 4 more: `carrick check src/routes/users.ts` lists them.");
  assert.equal(lines.length, 5);
  for (const line of lines) assert.ok(line.length <= MAX_EDIT_LINE_CHARS, line);
});

test("a re-checked edit with no problem says so in one line", () => {
  const result = rechecked();
  result.items = result.items!.filter((item) => item.verdict?.result == null || item.verdict.result === "compatible");
  result.items.push(...connectedRows(40));
  assert.equal(
    renderPostToolUse(result),
    "Carrick re-checked src/routes/users.ts after this edit: no contract problem.",
  );
});

// No run-the-check clause: the per-file re-check answered, and the Stop hook
// queues `check --changed` itself for a change it could not state (carrick#3466).
test("a re-check that judged a service reading the edit at its indexed version never says no contract problem", () => {
  const result = rechecked();
  result.items = result.items!.filter((item) => item.verdict?.result == null || item.verdict.result === "compatible");
  result.recheck = {
    ran: "extraction+types",
    elapsed_ms: 900,
    scope: { reread: ["catalog"], not_reread: [], stale: ["inventory", "billing", "ledger", "search"] },
  };
  assert.equal(
    renderPostToolUse(result),
    "Carrick re-checked src/routes/users.ts: no contract problem in what it re-read; 4 services that read this edit were judged at their indexed version (inventory, billing, ledger, and 1 more).",
  );
  // With nothing stale, the headline is the plain one.
  result.recheck.scope = { reread: ["catalog"], not_reread: [], stale: [] };
  assert.equal(
    renderPostToolUse(result),
    "Carrick re-checked src/routes/users.ts after this edit: no contract problem.",
  );
});

/** A routed file whose pairs the check did not judge: every verdict is `not_checked`. */
function unjudged(): CheckResult {
  const result = rechecked();
  result.items = connectedRows(3).map((item) => ({
    ...item,
    verdict: { state: "not_checked", result: null, detail: "" },
  }));
  return result;
}

test("a re-check that judged no type pair never says no contract problem", () => {
  const result = unjudged();
  result.boundary = { types_degraded: { stage: "type check", detail: "sidecar not found" } };
  const line = renderPostToolUse(result, "src/routes/users.ts");
  assert.equal(
    line,
    "Carrick re-checked src/routes/users.ts after this edit: types were not checked (sidecar not found), so no contract verdict.",
  );
  assert.doesNotMatch(line ?? "", /no contract problem/);
  // With no reason on the answer, it still says types were not checked.
  delete result.boundary;
  assert.match(
    renderPostToolUse(result, "src/routes/users.ts") ?? "",
    /: types were not checked \(no pair in this file was compared\), so no contract verdict\.$/,
  );
});

test("a re-check whose pairs were compared but none resolved says so, with the count", () => {
  const result = rechecked();
  result.items = connectedRows(3);
  assert.match(
    renderPostToolUse(result, "src/routes/users.ts") ?? "",
    /: types were not checked \(3 pairs were compared but none resolved\), so no contract verdict\.$/,
  );
  result.items = connectedRows(1);
  assert.match(
    renderPostToolUse(result, "src/routes/users.ts") ?? "",
    /\(1 pair was compared but did not resolve\)/,
  );
  // A stated cause still comes first.
  result.boundary = { types_degraded: { stage: "type check", detail: "sidecar not found" } };
  assert.match(renderPostToolUse(result, "src/routes/users.ts") ?? "", /\(sidecar not found\)/);
});

test("the post-edit line names the reason the whole index was not type-checked", () => {
  const result = unjudged();
  // The boundary says something else: the build's own statement wins.
  result.boundary = { types_degraded: { stage: "type check", detail: "a different cause" } };
  result.type_check = { ran: false, reason: "type checker not found.", pairs: 2, judged: 0 };
  assert.equal(
    renderPostToolUse(result, "src/routes/users.ts"),
    "Carrick re-checked src/routes/users.ts after this edit: types were not checked (type checker not found), so no contract verdict.",
  );
  // A check that ran leaves the old reasons alone.
  result.type_check = { ran: true, pairs: 2, judged: 0 };
  assert.match(renderPostToolUse(result, "src/routes/users.ts") ?? "", /\(a different cause\)/);
});

test("session start says when the whole index was built without type checking", () => {
  const status = statusFixture("status-workspace.json");
  status.type_check = { ran: false, reason: "type checker not found", pairs: 3, judged: 0 };
  const lines = renderSessionStart(status).split("\n");
  assert.ok(
    lines.includes(
      "Types were not checked for this index (type checker not found), so no pair has a type verdict.",
    ),
    lines.join("\n"),
  );
  status.type_check = { ran: true, pairs: 3, judged: 3 };
  assert.doesNotMatch(renderSessionStart(status), /Types were not checked/);
  delete status.type_check;
  assert.doesNotMatch(renderSessionStart(status), /Types were not checked/);
});

test("a re-check that judged pairs and found none wrong still says no contract problem", () => {
  const result = rechecked();
  result.items = [
    {
      kind: "route",
      method: "GET",
      path: "/api/users",
      line: 10,
      col: 1,
      source: "fact",
      counterparts: [{ role: "consumer", service: "web", file: "src/api.ts", line: 4 }],
      verdict: { state: "resolved", result: "compatible", detail: "" },
    },
  ];
  assert.equal(
    renderPostToolUse(result, "src/routes/users.ts"),
    "Carrick re-checked src/routes/users.ts after this edit: no contract problem.",
  );
});

test("a file restored to its indexed text says nothing was re-checked", () => {
  const result = rechecked();
  result.items = [
    {
      kind: "route",
      method: "GET",
      path: "/api/users",
      line: 10,
      col: 1,
      source: "fact",
      verdict: { state: "resolved", result: "compatible", detail: "" },
    },
  ];
  result.stale = false;
  result.recheck = { ran: "none", elapsed_ms: 0, reason: "the file has not changed since the index" };
  const line = renderPostToolUse(result, "src/routes/users.ts");
  assert.equal(
    line,
    "Carrick: src/routes/users.ts matches the index, so there was nothing to re-check. The index shows: no contract problem.",
  );
  assert.doesNotMatch(line ?? "", /re-checked src/);
});

test("an edit never carries the service-wide boundary", () => {
  for (const name of ["check-mismatch.json", "check-pre-rendered-boundary.json"]) {
    const context = renderPostToolUse(fixture(name)) ?? "";
    assert.equal(context.includes("Boundary:"), false, name);
    assert.equal(context.includes("candidate(s) in this service need a model"), false, name);
    assert.equal(context.includes("sent to the analyzer"), false, name);
    assert.equal(context.includes("candidates not classified locally"), false, name);
  }
});

/** One fixture row as `itemLine` renders it, found by its path. */
function lineFor(name: string, pathPart: string): string {
  const result = fixture(name);
  const item = (result.items ?? []).find((row) => (row.path ?? "").includes(pathPart));
  assert.ok(item, pathPart);
  return itemLine(item, result.file);
}

test("a candidate row is labelled as one and names what produced it", () => {
  const line = lineFor("check-mismatch.json", "/api/orders");
  assert.ok(line);
  assert.match(line, /\[candidate, model\]/);
  assert.match(line, /method_mismatch \(no type verdict\): order-service serves PUT at this path/);
});

test("consumer sites ride the item line", () => {
  const line = lineFor("check-mismatch.json", "/api/users/:id");
  assert.match(line, /Consumers: order-service src\/clients\/users\.ts:18/);
  assert.match(line, /billing-service src\/lookup\.ts:7/);
});

test("a peer counterpart gets locations and no producer or consumer word", () => {
  const line = lineFor("check-mismatch.json", "example-payments");
  assert.match(line, /Same contract in: billing-service src\/charges\.ts:31/);
  assert.equal(/consumer|producer/i.test(line), false);
});

test("a not_checked row prints the state, never the word null", () => {
  const line = lineFor("check-mismatch.json", "example-payments");
  // result is null wherever the state is the whole statement.
  assert.match(line, /no type verdict: matched to a shared external contract/);
  assert.equal(/\bnull\b/.test(line), false);
  assert.equal(line.includes("not_checked"), false);
});

test("no rendered line anywhere prints the word null", () => {
  const rendered = [
    renderPostToolUse(rechecked()) ?? "",
    renderPostToolUse(fixture("check-mismatch.json")) ?? "",
    renderPostToolUse(fixture("check-deleted.json")) ?? "",
    renderPostToolUse(fixture("check-pre-rendered-boundary.json")) ?? "",
    renderSessionStart(statusFixture("status-workspace.json")),
  ].join("\n");
  assert.equal(/\bnull\b/.test(rendered), false);
  assert.equal(rendered.includes("undefined"), false);
});

test("a verdict that claims nothing says so, and a compiler verdict stands alone", () => {
  // `unresolved` is the type layer's word: a side would not resolve, so there
  // is no result to print and nothing is asserted about the pair.
  const unresolved = lineFor("check-mismatch.json", "/api/audit/:id");
  assert.match(unresolved, /no usable type on one side, so nothing is claimed: the consumer's expected type resolved to `any`/);
  assert.equal(/type_mismatch/.test(unresolved), false);
  const compared = lineFor("check-mismatch.json", "/api/users/:id");
  assert.match(compared, /type_mismatch: the response no longer carries/);
  assert.equal(compared.includes("no type verdict"), false);
});

test("evidence rides the item line when the row states it", () => {
  const line = lineFor("check-mismatch.json", "/api/users/:id");
  assert.match(line, /Read off loader export claimed by a file-route convention/);
});

test("a deleted file says so and names the problem it leaves", () => {
  const context = renderPostToolUse(fixture("check-deleted.json")) ?? "";
  const lines = context.split("\n");
  assert.equal(
    lines[0],
    "Carrick: src/routes/users.ts is gone from disk and the index still holds 1 row(s) for it, with 1 counterpart(s) still on the other side.",
  );
  assert.match(lines[1] ?? "", /^- src\/routes\/users\.ts:42:3 route GET \/api\/users\/:id .*producer_removed/);
  assert.equal(lines.length, 2);
});

test("a file with nothing to report still prints one line, and only that", () => {
  // The line is the only sign the hook ran: silence reads the same as a hook
  // that is not installed.
  for (const name of ["check-silent.json", "check-clean.json"]) {
    const result = fixture(name);
    result.boundary = { unemitted_literal_sites: [`${result.file}:7`] };
    // No rows: there was nothing to judge, so no contract claim is made.
    assert.equal(
      renderPostToolUse(result),
      "Carrick re-checked src/util/format.ts after this edit: no route or call to check.",
      name,
    );
  }
});

test("an error payload prints nothing", () => {
  assert.equal(renderPostToolUse(fixture("check-not-indexed.json")), null);
});

test("no user-visible line carries an em-dash", () => {
  const rendered = [
    renderPostToolUse(rechecked()) ?? "",
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
  assert.ok(lines.includes("  4 route-literal call site(s) left unclassified"));
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

test("a dropped model endpoint is printed with the row it names", () => {
  const lines = boundaryLines(
    {
      commit_hash: "abc1234def",
      files_attempted: 1,
      model_endpoints_dropped_unanchored: {
        total: 2,
        reasons: ["src/worker.ts:14 GET /metrics", "src/worker.ts:30 POST /audit"],
      },
    },
    "worker",
  );
  assert.ok(
    lines.includes(
      "  2 route(s) found by analysis but left out of the index: their registration site could not be located (e.g. src/worker.ts:14 GET /metrics)",
    ),
    lines.join("\n"),
  );
  const none = boundaryLines(
    {
      commit_hash: "abc1234def",
      files_attempted: 1,
      model_endpoints_dropped_unanchored: { total: 0 },
    },
    "worker",
  );
  assert.equal(none.some((line) => line.includes("model endpoint(s) dropped:")), false);
});

test("the CLI's own boundary lines are what boundaryFor returns", () => {
  const result = fixture("check-pre-rendered-boundary.json");
  assert.deepEqual(boundaryFor(result), result.boundary_lines);
});

test("without the CLI's lines the note leads the counts", () => {
  const result = fixture("check-mismatch.json");
  assert.equal(result.boundary_lines, undefined);
  assert.deepEqual(boundaryFor(result), [
    result.boundary_note,
    ...boundaryLines(result.boundary, result.service),
  ]);
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
  assert.equal(lines[1], "  3 route-literal call site(s) left unclassified");
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
    /^- user-service at 6a1b2c3: 157 route\(s\), 12 call\(s\), changed since index: 7$/,
  );
  assert.match(lines[3] ?? "", /^- order-service at 9988776: 40 route\(s\), 61 call\(s\), changed since index: 120$/);
});

// A workspace of fifteen services printed about ninety lines into every
// session: each service, then each service's boundary report. Above three
// services the session gets the total, the services with something to do about
// them, and the command that prints the rest (carrick#1365).
test("a large workspace gets one count and the services worth acting on, and a pointer to the rest", () => {
  const base = statusFixture("status-workspace.json");
  const template = base.services[0]!;
  const services = Array.from({ length: 15 }, (_, index) => ({
    ...template,
    service: `svc-${index}`,
    routes: 2,
    calls: 1,
    changed_since_index: index < 10 ? 1 : 0,
    stale_files: index < 10 ? ["src/a.ts"] : [],
    stale_files_total: index < 10 ? 1 : 0,
    // Two of them wait on the cloud scan; the others have nothing to act on.
    boundary: index < 2 ? { ...template.boundary, candidates_awaiting_model: 4 } : undefined,
    hosted_state: index < 2 ? ("no_index_yet" as const) : ("enriched" as const),
    boundary_lines: ["boundary line nobody acts on at session start"],
  }));
  const lines = renderSessionStart({ ...base, services, repos: [] }).split("\n");
  assert.match(lines[0] ?? "", /^Carrick indexed 15 service\(s\).*: 30 route\(s\), 15 call\(s\)\.$/);
  assert.equal(lines[1], "10 services have local changes since the index; `carrick status` lists them.");
  // Only the two with something to act on are named; the eight that merely
  // moved are in the count, and no boundary report prints.
  assert.deepEqual(
    lines.filter((line) => line.startsWith("- svc-")).map((line) => line.split(" ")[1]),
    ["svc-0", "svc-1"],
  );
  assert.ok(!lines.some((line) => line.includes("src/a.ts")));
  assert.ok(!lines.some((line) => line.includes("boundary line nobody acts on")));
  assert.equal(lines.at(-1), BOUNDARY_POINTER);
  assert.equal(lines.length, 1 + 1 + 2 + 1);
});

test("services past the listing cap are counted, not named", () => {
  const base = statusFixture("status-workspace.json");
  const template = base.services[0]!;
  const services = Array.from({ length: LISTED_SERVICES + 2 }, (_, index) => ({
    ...template,
    service: `svc-${index}`,
    changed_since_index: 0,
    hosted_state: "no_index_yet" as const,
  }));
  const lines = renderSessionStart({ ...base, services, repos: [] }).split("\n");
  assert.equal(lines.filter((line) => line.startsWith("- svc-")).length, LISTED_SERVICES);
  assert.ok(lines.includes("- +2 more service(s) with something to act on"));
});

// One edit to a shared file moves every service that reads it. Session start
// printed the same five paths on each of eight service lines (carrick#3477).
test("one shared file edited under eight services names no path and is three lines", () => {
  const base = statusFixture("status-workspace.json");
  const template = base.services[0]!;
  const services = Array.from({ length: 8 }, (_, index) => ({
    ...template,
    service: `lambda-${index}`,
    changed_since_index: 1,
    stale_files: ["_shared/db/client.ts"],
    stale_files_total: 1,
    boundary: undefined,
    hosted_state: "enriched" as const,
  }));
  const repos = [
    { repo: "/repos/mono", name: "mono", changed_since_index: 1, outside_every_service: 0, stale_files: [] },
  ];
  const rendered = renderSessionStart({ ...base, services, repos });
  const lines = rendered.split("\n");
  assert.doesNotMatch(rendered, /\.ts|_shared/, "no file path");
  assert.equal(lines.length, 3);
  assert.equal(lines[1], "8 services have local changes since the index; `carrick status` lists them.");
  assert.equal(lines[2], BOUNDARY_POINTER);
});

test("a small workspace's service lines carry counts and no file paths", () => {
  const lines = renderSessionStart(statusFixture("status-workspace.json")).split("\n");
  assert.match(lines[1] ?? "", /changed since index: 7$/);
  assert.match(lines[3] ?? "", /changed since index: 120$/, "the count comes from changed_since_index");
  assert.ok(!lines.slice(1, 4).some((line) => line.includes("src/")));
});

test("every service states the count of what its own scan reads", () => {
  // Services of one repo no longer share a changed-file count: each is told
  // about the files it reads, and the repo carries the rest (carrick#997).
  const lines = renderSessionStart(statusFixture("status-workspace.json")).split("\n");
  assert.match(lines[2] ?? "", /^- user-admin at 6a1b2c3: 12 route\(s\), 3 call\(s\), changed since index: /);
  assert.equal(
    (lines[2] ?? "").includes("Same repo as"),
    false,
    "no service points at another service's count",
  );
});

test("a service line names its own count, and what is waiting for a paid scan", () => {
  const [service] = statusFixture("status-workspace.json").services;
  assert.ok(service);
  assert.match(serviceLine(service), /changed since index: 7/);
  assert.equal(
    serviceLine(service).includes("waiting for"),
    false,
    "a service with nothing waiting says nothing",
  );
  // The scan they wait for is Carrick's, never one to run here (carrick#3361).
  assert.match(
    serviceLine({ ...service, boundary: { ...service.boundary, candidates_awaiting_model: 8 } }),
    /8 candidate\(s\) waiting for Carrick's cloud scan$/,
  );
});

test("a service answered by the hosted index says nothing about candidates waiting", () => {
  const [service] = statusFixture("status-workspace.json").services;
  assert.ok(service);
  const counted = { ...service, boundary: { ...service.boundary, candidates_awaiting_model: 8 } };
  assert.equal(serviceLine({ ...counted, hosted_state: "enriched" }).includes("candidate"), false);
  assert.match(serviceLine({ ...counted, hosted_state: "no_index_yet" }), /8 candidate\(s\) waiting/);
  assert.equal(hasSomethingToDo({ ...counted, changed_since_index: 0, hosted_state: "enriched" }), false);
});

test("files outside every service are the repo's line, not each service's", () => {
  const status = statusFixture("status-workspace.json");
  const rendered = renderSessionStart({
    ...status,
    repos: [
      {
        repo: "/repos/monorepo",
        name: "monorepo",
        changed_since_index: 9,
        outside_every_service: 2,
        stale_files: ["carrick.json", ".github/workflows/carrick.yml"],
      },
    ],
  });
  assert.match(
    rendered,
    /- monorepo: 2 file\(s\) changed outside every service$/m,
  );
  assert.doesNotMatch(rendered, /carrick\.json|workflows/, "the repo line counts files, it does not name them");
});

test("no index gives one line and the command that reads one in", () => {
  const rendered = renderSessionStart(statusFixture("status-not-indexed.json"));
  assert.match(rendered, /^Carrick has no index for this workspace/);
  // Carrick builds it; this machine reads it in, and never scans (carrick#3361).
  assert.match(rendered, /`carrick refresh --workspace <dir>` downloads the index\.$/);
  assert.doesNotMatch(rendered, /carrick index/);
});

test("a session start states the refusal's own sentence, not its wire code (carrick#1009)", () => {
  // Every scanner release that moves the index format refuses every existing
  // workspace, and `index_unreadable` alone names no move.
  const refused = renderSessionStart({
    schema: "carrick.status/0",
    error: "index_unreadable",
    message:
      "/w/.carrick/index.json was written by a different scanner (index format 2, this build reads 3). Run `carrick refresh` to download it again.",
    services: [],
  });
  assert.match(refused, /index format 2, this build reads 3/);
  assert.match(refused, /Run `carrick refresh` to download it again/);

  // With nothing more specific to say, the code is still the answer.
  assert.match(
    renderSessionStart({ schema: "carrick.status/0", error: "index_unreadable", services: [] }),
    /index_unreadable/,
  );
});

test("a short hash is seven characters, and an absent one says so", () => {
  assert.equal(shortHash("6a1b2c3d4e5f"), "6a1b2c3");
  assert.equal(shortHash(undefined), "an unknown commit");
});

test("an item with no counterparts and no verdict still renders one line", () => {
  const line = itemLine({ kind: "route", method: "GET", path: "/x", line: 3, col: 1 }, "a.ts");
  assert.equal(line, "- a.ts:3:1 route GET /x");
});

test("a session that starts while a scan is running is told so, not told there is no index", () => {
  // The detached scan is the ruled first run's shape (carrick#992): told "no
  // index", an agent starts a second one.
  const building = renderSessionStart({
    schema: "carrick.status/0",
    error: "not_indexed",
    services: [],
    running_scans: [
      {
        scan_id: "5089ed60",
        pid: 41234,
        started_at: "2026-09-12T10:00:00Z",
        status: "running",
        infer: true,
        phase: "indexing gateway",
        progress: { service: "gateway", phase: "files", done: 118, total: 240 },
        notice: "model busy: slowing analyze-file to 4 requests at a time",
      },
    ],
  });
  assert.match(building, /^Carrick has no index for this workspace yet, and a scan is building one\./);
  // Why it is slow rides beside the counts (carrick#1122).
  assert.match(
    building,
    /- scan 5089ed60 is running: indexing gateway, 118 of 240 files \(model busy: slowing analyze-file to 4 requests at a time\)\./,
  );
  assert.match(building, /\.carrick\/scan-5089ed60\.log/);

  const withIndex = renderSessionStart({
    ...statusFixture("status-workspace.json"),
    running_scans: [
      {
        scan_id: "abc12345",
        pid: 1,
        started_at: "2026-09-12T10:00:00Z",
        status: "failed",
        error: "the scan of api failed: A scan of acme/api is already running.\nUsing TeeStorage (laptop scan)\n... 12 line(s) not shown ...",
      },
    ],
  });
  // The reason the error leads with, and none of the log excerpt after it
  // (carrick#1103).
  assert.match(withIndex, /- scan abc12345 failed: the scan of api failed: A scan of acme\/api is already running\.$/m);
  assert.doesNotMatch(withIndex, /TeeStorage|not shown/);

  // The word the scaffold's poll loop waits for (carrick#1007 item 4).
  const done = renderSessionStart({
    ...statusFixture("status-workspace.json"),
    running_scans: [
      {
        scan_id: "5089ed60",
        pid: 41234,
        started_at: "2026-09-12T10:00:00Z",
        finished_at: "2026-09-12T10:00:58Z",
        infer: true,
        status: "finished",
      },
    ],
  });
  assert.match(done, /- scan 5089ed60 finished\. The index is written\./);
  // What a scan costs us never reaches a customer's terminal (carrick#1236).
  assert.doesNotMatch(done, /paid|US\$/);
});

// carrick#1033: the two shapes the compiler compared, in the line itself.

test("a response mismatch names both types and the consumer that reads one", () => {
  const context = renderPostToolUse(fixture("check-types.json")) ?? "";
  const line = context.split("\n").find((text) => text.includes("GET /api/users"));
  assert.ok(line);
  assert.ok(
    line.includes(
      "GET /api/users [fact, file_based_route] response is UsersResponse { users: UserV2[] }, consumer at notification-service server.ts:25 reads number",
    ),
    line,
  );
  // The compiler's own reason is kept, as its own segment.
  assert.match(line, /Type 'UsersResponse' is not assignable to type 'number'/);
});

test("a request mismatch swaps the roles and the row says it is the reader", () => {
  const context = renderPostToolUse(fixture("check-types.json")) ?? "";
  const line = context.split("\n").find((text) => text.includes("POST /api/users"));
  assert.ok(line);
  assert.ok(
    line.includes(
      "request is { name: string }, producer at identity-service src/routes/users.ts:14 expects CreateUser { name: string; email: string }",
    ),
    line,
  );
});

test("the typed sentence is built from the direction, and names no counterpart it cannot pin", () => {
  const response: CheckItem = {
    kind: "route",
    method: "GET",
    path: "/api/users",
    source: "fact",
    direction: "response",
    actual_type: "UsersResponse { users: UserV2[] }",
    expected_type: "number",
    verdict: { state: "resolved", result: "type_mismatch", detail: "not assignable" },
    counterparts: [
      { role: "consumer", service: "notification-service", file: "server.ts", line: 25 },
    ],
  };
  assert.equal(
    typedMismatchClause(response),
    "response is UsersResponse { users: UserV2[] }, consumer at notification-service server.ts:25 reads number",
  );

  // Two consumers: the verdict belongs to the pairing the finding named, not
  // to a consumer this row can identify, so neither is named.
  assert.equal(
    typedMismatchClause({
      ...response,
      counterparts: [
        { role: "consumer", service: "notification-service", file: "server.ts", line: 25 },
        { role: "consumer", service: "billing-service", file: "src/lookup.ts", line: 7 },
      ],
    }),
    "response is UsersResponse { users: UserV2[] }, a consumer reads number",
  );

  // The same contract read from the consumer's own file: the producer is the
  // sending side, and this row is the one that reads.
  assert.equal(
    typedMismatchClause({
      ...response,
      kind: "call",
      counterparts: [
        { role: "producer", service: "user-service", file: "src/routes/users.ts", line: 42 },
      ],
    }),
    "response is UsersResponse { users: UserV2[] } from producer at user-service src/routes/users.ts:42, this call reads number",
  );

  // A request read from the route that serves it: the roles swap again.
  assert.equal(
    typedMismatchClause({
      ...response,
      direction: "request",
      actual_type: "{ name: string }",
      expected_type: "CreateUser",
    }),
    "request is { name: string } from consumer at notification-service server.ts:25, this route expects CreateUser",
  );
});

test("a payload with no types keeps the sentence it had", () => {
  const untyped: CheckItem = {
    kind: "route",
    method: "GET",
    path: "/api/users",
    source: "fact",
    verdict: { state: "resolved", result: "type_mismatch", detail: "not assignable" },
  };
  assert.equal(typedMismatchClause(untyped), null);
  assert.match(itemLine(untyped, "a.ts"), /type_mismatch: not assignable/);
  // Half the fields is not enough to state a side, and neither is a verdict
  // that says something other than "the compiler compared these two".
  assert.equal(
    typedMismatchClause({ ...untyped, direction: "response", actual_type: "Widget" }),
    null,
  );
  assert.equal(
    typedMismatchClause({
      ...untyped,
      direction: "response",
      actual_type: "Widget",
      expected_type: "number",
      verdict: { state: "unresolved", result: null, detail: "a side did not resolve" },
    }),
    null,
  );
});

// carrick#1229: a workspace whose analysis is being done in the cloud is not a
// workspace with no index and nothing happening. The hook renders this on
// every session start.
test("a session that starts while Carrick Cloud is analysing is told so", () => {
  const building = renderSessionStart({
    schema: "carrick.status/0",
    error: "not_indexed",
    message: "no index",
    services: [],
    analysing: [
      "Carrick Cloud is still analysing owner/api — 64% (8570 of 13389 files). Run `carrick resume` when it is done.",
    ],
  });
  assert.match(building, /a scan is building one/);
  assert.match(building, /still analysing owner\/api/);
  assert.match(building, /carrick resume/);

  const handed = runningScanLine({
    scan_id: "5089ed60",
    pid: 1,
    started_at: "2026-09-16T10:00:00Z",
    status: "dispatched",
  });
  assert.match(handed, /handed this workspace to Carrick Cloud/);
  assert.doesNotMatch(handed, /is running|\.log/, "it is not still going, and its log ended");
});

// Who uses the edited file (carrick#2067): one line that counts the uses and
// names the command that lists them, after everything else, and only when the
// hook asks. The uses themselves are the Read hook's to print.
test("an edit's uses are one counted line, never the list", () => {
  const many = Array.from({ length: 400 }, (_, n) => `- f${n} (line ${n + 1}) has 1 caller(s) outside this file: src/x.ts:${n + 1}`);
  for (const result of [
    { ...rechecked(), uses_lines: many },
    { ...fixture("check-mismatch.json"), uses_lines: many },
  ]) {
    const lines = (renderPostToolUse(result) ?? "").split("\n");
    assert.equal(
      lines.at(-1),
      "Other files use what this file declares: `carrick check src/routes/users.ts` lists the 400 uses; open them if this edit changed a signature or a type.",
    );
    assert.equal(lines.some((line) => line.startsWith("- f")), false);
  }
  const uses = fixture("check-uses.json");
  assert.equal(
    renderPostToolUse(uses),
    "Carrick re-checked src/client.ts after this edit: no route or call to check.\nOther files use what this file declares: `carrick check src/client.ts` lists the 2 uses; open them if this edit changed a signature or a type.",
  );
});

test("a hook that has already shown a file's uses prints the rest unchanged", () => {
  const uses = fixture("check-uses.json");
  const { uses_lines: _dropped, ...without } = uses;
  assert.equal(renderPostToolUse(uses, undefined, { uses: false }), renderPostToolUse(without));
});

test("a payload without uses lines renders exactly as before", () => {
  for (const name of ["check-mismatch.json", "check-counterpart.json", "check-clean.json"]) {
    const parsed = fixture(name);
    assert.equal(parsed.uses_lines, undefined, name);
    assert.equal(renderPostToolUse(parsed), renderPostToolUse(parsed, undefined, { uses: true }), name);
  }
});

test("the session-start check line is short and names both exit codes", () => {
  assert.equal(
    CHECK_BEFORE_DONE,
    "When your change touches a route, a call to another service, an event or a type another service reads, run `carrick check --changed` before you call it done. Exit 1 is a contract problem; exit 2 means nothing could be checked.",
  );
});

test("an edit names the rows outside the file that it moved, and calls an unjudged one no break", () => {
  const result = rechecked();
  result.recheck = {
    ran: "extraction+types",
    elapsed_ms: 900,
    moved: [
      {
        file: "web/src/client.ts",
        line: 14,
        role: "call",
        method: "GET",
        path: "/api/v2/widgets/:id",
        before: "matched",
        after: "no_route",
        counterparts: [],
        reason: "the index matched this call, and no service serves it now",
        not_judged_before: false,
      },
      {
        file: "api/src/widgets.ts",
        line: 9,
        role: "route",
        method: "GET",
        path: "/api/widgets/:id",
        before: "not_judged",
        after: "type_mismatch",
        counterparts: ["web web/src/client.ts:20"],
        reason: "x",
        not_judged_before: true,
      },
    ],
  };
  const text = renderPostToolUse(result) ?? "";
  assert.match(
    text,
    /- web\/src\/client\.ts:14: this call GET \/api\/v2\/widgets\/:id matched a route in the index and this edit left it with none\./,
  );
  assert.match(text, /api\/src\/widgets\.ts:9: this route GET .* against web web\/src\/client\.ts:20 reads type_mismatch now; the index never judged it, so this edit may not be the cause\./);
});

test("a moved row of another build's index says that build may be the cause", () => {
  const result = rechecked();
  result.recheck = {
    ran: "extraction+types",
    elapsed_ms: 900,
    moved: [
      {
        file: "api/src/widgets.ts",
        line: 9,
        role: "route",
        method: "GET",
        path: "/api/widgets/:id",
        before: "compatible",
        after: "type_mismatch",
        counterparts: ["web web/src/client.ts:20"],
        reason: "x",
        not_judged_before: false,
        other_build: true,
      },
    ],
  };
  const text = renderPostToolUse(result) ?? "";
  assert.match(
    text,
    /api\/src\/widgets\.ts:9: this route GET .* against web web\/src\/client\.ts:20 reads type_mismatch now; the index was built by another carrick version, so this edit may not be the cause\./,
  );
  assert.doesNotMatch(text, /agreed in the index/);
});

test("a check --changed that ran after the last edit drops the clause that sends the reader to it", () => {
  assert.equal(
    notRecheckedLine("src/a.ts", "an earlier re-check of this repo ran past its budget", true),
    "Carrick: src/a.ts was not re-checked: an earlier re-check of this repo ran past its budget.",
  );
  assert.match(notRecheckedLine("src/a.ts", "an earlier re-check of this repo ran past its budget"), /run `carrick check --changed` before you finish\.$/);
  // A re-check that answered, with a service judged at its indexed version,
  // never carries it: the Stop hook queues the check (carrick#3466).
  const result = rechecked();
  result.items = result.items!.filter((item) => item.verdict?.result == null || item.verdict.result === "compatible");
  result.recheck = {
    ran: "extraction+types",
    elapsed_ms: 900,
    scope: { reread: ["catalog"], not_reread: [], stale: ["inventory"] },
  };
  assert.doesNotMatch(renderPostToolUse(result) ?? "", /check --changed/);
});
