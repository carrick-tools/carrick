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

import path from "node:path";
import { check } from "../cli.ts";
import { resolveChannel } from "../channel.ts";
import { createLogger } from "../log.ts";
import { renderPostRead } from "../render.ts";
import { resolveRoot, rootNote } from "../root.ts";
import { markRead, markTold, toldKey, wasRead } from "./reuse.ts";
import { markInvoker } from "./invoker.ts";
import { deliverLate } from "./late.ts";

const log = createLogger("carrick-hook");
/** The files the index has rows for. */
const CHECKED = /\.(ts|tsx|mts|cts)$/;
/** The limit on the one check, unless the caller set its own. */
const CHECK_TIMEOUT_MS = "2000";

type Payload = {
  cwd?: string;
  session_id?: string;
  tool_input?: { file_path?: string };
};

async function readStdin(): Promise<string> {
  const chunks: Buffer[] = [];
  for await (const chunk of process.stdin) chunks.push(Buffer.from(chunk));
  return Buffer.concat(chunks).toString("utf8");
}

/** The one JSON object this hook prints, or nothing when there are no lines. */
function emit(contexts: string[]): void {
  if (contexts.length === 0) return;
  process.stdout.write(
    JSON.stringify({
      hookSpecificOutput: { hookEventName: "PostToolUse", additionalContext: contexts.join("\n\n") },
    }),
  );
}

async function main(): Promise<void> {
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
  const session = payload.session_id;
  const late = deliverLate(session);
  const named = payload.tool_input?.file_path;
  if (!named || !CHECKED.test(named)) return emit(late);
  const file = path.isAbsolute(named) ? named : path.resolve(payload.cwd ?? process.cwd(), named);
  if (session && wasRead(session, file)) return emit(late);

  const choice = resolveRoot({
    clientRoot: payload.cwd ?? null,
    projectDir: process.env["CLAUDE_PROJECT_DIR"] ?? null,
    filePath: file,
  });
  const note = rootNote(choice);
  if (note) log(note);

  const relative = path.relative(choice.root, file);
  const env = { ...process.env, CARRICK_TIMEOUT_MS: process.env["CARRICK_TIMEOUT_MS"] ?? CHECK_TIMEOUT_MS };
  const outcome = await check(relative, { cwd: choice.root, env });
  // Asked once: an error or a timeout is not asked again on the next Read.
  if (session) markRead(session, file);
  if (!outcome.result) {
    log("no answer for", relative, outcome.failure ?? "");
    return emit(late);
  }

  const context = renderPostRead(outcome.result, relative);
  log(`check ${relative} -> ${context ? "context" : "nothing to say"} in ${outcome.ms}ms`);
  emit(context ? [...late, context] : late);
  // The post-edit hook that follows prints this file's header and items but
  // not the uses lines it was just shown.
  if (session && outcome.result.uses_lines?.length) {
    markTold(session, [toldKey(outcome.result, relative)]);
  }
}

await main();
process.exit(0);
