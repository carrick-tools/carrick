// Late delivery: what a carrier prints for background re-checks that finished.
//
// The post-edit hook queues an edit it could not answer in hand (rechecks.ts);
// the runner writes the answer to the session's files. Every carrier (the next
// post-edit, a Read, Codex's next prompt) calls `deliverLate` before its own
// early returns, so a finished answer reaches the model at the next moment
// any hook speaks. Each answer is claimed with `claimDelivery`, so parallel
// hooks print it once.

import { createLogger } from "../log.ts";
import { answersTheEdit, changedLines, renderPostToolUse } from "../render.ts";
import { claimDelivery, finishedFor, isChanged } from "./rechecks.ts";
import type { Answer } from "./rechecks.ts";
import { markLed, markTold, toldKey, wasLed, wasTold } from "./reuse.ts";
import { leadLines } from "./lead.ts";

const log = createLogger("carrick-late");

/** The words for one finished answer, or null when there is nothing to say. */
export function lateContext(answer: Answer, uses: boolean): string | null {
  // The whole-change check a Stop queued (carrick#3466) speaks only with a
  // problem: a clean run, a run that could not check and a run that gave no
  // answer print nothing (a failure is logged by `deliverLate`).
  if (isChanged(answer)) return answer.changed ? changedLines(answer.changed) || null : null;
  // A re-check that stopped, gave no answer or answered nothing prints
  // nothing: a failure is logged by `deliverLate`, and the Stop hook names a
  // file left unchecked.
  if (!answer.result || answer.result.error || !answersTheEdit(answer.result)) return null;
  return renderPostToolUse(answer.result, answer.relative, { uses, late: true });
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
      const context = lateContext(answer, uses);
      const seconds = Math.round((now - Date.parse(answer.enqueued_at)) / 1000);
      log(`late answer ${answer.relative} delivered ${seconds} s after the edit`);
      // A lead the probe already delivered is in `led`; this one is for a file
      // whose probe had no answer (carrick#3249).
      const found = answer.result
        ? leadLines(answer.result, answer.relative, (leadKey) => !wasLed(session, leadKey), log)
        : null;
      if (found && found.lines.length > 0) {
        contexts.push(...found.lines);
        markLed(session, found.keys);
      }
      if (!context) continue;
      contexts.push(context);
      if (key !== null && uses && answer.result?.uses_lines?.length) markTold(session, [key]);
    }
  } catch (error) {
    log("late delivery failed", String(error));
  }
  return contexts;
}
