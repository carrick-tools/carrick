// What the Stop hook says about the background re-checks of this session's
// edits (carrick#3198, B3): deliver what has finished, wait for what is still
// running (up to `--wait-s`), and say so when it outlives the wait.
//
// Order, as the Stop hook calls it: restart a dead runner, deliver finished
// answers, poll every 250 ms until nothing this session has not yet been told
// about is pending (or the wait is up), deliver again, then state the
// session's check. Only answers with something to act on are printed, with
// any clean answers riding along: a Stop `additionalContext` continues the
// turn, so "no contract problem" would cost a model turn on every reply.
//
// The session's check (carrick#3466, R1 of the hook-wording audit): Stop
// speaks only when the check ran on something this session changed. No route
// or call in the edited files, or only files no service reads: nothing, and
// the change is recorded as stated. Judged operations with no problem: one
// line naming them, once per distinct set of operations. Problems: a header
// and the problem lines. A re-check still to run is waited for through the
// queued `carrick check --changed`, inside the same wait: only its problems
// print, or one line naming the unchecked files if the wait runs out. Any
// other gap (a stale service, a deferred or failed re-check) is named, with
// the check queued behind the re-checks and not waited for.
//
// Reference: the B3 scope in the Arber workflow plan (pins S4, S5, S6c), and
// carrick#3466 for the session's check.

import type { CheckResult } from "../contract.ts";
import { problemItems } from "../contract.ts";
import { createLogger } from "../log.ts";
import {
  answersTheEdit,
  movedLines,
  notCoveredLine,
  notFinishedLine,
  renderPostToolUse,
  sessionCheckedLines,
  staleClause,
} from "../render.ts";
import { checkedSinceLastEdit } from "./checked.ts";
import { lateContext } from "./late.ts";
import {
  changedAnsweredAt,
  claimDelivery,
  enqueueChanged,
  ensureRunner,
  finishedFor,
  isChanged,
  markToldRunning,
  newestPerFile,
  pendingFor,
  type Answer,
  type Pending,
} from "./rechecks.ts";
import { markSaid, markStated, readSession, wasSaid } from "./reuse.ts";
import { worktreeGone } from "./worktree.ts";

const log = createLogger("carrick-stop");

/** How long Stop waits when its entry carries no `--wait-s` (an older install). */
export const DEFAULT_WAIT_S = 3;

const POLL_MS = 250;

/**
 * What a re-check says for a file no service's scan reads
 * (`not_run("no service reads this file")` in `src/local_mode/query.rs`):
 * there is nothing to check in it, so Stop says nothing and queues nothing.
 */
const UNREAD_FILE = "no service reads this file";
/** The start of `UNREAD_EDIT_REASON` (same file): the edit changed nothing the scan reads. */
const UNREAD_EDIT = "the edit changed nothing the scan reads";

function readsNothing(result: CheckResult): boolean {
  const reason = result.recheck?.reason;
  return !answersTheEdit(result) && reason !== undefined && reason !== null && (reason === UNREAD_FILE || reason.startsWith(UNREAD_EDIT));
}

/** The `--wait-s <n>` (or `--wait-s=<n>`) in an argument list, else the default. */
export function waitSeconds(argv: readonly string[]): number {
  for (let at = 0; at < argv.length; at += 1) {
    const arg = argv[at]!;
    const raw = arg === "--wait-s" ? argv[at + 1] : arg.startsWith("--wait-s=") ? arg.slice(9) : undefined;
    if (raw === undefined) continue;
    const parsed = Number.parseInt(raw, 10);
    if (Number.isFinite(parsed) && parsed >= 0) return parsed;
  }
  return DEFAULT_WAIT_S;
}

