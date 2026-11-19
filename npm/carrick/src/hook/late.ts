// Late delivery: what a carrier prints for background re-checks that finished.
//
// The post-edit hook queues an edit it could not answer in hand (rechecks.ts);
// the runner writes the answer to the session's files. Every carrier (the next
// post-edit, a Read, Codex's next prompt) calls `deliverLate` before its own
// early returns, so a finished answer reaches the model at the next moment
// any hook speaks. Each answer is claimed with `claimDelivery`, so parallel
// hooks print it once.

import { resolveChannel } from "../channel.ts";
import path from "node:path";
import { problemItems } from "../contract.ts";
import type { CheckResult } from "../contract.ts";
import { realPath } from "../fs-util.ts";
import { createLogger } from "../log.ts";
import { answersTheEdit, changedCleanLine, changedLines, correctionLine, movedLines, renderPostToolUse } from "../render.ts";
import { claimDelivery, claimEarly, earlyOwedFor, earlySaid, isChanged, newestEarlies, owedFor, readAnswer, readEarly, sessionListing } from "./rechecks.ts";
import type { Answer, EarlyAnswer } from "./rechecks.ts";
import { confirmedResult, earlyRows, keepRows, standingEarly, standsAsRun, toldBreak, withoutTold } from "./confirm.ts";
import { markLed, markSaid, markTold, readSession, toldKey, wasLed, wasSaid, wasTold } from "./reuse.ts";
import { similarLines } from "./similar.ts";
import { needsBackground } from "./probe.ts";
import { firstLineGeneration, stampOf } from "./generation.ts";
import type { LocalStamp } from "../hosted-snapshot.ts";

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
  if (!answer.result || answer.result.error) return null;
  // An answer queued in place of the edit-time probe (a fact-file workspace)
  // says what the probe's answer would have said in hand, the uses-count line
  // on a result that does not answer the edit included (T12-2). Only where the
  // probe would have printed it: one it would have left for the background
  // re-check (`needsBackground`) is gated as any late answer is (R46.4).
  const inHand = answer.instead_of_probe === true && !needsBackground(answer.result);
  if (!inHand && !answersTheEdit(answer.result)) return null;
  return renderPostToolUse(answer.result, answer.relative, { uses, late: true });
}

/**
 * The full answer as a carrier prints it once its entry's early answer has
 * spoken (part 3, B1, P10): without the breaks the early rows already stated
 * and their mirror call rows, and one correction line for each early break the
 * full re-check found compatible. An entry with no early answer, or whose early
 * answer was never printed, is printed as it stands: an early break part 2 did
 * not judge is then printed by its own carrier (`deliverEarly`).
 */
export function confirmAnswer(answer: Answer, home?: string): { answer: Answer; corrections: string[] } {
  if (isChanged(answer) || !answer.result || answer.result.error) return { answer, corrections: [] };
  const early = readEarly(answer.session, answer.seq, home);
  const said = early === null ? null : earlySaid(answer.session, answer.seq, home);
  if (early === null || said === null || said.size === 0) return { answer, corrections: [] };
  const { result, corrected } = confirmedResult(answer.result, early.result, said);
  if (result !== answer.result) log("late rows dropped: the early answer stated them", answer.relative);
  return { answer: { ...answer, result }, corrections: corrected.map((label) => correctionLine(answer.relative, label)) };
}

/**
 * What a carrier prints for one finished answer it has claimed, oldest first:
 * its similar-function blocks, then its words (`lateContext`), then any
 * correction of its early answer, with the uses-count and lead keys marked as
 * told. Shared by `deliverLate` and by the Stop hook for an answer queued in
 * place of the probe (R46.2), so both print it the same way.
 */
export function deliverOne(session: string, raw: Answer, home?: string): string[] {
  const confirmed = confirmAnswer(raw, home);
  const corrections = confirmed.corrections;
  // A break the session was told and that still stands is not said again at a later entry (T1 P1.3).
  const fresh = !isChanged(confirmed.answer) && confirmed.answer.result && !confirmed.answer.result.error
    ? withoutTold(confirmed.answer.result, confirmed.answer.relative, [...newestEarlies(session, home).values()].map((e) => e.result), (key) => wasTold(session, key, home))
    : null;
  const answer = fresh ? { ...confirmed.answer, result: fresh.result } : confirmed.answer;
  const contexts: string[] = [];
  const key = answer.result ? toldKey(answer.result, answer.relative) : null;
  // As the edit-time answer would have: no uses-count line where the fact
  // lines answered the file's callers.
  const uses = !answer.facts_callers && (key === null || !wasTold(session, key, home));
  const context = isChanged(answer) ? changedLine(session, answer, home) : lateContext(answer, uses);
  // A lead the probe already delivered is in `led`; this one is for a file
  // whose probe had no answer (carrick#3249).
  const found = answer.result
    ? similarLines(answer.result, answer.relative, answer.cwd ?? null, (similarKey) => !wasLed(session, similarKey, home), log)
    : null;
  if (found && found.lines.length > 0) {
    contexts.push(...found.lines);
    markLed(session, found.keys, home);
  }
  if (context) {
    contexts.push(context);
    if (key !== null && uses && answer.result?.uses_lines?.length) markTold(session, [key], home);
    if (fresh) markTold(session, fresh.keys, home);
  }
  contexts.push(...corrections);
  return contexts;
}

