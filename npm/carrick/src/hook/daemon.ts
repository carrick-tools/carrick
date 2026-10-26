// Warming the resident type daemon at session start (carrick#2884).
//
// `carrick check --recheck` (each edit) and `carrick check --changed` answer
// through a resident type process that keeps each service's TypeScript
// program between checks. The binary starts it by itself the first time a
// check finds none, so the first check of a session would pay the cold path.
// The session-start hook starts it instead, and asks it to re-scan the repos
// with changes through the daemon, so the first edit finds its programs built.
//
// A hook may not take time: `carrick daemon start --warm` is started detached
// and not waited for. Two sessions opening at once must not both start one,
// so the start is claimed with the same hard-link claim the refresh uses, with
// a short cooldown. The binary's own claims (the daemon's `typed.json`, the
// warm-up's) hold the rest. Stop and UserPromptSubmit hooks never stop it: it
// stops itself after 15 minutes with no request.

import path from "node:path";
import { binary } from "../cli.ts";
import { claimRefresh, startDetached, type RefreshOptions } from "./refresh.ts";

/** Gap between two session starts' daemon starts in one workspace. */
export const DAEMON_START_COOLDOWN_MS = 60_000;

/** The claim file, inside the ignored directory the index already owns. */
export function daemonMarkerPath(root: string): string {
  return path.join(root, ".carrick", "last-daemon-start");
}

/** Whether the daemon is turned off (`CARRICK_TYPED=off`). */
export function daemonOff(env: NodeJS.ProcessEnv): boolean {
  const value = (env["CARRICK_TYPED"] ?? "").trim();
  return value === "off" || value === "0" || value === "false";
}

/**
 * Start `carrick daemon start --warm` in the background for this workspace,
 * unless the daemon is off or another session started one within the
 * cooldown. Returns whether it was started; prints nothing.
 */
export function warmDaemonInBackground(root: string, options: RefreshOptions = {}): boolean {
  const env = options.env ?? process.env;
  if (daemonOff(env)) return false;
  const now = options.now ?? Date.now();
  if (!claimRefresh(daemonMarkerPath(root), now, DAEMON_START_COOLDOWN_MS)) return false;
  (options.start ?? startDetached)(
    binary(env),
    ["daemon", "start", "--warm", "--workspace", root],
    // Nobody is waiting on it, so it holds nothing awake (carrick#1889).
    { ...env, CARRICK_NO_KEEP_AWAKE: "1" },
  );
  return true;
}