/** Whether an answer needs the reader to act, or only reports a clean file. */
function actionable(answer: Answer): boolean {
  // The whole-change check speaks at Stop only with a problem.
  if (isChanged(answer)) return answer.changed?.exit === 1;
  const result = answer.result;
  if (answer.state !== "answered" || result === undefined) return true;
  if (result !== undefined && readsNothing(result)) return false;
  if (result.error || !answersTheEdit(result)) return true;
  if (result.deleted) return true;
  if (problemItems(result).length > 0 || movedLines(result).length > 0) return true;
  return (result.recheck?.scope?.stale ?? []).length > 0;
}

/** Whether an answer holds a row to act on: a problem, a moved row, a deletion. */
function hasFindings(answer: Answer): boolean {
  const result = answer.result;
  if (answer.state !== "answered" || result === undefined || result.error || !answersTheEdit(result)) return false;
  return Boolean(result.deleted) || problemItems(result).length > 0 || movedLines(result).length > 0;
}

/** One answer's lines, for a reader who was not told this edit was being re-checked. */
function render(answer: Answer, settled: boolean): string {
  if (isChanged(answer)) return answer.changed?.exit === 1 ? (lateContext(answer, false) ?? "") : "";
  const where = answer.relative;
  const result: CheckResult | undefined = answer.result;
  if (answer.state !== "answered" || result === undefined || result.error) {
    const reason = answer.failure ?? "the re-check gave no answer";
    return `Carrick: ${where} was not re-checked: ${reason.replace(/\.+$/, "")}.`;
  }
  if (readsNothing(result)) return "";
  if (!answersTheEdit(result)) {
    const reason = result.recheck?.reason;
    return reason
      ? `Carrick: ${where} was not re-checked: ${reason.replace(/\.+$/, "")}.`
      : `Carrick: ${where} was not re-checked here.`;
  }
  return renderPostToolUse(result, where, { uses: false, late: true, settled }) ?? "";
}

export type StopOptions = {
  waitS: number;
  home?: string;
  /** Test seams. */
  now?: () => number;
  sleep?: (ms: number) => Promise<void>;
  ensure?: (root: string) => boolean;
};

export type StopResult = {
  /** Lines to print; empty when there is nothing to say. */
  lines: string[];
};

/** Claim the right to print each finished answer: the ones this call owns. */
function collect(session: string, home: string | undefined, now: number): Answer[] {
  const mine: Answer[] = [];
  for (const answer of finishedFor(session, home)) {
    if (claimDelivery(session, answer.seq, home, now)) mine.push(answer);
  }
  return mine;
}

export type Coverage = {
  /** How many files this session recorded an edit of in the workspace. */
  files: number;
  /** The newest of those edits: what a statement about the change covers. */
  upTo: number;
  /** The answers that cover their file, one per covered file. */
  covered: Answer[];
  /** Each file not covered, workspace-relative, and why. */
  gaps: Array<{ file: string; reason: string; /** A re-check still to run: Stop waits for the whole-change check. */ pending: boolean }>;
};

/**
 * Whether the per-file re-checks answered every file this session edited in
 * the workspace at `root` (carrick#3466). A file is covered when its newest
 * entry was queued (or answered in hand) at or after its last edit, is
 * answered with no error, answers the edit as it stands on disk, and judged
 * no service that reads it at its indexed version. Everything else is a gap,
 * with the reason a reader can act on.
 */
