// The post-edit hook's work once the edited files are known: ask the binary
// once, queue what needs a re-scan, print what the edit needs acted on.
//
// Shared by `hook post-edit` (a file edit, either host) and `hook post-bash`
// (a shell command that wrote a source file), so a write through the shell
// gets the same answer an Edit does: a finding, or nothing.

import path from "node:path";
import type { Channel } from "../channel.ts";
import { createLogger } from "../log.ts";
import { renderPostToolUse } from "../render.ts";
import { resolveRoot, rootNote } from "../root.ts";
import { throttledVersionMismatch } from "../init/outdated.ts";
import { currentVersion } from "../update.ts";
import { markFacts, markLed, markTold, recordEdit, toldKey, wasLed, wasTold } from "./reuse.ts";
import { similarLines } from "./similar.ts";
import type { SimilarLines } from "./similar.ts";
import { deliverEarly, deliverLateFirst, emitPostToolUse } from "./late.ts";
import { needsBackground, probeEdits, probeTimeoutMs } from "./probe.ts";
import { inOtherWorktree } from "./worktree.ts";
import { answerInHand, enqueue, ensureRunner, readEarly } from "./rechecks.ts";
import type { QueueEntry } from "./rechecks.ts";
import { engineReadyOf } from "./timings.ts";
import type { Timing } from "./timings.ts";
import type { CheckResult } from "../contract.ts";
import { factLines } from "../facts/lines.ts";
import { openFacts } from "../facts/store.ts";
import type { FactEdit } from "../facts/lines.ts";
import { isManifest } from "../facts/rules/packages.ts";
import { chainLinesEnabled, compilerLinesEnabled, contractLinesEnabled, factsEnabled } from "../facts/enabled.ts";
import { askHeld } from "../held/client.ts";
import type { AskAnswer } from "../held/client.ts";
import { firstLineGeneration, stampOf } from "./generation.ts";
import type { LocalStamp } from "../hosted-snapshot.ts";

const log = createLogger("carrick-hook");
/** D13: at most this many compiler lines in one action. */

/**
 * Check `edited` (absolute paths, at least one) as the post-edit hook does and
 * print the one object for them. `extra` lines (a read answer from the same
 * command) are printed first, in the same object.
 */
