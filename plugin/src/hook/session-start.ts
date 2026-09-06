#!/usr/bin/env node
// SessionStart hook: one orientation line about the index this workspace has.
//
// INERT UNTIL carrick#728. As of the contract pinned on 2026-09-06, `touch` and
// `check` each take exactly one file and neither answers for a workspace: with
// no path they print a usage line and exit 2. The workspace summary is a
// separate command that is not built. So this hook asks for the fileless form,
// gets no `carrick.check/0` payload, logs one line and prints nothing. The
// moment that command exists it answers here with no change to this file, and
// the renderer is tested against the shape it will return.
//
// Claude Code adds a SessionStart hook's stdout to the session on exit 0, so a
// plain print is the whole mechanism.
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
