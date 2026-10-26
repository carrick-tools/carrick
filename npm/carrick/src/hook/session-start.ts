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
//
// Codex runs the same entry (`codexHooks`, carrick#2848). Its parser reads a
// SessionStart hook's context from `hookSpecificOutput.additionalContext`
// (`codex-rs/hooks/src/engine/output_parser.rs`, `parse_session_start`), so
// there the lines go out as that JSON. Claude Code keeps the plain print.

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

async function readStdin(): Promise<string> {
  if (process.stdin.isTTY) return "";
  const chunks: Buffer[] = [];
  for await (const chunk of process.stdin) chunks.push(Buffer.from(chunk));
  return Buffer.concat(chunks).toString("utf8");
}

/**
 * Whether Codex, not Claude Code, is running this hook.
 *
 * Claude Code sets `CLAUDE_PROJECT_DIR` for every hook and Codex does not (its
 * hook environment is the plugin variables only). A by-hand run has no payload
 * on stdin, so it reads as neither and keeps the plain print.
 */
export function runByCodex(env: NodeJS.ProcessEnv, stdin: string): boolean {
  if (env["CLAUDE_PROJECT_DIR"]) return false;
  try {
    const payload = JSON.parse(stdin) as { hook_event_name?: unknown };
    return payload.hook_event_name === "SessionStart";
  } catch {
    return false;
  }
}

/** The shape Codex's `parse_session_start` takes the model context from. */
export function codexEmission(context: string): string {
  return JSON.stringify({
    hookSpecificOutput: { hookEventName: "SessionStart", additionalContext: context },
  });
}

async function main(): Promise<void> {
  if (resolveChannel({ hooksInstalled: true }).channel === "off") {
    log("CARRICK_CHANNEL=off; printing nothing");
    return;
  }
  const lines: string[] = [];
  const codex = runByCodex(process.env, await readStdin());
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
  if (version) lines.push(version);

  // And the other mismatch: not "a newer one is published" but "the one
  // answering this hook is not the one that wrote these files". Unthrottled,
  // because this runs once per session and the agent about to work here is the
  // reader who can act on it (carrick#1372).
  if (choice.markerFound) {
    const mismatch = versionMismatch(choice.root, currentVersion());
    if (mismatch) lines.push(mismatch);
  }

  const outcome = await status({ cwd: choice.root, workspace: choice.markerFound ? choice.root : null });
  if (!outcome.result) {
    log("no answer", outcome.failure ?? "");
    flush(lines, codex);
    return;
  }
  // Ends with a newline: this is a line a developer also runs by hand and
  // compares against `carrick status`, and that one ends its output properly.
  lines.push(renderSessionStart(outcome.result));

  // The hosted index the first CI scan writes is picked up here rather than by
  // a command the developer has to remember (carrick#955). Detached and
  // unwaited: a refresh is minutes of work and a hook is not.
  let started: string | null = null;
  try {
    started = refreshInBackground(choice.root, outcome.result);
  } catch (error) {
    log("background refresh not started", String(error));
  }
  if (started) lines.push(started);
  flush(lines, codex);
}

/** One write at the end, so the Codex form is a single JSON document. */
function flush(lines: string[], codex: boolean): void {
  if (lines.length === 0) return;
  if (codex) process.stdout.write(codexEmission(lines.join("\n")));
  else process.stdout.write(`${lines.join("\n")}\n`);
}

await main();
process.exit(0);
