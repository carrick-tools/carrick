#!/usr/bin/env node
// PostToolUse hook for Grep, Glob and MCP tool calls, on Claude Code.
//
// None of these reads or writes a file Carrick answers for, so this says
// nothing of its own. It exists so a background re-check that landed after the
// last edit reaches an agent that only searches (the Stop hook never waits for
// one; `late.ts`). The work is a node start, a stdin read and one claim of the
// finished answers. Whatever happens it exits 0 and prints at most the one
// JSON object.

import { readStdin } from "./stdin.ts";

async function main(): Promise<void> {
  let session: string | undefined;
  try {
    session = (JSON.parse((await readStdin()) || "{}") as { session_id?: string }).session_id;
  } catch {
    return;
  }
  const { printLateOnly } = await import("./late.ts");
  printLateOnly(session);
}

await main();
process.exit(0);
