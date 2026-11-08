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
// The session's check (carrick#3466), once per change and per workspace:
// when every file the session edited has a finished re-check newer than its
// last edit, with no service judged at its indexed version, one "Carrick
// checked the N files" line replaces the per-answer lines, at no extra wait.
// Otherwise one line names each file not answered and why, and one `carrick
// check --changed` is queued behind the re-checks. Stop never waits for it;
// its answer comes with the next carrier (`late.ts`). Either way no line asks
// the agent to run the check: the clause is printed only for a workspace this
// session recorded no edited file in.
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
import { markStated, readSession } from "./reuse.ts";

const log = createLogger("carrick-stop");

/** How long Stop waits when its entry carries no `--wait-s` (an older install). */
export const DEFAULT_WAIT_S = 3;

const POLL_MS = 250;

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
  if (isChanged(answer)) return true;
  const result = answer.result;
  if (answer.state !== "answered" || result === undefined) return true;
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
  if (isChanged(answer)) return lateContext(answer, false) ?? "";
  const where = answer.relative;
  const result: CheckResult | undefined = answer.result;
  if (answer.state !== "answered" || result === undefined || result.error) {
    const reason = answer.failure ?? "the re-check gave no answer";
    return `Carrick: ${where} was not re-checked: ${reason.replace(/\.+$/, "")}.`;
  }
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
  gaps: Array<{ file: string; reason: string }>;
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
  const edits = Object.entries(record.files).filter(([, edit]) => edit.root === root);
  const newest = newestPerFile(session, home);
  const coverage: Coverage = { files: edits.length, upTo: record.edited[root] ?? 0, covered: [], gaps: [] };
  // A whole-change check that answered after a file's last edit settled it:
  // the gap was reported and the background check read the file, so it is not
  // named again until the file changes (carrick#3543).
  const changedAt = changedAnsweredAt(session, root, home);
  for (const [file, edit] of edits) {
    const relative = file.startsWith(`${root}/`) ? file.slice(root.length + 1) : file;
    const retired = edit.at <= changedAt;
    const gap = (reason: string) => {
      if (retired) coverage.files -= 1;
      else coverage.gaps.push({ file: relative, reason });
    };
    const held = newest.get(file);
    if (held === undefined) {
      gap("no re-check answered it");
      continue;
    }
    const { entry, answer } = held;
    if (Date.parse(entry.enqueued_at) < edit.at) {
      gap("it was edited after its last re-check");
      continue;
    }
    if (answer === null) {
      const seconds = Math.max(0, Math.round((now - Date.parse(entry.enqueued_at)) / 1000));
      gap(`its re-check is still running after ${seconds} s`);
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
    const deadline = clock() + options.waitS * 1000;
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
      stated.set(root, sessionCheckedLines(coverage.covered.map((answer) => answer.result!), fresh));
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
    stated.set(root, notCoveredLine(coverage.gaps));
    gapped.add(root);
    // An answer whose only news is the gap the line just named says nothing more.
    for (const answer of delivered) {
      if (answer.root === root && !isChanged(answer) && !hasFindings(answer)) folded.add(answer.seq);
    }
    markStated(session, root, coverage.upTo, false, home);
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