export async function editFlow(
  input: {
    cwd: string | null;
    session: string | undefined;
    /**
     * The payload carries an `agent_id`: a sub-agent's edit. Nothing reads a
     * sub-agent's late answers once it ends, so its edit keeps the probe in
     * hand, fact file or not (R46.3).
     */
    subAgent?: boolean;
    channel: Channel;
    timing?: Timing;
    /**
     * Absolute path -> where the edit changed the file: the lines of its
     * patch, else the text it put in (an Edit's `new_string`), else `whole`
     * for a file the call created. A file not named here (a Write over a
     * file, a shell write, a patch hunk that cannot be placed) gets no fact
     * lines: which lines changed is not known.
     */
    changes?: ReadonlyMap<string, Omit<FactEdit, "file">>;
    /**
     * The files came from a shell command's write shapes, not from an edit
     * tool, so who wrote them is not proven: a file in another linked
     * worktree (another session's) gets no fact lines.
     */
    shell?: boolean;
    /**
     * Files the fact path reads that the probe does not (JavaScript,
     * `FACT_FILE`; a `package.json`): they get fact lines and nothing else,
     * are never probed, queued or recorded as an edit, so `edited` may be empty.
     */
    factOnly?: string[];
    /** The three calls whose timing the edit-flow tests fake; the hook passes none. */
    run?: { factLines: typeof factLines; probeEdits: typeof probeEdits; openFacts: typeof openFacts };
  },
  edited: string[],
  extra: string[] = [],
): Promise<void> {
  const { cwd, session } = input;
  const channel = { channel: input.channel };

  // One root for the call: every file in one patch is in the workspace the tool
  // ran in, and resolving it per file would ask the same question repeatedly.
  const choice = resolveRoot({
    clientRoot: cwd,
    projectDir: process.env["CLAUDE_PROJECT_DIR"] ?? null,
    filePath: (edited[0] ?? input.factOnly?.[0])!,
  });
  const note = rootNote(choice);
  if (note) log(note);
  // Stamped before any line is built: a `carrick check --changed` that began
  // earlier did not read this edit (`checked.ts`). The files themselves are
  // recorded once the probe has answered (below).
  const editedAt = Date.now();
  if (session && edited.length > 0) recordEdit(session, choice.root, editedAt);

  const contexts: string[] = [];
  // Files whose uses lines this message carries, remembered only once it is
  // delivered: who uses a file is shown once per file per session (carrick#2067).
  const told: string[] = [];
  // Similar-function blocks (carrick-cloud#3490) are remembered the same way, once delivered.
  const led: string[] = [];
  const relatives = edited.map((file) => path.relative(choice.root, file));
  // Where each operation the edited code serves or calls sits in other
  // files, and who calls the edited functions, from the fact file
  // (`facts/lines.ts`), read locally. Read before any probe answer is
  // rendered: a file the fact file covers gets its callers from there, not
  // the uses-count line.
  const factFiles = [...edited, ...(input.factOnly ?? [])].filter((file) => !(input.shell && inOtherWorktree(file, cwd ?? process.cwd())));
  // The held compiler (part 3, B1): one ask per queued entry of a file in this
  // session's own checkout, made once part 2's entry exists and named by its
  // seq, so the daemon files its early answer beside that entry. The hook waits
  // for the asks until the probe's budget since the edit has run out
  // (`probeTimeoutMs`); a slower answer is printed by the next carrier
  // (`deliverLate`). Off, nothing is asked and nothing below changes.
  const asks: Array<Promise<{ entry: QueueEntry; answer: AskAnswer }>> = [];
  const askFor = (entries: readonly QueueEntry[]): void => {
    if (!compilerLinesEnabled()) return;
    for (const entry of entries) {
      if (inOtherWorktree(entry.file, cwd ?? process.cwd())) continue;
      const waitMs = Math.max(0, editedAt + probeTimeoutMs() - Date.now());
      asks.push(
        askHeld(choice.root, [entry.file], { session: entry.session, kind: "edit", editedAt, waitMs, seq: entry.seq }).then((answer) => ({ entry, answer })),
      );
    }
  };
  const run = input.run ?? { factLines, probeEdits, openFacts };
  const hook = channel.channel === "hook";
  const queueable = hook && Boolean(session);
  // The store is opened once, here: "is there a fact file" is known from the
  // open, so which files the probe covers is settled before the fact lines
  // finish, and the probe can run beside them (A7). `factLines` takes the
  // opened store and closes it.
  const store = hook && factsEnabled() ? await run.openFacts(choice.root) : null;
  // The edit just landed, so the index describes the file as it was before
  // it. Without a fact file the hook asks the binary once, with a probe that
  // never starts a re-scan: an answer that needs none (the file reads the
  // same, a known miss) comes back at once and prints
  // below. Every other file is queued for the background runner
  // (carrick#3198). With a fact file the fact lines print now and every file
  // is queued in place of the probe: its answer, problem and uses lines and
  // leads included, arrives on the next carrier (T12-2). A sub-agent's edit
  // keeps the probe (R46.3), and a file the queue did not take is probed in
  // hand as before (R46.1).
  const insteadOfProbe = queueable && store !== null && !input.subAgent;
  const probeOf = (indexes: number[]) =>
    indexes.length > 0
      ? run.probeEdits(
          indexes.map((index) => relatives[index]!),
          choice.root,
        )
      : Promise.resolve({ results: [], failure: null, ms: 0 });
  // The fact lines and the probe run side by side: the wait is the longer of
  // the two, not their sum. Files queued in place of the probe are not probed.
  const [facts, early] = await Promise.all([
    hook
      ? run.factLines(
          choice.root,
          factFiles.map((file) => ({ file, ...input.changes?.get(file) })),
          session,
          { contract: contractLinesEnabled(), chain: chainLinesEnabled(), store },
        )
      : null,
    insteadOfProbe ? null : probeOf([...relatives.keys()]),
  ]);
  const covered = new Set(facts?.callersRead ?? []);
  const queued = new Set<string>();
  if (insteadOfProbe && edited.length > 0) {
    const entries = enqueue(choice.root, session!, edited, editedAt, { factsCallers: covered, insteadOfProbe, cwd });
    if (entries.length > 0) ensureRunner(choice.root);
    for (const entry of entries) queued.add(entry.file);
    askFor(entries);
  }
  const probed = [...relatives.keys()].filter((index) => !queued.has(edited[index]!));
  const answers = early ?? (await probeOf(probed));
  if (input.timing) input.timing.waitedMs = answers.ms;
  // Each file's probe answer by its place in `edited`; null for a file queued
  // in place of the probe or one the probe gave no answer for.
  const results: Array<CheckResult | null> = relatives.map(() => null);
  for (const [at, index] of probed.entries()) results[index] = answers.results[at] ?? null;
  const toQueue: number[] = [];
  const rendered = new Map<number, string>();
  const leads = new Map<number, SimilarLines>();
  for (const [index, relative] of relatives.entries()) {
    if (queued.has(edited[index]!)) {
      log("queued in place of the probe:", relative);
      continue;
    }
    const result = results[index] ?? null;
    const engineReady = engineReadyOf(result);
    if (input.timing && engineReady !== undefined) input.timing.engineReady = engineReady;
    if (!result) {
      // A probe with no answer queues every file; one unreadable file in a
      // patch of four is no reason to say nothing about the other three.
      log("no answer for", relative, answers.failure ?? "");
      if (queueable) toQueue.push(index);
      continue;
    }
    // The block rides on the probe answer, so a queued file gets it too.
    const found = similarLines(result, relative, cwd, (key) => !session || !wasLed(session, key), log);
    if (found.lines.length > 0) leads.set(index, found);
    if (queueable && needsBackground(result)) {
      toQueue.push(index);
      continue;
    }
    const key = toldKey(result, relative);
    // With a fact file for this file's repo, its callers are fact lines
    // (v1-agent's Q2 ruling); the uses-count line is the fallback without one.
    const show = !covered.has(edited[index]!) && (!session || !wasTold(session, key));
    if (show && result.uses_lines?.length) {
      told.push(key);
      // Counted only where the line is delivered (the hook channel) and the
      // fact lines could have stood in for it (the switch is on).
      if (input.timing && facts !== null && factsEnabled()) input.timing.usesFallback = true;
    }
    const context = renderPostToolUse(result, relative, { uses: show });
    log(`check ${relative} -> ${context ? "context" : "nothing to say"} in ${answers.ms}ms`);
    if (context) rendered.set(index, context);
  }

  if (toQueue.length > 0 && session) {
    // A file whose callers the fact lines answered keeps its late answer free
    // of the uses-count line too, as this answer would have been.
    const entries = enqueue(choice.root, session, toQueue.map((index) => edited[index]!), editedAt, { factsCallers: covered, cwd });
    if (entries.length > 0) ensureRunner(choice.root);
    for (const entry of entries) queued.add(entry.file);
    askFor(entries);
    // A queued file says nothing now: its answer is delivered with the next
    // Carrick line only when it holds a finding, and a refused queue entry is a
    // gap the Stop hook names (hook-wording audit T4).
  }
  // What the Stop hook states the session's check from (carrick#3466): each
  // file, at the time its entry or in-hand answer carries, and every answer
  // this hook had in hand as the session's answer for that edit. Only on the
  // hook channel, which is the one that queues and speaks at Stop. A file the
  // binary refused (`error`: no index, an unreadable one) is not recorded:
  // there is no check to state, and a fallback would fail the same way.
  if (session && channel.channel === "hook") {
    const recorded: string[] = [];
    for (const [index, file] of edited.entries()) {
      const result = results[index] ?? null;
      if (result?.error) continue;
      // A subagent's own worktree is not this session's change (carrick#3543).
      if (inOtherWorktree(file, cwd ?? process.cwd())) continue;
      if (result !== null && !queued.has(file)) answerInHand(choice.root, session, file, result, editedAt);
      if (result !== null || queued.has(file)) recorded.push(file);
    }
    if (recorded.length > 0) recordEdit(session, choice.root, editedAt, undefined, recorded);
  }
  // The early answers in hand: each ask ends by the edit's deadline, and one
  // that answered `ready` has filed its early answer before replying. Each is
  // claimed before the late answers below are read, so no carrier says it again.
  const answered = await Promise.all(asks);
  const held = mergedAsk(answered.map((a) => a.answer));
  if (input.timing && held && held.state !== "off") {
    input.timing.heldMs = held.ms;
    input.timing.heldState = held.state;
  }
  const seen = new Set<string>();
  const earlyNow: string[] = [];
  for (const { entry, answer } of answered) {
    if (answer.state !== "ready") continue;
    const early = readEarly(entry.session, entry.seq);
    if (early) earlyNow.push(...deliverEarly(entry.session, early, seen, { late: false }));
  }
  // Delivered after this edit is queued, so an older answer for a file this
  // edit queues again is skipped (the newer entry carries it).
  const stamps: LocalStamp[] = [];
  const late = channel.channel === "hook" ? await deliverLateFirst(session, Date.now(), seen) : [];
  const generationAt = contexts.length;
  contexts.push(...late);
  for (const index of relatives.keys()) {
    const context = rendered.get(index);
    if (context) contexts.push(context);
    const stamp = context && results[index] ? stampOf(results[index]!) : null;
    if (stamp) stamps.push(stamp);
    const found = leads.get(index);
    if (found) {
      // A late answer delivered just above may have claimed the same function.
      for (const [at, key] of found.keys.entries()) {
        if (session && wasLed(session, key)) continue;
        contexts.push(found.lines[at]!);
        led.push(key);
      }
    }
  }

  if (facts === null) {
    log(`the ${channel.channel} channel owns delivery in this install; printing nothing`);
    return;
  }
  if (input.timing) {
    input.timing.factsMs = facts.ms;
    input.timing.factsStops = facts.stops;
  }
  // The first part-2 line of the session says which index it read while it
  // differs from the fact file's (once per session).
  const generation = contexts.length > generationAt ? await firstLineGeneration(choice.root, session, stamps) : null;
  if (generation) contexts.splice(generationAt, 0, generation);
  // An early line is ADDED to part 1's: it never replaces a contract line.
  contexts.push(...earlyNow);
  contexts.push(...facts.lines);
  // A missing or stale fact file is refreshed in the background, detached.
  if (facts.stale) (await import("../facts/fetch.ts")).startFactsRefresh(choice.root);
  // The `carrick` this hook is running is whatever PATH answered with, which
  // an upgrade elsewhere can leave behind (carrick#1372). Claimed at the last
  // possible moment, so the once-a-day stamp is spent only on a line that is
  // actually delivered, and inside `additionalContext` rather than on a bare
  // stdout line, which this host reads as a malformed hook response.
  // A call that touched only `package.json` files says only its fact lines and
  // owed late answers, as before fact files: no version notice.
  const sourceTouched = edited.length > 0 || (input.factOnly ?? []).some((file) => !isManifest(file));
  const mismatch = sourceTouched ? throttledVersionMismatch(choice.root, currentVersion()) : null;
  if (mismatch) contexts.push(mismatch);
  if (told.length > 0 && session) markTold(session, told);
  if (led.length > 0 && session) markLed(session, led);
  if (session) markFacts(session, facts.claims);
  contexts.unshift(...extra);
  emitPostToolUse(contexts);
}

/** One state and time for the action's asks: the first that answered `ready`, else the first; the longest wait. */
function mergedAsk(answers: readonly AskAnswer[]): AskAnswer | null {
  if (answers.length === 0) return null;
  const first = answers.find((a) => a.state === "ready") ?? answers[0]!;
  return { ...first, ms: Math.max(...answers.map((a) => a.ms)) };
}
