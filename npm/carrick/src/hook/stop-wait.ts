// What the Stop hook says about the background re-checks of this session's
// edits (carrick#3198, B3): deliver what has finished, wait for what is still
// running (up to `--wait-s`), and say so when it outlives the wait.
//
// Order, as the Stop hook calls it: restart a dead runner, deliver finished
// answers, poll every 250 ms until nothing this session has not yet been told
// about is pending (or the wait is up), deliver again, then name what is still
// running. Only answers with something to act on are printed, with any clean
// answers riding along: a Stop `additionalContext` continues the turn, so
// "no contract problem" would cost a model turn on every reply.
//
// Reference: the B3 scope in the Arber workflow plan (pins S4, S5, S6c).

import type { CheckResult } from "../contract.ts";
import { problemItems } from "../contract.ts";
import { createLogger } from "../log.ts";
import { answersTheEdit, movedLines, renderPostToolUse, runClause } from "../render.ts";
import { checkedSinceLastEdit } from "./checked.ts";
import {
  claimDelivery,
  ensureRunner,
  finishedFor,
  markToldRunning,
  pendingFor,
  type Answer,
  type Pending,
} from "./rechecks.ts";

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
  const result = answer.result;
  if (answer.state !== "answered" || result === undefined) return true;
  if (result.error || !answersTheEdit(result)) return true;
  if (result.deleted) return true;
  if (problemItems(result).length > 0 || movedLines(result).length > 0) return true;
  return (result.recheck?.scope?.stale ?? []).length > 0;
}

/** One answer's lines, for a reader who was not told this edit was being re-checked. */
function render(answer: Answer, settled: boolean): string {
  const where = answer.relative;
  const result: CheckResult | undefined = answer.result;
  if (answer.state !== "answered" || result === undefined || result.error) {
    const reason = answer.failure ?? "the re-check gave no answer";
    return `Carrick: ${where} was not re-checked: ${reason.replace(/\.+$/, "")}.${runClause(settled)}`;
  }
  if (!answersTheEdit(result)) {
    const reason = result.recheck?.reason;
    return reason
      ? `Carrick: ${where} was not re-checked: ${reason.replace(/\.+$/, "")}.${runClause(settled)}`
      : `Carrick: ${where} was not re-checked here.${runClause(settled)}`;
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

export async function stopAnswers(session: string, options: StopOptions): Promise<StopResult> {
  const clock = options.now ?? Date.now;
  const sleep = options.sleep ?? ((ms: number) => new Promise<void>((resolve) => setTimeout(resolve, ms)));
  const ensure = options.ensure ?? ((root: string) => ensureRunner(root));
  const home = options.home;

  const toWaitFor = (): Pending[] => pendingFor(session, home).filter((entry) => !entry.toldRunning);
  const delivered: Answer[] = collect(session, home, clock());

  // A runner that died holding the queue would hold the reply for the whole
  // wait: start one before waiting.
  const waiting = toWaitFor();
  if (waiting.length > 0) {
    for (const root of new Set(waiting.map((entry) => entry.root))) {
      try {
        ensure(root);
      } catch (error) {
        log("could not restart the re-check runner", String(error));
      }
    }
    const deadline = clock() + options.waitS * 1000;
    while (toWaitFor().length > 0 && clock() < deadline) {
      await sleep(Math.min(POLL_MS, Math.max(0, deadline - clock())));
      delivered.push(...collect(session, home, clock()));
    }
    delivered.push(...collect(session, home, clock()));
  }

  const lines: string[] = [];
  if (delivered.some(actionable)) {
    for (const answer of delivered) {
      const text = render(answer, checkedSinceLastEdit(session, answer.root, home));
      if (text) lines.push(text);
    }
  }

  // Still running after the wait: said once per entry. A second Stop never
  // waits twice for it, and the next Stop after the answer lands delivers it.
  const running = toWaitFor().filter((entry) => markToldRunning(session, entry.seq, home));
  if (running.length > 0) {
    const at = clock();
    const files = [...new Set(running.map((entry) => entry.relative))];
    const oldest = Math.max(...running.map((entry) => at - Date.parse(entry.enqueued_at)));
    const seconds = Math.max(0, Math.round(oldest / 1000));
    const settled = running.every((entry) => checkedSinceLastEdit(session, entry.root, home));
    lines.push(
      `Carrick: the re-check of ${files.join(", ")} is still running after ${seconds} s.${runClause(settled)}`,
    );
  }
  return { lines };
}