export function sessionCoverage(session: string, root: string, home?: string, now: number = Date.now()): Coverage {
  const record = readSession(session, home);
  // An edit made in a linked worktree that has since been removed is not a
  // change to name (carrick#3595). A missing file in the main checkout is a
  // real deletion and stays: only the recorded worktree top decides.
  const edits = Object.entries(record.files).filter(
    ([, edit]) => edit.root === root && !(edit.checkout !== undefined && worktreeGone(edit.checkout)),
  );
  const newest = newestPerFile(session, home);
  const coverage: Coverage = { files: edits.length, upTo: record.edited[root] ?? 0, covered: [], gaps: [] };
  // A whole-change check that answered after a file's last edit settled it:
  // the gap was reported and the background check read the file, so it is not
  // named again until the file changes (carrick#3543).
  const changedAt = changedAnsweredAt(session, root, home);
  for (const [file, edit] of edits) {
    const relative = file.startsWith(`${root}/`) ? file.slice(root.length + 1) : file;
    const retired = edit.at <= changedAt;
    const gap = (reason: string, pending = false) => {
      if (retired) coverage.files -= 1;
      else coverage.gaps.push({ file: relative, reason, pending });
    };
    const held = newest.get(file);
    if (held === undefined) {
      gap("no re-check answered it", true);
      continue;
    }
    const { entry, answer } = held;
    if (Date.parse(entry.enqueued_at) < edit.at) {
      gap("it was edited after its last re-check", true);
      continue;
    }
    if (answer === null) {
      const seconds = Math.max(0, Math.round((now - Date.parse(entry.enqueued_at)) / 1000));
      gap(`its re-check is still running after ${seconds} s`, true);
      continue;
    }
    const result = answer.result;
    if (answer.state !== "answered" || result === undefined) {
      gap(answer.failure ?? "the re-check gave no answer");
      continue;
    }
    if (result.error) {
      gap(result.message ?? result.error);
      continue;
    }
    if (readsNothing(result)) {
      coverage.covered.push(answer);
      continue;
    }
    if (!answersTheEdit(result)) {
      gap(result.recheck?.reason ?? "no re-check of it ran");
      continue;
    }
    const stale = result.recheck?.scope?.stale ?? [];
    if (stale.length > 0) {
      gap(staleClause(stale, "it"));
      continue;
    }
    coverage.covered.push(answer);
  }
  return coverage;
}

