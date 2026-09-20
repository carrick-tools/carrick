#!/usr/bin/env node
// PostToolUse hook for Edit, Write and MultiEdit.
//
// Reads the tool payload on stdin, runs `carrick check <file> --json` from the
// workspace root and prints the verdicts as `additionalContext`, which Claude
// Code attaches to the tool result. Attaching to the result rather than to the
// next turn is why this channel has no "the run ended on the edit" hole.
//
// It exits 0 whatever happens. An edit never fails because Carrick had nothing
// to say, could not find its binary, or timed out.

import path from "node:path";
import { check } from "../cli.ts";
import { resolveChannel } from "../channel.ts";
import { createLogger } from "../log.ts";
import { renderPostToolUse } from "../render.ts";
import { resolveRoot, rootNote } from "../root.ts";
import { record } from "./reuse.ts";
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

export function fileFromPayload(payload: Payload): string | null {
  const input = payload.tool_input ?? {};
  return input.file_path ?? input.filePath ?? input.edits?.[0]?.file_path ?? null;
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

  const file = fileFromPayload(payload);
  if (!file || !CHECKED.test(file)) return;

  const choice = resolveRoot({
    clientRoot: payload.cwd ?? null,
    projectDir: process.env["CLAUDE_PROJECT_DIR"] ?? null,
    filePath: file,
  });
  const note = rootNote(choice);
  if (note) log(note);

  const relative = path.isAbsolute(file) ? path.relative(choice.root, file) : file;
  // The edit just landed, so the index describes the file as it was before it.
  // This is the one surface that asks for the file to be re-judged from the
  // working tree; it fires once per completed edit, where the language server
  // fires on every save (carrick#1036).
  const outcome = await check(relative, { cwd: choice.root, recheck: true });
  if (!outcome.result) {
    log("no answer for", relative, outcome.failure ?? "");
    return;
  }
  // Silent and local: the names go to a file under the user's home directory
  // and nothing about them is printed here.
  const found = newFunctions(outcome.result, relative);
  if (found.length > 0 && payload.session_id) {
    record(payload.session_id, found);
    log(`recorded ${found.length} new function(s) for ${payload.session_id}`);
  }

  if (channel.channel !== "hook") {
    log(`the ${channel.channel} channel owns delivery in this install; printing nothing`);
    return;
  }
  const context = renderPostToolUse(outcome.result, relative);
  log(`check ${relative} -> ${context ? "context" : "nothing to say"} in ${outcome.ms}ms`);
  if (context) emit(context);
}

await main();
process.exit(0);
