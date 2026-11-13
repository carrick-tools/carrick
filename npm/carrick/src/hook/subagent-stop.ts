#!/usr/bin/env node
// SubagentStop hook: marks a sub-agent as ended (carrick#4074).
//
// A sub-agent's background re-checks are filed under `<session>.<agent>`, and
// none of its own hooks runs once it has ended, so an answer landing after
// that was never read. This writes `<session>.<agent>.ended` in the sessions
// folder; the main session's carriers and Stop then deliver what the ended
// agent is owed (`rechecks.ts` `owedFor`, `late.ts` `adoptedLine`). A sub-agent
// resumed later deletes its own marker with its next hook.
//
// It prints nothing and exits 0 on every path. Codex has no sub-agent-end
// event, so nothing here applies to it.
//
// Reference: `docs/reference/task-skills.md`, "What the hooks deliver".

import { readStdin } from "./stdin.ts";
import { sessionKey } from "./session-key.ts";
import { createLogger } from "../log.ts";
import { markEnded } from "./rechecks.ts";
import { timed } from "./timings.ts";

const log = createLogger("carrick-subagent-stop");

type Payload = {
  session_id?: string;
  agent_id?: string;
};

async function main(): Promise<void> {
  let payload: Payload;
  try {
    payload = JSON.parse((await readStdin()) || "{}") as Payload;
  } catch (error) {
    log("unparseable payload", String(error));
    return;
  }
  // Only a payload that names an agent ends one: the key is then
  // `<session>.<agent>`, never the main session's.
  if (typeof payload.agent_id !== "string" || payload.agent_id === "") {
    log("no agent id in the payload; nothing to mark");
    return;
  }
  const key = sessionKey(payload);
  if (!key) {
    log("no session id in the payload; nothing to mark");
    return;
  }
  markEnded(key);
}

await timed("subagent-stop", main);
process.exit(0);
