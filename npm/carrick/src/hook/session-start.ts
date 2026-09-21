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
import { currentVersion, readUpdateState, suppressed, updateNotice } from "../update.ts";
import { refreshInBackground } from "./refresh.ts";
import { versionMismatch } from "../init/outdated.ts";

const log = createLogger("carrick-session");

/**
 * "Start of session, not mid session" — the moment a version notice is worth
 * anything, because it is the moment before the agent does any work.
 *
 * Read from the cache only: a hook has a 300 ms budget and no business dialling
 * a registry. The cache is refreshed by a detached child the shim starts, so
 * what this prints is at most one run behind. On stdout, unlike the shim's
 * stderr line, because stdout is what Claude Code adds to the session — the
 * agent is the one that can act on it, and an agent that forgets to upgrade is
 * the failure this exists for.
 */
function versionLine(): string | null {
  if (suppressed(process.env)) return null;
  const state = readUpdateState(process.env);
  return updateNotice(currentVersion(), state?.latest ?? null, { env: process.env });
}

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

  // Printed whatever the index turns out to say, and before the read that may
  // have nothing to report: a machine with no index yet is a machine about to
  // run its first scan, which is the worst moment to be on a stale build.
  const version = versionLine();
  if (version) process.stdout.write(`${version}\n`);

  // And the other mismatch: not "a newer one is published" but "the one
  // answering this hook is not the one that wrote these files". Unthrottled,
  // because this runs once per session and the agent about to work here is the
  // reader who can act on it (carrick#1372).
  if (choice.markerFound) {
    const mismatch = versionMismatch(choice.root, currentVersion());
    if (mismatch) process.stdout.write(`${mismatch}\n`);
  }

  const outcome = await status({ cwd: choice.root, workspace: choice.markerFound ? choice.root : null });
  if (!outcome.result) {
    log("no answer", outcome.failure ?? "");
    return;
  }
  // Ends with a newline: this is a line a developer also runs by hand and
  // compares against `carrick status`, and that one ends its output properly.
  process.stdout.write(`${renderSessionStart(outcome.result)}\n`);

  // The hosted index the first CI scan writes is picked up here rather than by
  // a command the developer has to remember (carrick#955). Detached and
  // unwaited: a refresh is minutes of work and a hook is not.
  let started: string | null = null;
  try {
    started = refreshInBackground(choice.root, outcome.result);
  } catch (error) {
    log("background refresh not started", String(error));
  }
  if (started) process.stdout.write(`${started}\n`);
}

await main();
process.exit(0);
