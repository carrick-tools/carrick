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

const log = createLogger("carrick-hook");
/** The files the index has rows for. */
const CHECKED = /\.(ts|tsx|mts|cts)$/;

type Payload = {
  cwd?: string;
  tool_input?: {
    file_path?: string;
    filePath?: string;
    edits?: Array<{ file_path?: string }>;
  };
};

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
  const channel = resolveChannel({ hooksInstalled: true });
  if (channel.channel !== "hook") {
    log(`the ${channel.channel} channel owns delivery in this install; printing nothing`);
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
  const outcome = await check(relative, { cwd: choice.root });
  if (!outcome.result) {
    log("no answer for", relative, outcome.failure ?? "");
    return;
  }
  const context = renderPostToolUse(outcome.result, relative);
  log(`check ${relative} -> ${context ? "context" : "nothing to say"} in ${outcome.ms}ms`);
  if (context) emit(context);
}

await main();
process.exit(0);
