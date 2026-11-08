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

import { readStdin } from "./stdin.ts";
import { status } from "../cli.ts";
import { resolveChannel } from "../channel.ts";
import { createLogger } from "../log.ts";
import { CHECK_BEFORE_DONE, heldLine, hostedLine, renderSessionStart } from "../render.ts";
import { lastSync } from "../hosted-snapshot.ts";
import { resolveRoot, rootNote } from "../root.ts";
import { currentVersion } from "../update.ts";
import { upgradeLines } from "./upgrade.ts";
import {
  claimRefresh,
  cooldownMs,
  forwardInBackground,
  markerPath,
  refreshInBackground,
} from "./refresh.ts";
import { outdatedFiles, outdatedInstall, versionMismatch } from "../init/outdated.ts";
import { bringUpToDate } from "../init/self-update.ts";
import { markInvoker } from "./invoker.ts";

const log = createLogger("carrick-session");

/**
 * "Start of session, not mid session" — the moment a version notice is worth
 * anything, because it is the moment before the agent does any work.
 *
 * Read from the cache only: a hook has a 300 ms budget and no business dialling
 * a registry. The cache is refreshed by a detached child the shim starts, so
 * what this prints is at most one run behind. A global install that is behind
 * is upgraded by a detached child (`./upgrade.ts`); everything else gets the
 * notice. On stdout, unlike the shim's
 * stderr line, because stdout is what Claude Code adds to the session — the
 * agent is the one that can act on it, and an agent that forgets to upgrade is
 * the failure this exists for.
 */
function versionLines(): string[] {
  return upgradeLines();
}

async function readStdinUnlessTty(): Promise<string> {
  return process.stdin.isTTY ? "" : readStdin();
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
  markInvoker("session-start");
  if (resolveChannel({ hooksInstalled: true }).channel === "off") {
    log("CARRICK_CHANNEL=off; printing nothing");
    return;
  }
  const lines: string[] = [];
  const codex = runByCodex(process.env, await readStdinUnlessTty());
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
  lines.push(...versionLines());

  // And the other mismatch: not "a newer one is published" but "the one
  // answering this hook is not the one that wrote these files". Unthrottled,
  // because this runs once per session and the agent about to work here is the
  // reader who can act on it (carrick#1372).
  if (choice.markerFound) {
    // Both harnesses: Codex gets the AGENTS.md section and the skills, which it
    // reads with no trust gate (carrick#3435). It has no `CLAUDE_PROJECT_DIR`, so
    // the Claude Code settings files are left to a Claude Code session, and
    // `.codex/hooks.json` is never written (`init/self-update.ts`).
    // A by-hand run (neither harness) writes nothing.
    const projectDir = codex ? null : process.env["CLAUDE_PROJECT_DIR"] || null;
    const held: string[] = [];
    if (codex || projectDir) {
      try {
        lines.push(...bringUpToDate(choice.root, projectDir, currentVersion(), (message) => log(message), held));
      } catch (error) {
        log("self-update failed", String(error));
      }
    }
    // When every outdated file is one git tracks, the held line has said it.
    const outdated = outdatedFiles(outdatedInstall(choice.root));
    const allHeld = held.length > 0 && outdated.every((file) => held.includes(file));
    const mismatch = versionMismatch(choice.root, currentVersion());
    if (mismatch && !allHeld) lines.push(mismatch);
  }

  const outcome = await status({ cwd: choice.root, workspace: choice.markerFound ? choice.root : null });
  if (!outcome.result) {
    log("no answer", outcome.failure ?? "");
    flush(lines, codex);
    return;
  }
  // What the MCP answers from, and the copy the hooks compare against, ahead
  // of this machine's own scan: that scan's date alone was read as the age of
  // every answer (carrick#3432). From the last background move's record; the
  // move itself runs after this read, so the line is at most one run behind.
  const hosted = outcome.result.error ? null : hostedSync(choice.root, Date.now());
  if (hosted) lines.push(...hosted);
  // Ends with a newline: this is a line a developer also runs by hand and
  // compares against `carrick status`, and that one ends its output properly.
  lines.push(renderSessionStart(outcome.result, { afterHostedLine: hosted !== null }));
  if (!outcome.result.error) lines.push(CHECK_BEFORE_DONE);

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
  // Otherwise move the hosted index forward along this checkout's history, so
  // the next `check --changed` judges the branch and not every merge
  // since. Silent and unwaited.
  else {
    try {
      forwardInBackground(choice.root, outcome.result);
    } catch (error) {
      log("background hosted move not started", String(error));
    }
  }
  flush(lines, codex);
}

/** Default gap between two statements that HEAD keeps the copy back. */
export const HELD_NOTICE_COOLDOWN_MS = 24 * 60 * 60_000;

/**
 * The hosted line, and, at most once per cooldown, the line saying HEAD keeps
 * this machine's copy back (carrick#3433). Null where no record exists yet,
 * which keeps the session start as it was. The claim is taken only when there
 * is a held line to print, so a day of current copies does not use it up.
 */
export function hostedSync(root: string, now: number, env: NodeJS.ProcessEnv = process.env): string[] | null {
  const sync = lastSync(root);
  const hosted = sync ? hostedLine(sync, now) : null;
  if (!sync || !hosted) return null;
  const lines = [hosted];
  const held = heldLine(sync);
  const cooldown = cooldownMs(env, "CARRICK_HELD_NOTICE_COOLDOWN_MS", HELD_NOTICE_COOLDOWN_MS);
  if (held && claimRefresh(markerPath(root, "last-held-notice"), now, cooldown)) lines.push(held);
  return lines;
}

/** One write at the end, so the Codex form is a single JSON document. */
function flush(lines: string[], codex: boolean): void {
  if (lines.length === 0) return;
  if (codex) process.stdout.write(codexEmission(lines.join("\n")));
  else process.stdout.write(`${lines.join("\n")}\n`);
}

await main();
process.exit(0);
