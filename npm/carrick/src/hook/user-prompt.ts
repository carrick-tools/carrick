#!/usr/bin/env node
// UserPromptSubmit hook: the finished background re-checks no other hook has
// delivered yet. It names no function a task added (carrick#3436).
//
// On Codex (carrick#1335) it stands in for Stop. Codex accepts
// `additionalContext` on `PreToolUse`, `PostToolUse`, `SessionStart`,
// `UserPromptSubmit` and `SubagentStart`, and warns "this event cannot emit
// additionalContext" for every other one (`codex-rs/hooks/src/engine/discovery.rs`).
// A Codex `Stop` hook can reach the model only by refusing to let the turn
// end, which is a block.
//
// On Claude Code it carries what landed after Stop returned: Stop never waits
// for a re-check (hook-wording audit T2), so an answer that lands later comes
// with the next prompt, unless a tool hook carried it first.
//
// What that costs: an answer arrives with the NEXT prompt rather than at the
// end of the task that earned it, and a run that ends on that task never sees
// it. Each answer is delivered once (`deliverLate`).
//
// It exits 0 on every path, like the other hooks. A prompt never fails because
// Carrick had nothing to say.
//
// Reference: `docs/reference/task-skills.md`, "What the hooks deliver".

import { readStdin } from "./stdin.ts";
import { sessionKey } from "./session-key.ts";
import { createLogger } from "../log.ts";
import { resolveChannel } from "../channel.ts";
import { deliverLate } from "./late.ts";
import { initialisedRoot, throttledVersionMismatch } from "../init/outdated.ts";
import { currentVersion } from "../update.ts";
import { timed } from "./timings.ts";

const log = createLogger("carrick-user-prompt");

type Payload = {
  session_id?: string;
  agent_id?: string;
};

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
  const session = sessionKey(payload);
  if (!session) {
    log("no session id in the payload; nothing to look up");
    return;
  }

  // Finished background re-checks no hook has delivered yet.
  const parts: string[] = [...deliverLate(session)];
  const root = initialisedRoot(process.env["CLAUDE_PROJECT_DIR"] ?? process.cwd());

  // The same backstop the other hooks carry: this `carrick` is not the one
  // that set this workspace up (carrick#1372), once a day per workspace.
  const mismatch = root === null ? null : throttledVersionMismatch(root, currentVersion());
  if (mismatch) parts.push(mismatch);

  if (parts.length === 0) return;
  process.stdout.write(emission(parts.join("\n\n")));
}

await timed("prompt-submit", main);
process.exit(0);
