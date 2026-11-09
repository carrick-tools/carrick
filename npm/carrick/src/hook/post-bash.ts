#!/usr/bin/env node
// PostToolUse hook for a shell command, on Claude Code.
//
// Most reads of a source file in an agent session go through the shell, where
// the Read hook cannot fire. This decides in one process what the command
// touched: a read of a TypeScript file gets the Read hook's answer (who depends
// on it). The write branch (a shell write through the post-edit flow) is the
// next ticket's and lands beside `reads` below, not as a second hook.
//
// It runs on every shell command, so the work before a target is found is a
// node start, a stdin read and one regex: nothing else is imported. A command
// that names no source file prints nothing and starts nothing. Whatever
// happens it exits 0 and prints at most the one JSON object.

import { readStdin } from "./stdin.ts";
import path from "node:path";
import { bashTargets, MENTIONS_SOURCE } from "./bash-targets.ts";

/** At most this many files are asked about for one command. */
const MAX_FILES = 2;

type Payload = {
  cwd?: string;
  session_id?: string;
  tool_input?: { command?: string };
};

async function main(): Promise<void> {
  let payload: Payload;
  try {
    payload = JSON.parse((await readStdin()) || "{}") as Payload;
  } catch {
    return;
  }
  const command = payload.tool_input?.command;
  if (!command || !MENTIONS_SOURCE.test(command)) return;
  const cwd = payload.cwd ?? process.cwd();
  const { reads } = bashTargets(command, path.resolve(cwd));
  if (reads.length === 0) return;

  const { markInvoker } = await import("./invoker.ts");
  // The answer is the Read hook's, so the cloud's log tells it the same way.
  markInvoker("post-read");
  const { resolveChannel } = await import("../channel.ts");
  if (resolveChannel({ hooksInstalled: true }).channel !== "hook") return;
  const { wasRead } = await import("./reuse.ts");
  const { deliverLate } = await import("./late.ts");
  const { answerRead } = await import("./read-answer.ts");

  const session = payload.session_id;
  const late = deliverLate(session);
  const fresh = reads.filter((file) => !(session && wasRead(session, file))).slice(0, MAX_FILES);
  const answers = await Promise.all(fresh.map((file) => answerRead(file, payload.cwd, session)));
  const contexts = [...late, ...answers.filter((line): line is string => line !== null)];
  if (contexts.length === 0) return;
  process.stdout.write(
    JSON.stringify({
      hookSpecificOutput: { hookEventName: "PostToolUse", additionalContext: contexts.join("\n\n") },
    }),
  );
}

await main();
process.exit(0);
