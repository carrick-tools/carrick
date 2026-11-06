// Late delivery: what a carrier prints for background re-checks that finished.
//
// The post-edit hook queues an edit it could not answer in hand (rechecks.ts);
// the runner writes the answer to the session's files. Every carrier (the next
// post-edit, a Read, Codex's next prompt) calls `deliverLate` before its own
// early returns, so a finished answer reaches the model at the next moment
// any hook speaks. Each answer is claimed with `claimDelivery`, so parallel
// hooks print it once.

import { createLogger } from "../log.ts";
import { answersTheEdit, changedFailedLine, changedLines, notRecheckedLine, renderPostToolUse } from "../render.ts";
import { claimDelivery, finishedFor, isChanged } from "./rechecks.ts";
import type { Answer } from "./rechecks.ts";
import { checkedSinceLastEdit } from "./checked.ts";
import { markTold, toldKey, wasTold } from "./reuse.ts";

const log = createLogger("carrick-late");

/** The words for one finished answer, or null when there is nothing to say. */
export function lateContext(answer: Answer, uses: boolean, settled = false): string | null {
  // The whole-change check a Stop queued (carrick#3466): its verdict, or that
  // it gave none. Never a clause: it is the check the clause asks for.
  if (isChanged(answer)) {
    if (answer.changed) return changedLines(answer.changed);
    return changedFailedLine(
      answer.state === "stopped" ? (answer.failure ?? "it stopped") : "the binary failed or printed no answer",
    );
  }
  if (!answer.result) {
    // An internal failure is logged, never shown: the reader needs the next step.
    const reason =
      answer.state === "stopped" ? (answer.failure ?? "the background re-check stopped") : "the background re-check gave no answer";
    return notRecheckedLine(answer.relative, reason, settled);
  }
  if (answer.result.error) return null;
  if (!answersTheEdit(answer.result)) {
    return notRecheckedLine(answer.relative, answer.result.recheck?.reason, settled);
  }
  return renderPostToolUse(answer.result, answer.relative, { uses, late: true, settled });
}

/**
 * Claim and render every finished answer this session has not been given.
 * Returns the contexts to print, oldest first. Nothing here throws.
 */
export function deliverLate(session: string | undefined, now: number = Date.now()): string[] {
  if (!session) return [];
  const contexts: string[] = [];
  try {
    for (const answer of finishedFor(session)) {
      if (!claimDelivery(session, answer.seq, undefined, now)) continue;
      if (answer.failure) log("background re-check failed", answer.relative, answer.failure);
      const key = answer.result ? toldKey(answer.result, answer.relative) : null;
      const uses = key === null || !wasTold(session, key);
      const context = lateContext(answer, uses, checkedSinceLastEdit(session, answer.root));
      const seconds = Math.round((now - Date.parse(answer.enqueued_at)) / 1000);
      log(`late answer ${answer.relative} delivered ${seconds} s after the edit`);
      if (!context) continue;
      contexts.push(context);
      if (key !== null && uses && answer.result?.uses_lines?.length) markTold(session, [key]);
    }
  } catch (error) {
    log("late delivery failed", String(error));
  }
  return contexts;
}
