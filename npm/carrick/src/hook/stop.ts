#!/usr/bin/env node
// Stop hook: what the background re-checks of this session's edits answered,
// and which are still running (`stop-wait.ts`, carrick#3198). It never waits. It
// reports what the agent's edits came to; it never asks the agent to start
// something new at the end of a task (carrick#3436).
//
// What it prints, and why that shape: Claude Code delivers a Stop hook's
// `hookSpecificOutput.additionalContext` to the MODEL and lets the turn
// continue — non-error feedback the model can act on. The alternatives were
// both wrong for this: `systemMessage` is shown to the user and never reaches
// the model (measured, carrick session 2026-09-03 — a Stop hook fired in 21
// sessions and changed nothing), and `decision: "block"` reaches the model by
// refusing to let the turn end, which is a block.
//
// It states the session's contract check only when there is something to say
// (`stop-wait.ts`): a clean line naming the operations checked, once per set,
// or the problems; a session that changed no route or call prints nothing.
//
// It exits 0 on every path, like the other hooks.
//
// Reference: `docs/reference/task-skills.md`, "What the hooks deliver".

import { readStdin } from "./stdin.ts";
import { createLogger } from "../log.ts";
import { resolveChannel } from "../channel.ts";
import { initialisedRoot, throttledVersionMismatch } from "../init/outdated.ts";
import { currentVersion } from "../update.ts";
import { markInvoker } from "./invoker.ts";
import { stopAnswers } from "./stop-wait.ts";

const log = createLogger("carrick-stop");

type Payload = {
  session_id?: string;
};

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
  markInvoker("stop");
  // `off` silences every surface this package has, and this is one of them.
  // The `lsp` channel is NOT excluded: it decides who delivers a verdict about
  // an edited file as it is edited, and the language server has no Stop.
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

  // What the background re-checks of this session's edits answered, and the
  // ones still running, named without waiting. An older entry's `--wait-s` is
  // ignored: Stop never holds up the agent.
  const parts: string[] = [];
  const late = stopAnswers(session);
  parts.push(...late.lines);

  // The other thing a task's end is a good moment for: the `carrick` these
  // hooks resolve is not the one that set this workspace up (carrick#1372).
  // Once a day, through the same channel, because a mismatch stands until
  // somebody fixes it and a line per turn would be noise.
  const root = initialisedRoot(process.env["CLAUDE_PROJECT_DIR"] ?? process.cwd());
  const mismatch = root === null ? null : throttledVersionMismatch(root, currentVersion());
  if (mismatch) parts.push(mismatch);

  if (parts.length === 0) return;
  process.stdout.write(emission(parts.join("\n\n")));
}

await main();
process.exit(0);
