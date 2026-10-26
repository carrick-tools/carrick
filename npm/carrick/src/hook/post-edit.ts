#!/usr/bin/env node
// PostToolUse hook for a file edit, on either host.
//
// Reads the tool payload on stdin, runs `carrick check <file> --json` from the
// workspace root and prints what the edit needs acted on (`renderPostToolUse`:
// its problems, or a pointer to `carrick check --changed` when the file could
// not be re-checked) as `additionalContext`, which the host
// attaches to the tool result. Attaching to the result rather than to the next
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

import path from "node:path";
import { check, checkEach } from "../cli.ts";
import { resolveChannel } from "../channel.ts";
import { createLogger } from "../log.ts";
import { renderPostToolUse } from "../render.ts";
import { resolveRoot, rootNote } from "../root.ts";
import { patchedFiles } from "./apply-patch.ts";
import { throttledVersionMismatch } from "../init/outdated.ts";
import { currentVersion } from "../update.ts";
import { markTold, record, toldKey, wasTold } from "./reuse.ts";
import type { CheckResult } from "../contract.ts";
import type { NewFunction } from "./reuse.ts";

const log = createLogger("carrick-hook");
/** The files the index has rows for. */
const CHECKED = /\.(ts|tsx|mts|cts)$/;

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
 * What the re-check said this file declares and the index does not
 * (carrick#1330).
 *
 * Nothing is printed about it here. This hook fires on every edit and most
 * edits add no function; the one line about the ones that did belongs at the
 * end of the task, where it costs one model turn instead of one per edit. All
 * this does is put the names somewhere the Stop hook can find them.
 */
export function newFunctions(result: CheckResult, file: string): NewFunction[] {
  return (result.recheck?.new_functions ?? []).map((entry) => ({
    name: entry.name,
    file: result.file ?? file,
    indexCommit: result.index_commit ?? "",
  }));
}

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

/**
 * Every edited file's answer, in order, from one binary call: the
 * single-file `check` for one file (what Claude Code's edits always are), and
 * `check --each` for a patch that touches several.
 */
async function answersFor(
  relatives: string[],
  cwd: string,
): Promise<{ results: Array<CheckResult | null>; failure: string | null; ms: number }> {
  if (relatives.length === 1) {
    const outcome = await check(relatives[0]!, { cwd, recheck: true });
    return { results: [outcome.result], failure: outcome.failure, ms: outcome.ms };
  }
  const outcome = await checkEach(relatives, { cwd });
  return { results: outcome.result ?? [], failure: outcome.failure, ms: outcome.ms };
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

async function readStdin(): Promise<string> {
  const chunks: Buffer[] = [];
  for await (const chunk of process.stdin) chunks.push(Buffer.from(chunk));
  return Buffer.concat(chunks).toString("utf8");
}

async function main(): Promise<void> {
  // `off` stops the work as well as the printing. `lsp` stops only the
  // printing: it decides which surface delivers a verdict, and the reuse
  // record below is not a verdict — it is what the Stop hook speaks from, and
  // the language server has no equivalent of that moment (carrick#1330).
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
  if (edited.length === 0) return;

  // One root for the call: every file in one patch is in the workspace the tool
  // ran in, and resolving it per file would ask the same question repeatedly.
  const choice = resolveRoot({
    clientRoot: cwd,
    projectDir: process.env["CLAUDE_PROJECT_DIR"] ?? null,
    filePath: edited[0]!,
  });
  const note = rootNote(choice);
  if (note) log(note);

  const found: NewFunction[] = [];
  const contexts: string[] = [];
  // Files whose uses lines this message carries, remembered only once it is
  // delivered: who uses a file is shown once per file per session (carrick#2067).
  const told: string[] = [];
  const relatives = edited.map((file) => path.relative(choice.root, file));
  // The edit just landed, so the index describes the file as it was before
  // it. This is the one surface that asks for the file to be re-judged from
  // the working tree; it fires once per completed edit, where the language
  // server fires on every save (carrick#1036). A patch of several files is
  // one binary call, not one per file: each call re-reads the index and, on a
  // repo whose re-check does not fit, would wait out a budget of its own.
  const answers = await answersFor(relatives, choice.root);
  for (const [index, relative] of relatives.entries()) {
    const result = answers.results[index] ?? null;
    if (!result) {
      // One unreadable file in a patch of four is not a reason to say nothing
      // about the other three.
      log("no answer for", relative, answers.failure ?? "");
      continue;
    }
    found.push(...newFunctions(result, relative));
    const key = toldKey(result, relative);
    const show = !payload.session_id || !wasTold(payload.session_id, key);
    if (show && result.uses_lines?.length) told.push(key);
    const context = renderPostToolUse(result, relative, { uses: show });
    log(`check ${relative} -> ${context ? "context" : "nothing to say"} in ${answers.ms}ms`);
    if (context) contexts.push(context);
  }

  // Silent and local: the names go to a file under the user's home directory
  // and nothing about them is printed here.
  if (found.length > 0 && payload.session_id) {
    record(payload.session_id, found);
    log(`recorded ${found.length} new function(s) for ${payload.session_id}`);
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
  if (told.length > 0 && payload.session_id) markTold(payload.session_id, told);
  if (contexts.length > 0) emit(contexts.join("\n\n"));
}

await main();
process.exit(0);