/**
 * The early rows with the named files `session` edited before `now` marked on
 * them (T2 P2.3): the check held the other side fixed at main's copy, so an
 * edit of a file that holds it never reached the line. Decided here, at print
 * time, never when the answer is filed: an edit lands between the two.
 */
function withEditedNamed(early: CheckResult, session: string, root: string, now: number, home?: string): CheckResult {
  const edited = new Set(Object.entries(readSession(session, home).files).filter(([, edit]) => edit.at <= now).map(([file]) => realPath(file)));
  if (edited.size === 0) return early;
  const shown = (file: string): string => {
    const rel = path.relative(realPath(root), realPath(file));
    return rel.startsWith("..") || path.isAbsolute(rel) ? file : rel;
  };
  const mark = <T extends { named_files?: string[] }>(row: T): T => {
    const hit = [...new Set((row.named_files ?? []).filter((file) => edited.has(realPath(file))).map(shown))].sort();
    return hit.length === 0 ? row : { ...row, edited_named: hit };
  };
  return {
    ...early,
    items: (early.items ?? []).map(mark),
    ...(early.recheck ? { recheck: { ...early.recheck, moved: (early.recheck.moved ?? []).map(mark) } } : {}),
  };
}

/**
 * What a carrier prints for one early answer (part 3, B1) it owes: the breaks
 * not yet printed in this message (`seen`, the rows' keys), through the one
 * renderer, claimed once under `<seq>.early.said` with the keys of every row the
 * message states. When the full answer has already landed, only the rows it did
 * not judge stand and print (R5a); the rest are its to say.
 *
 * A break is said once (T1, carrick#4728): a row the session's newest early
 * answers no longer list as the same run was fixed since and is dropped; a row
 * `reader` was already told of is kept in the claim and not printed. Each
 * printed row's told key is marked for `reader`, the session the words reach.
 */
export function deliverEarly(owner: string, early: EarlyAnswer, seen: Set<string>, options: { home?: string; now?: number; late?: boolean; reader?: string } = {}): string[] {
  const reader = options.reader ?? owner;
  const full = readAnswer(owner, early.seq, options.home);
  const judged = full?.state === "answered" && full.result && !full.result.error ? standingEarly(early.result, full.result) : early.result;
  const newest = early.service === undefined ? undefined : newestEarlies(owner, options.home).get(early.service);
  const view = keepRows(judged, (row) => standsAsRun(row, newest));
  const rows = earlyRows(view);
  if (!claimEarly(owner, early.seq, rows.map((row) => row.key), options.home, options.now)) return [];
  const told = (row: (typeof rows)[number]): boolean => wasTold(reader, toldBreak(row.listed, row.run), options.home);
  const fresh = keepRows(view, (row) => !seen.has(row.key) && !told(row));
  for (const row of rows) seen.add(row.key);
  const text = renderPostToolUse(withEditedNamed(fresh, reader, early.root, options.now ?? Date.now(), options.home), early.relative, { early: true, uses: false, late: options.late !== false });
  if (text) {
    markTold(reader, earlyRows(fresh).map((row) => toldBreak(row.listed, row.run)), options.home);
    log(`early answer ${early.relative} delivered ${Math.round(((options.now ?? Date.now()) - Date.parse(early.enqueued_at)) / 1000)} s after the edit`);
  }
  return text ? [text] : [];
}

/** Every early answer `session` is owed, printed oldest first (`deliverEarly`). */
export function deliverEarlies(session: string, seen: Set<string>, home?: string, now: number = Date.now(), listing?: readonly string[]): string[] {
  return earlyOwedFor(session, home, listing).flatMap(({ owner, early }) => deliverEarly(owner, early, seen, { home, now, reader: session }));
}

/**
 * The clean line for a `check --changed` that answered with no problem over
 * files a Stop named as still checking (hook-wording audit decision 1: a
 * finding or a named clean result, once per set). Null for any other answer,
 * and for a set already said this session.
 */
