#!/usr/bin/env node
// PostToolUse hook for a file read, on Claude Code (carrick#2069).
//
// Before the agent edits a file, tell it who else depends on it: the callers of
// the file's functions, the operations that read its types, and the other side
// of each route or call in it. The answer is `carrick check <file> --json` with
// no re-check, rendered as locations only. The verdicts stay in the post-edit
// hook, which judges the edited text.
//
// A Read fires on every file the agent opens, so the work is bounded: nothing
// starts for a non-TypeScript file, for a file this session has already asked
// about, or on a channel that does not print through hooks. A file is asked
// about once per session whatever the answer was. Whatever happens it exits 0
// and prints at most the one JSON object.

import { readStdin } from "./stdin.ts";
import path from "node:path";
import { resolveChannel } from "../channel.ts";
import { createLogger } from "../log.ts";
import { markFacts, wasRead } from "./reuse.ts";
import { sessionKey } from "./session-key.ts";
import { markInvoker } from "./invoker.ts";
import { deliverLate, emitPostToolUse as emit } from "./late.ts";
import { answerReads } from "./read-answer.ts";
import { timed, type Timing } from "./timings.ts";

import { resolveRoot } from "../root.ts";
import { compilerLinesEnabled } from "../facts/enabled.ts";
import { askHeld } from "../held/client.ts";
import { inOtherWorktree } from "./worktree.ts";

const log = createLogger("carrick-hook");
/** A Read only starts engines: the daemon answers `cold` at once, so a short wait is enough. */
const HELD_READ_WAIT_MS = 300;
/** The files the index has rows for. */
const CHECKED = /\.(ts|tsx|mts|cts)$/;

type Payload = {
  cwd?: string;
  session_id?: string;
  agent_id?: string;
  tool_input?: { file_path?: string };
};

async function main(timing: Timing): Promise<void> {
  markInvoker("post-read");
  // Only `hook` prints from here; `lsp` and `off` start nothing and write
  // nothing, so a file is not marked as asked about by a channel that said
  // nothing.
  if (resolveChannel({ hooksInstalled: true }).channel !== "hook") return;

  // The payload carries the file's text in `tool_response`; it is read to the
  // end and ignored.
  let payload: Payload;
  try {
    payload = JSON.parse((await readStdin()) || "{}") as Payload;
  } catch (error) {
    log("unparseable payload", String(error));
    return;
  }
  // A finished background re-check is owed to the model whatever this Read
  // is: delivered before the early returns below, in the same one object.
  const session = sessionKey(payload);
  const late = deliverLate(session);
  const named = payload.tool_input?.file_path;
  if (!named || !CHECKED.test(named)) return emit(late);
  const file = path.isAbsolute(named) ? named : path.resolve(payload.cwd ?? process.cwd(), named);
  if (session && wasRead(session, file)) return emit(late);

  const waitStarted = Date.now();
  // Part 3: tell the daemon this file was read so it can start the service's
  // compiler. Nothing is printed from the answer; it runs beside the read's own
  // work and is only joined so the request is sent before the hook exits.
  const heldRoot = compilerLinesEnabled() && session && !inOtherWorktree(file, payload.cwd ?? process.cwd())
    ? resolveRoot({ clientRoot: payload.cwd ?? null, projectDir: process.env["CLAUDE_PROJECT_DIR"] ?? null, filePath: file }).root
    : null;
  const heldAsked = heldRoot && session ? askHeld(heldRoot, [file], { session, kind: "read", waitMs: HELD_READ_WAIT_MS }) : null;
  const answers = await answerReads([file], payload.cwd, session);
  await heldAsked;
  timing.waitedMs = Date.now() - waitStarted;
  if (answers.factsMs !== undefined) timing.factsMs = answers.factsMs;
  emit([...late, ...answers.contexts]);
  // Claimed only now that the lines are printed.
  if (session) markFacts(session, answers.claims);
}

await timed("post-read", main);
process.exit(0);
