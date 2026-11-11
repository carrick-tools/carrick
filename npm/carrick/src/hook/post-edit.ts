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
import { patchedFiles } from "./apply-patch.ts";
import { markInvoker } from "./invoker.ts";
import { deliverLate, emitPostToolUse } from "./late.ts";
import { editFlow } from "./edit-flow.ts";
import { createLogger } from "../log.ts";

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
      emitPostToolUse(late);
    }
    return;
  }

  await editFlow({ cwd, session, channel: channel.channel }, edited);
}

await main();
process.exit(0);
