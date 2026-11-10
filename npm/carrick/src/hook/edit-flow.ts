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
import { markTold, recordEdit, toldKey, wasTold } from "./reuse.ts";
import { deliverLate } from "./late.ts";
import { needsBackground, probeEdits } from "./probe.ts";
import { inOtherWorktree } from "./worktree.ts";
import { answerInHand, enqueue, ensureRunner } from "./rechecks.ts";

const log = createLogger("carrick-hook");

/** Print one `additionalContext` object, which the host attaches to the tool result. */
export function emit(context: string): void {
  process.stdout.write(
    JSON.stringify({
      hookSpecificOutput: {
        hookEventName: "PostToolUse",
        additionalContext: context,
      },
    }),
  );
}

/**
 * Check `edited` (absolute paths, at least one) as the post-edit hook does and
 * print the one object for them. `extra` lines (a read answer from the same
 * command) are printed first, in the same object.
 */
export async function editFlow(
  input: { cwd: string | null; session: string | undefined; channel: Channel },
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
    filePath: edited[0]!,
  });
  const note = rootNote(choice);
  if (note) log(note);
  // Stamped before any line is built: a `carrick check --changed` that began
  // earlier did not read this edit (`checked.ts`). The files themselves are
  // recorded once the probe has answered (below).
  const editedAt = Date.now();
  if (session) recordEdit(session, choice.root, editedAt);

  const contexts: string[] = [];
  // Files whose uses lines this message carries, remembered only once it is
  // delivered: who uses a file is shown once per file per session (carrick#2067).
  const told: string[] = [];
  const relatives = edited.map((file) => path.relative(choice.root, file));
  // The edit just landed, so the index describes the file as it was before
  // it. The hook asks the binary once, with a probe that never starts a
  // re-scan: an answer that needs none (the file reads the same, no service
  // reads it, a known miss) comes back at once and prints below. Every other
  // file is queued for the background runner (carrick#3198).
  const answers = await probeEdits(relatives, choice.root);
  const queueable = channel.channel === "hook" && Boolean(session);
  const toQueue: number[] = [];
  const rendered = new Map<number, string>();
  for (const [index, relative] of relatives.entries()) {
    const result = answers.results[index] ?? null;
    if (!result) {
      // A probe with no answer queues every file; one unreadable file in a
      // patch of four is no reason to say nothing about the other three.
      log("no answer for", relative, answers.failure ?? "");
      if (queueable) toQueue.push(index);
      continue;
    }
    if (queueable && needsBackground(result)) {
      toQueue.push(index);
      continue;
    }
    const key = toldKey(result, relative);
    const show = !session || !wasTold(session, key);
    if (show && result.uses_lines?.length) told.push(key);
    const context = renderPostToolUse(result, relative, { uses: show });
    log(`check ${relative} -> ${context ? "context" : "nothing to say"} in ${answers.ms}ms`);
    if (context) rendered.set(index, context);
  }

  const queued = new Set<string>();
  if (toQueue.length > 0 && session) {
    const entries = enqueue(choice.root, session, toQueue.map((index) => edited[index]!), editedAt);
    if (entries.length > 0) ensureRunner(choice.root);
    for (const entry of entries) queued.add(entry.file);
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
      const result = answers.results[index] ?? null;
      if (result?.error) continue;
      // A subagent's own worktree is not this session's change (carrick#3543).
      if (inOtherWorktree(file, cwd ?? process.cwd())) continue;
      if (result !== null && !queued.has(file)) answerInHand(choice.root, session, file, result, editedAt);
      if (result !== null || queued.has(file)) recorded.push(file);
    }
    if (recorded.length > 0) recordEdit(session, choice.root, editedAt, undefined, recorded);
  }
  // Delivered after this edit is queued, so an older answer for a file this
  // edit queues again is skipped (the newer entry carries it).
  const late = channel.channel === "hook" ? deliverLate(session) : [];
  contexts.push(...late);
  for (const index of relatives.keys()) {
    const context = rendered.get(index);
    if (context) contexts.push(context);
  }

  if (channel.channel !== "hook") {
    log(`the ${channel.channel} channel owns delivery in this install; printing nothing`);
    return;
  }
  // The `carrick` this hook is running is whatever PATH answered with, which
  // an upgrade elsewhere can leave behind (carrick#1372). Claimed at the last
  // possible moment, so the once-a-day stamp is spent only on a line that is
  // actually delivered, and inside `additionalContext` rather than on a bare
  // stdout line, which this host reads as a malformed hook response.
  const mismatch = throttledVersionMismatch(choice.root, currentVersion());
  if (mismatch) contexts.push(mismatch);
  if (told.length > 0 && session) markTold(session, told);
  contexts.unshift(...extra);
  if (contexts.length > 0) emit(contexts.join("\n\n"));
}
