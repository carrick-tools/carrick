#!/usr/bin/env node
// Stop hook: one reuse nudge per task, or nothing at all (carrick#1330).
//
// What it prints, and why that shape: Claude Code delivers a Stop hook's
// `hookSpecificOutput.additionalContext` to the MODEL and lets the turn
// continue — non-error feedback the model can act on. The alternatives were
// both wrong for this: `systemMessage` is shown to the user and never reaches
// the model (measured, carrick session 2026-09-03 — a Stop hook fired in 21
// sessions and changed nothing), and `decision: "block"` reaches the model by
// refusing to let the turn end, which is a block. This nudge is advice.
//
// It is silent whenever it has nothing new to say: no session id, no recorded
// functions, or every recorded function already named by an earlier stop of
// the same session. A stop after a stop is the common case — a task is
// several turns — so "already nudged" is the state this has to get right or
// the line repeats on every turn until the session ends.
//
// It exits 0 on every path, like the other two.
//
// Reference: `docs/reference/task-skills.md`, "The reuse nudge" — the three
// parts, the channel measurement behind this shape, and what Codex lacks.

import { createLogger } from "../log.ts";
import { resolveChannel } from "../channel.ts";
import { drain } from "./reuse.ts";

const log = createLogger("carrick-stop");

type Payload = {
  session_id?: string;
  /** True when this stop is itself the result of a hook holding the turn. */
  stop_hook_active?: boolean;
};

async function readStdin(): Promise<string> {
  const chunks: Buffer[] = [];
  for await (const chunk of process.stdin) chunks.push(Buffer.from(chunk));
  return Buffer.concat(chunks).toString("utf8");
}

/** The one channel a Stop hook has that the model reads. */
export function emission(context: string): string {
  return JSON.stringify({
    hookSpecificOutput: {
      hookEventName: "Stop",
      additionalContext: context,
    },
  });
}

async function main(): Promise<void> {
  // `off` silences every surface this package has, and this is one of them.
  // The `lsp` channel is NOT excluded: it decides who delivers a verdict about
  // an edited file, and this says nothing about a verdict.
  if (resolveChannel({ hooksInstalled: true }).channel === "off") {
    log("CARRICK_CHANNEL=off; printing nothing");
    return;
  }

  let payload: Payload;
  try {
    payload = JSON.parse((await readStdin()) || "{}") as Payload;
  } catch (error) {
    log("unparseable payload", String(error));
    return;
  }
  const session = payload.session_id;
  if (!session) {
    log("no session id in the payload; nothing to look up");
    return;
  }

  // `drain` reads the store, builds the line and marks the set as spoken, in
  // that order. Codex's `UserPromptSubmit` hook calls the same function
  // (carrick#1335); the difference between the two hosts is the event name on
  // the wire and nothing else.
  const spoken = drain(session);
  if (spoken === null) {
    log(`nothing new in ${session}: everything recorded has been named`);
    return;
  }
  process.stdout.write(emission(spoken.line));
  log(`named ${spoken.named} new function(s) to the model, of ${spoken.found} recorded`);
}

await main();
process.exit(0);
