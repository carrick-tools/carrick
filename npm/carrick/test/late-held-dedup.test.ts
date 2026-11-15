// Part 2 does not repeat a part-3 line (part 3 T7): a background re-check's
// problem for a route the held compiler's line already named, in this session
// and at or after the edit, is dropped from the late answer. Every name is
// invented.

import assert from "node:assert/strict";
import test from "node:test";
import fs from "node:fs";
import os from "node:os";
import path from "node:path";
import { deliverOne, lateContext } from "../src/hook/late.ts";
import type { Answer } from "../src/hook/rechecks.ts";
import { markFacts } from "../src/hook/reuse.ts";
import { parseCheckResult } from "../src/contract.ts";
import { parseHeldClaim } from "../src/held/line.ts";
import { contractKey } from "../src/facts/lines.ts";
import { fixturePath } from "./helpers.ts";

const EDITED = Date.parse("2026-11-15T10:00:00Z");
const ROUTE = "GET /api/users/:id";
const ROUTE_FILE = "src/routes/users.ts";
const claimFor = (file: string, label: string, repo = "user-service") => `held::${contractKey({ repo, file, side: "producer", label }, 0)}::deadbeef`;

function scratch() {
  const home = fs.mkdtempSync(path.join(os.tmpdir(), "carrick-late-held-"));
  return { home, cleanup: () => fs.rmSync(home, { recursive: true, force: true }) };
}

function lateAnswer(file = `/w/${ROUTE_FILE}`): Answer {
  const result = parseCheckResult(fs.readFileSync(fixturePath("check-mismatch.json"), "utf8"))!;
  return {
    seq: "1",
    session: "s",
    file,
    relative: path.relative("/w", file),
    root: "/w",
    state: "answered",
    enqueued_at: new Date(EDITED).toISOString(),
    started_at: null,
    finished_at: new Date(EDITED + 5000).toISOString(),
    result: { ...result, stale: false },
  };
}

const say = (home: string, claim: string, at: number) => markFacts("s", { [claim]: { at, named: [] } }, home);
const printed = (home: string) => deliverOne("s", lateAnswer(), home).join("\n");

test("a part-3 line's claim spells its row's label as a late problem names it", () => {
  // The held claim carries the row's label; the late item builds `METHOD path{dispatch}`. One spelling.
  assert.deepEqual(parseHeldClaim(claimFor(ROUTE_FILE, ROUTE)), { repo: "user-service", file: ROUTE_FILE, label: ROUTE });
  assert.deepEqual(parseHeldClaim(claimFor("src/h.ts", "POST /events {type=refund}")), {
    repo: "user-service",
    file: "src/h.ts",
    label: "POST /events {type=refund}",
  });
  assert.equal(parseHeldClaim("contract::x::y::z"), null);
  // The wire row (fact-file fixture) labels a dispatching route as the CheckItem does.
  const labels = (fs.readFileSync(fixturePath("edit-facts.s1.ndjson"), "utf8").match(/"label":"[^"]*"/g) ?? []) as string[];
  assert.ok(labels.includes('"label":"POST /events {type=refund}"'));
});

test("a part-3 claim, then a matching late problem: the problem is not printed", () => {
  const s = scratch();
  try {
    say(s.home, claimFor(ROUTE_FILE, ROUTE), EDITED + 100);
    const text = printed(s.home);
    assert.doesNotMatch(text, /GET \/api\/users\/:id/);
    // The other problem of the same answer (a call) still prints.
    assert.match(text, /POST \/api\/orders/);
  } finally {
    s.cleanup();
  }
});

test("a claim older than the entry's edit: the problem is printed", () => {
  const s = scratch();
  try {
    say(s.home, claimFor(ROUTE_FILE, ROUTE), EDITED - 1);
    assert.match(printed(s.home), /GET \/api\/users\/:id/);
  } finally {
    s.cleanup();
  }
});

test("a different operation, or a different file: the problem is printed", () => {
  const s = scratch();
  try {
    say(s.home, claimFor(ROUTE_FILE, "GET /api/users"), EDITED + 100);
    say(s.home, claimFor("src/routes/other.ts", ROUTE), EDITED + 100);
    assert.match(printed(s.home), /GET \/api\/users\/:id/);
  } finally {
    s.cleanup();
  }
});

test("a part-3 line whose value sits in another file than its route still suppresses the problem", () => {
  const s = scratch();
  try {
    // The claim's `named` is the VALUE's file (HeldLine.file); the match reads the row's file from the row key.
    markFacts("s", { [claimFor(ROUTE_FILE, ROUTE)]: { at: EDITED + 100, named: ["src/shared/reply.ts"] } }, s.home);
    assert.doesNotMatch(printed(s.home), /GET \/api\/users\/:id/);
  } finally {
    s.cleanup();
  }
});

test("a part-3 line of this same action (not yet saved) suppresses it too", () => {
  const s = scratch();
  try {
    const pending = [{ repo: "user-service", file: ROUTE_FILE, label: ROUTE }];
    assert.doesNotMatch(deliverOne("s", lateAnswer(), s.home, pending).join("\n"), /GET \/api\/users\/:id/);
  } finally {
    s.cleanup();
  }
});

test("with no part-3 claim the output is what it was before (switch off)", () => {
  const s = scratch();
  try {
    // A part-1 contract claim and an unrelated fact claim are not part-3 claims.
    say(s.home, `contract::user-service::${ROUTE_FILE}::producer::${ROUTE}::0`, EDITED + 100);
    const answer = lateAnswer();
    const before = lateContext(answer, true);
    assert.ok(before && /GET \/api\/users\/:id/.test(before));
    assert.equal(deliverOne("s", answer, s.home).join("\n"), before);
  } finally {
    s.cleanup();
  }
});
