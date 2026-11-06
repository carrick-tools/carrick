#!/usr/bin/env node
// PostToolUse hook for a file edit, on either host.
//
// Reads the tool payload on stdin and asks the binary once, from the workspace
// root, with a probe that never starts a re-scan (`probe.ts`). An answer in
// hand prints what the edit needs acted on (`renderPostToolUse`: its problems,
// or why it was not re-checked); an edit that needs a re-scan is queued for the
// background runner (`rechecks.ts`), prints one pending line, and its answer is
// delivered by the next carrier (`late.ts`). Output is `additionalContext`,
// which the host attaches to the tool result. Attaching to the result rather than to the next
// turn is why this channel has no "the run ended on the edit" hole.
//
// Two payload shapes reach it, because the two hosts edit differently
// (carrick#1335). Claude Code's `Edit`, `Write` and `MultiEdit` name one file
// in `tool_input.file_path`. Codex's `apply_patch` carries a patch in
// `tool_input.command`, and one patch can touch several files — so the file is
// a list here, checked in the order the payload names them.
//
// It exits 0 whatever happens. An edit never fails because Carrick had nothing
// to say, could not find its binary, or timed out.

import { readStdin } from "./stdin.ts";
import path from "node:path";
import { resolveChannel } from "../channel.ts";
import { createLogger } from "../log.ts";
import { answersTheEdit, notRecheckedLine, pendingLine, renderPostToolUse } from "../render.ts";
import { resolveRoot, rootNote } from "../root.ts";
import { patchedFiles } from "./apply-patch.ts";
import { throttledVersionMismatch } from "../init/outdated.ts";
import { currentVersion } from "../update.ts";
import { markTold, recordEdit, toldKey, wasTold } from "./reuse.ts";
import { markInvoker } from "./invoker.ts";
import { deliverLate } from "./late.ts";
import { needsBackground, probeEdits } from "./probe.ts";
import { answerInHand, enqueue, ensureRunner } from "./rechecks.ts";

const log = createLogger("carrick-hook");

/** The files the index has rows for. */
const CHECKED = /\.(ts|tsx|mts|cts|graphql|gql)$/;

type Payload = {
  cwd?: string;
  session_id?: string;
  tool_input?: {
    file_path?: string;
    filePath?: string;
    edits?: Array<{ file_path?: string }>;
    /** Codex's `apply_patch`: the patch text itself. */
    command?: unknown;
  };
};

/**
 * Every file one tool call left on disk, whichever host made it.
 *
 * A named file wins where there is one: that is Claude Code, and its path is
 * absolute. Otherwise the `command` field is read as a patch, which is Codex's
 * `apply_patch` — its paths are relative to the cwd the tool ran in, and a
 * string that is not a patch gives nothing, so nothing here has to ask which
 * host is calling.
 *
 * A shell command that is not a patch reaches the same branch and yields an
 * empty list, which is why this does not test `tool_name`: the patch text
 * recognises itself, and an `apply_patch` run through a shell is recorded on
 * the same terms as one run as a tool.
 */
export function filesFromPayload(payload: Payload): string[] {
  const input = payload.tool_input ?? {};
  const named = input.file_path ?? input.filePath ?? input.edits?.[0]?.file_path ?? null;
  if (named) return [named];
  return typeof input.command === "string" ? patchedFiles(input.command) : [];
}

function emit(context: string): void {
  process.stdout.write(
    JSON.stringify({
      hookSpecificOutput: {
        hookEventName: "PostToolUse",
        additionalContext: context,
      },
    }),
  );
}

async function main(): Promise<void> {
  markInvoker("post-edit");
  // `off` stops the work as well as the printing. `lsp` stops only the
  // printing: it decides which surface delivers a verdict, and the session
  // record below (the edit stamp) is not a verdict.
  const channel = resolveChannel({ hooksInstalled: true });
  if (channel.channel === "off") {
    log("CARRICK_CHANNEL=off; doing nothing");
    return;
  }

  let payload: Payload;
  try {
    payload = JSON.parse((await readStdin()) || "{}") as Payload;
  } catch (error) {
    log("unparseable payload", String(error));
    return;
  }

  // A patch can name a file this index has no rows for beside one it does, so
  // the filter is per file rather than a reason to drop the call.
  const cwd = payload.cwd ?? null;
  const edited = filesFromPayload(payload)
    .filter((file) => CHECKED.test(file))
    .map((file) => (path.isAbsolute(file) ? file : path.resolve(cwd ?? process.cwd(), file)));
  const session = payload.session_id;
  if (edited.length === 0) {
    // Nothing of ours to check in this edit, but a finished background answer
    // is still owed to the model (B3: every carrier delivers).
    if (channel.channel === "hook") {
      const late = deliverLate(session);
      if (late.length > 0) emit(late.join("\n\n"));
    }
    return;
  }

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
  // earlier did not read this edit (`checked.ts`). Each file is recorded with
  // the same time its entry or in-hand answer carries, so the Stop hook can
  // tell an answer to this edit from an older one (carrick#3466).
  const editedAt = Date.now();
  if (session) recordEdit(session, choice.root, editedAt, undefined, edited);

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
    for (const index of toQueue) {
      const result = answers.results[index] ?? null;
      if (queued.has(edited[index]!)) {
        rendered.set(index, pendingLine(relatives[index]!, result?.service));
      } else {
        // The queue refused it: say what is true rather than nothing.
        rendered.set(index, notRecheckedLine(relatives[index]!, result?.recheck?.reason));
      }
    }
  }
  // Every answer this hook had in hand is the session's answer for that edit,
  // for the Stop hook's statement of the change (`sessionCoverage`).
  if (session) {
    for (const [index, file] of edited.entries()) {
      const result = answers.results[index] ?? null;
      if (result !== null && !queued.has(file)) answerInHand(choice.root, session, file, result, editedAt);
    }
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
  if (contexts.length > 0) emit(contexts.join("\n\n"));
}

await main();
process.exit(0);
