// A change to both sides of a contract in one session: a per-file check that
// read main's or the indexed copy of the other side's file does not state a
// break on that pair; the whole-change check (`check --changed`, both sides
// from the working tree) answers instead, and speaks only with a problem.
//
// The fixture, in our own words: a route `POST /items/upload` in
// `api/src/items.ts` replies `{ ok, itemId, fileName }`, and the page
// `web/src/pages/upload.tsx` reads that reply as the same fields. The rule is
// `withoutStaleCopyBreaks` (hook/late.ts) over `readStaleCopy` (hook/confirm.ts).

import assert from "node:assert/strict";
import test from "node:test";
import fs from "node:fs";
import os from "node:os";
import path from "node:path";

import { parseChangedResult, type CheckResult } from "../src/contract.ts";
import type { HeldLine } from "../src/held/daemon.ts";
import { deliverLate } from "../src/hook/late.ts";
import { answerEarly, enqueue, enqueueChanged, lockPath, queueDir, type Answer, type QueueEntry } from "../src/hook/rechecks.ts";
import { recordEdit, sessionsDir, writeAtomic } from "../src/hook/reuse.ts";
import { stopAnswers } from "../src/hook/stop-wait.ts";

const SESSION = "two-sided";
const ROUTE = "api/src/items.ts";
const PAGE = "web/src/pages/upload.tsx";
const OTHER = "api/src/jobs.ts";
const LABEL = "POST /items/upload";

type Bed = { root: string; home: string; at: (rel: string) => string };

function bed(t: { after: (fn: () => void) => void }): Bed {
  const root = fs.realpathSync(fs.mkdtempSync(path.join(os.tmpdir(), "carrick-two-sided-")));
  const home = fs.realpathSync(fs.mkdtempSync(path.join(os.tmpdir(), "carrick-home-")));
  const at = (rel: string): string => path.join(root, rel);
  for (const rel of [ROUTE, PAGE, OTHER]) {
    fs.mkdirSync(path.dirname(at(rel)), { recursive: true });
    fs.writeFileSync(at(rel), "export {};\n");
  }
  // A live holder of the runner lock: nothing here starts a real runner.
  fs.mkdirSync(path.dirname(lockPath(root)), { recursive: true });
  const now = new Date().toISOString();
  fs.writeFileSync(lockPath(root), `${JSON.stringify({ pid: process.pid, started: now, touched: now })}\n`);
  t.after(() => {
    fs.rmSync(root, { recursive: true, force: true });
    fs.rmSync(home, { recursive: true, force: true });
  });
  return { root, home, at };
}

/** The session edited `rel` at `at` (what the post-edit hook records). */
const edited = (b: Bed, rel: string, at: number): void => recordEdit(SESSION, b.root, at, b.home, [b.at(rel)]);

function queue(b: Bed, rel: string): QueueEntry {
  return enqueue(b.root, SESSION, [b.at(rel)], Date.now(), { home: b.home })[0]!;
}

function answer(b: Bed, entry: QueueEntry, result: CheckResult): void {
  const body: Answer = {
    seq: entry.seq,
    session: entry.session,
    file: entry.file,
    relative: entry.relative,
    root: entry.root,
    state: "answered",
    enqueued_at: entry.enqueued_at,
    started_at: entry.enqueued_at,
    finished_at: new Date().toISOString(),
    result,
  };
  writeAtomic(path.join(sessionsDir(b.home), `${SESSION}.recheck.${entry.seq}.answer.json`), `${JSON.stringify(body)}\n`);
}

const changedQueued = (b: Bed): QueueEntry[] =>
  fs.existsSync(queueDir(b.root))
    ? fs
        .readdirSync(queueDir(b.root))
        .map((name) => JSON.parse(fs.readFileSync(path.join(queueDir(b.root), name), "utf8")) as QueueEntry)
        .filter((entry) => entry.kind === "changed")
    : [];

/** The held compiler's break on the route (a row in another file than the edited one), against the page, the page read from main. */
function routeBreak(b: Bed): HeldLine {
  return {
    row_key: `contract::api::src/items.ts::producer::${LABEL}::0`,
    group: 0,
    label: LABEL,
    consumer: "web",
    file: "src/items.ts",
    line: 9,
    message: "Property 'fileName' is missing in type '{ ok: boolean; itemId: string; fileLabel: string; }'.",
    row_file: "src/items.ts",
    row_line: 9,
    row_path: b.at(ROUTE),
    places: [{ service: "web", file: "src/pages/upload.tsx", line: 14 }],
    expected: "{ ok: boolean; itemId: string; fileName: string }",
    main_commit: "0123456789abcdef0123456789abcdef01234567",
    named_files: [b.at(PAGE)],
    run: "e1.1",
  };
}

