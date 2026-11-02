#!/usr/bin/env node
// UserPromptSubmit hook: one line on the first prompt of a session, telling the
// agent to prefer one inline `search_by_intent` over a grep sweep or a search
// subagent. Once per session, as the hosted scaffold's reminder was; the line
// is advice, never a block. Works under Claude Code and Codex.
//
// Silent when the channel is off, with no session id, on a session already
// given the line, or when a committed settings file still runs the old pack's
// reminder script.

import { resolveChannel } from "../channel.ts";
import { createLogger } from "../log.ts";
import { candidateRoots, emit, oldPackWired, parsePayload, readStdin } from "./levers.ts";
import { claimOnce } from "./reuse.ts";

const log = createLogger("carrick-first-prompt");

export const FIRST_PROMPT_LINE =
  "Prefer one inline mcp__carrick__search_by_intent call over a grep sweep or a search subagent: describe the behaviour in plain English and it answers in one call, covering every repo in this project rather than only this one.";

async function main(): Promise<void> {
  if (resolveChannel({ hooksInstalled: true }).channel === "off") {
    log("CARRICK_CHANNEL=off; printing nothing");
    return;
  }
  const payload = parsePayload<{ session_id?: string; cwd?: string }>(await readStdin());
  if (!payload.session_id) {
    log("no session id in the payload; nothing to mark");
    return;
  }
  if (oldPackWired(candidateRoots(payload.cwd), "turn-reminder.sh")) {
    log("the old pack's turn reminder is wired; leaving the line to it");
    return;
  }
  if (!claimOnce(payload.session_id, "prompted")) return;
  process.stdout.write(emit("UserPromptSubmit", FIRST_PROMPT_LINE));
}

await main();
process.exit(0);