function cleanNamedLine(session: string, answer: Answer, home?: string): string | null {
  if (!isChanged(answer) || answer.changed?.exit !== 0) return null;
  const files = [...new Set(answer.named ?? [])].sort();
  if (files.length === 0) return null;
  const key = `clean:${files.join("\n")}`;
  if (wasSaid(session, answer.root, key, home)) return null;
  markSaid(session, answer.root, key, home);
  return changedCleanLine(files);
}

/** Whether an answer holds a row to act on: a problem, a moved row, a deletion. */
export function hasFindings(answer: Answer): boolean {
  const result = answer.result;
  if (answer.state !== "answered" || result === undefined || result.error || !answersTheEdit(result)) return false;
  return Boolean(result.deleted) || problemItems(result).length > 0 || movedLines(result).length > 0;
}

/**
 * What the main session says for an ended sub-agent's answer (carrick#4074):
 * the findings alone. No uses-count line and no leads, which a parent with
 * many sub-agents would get once per file it never edited.
 */
export function adoptedLine(raw: Answer, home?: string): string | null {
  if (isChanged(raw)) return lateContext(raw, false);
  const { answer, corrections } = confirmAnswer(raw, home);
  const said = hasFindings(answer) ? lateContext(answer, false) : null;
  const lines = [...(said ? [said] : []), ...corrections];
  return lines.length > 0 ? lines.join("\n\n") : null;
}

/**
 * The one reader of a finished `check --changed` answer, for every hook that
 * speaks it (the carriers and Stop): a problem, or the clean line for the set a
 * Stop named, once per set. Null when there is nothing to say.
 */
export function changedLine(session: string, answer: Answer, home?: string): string | null {
  return lateContext(answer, false) ?? cleanNamedLine(session, answer, home);
}

/** The index a delivered answer was read from, and where in the returned contexts its words start. */
export type LateStamp = { root: string; stamp: LocalStamp; at: number };

/**
 * `deliverLate`, with the sentence saying which index the first part-2 line of
 * the session read when it differs from the fact file's (once per session,
 * `generation.ts`), placed before that line.
 */
export async function deliverLateFirst(
  session: string | undefined,
  now: number = Date.now(),
  seen: Set<string> = new Set(),
  home?: string,
): Promise<string[]> {
  const stamps: LateStamp[] = [];
  const contexts = deliverLate(session, now, seen, home, stamps);
  const first = stamps[0];
  if (!first) return contexts;
  const line = await firstLineGeneration(first.root, session, stamps.filter((s) => s.root === first.root).map((s) => s.stamp), now, home);
  if (line) contexts.splice(first.at, 0, line);
  return contexts;
}

/**
 * Claim and render every finished answer this session has not been given.
 * Returns the contexts to print, oldest first. Nothing here throws. `seen` are
 * the early rows this message already states (the edit hook's in hand).
 */
export function deliverLate(
  session: string | undefined,
  now: number = Date.now(),
  seen: Set<string> = new Set(),
  home?: string,
  stamps?: LateStamp[],
): string[] {
  if (!session) return [];
  const contexts: string[] = [];
  try {
    // Early answers first, so a full answer delivered beside its entry's
    // early one is compared with it (`confirmAnswer`), never said twice.
    // One listing of the sessions folder serves both readers.
    const listing = sessionListing(home);
    contexts.push(...deliverEarlies(session, seen, home, now, listing));
    for (const { owner, answer } of owedFor(session, home, listing)) {
      if (!claimDelivery(owner, answer.seq, home, now)) continue;
      if (answer.failure) log("background re-check failed", answer.relative, answer.failure);
      if (owner !== session) {
        const adopted = adoptedLine(answer, home);
        log(`late answer ${answer.relative} of an ended sub-agent delivered`);
        if (adopted) contexts.push(adopted);
        continue;
      }
      const seconds = Math.round((now - Date.parse(answer.enqueued_at)) / 1000);
      log(`late answer ${answer.relative} delivered ${seconds} s after the edit`);
      const said = deliverOne(session, answer, home);
      contexts.push(...said);
      // The index the words came from, for the first-line generation sentence.
      const stamp = said.length > 0 && answer.result ? stampOf(answer.result) : null;
      if (stamp) stamps?.push({ root: answer.root, stamp, at: contexts.length - said.length });
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
export async function printLateOnly(session: string | undefined): Promise<void> {
  if (!session) return;
  if (resolveChannel({ hooksInstalled: true }).channel !== "hook") return;
  emitPostToolUse(await deliverLateFirst(session));
}