/** An entry for `rel` whose early answer lists the route's break and whose full answer is clean. */
function earlyOn(b: Bed, rel: string): QueueEntry {
  const entry = queue(b, rel);
  assert.ok(answerEarly(SESSION, entry.seq, { state: "ready", lines: [routeBreak(b)], ms: 3, service: "api/api", engine: "e1" }, Date.now(), b.home));
  return entry;
}

/** The page's own re-check: its call reads `fileLabel`, judged against the route at its indexed version (service `api` stale). */
function pageRecheck(b: Bed, reread: string[] = ["web"], stale: string[] = ["api"]): CheckResult {
  return {
    schema: "carrick.check/0",
    file: "src/pages/upload.tsx",
    service: "web",
    stale: true,
    items: [
      {
        kind: "call",
        method: "POST",
        path: "/items/upload",
        line: 14,
        col: 1,
        source: "fact",
        counterparts: [{ role: "producer", service: "api", repo: path.join(b.root, "api"), file: "src/items.ts", line: 9 }],
        verdict: { state: "resolved", result: "type_mismatch", detail: "Property 'fileLabel' is missing in type 'POST /items/upload → Response'" },
        direction: "response",
        stated: true,
      },
    ],
    recheck: {
      ran: "extraction+types",
      elapsed_ms: 5,
      scope: { reread, not_reread: stale.map((service) => ({ service, operations: 1, reason: "indexed" })), stale },
    },
  } as CheckResult;
}

const clean = (file: string): CheckResult => ({ schema: "carrick.check/0", file, items: [], recheck: { ran: "extraction+types", elapsed_ms: 5, scope: { reread: ["api"], not_reread: [], stale: [] } } }) as CheckResult;

/** The whole-change check's answer, as the runner writes it: clean, or the break on both sides. */
function answerChanged(b: Bed, entry: QueueEntry, broken: boolean): void {
  const raw = JSON.parse(fs.readFileSync(path.join(import.meta.dirname, "fixtures", "check-changed-clean.json"), "utf8")) as Record<string, unknown>;
  const problem = (file: string, role: string, line: number, other: string) => ({
    kind: "incompatible",
    file,
    line,
    role,
    method: "POST",
    path: "/items/upload",
    other: [other],
    reason: "Property 'fileTitle' is missing in type 'POST /items/upload → Response'",
    source: "fact",
    already_indexed: true,
    unconfirmed: false,
    outside_the_change: false,
    other_build: false,
    fails: true,
    stated: true,
  });
  const body = broken
    ? { ...raw, problems: [problem(ROUTE, "route", 9, `web ${PAGE}:14`), problem(PAGE, "call", 14, `api ${ROUTE}:9`)], failing: 2 }
    : raw;
  const changed = parseChangedResult(JSON.stringify(body), broken ? 1 : 0)!;
  writeAtomic(
    path.join(sessionsDir(b.home), `${SESSION}.recheck.${entry.seq}.answer.json`),
    `${JSON.stringify({ ...entry, state: "answered", started_at: entry.enqueued_at, finished_at: new Date().toISOString(), changed })}\n`,
  );
}

const states = (text: string): boolean => /POST \/items\/upload/.test(text) && /type_mismatch|is missing/.test(text);

// --- (a) a consistent change to both sides ---

for (const [order, gap] of [
  ["producer first", 300],
  ["producer first", 30_000],
  ["caller first", 300],
  ["caller first", 30_000],
] as const) {
  test(`(a) both sides renamed together, ${order}, ${gap / 1000} s apart: no break at the early answer or the page's re-check, the whole-change check queued, clean and silent`, (t) => {
    const b = bed(t);
    const start = Date.now() - gap - 1000;
    const [first, second] = order === "producer first" ? [ROUTE, PAGE] : [PAGE, ROUTE];
    edited(b, first, start);
    edited(b, second, start + gap);
    // The early answer on an edit of another file of the route's service names the route against the page (the evidence's shape).
    const trigger = earlyOn(b, OTHER);
    answer(b, trigger, clean("src/jobs.ts"));
    const page = queue(b, PAGE);
    answer(b, page, pageRecheck(b));
    const said = deliverLate(SESSION, Date.now(), new Set(), b.home).join("\n");
    assert.equal(states(said), false, said);
    assert.doesNotMatch(said, /read from main/, said);
    const queued = changedQueued(b);
    assert.equal(queued.length, 1, "the whole-change check is queued once");
    answerChanged(b, queued[0]!, false);
    assert.equal(deliverLate(SESSION, Date.now(), new Set(), b.home).join("\n"), "", "a clean whole-change check says nothing");
    assert.equal(stopAnswers(SESSION, { home: b.home, ensure: () => false }).lines.join("\n").match(/POST \/items\/upload/), null);
  });
}