export async function stopAnswers(session: string, options: StopOptions): Promise<StopResult> {
  const clock = options.now ?? Date.now;
  const sleep = options.sleep ?? ((ms: number) => new Promise<void>((resolve) => setTimeout(resolve, ms)));
  const ensure = options.ensure ?? ((root: string) => ensureRunner(root));
  const home = options.home;

  // A queued `check --changed` is never waited for: it runs after every
  // per-file entry, and its answer comes with the next carrier.
  const toWaitFor = (): Pending[] =>
    pendingFor(session, home).filter((entry) => !entry.toldRunning && !isChanged(entry));
  const delivered: Answer[] = collect(session, home, clock());
  const deadline = clock() + options.waitS * 1000;

  // A runner that died holding the queue would hold the reply for the whole
  // wait: start one before waiting. A queued `check --changed` needs one too,
  // though Stop does not wait for it.
  for (const root of new Set(pendingFor(session, home).map((entry) => entry.root))) {
    try {
      ensure(root);
    } catch (error) {
      log("could not restart the re-check runner", String(error));
    }
  }
  delivered.push(...collect(session, home, clock()));
  const waiting = toWaitFor();
  if (waiting.length > 0) {
    while (toWaitFor().length > 0 && clock() < deadline) {
      await sleep(Math.min(POLL_MS, Math.max(0, deadline - clock())));
      delivered.push(...collect(session, home, clock()));
    }
    delivered.push(...collect(session, home, clock()));
  }

  // The session's check, per workspace with an edit Stop has not yet spoken
  // about. A clean `check --changed` the agent ran since that edit already
  // said it: the per-answer lines below then speak as before.
  const record = readSession(session, home);
  const roots = new Set(Object.values(record.files).map((edit) => edit.root));
  const stated = new Map<string, string>();
  const folded = new Set<string>();
  const gapped = new Set<string>();
  const awaiting = new Map<string, { seq: string; gaps: Coverage["gaps"]; upTo: number }>();
  for (const root of roots) {
    const upTo = record.edited[root] ?? 0;
    if ((record.stated[root] ?? -1) >= upTo) continue;
    if (checkedSinceLastEdit(session, root, home)) continue;
    const coverage = sessionCoverage(session, root, home, clock());
    if (coverage.files === 0) continue;
    if (coverage.gaps.length === 0) {
      const fresh = delivered
        .filter((answer) => answer.root === root && !isChanged(answer) && answer.result !== undefined)
        .map((answer) => ({ result: answer.result!, where: answer.relative }));
      const said = sessionCheckedLines(
        coverage.covered.map((answer) => ({ result: answer.result!, where: answer.relative })),
        fresh,
      );
      // A clean or not-checked line names a set of operations: once per set.
      if (said.text !== "" && (said.key === null || !wasSaid(session, root, said.key, home))) {
        stated.set(root, said.text);
        if (said.key !== null) markSaid(session, root, said.key, home);
      }
      for (const answer of delivered) if (answer.root === root && !isChanged(answer)) folded.add(answer.seq);
      markStated(session, root, coverage.upTo, true, home);
      continue;
    }
    const queued = enqueueChanged(root, session, clock(), { home });
    if (queued === null) {
      log("could not queue check --changed for", root);
      continue;
    }
    try {
      ensure(root);
    } catch (error) {
      log("could not start the re-check runner", String(error));
    }
    gapped.add(root);
    // An answer whose only news is the gap says nothing more.
    for (const answer of delivered) {
      if (answer.root === root && !isChanged(answer) && !hasFindings(answer)) folded.add(answer.seq);
    }
    // A re-check still to run is waited for, below: its problems print, and
    // nothing else unless the wait runs out. Any other gap is named now.
    if (coverage.gaps.every((gap) => gap.pending)) {
      awaiting.set(root, { seq: queued.seq, gaps: coverage.gaps, upTo: coverage.upTo });
      continue;
    }
    stated.set(root, notCoveredLine(coverage.gaps));
    markStated(session, root, coverage.upTo, false, home);
  }

  // The whole-change check queued for pending gaps shares the Stop's budget.
  const queuedSeqs = new Set([...awaiting.values()].map((entry) => entry.seq));
  const stillQueued = (): boolean => pendingFor(session, home).some((entry) => queuedSeqs.has(entry.seq));
  while (queuedSeqs.size > 0 && stillQueued() && clock() < deadline) {
    await sleep(Math.min(POLL_MS, Math.max(0, deadline - clock())));
    delivered.push(...collect(session, home, clock()));
  }
  if (queuedSeqs.size > 0) delivered.push(...collect(session, home, clock()));
  for (const [root, entry] of awaiting) {
    const checked = delivered.find((answer) => answer.seq === entry.seq);
    if (checked?.state === "answered" && checked.changed !== undefined) {
      markStated(session, root, entry.upTo, checked.changed.exit === 0, home);
    } else {
      stated.set(root, notFinishedLine(entry.gaps));
      markStated(session, root, entry.upTo, false, home);
    }
  }

  // No clause where Stop stated the change, now or at an earlier Stop: it
  // either checked it or queued the check itself.
  const after = readSession(session, home);
  const settledAt = (root: string): boolean =>
    (after.stated[root] ?? -1) >= (after.edited[root] ?? 0) || checkedSinceLastEdit(session, root, home);

  const lines: string[] = [...stated.values()];
  const rest = delivered.filter((answer) => !folded.has(answer.seq));
  if (rest.some(actionable)) {
    for (const answer of rest) {
      const text = render(answer, settledAt(answer.root));
      if (text) lines.push(text);
    }
  }

  // Still running after the wait: said once per entry. A second Stop never
  // waits twice for it, and the next Stop after the answer lands delivers it.
  // A workspace whose gap line named it does not name it twice.
  const running = toWaitFor().filter((entry) => markToldRunning(session, entry.seq, home));
  const unnamed = running.filter((entry) => !gapped.has(entry.root));
  if (unnamed.length > 0) {
    const at = clock();
    const files = [...new Set(unnamed.map((entry) => entry.relative))];
    const oldest = Math.max(...unnamed.map((entry) => at - Date.parse(entry.enqueued_at)));
    const seconds = Math.max(0, Math.round(oldest / 1000));
    lines.push(
      `Carrick: the re-check of ${files.join(", ")} is still running after ${seconds} s.`,
    );
  }
  return { lines };
}
