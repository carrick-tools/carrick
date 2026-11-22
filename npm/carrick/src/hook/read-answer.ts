// The Read hook's answer for files: who depends on them, rendered as locations
// only. Shared by `hook post-read` (a Read of the file) and `hook post-bash`
// (a shell command that reads it).
//
// Behind its own opt-in switch, `CARRICK_READ_LINES=1` (`readLinesEnabled`):
// unset, every file takes the binary's `check`, as before. With it on and a
// fact file, the answer is the fact lines of the whole file (op and fn kinds,
// `facts/lines.ts` with `read: true`; never a contract line), read locally; no
// carrick process starts. A file the fact file cannot answer
// (`FactOutcome.answered`) takes the binary's `check`. The lines the fact file
// prints carry no uses-count line, and one that was said here is not said
// again at the edit.

import path from "node:path";
import { createLogger } from "../log.ts";
import { resolveRoot, rootNote } from "../root.ts";
import { chainLinesEnabled, contractLinesEnabled, readLinesEnabled } from "../facts/enabled.ts";
import { markOwed, markRead, markTold, owedRoots, toldKey } from "./reuse.ts";
import { stampOf } from "./generation.ts";
import type { LocalStamp } from "../hosted-snapshot.ts";

const log = createLogger("carrick-hook");
/** The limit on the one check, unless the caller set its own. */
const CHECK_TIMEOUT_MS = "2000";

export type ReadAnswers = {
  /** What to print, in order: the fact lines, then each unanswered file's `check` answer. */
  contexts: string[];
  /** The fact lines' claims: the caller marks them with `markFacts` once they are printed. */
  claims: Record<string, { at: number; named: string[] }>;
  /** The row keys of the fact lines, for a later call in the same action (`options.exclude`). */
  used: string[];
  /** Time the fact lookups took, for the timing record. */
  factsMs: number | undefined;
  /** The index each printed `check` answer came from, for the first-line generation sentence. */
  stamps: LocalStamp[];
  /** Where in `contexts` the `check` answers start. */
  checksAt: number;
};

/**
 * The `check` answer for `file` (absolute) from the binary, or null when
 * there is nothing to say. The file is marked as asked about whatever the
 * answer was, so an error or a timeout is not asked again.
 */
async function checkRead(file: string, root: string, session: string | undefined): Promise<{ context: string; stamp: LocalStamp | null } | null> {
  const { check } = await import("../cli.ts");
  const { renderPostRead } = await import("../render.ts");
  const relative = path.relative(root, file);
  const env = { ...process.env, CARRICK_TIMEOUT_MS: process.env["CARRICK_TIMEOUT_MS"] ?? CHECK_TIMEOUT_MS };
  const outcome = await check(relative, { cwd: root, workspace: root, env });
  if (session) markRead(session, file);
  if (!outcome.result) {
    log("no answer for", relative, outcome.failure ?? "");
    return null;
  }

  const context = renderPostRead(outcome.result, relative);
  log(`check ${relative} -> ${context ? "context" : "nothing to say"} in ${outcome.ms}ms`);
  // The post-edit hook that follows prints this file's header and items but
  // not the uses lines it was just shown.
  if (session && outcome.result.uses_lines?.length) {
    markTold(session, [toldKey(outcome.result, relative)]);
  }
  return context ? { context, stamp: stampOf(outcome.result) } : null;
}

/**
 * The answers for `files` (absolute): one cap across all of them (at most
 * `options.limit` fact lines, default `MAX_FACT_LINES`, skipping the keys in
 * `options.exclude`; `options.facts: false` skips the fact file, for a command
 * that also writes, P9). Each file the fact file answered is marked read and
 * starts no process; every other file takes the binary's `check`. A file read
 * while the workspace has no fact file is owed its fact lines, and the first
 * call after the file lands says them (`owed.ts`); `files` may be empty for
 * that.
 */
export async function answerReads(
  files: readonly string[],
  cwd: string | undefined,
  session: string | undefined,
  options: { limit?: number; exclude?: ReadonlySet<string>; facts?: boolean } = {},
): Promise<ReadAnswers> {
  const roots = new Map<string, string[]>();
  for (const file of files) {
    const choice = resolveRoot({
      clientRoot: cwd ?? null,
      projectDir: process.env["CLAUDE_PROJECT_DIR"] ?? null,
      filePath: file,
    });
    const note = rootNote(choice);
    if (note) log(note);
    roots.set(choice.root, [...(roots.get(choice.root) ?? []), file]);
  }

  const contexts: string[] = [];
  const claims: ReadAnswers["claims"] = {};
  const used: string[] = [];
  let factsMs: number | undefined;
  const unanswered: Array<{ file: string; root: string }> = [];
  // With the Read switch off, the fact file is still opened for the chain line
  // alone ("other services call the route this function runs in"): no other
  // fact line prints, and every file still takes the binary's `check`.
  const wholeFile = readLinesEnabled();
  const { MAX_FACT_LINES, factLines } = (wholeFile || chainLinesEnabled()) && options.facts !== false ? await import("../facts/lines.ts") : { MAX_FACT_LINES: 0, factLines: null };
  for (const [root, group] of roots) {
    const outcome =
      factLines === null
        ? null
        : await factLines(
            root,
            group.map((file) => ({ file, read: true })),
            session,
            { limit: (options.limit ?? MAX_FACT_LINES) - used.length, exclude: new Set([...(options.exclude ?? []), ...used]), contract: contractLinesEnabled(), chain: chainLinesEnabled(), chainOnly: !wholeFile },
          );
    if (outcome !== null) {
      factsMs = (factsMs ?? 0) + outcome.ms;
      // A missing or stale fact file is refreshed in the background, detached, as at an edit.
      if (outcome.stale) (await import("../facts/fetch.ts")).startFactsRefresh(root);
      contexts.push(...outcome.lines);
      used.push(...outcome.used);
      Object.assign(claims, outcome.claims);
    }
    // No fact file yet (a new workspace before its first refresh lands): these
    // files take the binary's answer below and are owed their fact lines.
    if (session && outcome !== null && !outcome.present && outcome.stale) markOwed(session, group, root);
    for (const file of group) {
      if (outcome?.answered.includes(file)) {
        if (session) markRead(session, file);
      } else {
        unanswered.push({ file, root });
      }
    }
  }
  // Files read before the fact file landed get their lines now that it has,
  // once, after this action's own and under the same cap (`owed.ts`).
  if (session && factLines !== null) {
    const { owedLines } = await import("./owed.ts");
    for (const root of owedRoots(session)) {
      const owed = await owedLines(root, session, { limit: (options.limit ?? MAX_FACT_LINES) - used.length, exclude: new Set([...(options.exclude ?? []), ...used]), skip: files });
      if (owed === null) continue;
      factsMs = (factsMs ?? 0) + owed.ms;
      contexts.push(...owed.lines);
      used.push(...owed.used);
      Object.assign(claims, owed.claims);
    }
  }
  const checked = await Promise.all(unanswered.map((entry) => checkRead(entry.file, entry.root, session)));
  const checksAt = contexts.length;
  const printed = checked.filter((entry): entry is NonNullable<typeof entry> => entry !== null);
  contexts.push(...printed.map((entry) => entry.context));
  const stamps = printed.flatMap((entry) => (entry.stamp ? [entry.stamp] : []));
  return { contexts, claims, used, factsMs, stamps, checksAt };
}