// --- (b) known positive 1: one side edited ---

test("(b) only the route edited: the early break is stated, with its copy line, and nothing is queued", (t) => {
  const b = bed(t);
  edited(b, ROUTE, Date.now() - 2000);
  const trigger = earlyOn(b, OTHER);
  answer(b, trigger, clean("src/jobs.ts"));
  const said = deliverLate(SESSION, Date.now(), new Set(), b.home).join("\n");
  assert.ok(states(said), said);
  assert.match(said, /The caller's side was read from main at 0123456\.$/m, said);
  assert.equal(changedQueued(b).length, 0);
});

test("(b) only the page edited: its re-check's break is stated", (t) => {
  const b = bed(t);
  edited(b, PAGE, Date.now() - 2000);
  answer(b, queue(b, PAGE), pageRecheck(b));
  const said = deliverLate(SESSION, Date.now(), new Set(), b.home).join("\n");
  assert.ok(states(said), said);
  assert.equal(changedQueued(b).length, 0);
});

test("(b) both edited, but the re-check re-read the route's service from the working tree: its break is stated", (t) => {
  const b = bed(t);
  edited(b, ROUTE, Date.now() - 3000);
  edited(b, PAGE, Date.now() - 2000);
  answer(b, queue(b, PAGE), pageRecheck(b, ["web", "api"], []));
  const said = deliverLate(SESSION, Date.now(), new Set(), b.home).join("\n");
  assert.ok(states(said), said);
});

/** The route's own full re-check after a one-sided edit: a stated break whose row names the route file itself, as an index answer may. */
function routeRecheck(b: Bed, scope: boolean): CheckResult {
  return {
    schema: "carrick.check/0",
    file: "src/items.ts",
    service: "api",
    stale: false,
    items: [
      {
        kind: "call",
        method: "POST",
        path: "/items/upload",
        line: 9,
        source: "fact",
        counterparts: [{ role: "producer", service: "api", repo: path.join(b.root, "api"), file: "src/items.ts", line: 9 }],
        verdict: { state: "resolved", result: "type_mismatch", detail: "Property 'fileName' is missing in type 'POST /items/upload → Response'" },
        direction: "response",
        stated: true,
      },
    ],
    recheck: {
      ran: "extraction+types",
      elapsed_ms: 5,
      ...(scope ? { scope: { reread: ["api"], not_reread: [{ service: "web", operations: 1, reason: "indexed" }], stale: [] } } : {}),
      moved: [
        {
          file: ROUTE,
          line: 9,
          role: "route",
          method: "POST",
          path: "/items/upload",
          before: "compatible",
          after: "type_mismatch",
          counterparts: [`web ${PAGE}:14`],
          reason: "the caller reads fileName",
          not_judged_before: false,
          stated: true,
        },
      ],
    },
  } as CheckResult;
}

for (const scope of [false, true]) {
  test(`(b) only the route edited: its full re-check's break is stated at the next carrier (row naming the edited file itself, ${scope ? "with" : "no"} re-read scope)`, (t) => {
    const b = bed(t);
    edited(b, ROUTE, Date.now() - 2000);
    answer(b, queue(b, ROUTE), routeRecheck(b, scope));
    const said = deliverLate(SESSION, Date.now(), new Set(), b.home).join("\n");
    assert.match(said, /contract problem/, said);
    assert.match(said, /POST \/items\/upload/, said);
    assert.equal(changedQueued(b).length, 0, "nothing held back");
  });
}

test("(b) only the route edited, the turn ends before any carrier: Stop states the full re-check's break", (t) => {
  const b = bed(t);
  edited(b, ROUTE, Date.now() - 2000);
  answer(b, queue(b, ROUTE), routeRecheck(b, true));
  const stop = stopAnswers(SESSION, { home: b.home, ensure: () => false }).lines.join("\n");
  assert.match(stop, /contract problem/, stop);
  assert.equal(changedQueued(b).length, 0);
});

// --- (c) known positive 2: both edited and still inconsistent ---

test("(c) both edited, still inconsistent: not stated early; the whole-change check's problem prints at Stop when it has landed", (t) => {
  const b = bed(t);
  edited(b, ROUTE, Date.now() - 3000);
  edited(b, PAGE, Date.now() - 2000);
  answer(b, queue(b, PAGE), pageRecheck(b));
  assert.equal(states(deliverLate(SESSION, Date.now(), new Set(), b.home).join("\n")), false);
  answerChanged(b, changedQueued(b)[0]!, true);
  const stop = stopAnswers(SESSION, { home: b.home, ensure: () => false }).lines.join("\n");
  assert.match(stop, /POST \/items\/upload/, stop);
  assert.match(stop, /fileTitle/, stop);
});

test("(c) both edited, still inconsistent, the turn ends first: Stop is silent, and the next carrier prints the whole-change check's problem", (t) => {
  const b = bed(t);
  edited(b, ROUTE, Date.now() - 3000);
  edited(b, PAGE, Date.now() - 2000);
  answer(b, queue(b, PAGE), pageRecheck(b));
  const stop = stopAnswers(SESSION, { home: b.home, ensure: () => false }).lines.join("\n");
  assert.equal(states(stop), false, stop);
  const queued = changedQueued(b);
  assert.ok(queued.length >= 1, "a whole-change check is queued");
  answerChanged(b, queued.at(-1)!, true);
  const next = deliverLate(SESSION, Date.now(), new Set(), b.home).join("\n");
  assert.match(next, /POST \/items\/upload/, next);
  assert.match(next, /fileTitle/, next);
});

test("(c) a held early break queues the whole-change check when the line is held, before any Stop", (t) => {
  const b = bed(t);
  edited(b, ROUTE, Date.now() - 3000);
  edited(b, PAGE, Date.now() - 2000);
  earlyOn(b, OTHER);
  assert.equal(states(deliverLate(SESSION, Date.now(), new Set(), b.home).join("\n")), false);
  assert.equal(changedQueued(b).length, 1);
  answerChanged(b, changedQueued(b)[0]!, true);
  assert.match(deliverLate(SESSION, Date.now(), new Set(), b.home).join("\n"), /fileTitle/);
});

// --- (d) the page edited nowhere near the call: per file, the line is held too ---

test("(d) the page edited only by a comment at its top: the route's early break is held too (the rule is per file)", (t) => {
  const b = bed(t);
  edited(b, ROUTE, Date.now() - 3000);
  edited(b, PAGE, Date.now() - 2000);
  earlyOn(b, OTHER);
  assert.equal(states(deliverLate(SESSION, Date.now(), new Set(), b.home).join("\n")), false);
  assert.equal(changedQueued(b).length, 1, "the whole-change check answers for it");
});

// --- (e) neither counterpart edited: as today ---

test("(e) a session that edited neither counterpart: the early line is word for word as before", (t) => {
  const b = bed(t);
  edited(b, OTHER, Date.now() - 2000);
  const trigger = earlyOn(b, OTHER);
  answer(b, trigger, clean("src/jobs.ts"));
  const said = deliverLate(SESSION, Date.now(), new Set(), b.home).join("\n");
  assert.equal(
    said,
    [
      `Carrick's compiler engine checked ${OTHER} after your earlier edit to it; routes in other files broke:`,
      `- ${ROUTE}:9: this route ${LABEL} against web src/pages/upload.tsx:14 agreed before the edit and now reads type_mismatch.`,
      "  The caller's side was read from main at 0123456.",
    ].join("\n"),
  );
  assert.equal(changedQueued(b).length, 0);
});

test("(e) a session that edited neither counterpart: the page's re-check is word for word as before", (t) => {
  const b = bed(t);
  edited(b, OTHER, Date.now() - 2000);
  answer(b, queue(b, PAGE), pageRecheck(b));
  const said = deliverLate(SESSION, Date.now(), new Set(), b.home).join("\n");
  assert.ok(said.startsWith(`Carrick re-checked ${PAGE}: 1 contract problem in what it re-read; 1 service connected to this edit by import or operation was judged at its indexed version (api).`), said);
  assert.equal(changedQueued(b).length, 0);
});

test("(e) a counterpart the session edited after the line printed does not hold it", (t) => {
  const b = bed(t);
  edited(b, ROUTE, Date.now() - 3000);
  edited(b, PAGE, Date.now() + 60_000);
  earlyOn(b, OTHER);
  assert.ok(states(deliverLate(SESSION, Date.now(), new Set(), b.home).join("\n")));
});

// One whole-change check per change: one already queued after the newest edit is not doubled.
test("a whole-change check already queued after the newest edit is not queued again", (t) => {
  const b = bed(t);
  edited(b, ROUTE, Date.now() - 3000);
  edited(b, PAGE, Date.now() - 2000);
  enqueueChanged(b.root, SESSION, Date.now(), { home: b.home });
  answer(b, queue(b, PAGE), pageRecheck(b));
  deliverLate(SESSION, Date.now(), new Set(), b.home);
  assert.equal(changedQueued(b).length, 1);
});
