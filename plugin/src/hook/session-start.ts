#!/usr/bin/env node
// SessionStart hook: one orientation line about the index this workspace has.
//
// `carrick touch --json` with no file answers for the workspace: which service
// the index holds, the commit it was taken at, how many files have changed
// since, and the boundary. Claude Code adds a SessionStart hook's stdout to the
// session on exit 0, so a plain print is the whole mechanism.
//
// This is not a verdict channel. `touch` returns every verdict null by
// contract, so the line carries no compatibility finding and cannot be confused
// with the hook or LSP delivery of one. `CARRICK_CHANNEL=off` silences it along
// with everything else.

import { touch } from "../cli.ts";
import { resolveChannel } from "../channel.ts";
import { createLogger } from "../log.ts";
import { renderSessionStart } from "../render.ts";
import { resolveRoot, rootNote } from "../root.ts";

const log = createLogger("carrick-session");

async function main(): Promise<void> {
  if (resolveChannel({ hooksInstalled: true }).channel === "off") {
    log("CARRICK_CHANNEL=off; printing nothing");
    return;
  }
  const choice = resolveRoot({
    clientRoot: process.cwd(),
    projectDir: process.env["CLAUDE_PROJECT_DIR"] ?? null,
    filePath: process.cwd(),
  });
  const note = rootNote(choice);
  if (note) log(note);

  const outcome = await touch(null, { cwd: choice.root });
  if (!outcome.result) {
    log("no answer", outcome.failure ?? "");
    return;
  }
  process.stdout.write(renderSessionStart(outcome.result));
}

await main();
process.exit(0);
