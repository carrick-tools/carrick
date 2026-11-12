// Late delivery: what a carrier prints for background re-checks that finished.
//
// The post-edit hook queues an edit it could not answer in hand (rechecks.ts);
// the runner writes the answer to the session's files. Every carrier (the next
// post-edit, a Read, Codex's next prompt) calls `deliverLate` before its own
// early returns, so a finished answer reaches the model at the next moment
// any hook speaks. Each answer is claimed with `claimDelivery`, so parallel
// hooks print it once.

import { resolveChannel } from "../channel.ts";
import { createLogger } from "../log.ts";
import { answersTheEdit, changedCleanLine, changedLines, renderPostToolUse } from "../render.ts";
import { claimDelivery, finishedFor, isChanged } from "./rechecks.ts";
import type { Answer } from "./rechecks.ts";
import { markLed, markSaid, markTold, toldKey, wasLed, wasSaid, wasTold } from "./reuse.ts";
import { leadLines } from "./lead.ts";

const log = createLogger("carrick-late");

/** The words for one finished answer, or null when there is nothing to say. */
export function lateContext(answer: Answer, uses: boolean): string | null {
  // The whole-change check a Stop queued (carrick#3466) speaks with a problem.
  // A clean run speaks only for the files Stop named, once per set
  // (`deliverLate`); a run that could not check and a run that gave no answer
  // print nothing (a failure is logged by `deliverLate`).
  if (isChanged(answer)) return answer.changed ? changedLines(answer.changed) || null : null;
  // A re-check that stopped, gave no answer or answered nothing prints
  // nothing: a failure is logged by `deliverLate`, and the Stop hook names a
  // file left unchecked.
  if (!answer.result || answer.result.error || !answersTheEdit(answer.result)) return null;
  return renderPostToolUse(answer.result, answer.relative, { uses, late: true });
}

/**
 * The clean line for a `check --changed` that answered with no problem over
 * files a Stop named as still checking (hook-wording audit decision 1: a
 * finding or a named clean result, once per set). Null for any other answer,
 * and for a set already said this session.
 */
function cleanNamedLine(session: string, answer: Answer): string | null {
  if (!isChanged(answer) || answer.changed?.exit !== 0) return null;
  const files = [...new Set(answer.named ?? [])].sort();
  if (files.length === 0) return null;
  const key = `clean:${files.join("\n")}`;
  if (wasSaid(session, answer.root, key)) return null;
  markSaid(session, answer.root, key);
  return changedCleanLine(files);
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
      const context = lateContext(answer, uses) ?? cleanNamedLine(session, answer);
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

/** The one JSON object a PostToolUse hook prints (the host attaches it to the tool result), or nothing when there are no lines. */
export function emitPostToolUse(contexts: readonly string[]): void {
  if (contexts.length === 0) return;
  process.stdout.write(
    JSON.stringify({
      hookSpecificOutput: { hookEventName: "PostToolUse", additionalContext: contexts.join("\n\n") },
    }),
  );
}

/**
 * Print the late answers alone, for a hook with nothing else to say (a shell
 * command that names no source file, a search tool): on the hook channel only,
 * and nothing when none has landed.
 */
export function printLateOnly(session: string | undefined): void {
  if (!session) return;
  if (resolveChannel({ hooksInstalled: true }).channel !== "hook") return;
  emitPostToolUse(deliverLate(session));
}
