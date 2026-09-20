// Files this package installed, and whether they are the ones it installs now.
//
// `npm install -g carrick@latest` changes the binary and nothing else. The
// hook entries in a settings file, the Codex ones, and the eight task skill
// bodies were written by whichever version first ran `carrick init` here, and
// nothing re-reads them: a repo can carry a hook that was replaced three
// releases ago and never be told (carrick#1333).
//
// What identifies a file as ours, and as current, is its content — not a
// version number written into it:
//
// * A **skill** carries a digest of the body this package rendered, and
//   `inspectTaskSkills` compares it against the body this version renders. A
//   release that does not change a skill leaves that skill current, which a
//   version stamp could not say.
// * A **hook entry** is identified by what it runs: its event, matcher,
//   command and timeout, which is exactly what `expectedCarrickHooks` states
//   and what survives any formatter run over somebody's settings file. That
//   is also why nothing here writes a marker into those files: a settings
//   document belongs to the agent that validates it, and a key it does not
//   know is a key it may one day reject — at which point every hook in the
//   file stops running, silently, because the hooks are built never to fail an
//   edit (carrick#837).
//
// The cost of reading content rather than a version is one consequence, and it
// is the right one: a hook entry somebody edited by hand reads the same as one
// an older version wrote, and `carrick init` rewrites it either way. Their
// file keeps everything else in it.
//
// The refresh is `carrick init` run again. It writes only what is still ours —
// an edited skill is named and left — so it is safe to run unattended.
// Reference: `docs/reference/task-skills.md`.

import fs from "node:fs";
import os from "node:os";
import path from "node:path";

import { ancestors, hasMarker } from "../root.ts";
import { CODEX_HOOKS_FILE, codexInUse, expectedCodexHooks, readHooksFile } from "./codex.ts";
import {
  expectedCarrickHooks,
  installedCarrickHooks,
  SETTINGS_FILES,
  type InstalledHook,
} from "./settings.ts";
import { inspectTaskSkills } from "./task-skills.ts";

/** What is installed here and is not what this version writes. */
export type Outdated = {
  /** Skill files still ours, carrying a body an older version rendered. */
  skills: string[];
  /** Hook files whose entries are not the ones this version installs. */
  hooks: string[];
};

/** Every file in it, for a caller that only needs the count and the names. */
export function outdatedFiles(found: Outdated): string[] {
  return [...found.hooks, ...found.skills];
}

/**
 * Whether a hook document holds entries this version no longer writes.
 *
 * The prefix is the file's own, as `carrick doctor` reads it: an install on a
 * machine with no `carrick` on PATH wrote absolute paths, and comparing those
 * against a bare `carrick` would report every entry as wrong on every run.
 * A file holding no entry of ours is not out of date — it is a file init has
 * never written to, which is a different finding and not this one.
 */
function entriesAreStale(body: string, expected: (prefix: string) => InstalledHook[]): boolean {
  let installed: InstalledHook[];
  try {
    installed = installedCarrickHooks(body);
  } catch {
    // Not JSON. `carrick doctor` says so in its own words; a line printed by
    // an unrelated command must not be the way somebody learns it.
    return false;
  }
  if (installed.length === 0) return false;
  const prefix = installed[0]!.command.split(/\s+hook\s+/)[0]!;
  return expected(prefix).some(
    (want) =>
      !installed.some(
        (have) =>
          have.event === want.event &&
          have.command === want.command &&
          have.matcher === want.matcher &&
          have.timeout === want.timeout,
      ),
  );
}

/**
 * What an upgrade left behind in this workspace.
 *
 * Reads files and nothing else: no network, no subprocess, no client
 * inspection — it runs from any command, so it has to cost nothing. The MCP
 * entry is deliberately not here: reading it can spawn an agent client's own
 * command, and `carrick doctor` is where that belongs.
 */
export function outdatedInstall(workspace: string): Outdated {
  const skills = inspectTaskSkills(workspace)
    .filter((skill) => skill.state === "ours" && !skill.current)
    .map((skill) => skill.path);

  const hooks: string[] = [];
  for (const relative of SETTINGS_FILES) {
    let body: string;
    try {
      body = fs.readFileSync(path.join(workspace, relative), "utf8");
    } catch {
      continue;
    }
    if (entriesAreStale(body, expectedCarrickHooks)) hooks.push(relative);
  }
  if (codexInUse(workspace)) {
    const body = readHooksFile(workspace);
    if (body !== null && entriesAreStale(body, expectedCodexHooks)) hooks.push(CODEX_HOOKS_FILE);
  }
  return { skills, hooks };
}

/**
 * The nearest folder at or above this one that `carrick init` has run in.
 *
 * The same marker every other surface roots on — the `.carrick` directory, not
 * a settings file or a git repository — so a command typed inside one repo of
 * a folder asks about the folder's install, which is the install there is.
 * Null where there is none, and a machine that never ran init never hears from
 * this.
 */
export function initialisedRoot(from: string): string | null {
  return ancestors(from).find((directory) => hasMarker(directory)) ?? null;
}

/**
 * The one line, and how often it is allowed to be said.
 *
 * Once a day, from any command that prints for a person to read. It is a
 * sentence about the install, not about the command somebody typed, so it goes
 * to stderr and never into an answer.
 */
export function noticeFile(home: string = os.homedir()): string {
  return path.join(home, ".carrick", "last-notice");
}

/** Today, as the throttle file spells it. */
function today(now: Date): string {
  return now.toISOString().slice(0, 10);
}

/** Delete the throttle file, as `carrick remove` does. True when there was one. */
export function removeNotice(home?: string): boolean {
  try {
    fs.rmSync(noticeFile(home));
    return true;
  } catch {
    return false;
  }
}

export type NoticeEnvironment = {
  home: string;
  now: Date;
};

/**
 * The line to print here, or null.
 *
 * Null covers every ordinary run: a folder Carrick was never set up in, an
 * install that is current, a day this has already been said on, and anything
 * at all going wrong while asking. A command's real work must never fail
 * because of a notice about it.
 *
 * The day is only recorded when there was something to say, so an install that
 * goes out of date an hour from now is still named an hour from now.
 */
export function refreshNotice(
  workspace: string,
  environment: NoticeEnvironment = { home: os.homedir(), now: new Date() },
): string | null {
  try {
    const file = noticeFile(environment.home);
    let last: string | null;
    try {
      last = fs.readFileSync(file, "utf8").trim();
    } catch {
      last = null;
    }
    if (last === today(environment.now)) return null;
    const found = outdatedInstall(workspace);
    const files = outdatedFiles(found);
    if (files.length === 0) return null;
    fs.mkdirSync(path.dirname(file), { recursive: true });
    fs.writeFileSync(file, `${today(environment.now)}\n`);
    return `${files.length} file(s) here were written by an older carrick (${files
      .slice(0, 2)
      .join(", ")}${files.length > 2 ? ", …" : ""}). Run \`carrick init\` to refresh them.`;
  } catch {
    return null;
  }
}
