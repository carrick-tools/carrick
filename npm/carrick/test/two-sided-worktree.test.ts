// The two-sided rule of two-sided-edit.test.ts, for a session working in a linked
// worktree: the index names the counterpart under the main checkout, the session's
// edits are recorded under its own worktree. Same repo-relative path = edited
// (`withoutStaleCopyBreaks`, hook/late.ts). Another worktree's edits do not count.

import assert from "node:assert/strict";
import test from "node:test";
import { execFileSync } from "node:child_process";
import fs from "node:fs";
import os from "node:os";
import path from "node:path";

import type { CheckResult } from "../src/contract.ts";
import { deliverLate } from "../src/hook/late.ts";
import { enqueue, lockPath, queueDir, type Answer, type QueueEntry } from "../src/hook/rechecks.ts";
import { recordEdit, sessionsDir, writeAtomic } from "../src/hook/reuse.ts";
import { stopAnswers } from "../src/hook/stop-wait.ts";

const SESSION = "two-sided-wt";
const ROUTE = "api/src/items.ts";
const PAGE = "web/src/pages/upload.tsx";

type Bed = { root: string; home: string; mine: string; theirs: string };

function git(cwd: string, ...args: string[]): void {
  execFileSync("git", ["-c", "user.email=t@t", "-c", "user.name=t", ...args], { cwd, stdio: "ignore" });
}

function bed(t: { after: (fn: () => void) => void }): Bed {
  const root = fs.realpathSync(fs.mkdtempSync(path.join(os.tmpdir(), "carrick-two-wt-")));
  const home = fs.realpathSync(fs.mkdtempSync(path.join(os.tmpdir(), "carrick-home-")));
  for (const rel of [ROUTE, PAGE]) {
    fs.mkdirSync(path.dirname(path.join(root, rel)), { recursive: true });
    fs.writeFileSync(path.join(root, rel), "export {};\n");
  }
  git(root, "init", "-q", "-b", "main");
  git(root, "add", ".");
  git(root, "commit", "-q", "-m", "init");
  const mine = path.join(root, ".claude", "worktrees", "mine");
  const theirs = path.join(root, ".claude", "worktrees", "theirs");
  git(root, "worktree", "add", "-q", "-b", "mine", mine);
  git(root, "worktree", "add", "-q", "-b", "theirs", theirs);
  fs.mkdirSync(path.dirname(lockPath(root)), { recursive: true });
  const now = new Date().toISOString();
  fs.writeFileSync(lockPath(root), `${JSON.stringify({ pid: process.pid, started: now, touched: now })}\n`);
  t.after(() => {
    fs.rmSync(root, { recursive: true, force: true });
    fs.rmSync(home, { recursive: true, force: true });
  });
  return { root, home, mine: fs.realpathSync(mine), theirs: fs.realpathSync(theirs) };
}

const edit = (b: Bed, dir: string, rel: string, at: number): void => recordEdit(SESSION, b.root, at, b.home, [path.join(dir, rel)]);

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

const changedQueued = (b: Bed): number =>
  fs.existsSync(queueDir(b.root))
    ? fs.readdirSync(queueDir(b.root)).filter((n) => (JSON.parse(fs.readFileSync(path.join(queueDir(b.root), n), "utf8")) as QueueEntry).kind === "changed").length
    : 0;

/** The page's re-check as the index judged it: the counterpart named under the MAIN checkout, its service stale. */
function pageRecheck(b: Bed): CheckResult {
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
      scope: { reread: ["web"], not_reread: [{ service: "api", operations: 1, reason: "indexed" }], stale: ["api"] },
    },
  } as CheckResult;
}

const states = (text: string): boolean => /POST \/items\/upload/.test(text) && /type_mismatch|is missing/.test(text);

function setup(b: Bed, routeIn: string | null): void {
  if (routeIn !== null) edit(b, routeIn, ROUTE, Date.now() - 3000);
  edit(b, b.mine, PAGE, Date.now() - 2000);
  const entry = enqueue(b.root, SESSION, [path.join(b.mine, PAGE)], Date.now(), { home: b.home })[0]!;
  answer(b, entry, pageRecheck(b));
}

test("worktree: both sides edited in the session's worktree: the page's re-check break is withheld at a Read or prompt carrier, and the whole-change check is queued", (t) => {
  const b = bed(t);
  setup(b, b.mine);
  const said = deliverLate(SESSION, Date.now(), new Set(), b.home).join("\n");
  assert.equal(states(said), false, said);
  assert.equal(changedQueued(b), 1, "the whole-change check answers for it");
});

test("worktree: both sides edited in the worktree: the same re-check is withheld at Stop too", (t) => {
  const b = bed(t);
  setup(b, b.mine);
  const stop = stopAnswers(SESSION, { home: b.home, ensure: () => false }).lines.join("\n");
  assert.equal(states(stop), false, stop);
});

test("worktree: only the page edited in the worktree: its break is still stated", (t) => {
  const b = bed(t);
  setup(b, null);
  const said = deliverLate(SESSION, Date.now(), new Set(), b.home).join("\n");
  assert.ok(states(said), said);
  assert.equal(changedQueued(b), 0);
});

test("worktree: the route edited in another worktree does not withhold", (t) => {
  const b = bed(t);
  setup(b, b.theirs);
  const said = deliverLate(SESSION, Date.now(), new Set(), b.home).join("\n");
  assert.ok(states(said), said);
});
