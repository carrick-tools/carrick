#!/usr/bin/env node
// PreToolUse hook (Claude Code): one line on the first search of a session.
//
// Fires for the Grep tool and for a Bash command that runs a search
// (`searchCommandIn`). It never blocks and never approves a tool call: the
// output is `additionalContext` only. Silent once the session has called a
// Carrick tool, which it reads off the transcript, and silent after its first
// line, which it marks in the session record.
//
// Silent when the channel is off, or when a committed settings file still runs
// the old pack's gate script. Any error leaves it silent.

import fs from "node:fs";

import { resolveChannel } from "../channel.ts";
import { createLogger } from "../log.ts";
import { candidateRoots, emit, oldPackWired, parsePayload, readStdin } from "./levers.ts";
import { alreadyClaimed, claimOnce } from "./reuse.ts";
import { searchCommandIn } from "./search-command.ts";

const log = createLogger("carrick-search-nudge");

export const SEARCH_NUDGE_LINE =
  "Carrick has an index of this workspace. For a helper, an endpoint, a consumer or a type that lives elsewhere, `search_by_intent` and the other Carrick tools answer what grep cannot; grep to confirm a location.";

/** The tool_use shape of a Carrick call, never the bare namespace (prose prints that). */
const CALLED = '"name":"mcp__carrick__';

/** Whether a transcript file holds a Carrick tool call. Unreadable counts as no. */
export function transcriptCalledCarrick(file: string): boolean {
  let fd: number;
  try {
    fd = fs.openSync(file, "r");
  } catch {
    return false;
  }
  try {
    const chunk = Buffer.alloc(1 << 20);
    let carry = "";
    for (;;) {
      const read = fs.readSync(fd, chunk, 0, chunk.length, null);
      if (read === 0) return false;
      const text = carry + chunk.toString("utf8", 0, read);
      if (text.includes(CALLED)) return true;
      carry = text.slice(-CALLED.length);
    }
  } catch {
    return false;
  } finally {
    fs.closeSync(fd);
  }
}

type Payload = {
  session_id?: string;
  cwd?: string;
  tool_name?: string;
  tool_input?: { command?: unknown };
  transcript_path?: string;
};

async function main(): Promise<void> {
  if (resolveChannel({ hooksInstalled: true }).channel === "off") {
    log("CARRICK_CHANNEL=off; printing nothing");
    return;
  }
  const payload = parsePayload<Payload>(await readStdin());
  const searching =
    payload.tool_name === "Grep" ||
    (payload.tool_name === "Bash" &&
      typeof payload.tool_input?.command === "string" &&
      searchCommandIn(payload.tool_input.command));
  if (!searching) return;
  const session = payload.session_id;
  if (!session) return;
  if (oldPackWired(candidateRoots(payload.cwd), "search-gate.sh")) {
    log("the old pack's search gate is wired; leaving the line to it");
    return;
  }
  if (alreadyClaimed(session, "searchNudged")) return;
  // A session that has called Carrick has no need of the line, and is marked so
  // the transcript is not read again.
  if (payload.transcript_path && transcriptCalledCarrick(payload.transcript_path)) {
    claimOnce(session, "searchNudged");
    return;
  }
  if (!claimOnce(session, "searchNudged")) return;
  process.stdout.write(emit("PreToolUse", SEARCH_NUDGE_LINE));
}

await main();
process.exit(0);
