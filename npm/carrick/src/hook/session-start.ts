#!/usr/bin/env node
// SessionStart hook: one orientation line about the index this workspace has.
//
// `carrick status --json` is the read that answers for a workspace: `check` and
// `touch` each take exactly one file. It carries, per service, what the index
// holds, the commit it was taken at, how far that repo has moved since, and the
// boundary. Claude Code adds a SessionStart hook's stdout to the session on
// exit 0, so a plain print is the whole mechanism.
//
// This is not a verdict channel. `status` states what is indexed and what each
// service could not classify, and nothing about whether a contract holds, so
// the line cannot be confused with the hook or LSP delivery of a verdict.
// `CARRICK_CHANNEL=off` silences it along with everything else.

import { status } from "../cli.ts";
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

  const outcome = await status({ cwd: choice.root, workspace: choice.markerFound ? choice.root : null });
  if (!outcome.result) {
    log("no answer", outcome.failure ?? "");
    return;
  }
  process.stdout.write(renderSessionStart(outcome.result));
}

await main();
process.exit(0);
