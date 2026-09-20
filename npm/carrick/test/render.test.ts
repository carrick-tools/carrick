import test from "node:test";
import assert from "node:assert/strict";
import {
  BOUNDARY_POINTER,
  LISTED_SERVICES,
  boundaryFor,
  boundaryLines,
  itemLine,
  renderPostToolUse,
  renderSessionStart,
  runningScanLine,
  serviceLine,
  shortHash,
  typedMismatchClause,
} from "../src/render.ts";
import type { CheckItem } from "../src/contract.ts";
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

test("a re-checked file says its verdicts came from the working tree", () => {
  const stale = fixture("check-mismatch.json");
  const rechecked = {
    ...stale,
    recheck: { ran: "extraction+types" as const, elapsed_ms: 2543 },
  };
  const context = renderPostToolUse(rechecked) ?? "";
  assert.match(context, /re-computed from your working tree in 2543 ms/);
  assert.doesNotMatch(context, /describe the indexed version/);
});

test("a re-check that missed its budget says how old the answer is", () => {
  const stale = fixture("check-mismatch.json");
  const degraded = {
    ...stale,
    recheck: {
      ran: "none" as const,
      elapsed_ms: 10004,
      stale_since: "2026-09-13T22:26:26Z",
      reason: "the re-scan ran past the budget",
    },
  };
  const context = renderPostToolUse(degraded) ?? "";
  assert.match(context, /describe the indexed version/);
  assert.match(context, /did not finish inside its budget.*2026-09-13T22:26:26Z/);
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

// A workspace of fifteen services printed about ninety lines into every
// session: each service, then each service's boundary report. Above three
// services the session gets the total, the services with something to do about
// them, and the command that prints the rest (carrick#1365).
test("a large workspace gets the services worth acting on, and a pointer to the rest", () => {
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
    hosted_state: "enriched" as const,
    boundary: undefined,
    boundary_lines: ["boundary line nobody acts on at session start"],
  }));
  const lines = renderSessionStart({ ...base, services, repos: [] }).split("\n");
  assert.match(lines[0] ?? "", /^Carrick indexed 15 service\(s\).*: 30 route\(s\), 15 call\(s\)\.$/);
  // Eight of the ten that moved, the other two counted, the five quiet ones
  // nowhere, and no boundary report at all.
  assert.equal(lines.filter((line) => line.startsWith("- svc-")).length, LISTED_SERVICES);
  assert.ok(lines.includes("- +2 more service(s) with files changed since the index"));
  assert.ok(!lines.some((line) => line.includes("svc-12")));
  assert.ok(!lines.some((line) => line.includes("boundary line nobody acts on")));
  assert.equal(lines.at(-1), BOUNDARY_POINTER);
  assert.equal(lines.length, 1 + LISTED_SERVICES + 1 + 1);
});

test("at most five stale files are named, and the rest are counted", () => {
  const lines = renderSessionStart(statusFixture("status-workspace.json")).split("\n");
  const first = lines[1] ?? "";
  assert.match(first, /\(src\/routes\/users\.ts, src\/routes\/orders\.ts, src\/lib\/db\.ts, src\/lib\/http\.ts, src\/util\/format\.ts, \+2 more\)$/);
  const truncated = lines[3] ?? "";
  assert.match(truncated, /, \+115 more\)$/, "the count comes from stale_files_total, not the list");
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
    serviceLine(service).includes("waiting for `carrick index`"),
    false,
    "a service with nothing waiting says nothing",
  );
  assert.match(
    serviceLine({ ...service, boundary: { ...service.boundary, candidates_awaiting_model: 8 } }),
    /8 candidate\(s\) waiting for `carrick index`$/,
  );
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
    /- monorepo: 2 file\(s\) changed outside every service \(carrick\.json, \.github\/workflows\/carrick\.yml\)/,
  );
});

test("no index gives one line and the command that builds one", () => {
  const rendered = renderSessionStart(statusFixture("status-not-indexed.json"));
  assert.match(rendered, /^Carrick has no index for this workspace/);
  assert.match(rendered, /`carrick index --workspace <dir>` builds one\.$/);
});

test("a session start states the refusal's own sentence, not its wire code (carrick#1009)", () => {
  // Every scanner release that moves the index format refuses every existing
  // workspace, and `index_unreadable` alone names no move.
  const refused = renderSessionStart({
    schema: "carrick.status/0",
    error: "index_unreadable",
    message:
      "/w/.carrick/index.json was written by a different scanner (index format 2, this build reads 3). Re-run `carrick index`.",
    services: [],
  });
  assert.match(refused, /index format 2, this build reads 3/);
  assert.match(refused, /Re-run `carrick index`/);

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
