#!/usr/bin/env node
// UserPromptSubmit hook: the reuse nudge, on Codex (carrick#1335).
//
// Same nudge, same store, same once-per-set marking as the Stop hook this
// package installs for Claude Code — `drain` is the shared half and the only
// difference is which event carries the line.
//
// Why this event and not `Stop`. Codex accepts `additionalContext` on
// `PreToolUse`, `PostToolUse`, `SessionStart`, `UserPromptSubmit` and
// `SubagentStart`, and warns "this event cannot emit additionalContext" for
// every other one (`codex-rs/hooks/src/engine/discovery.rs`). A Codex `Stop`
// hook can reach the model only by refusing to let the turn end, and this nudge
// is advice, never a block. `UserPromptSubmit` is the one event that is both
// model-visible and non-blocking there.
//
// What that costs: the line arrives with the NEXT prompt rather than at the end
// of the task that earned it, and a run that ends on that task never sees it.
// The named-once marking is what makes the late delivery safe — the set is
// spoken once, whenever the next prompt comes, and never again.
//
// It exits 0 on every path, like the other three. A prompt never fails because
// Carrick had nothing to say.
//
// Reference: `docs/reference/task-skills.md`, "The reuse nudge".

import { createLogger } from "../log.ts";
import { resolveChannel } from "../channel.ts";
import { drain } from "./reuse.ts";
import { initialisedRoot, throttledVersionMismatch } from "../init/outdated.ts";
import { currentVersion } from "../update.ts";

const log = createLogger("carrick-user-prompt");

type Payload = {
  session_id?: string;
};

async function readStdin(): Promise<string> {
  const chunks: Buffer[] = [];
  for await (const chunk of process.stdin) chunks.push(Buffer.from(chunk));
  return Buffer.concat(chunks).toString("utf8");
}

/**
 * The one channel a `UserPromptSubmit` hook has that the model reads.
 *
 * Codex parses this with `deny_unknown_fields`
 * (`codex-rs/hooks/src/schema.rs`), so the object is exactly these two keys and
 * a wrong event name is a parse failure rather than a silent drop.
 */
export function emission(context: string): string {
  return JSON.stringify({
    hookSpecificOutput: {
      hookEventName: "UserPromptSubmit",
      additionalContext: context,
    },
  });
}

async function main(): Promise<void> {
  // `off` silences every surface this package has, and this is one of them.
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

  const spoken = drain(session);
  const parts: string[] = [];
  if (spoken === null) log(`nothing new in ${session}: everything recorded has been named`);
  else parts.push(spoken.line);

  // The same backstop the other hooks carry: this `carrick` is not the one
  // that set this workspace up (carrick#1372), once a day per workspace.
  const root = initialisedRoot(process.env["CLAUDE_PROJECT_DIR"] ?? process.cwd());
  const mismatch = root === null ? null : throttledVersionMismatch(root, currentVersion());
  if (mismatch) parts.push(mismatch);

  if (parts.length === 0) return;
  process.stdout.write(emission(parts.join("\n\n")));
  if (spoken) log(`named ${spoken.named} new function(s) to the model, of ${spoken.found} recorded`);
}

await main();
process.exit(0);
