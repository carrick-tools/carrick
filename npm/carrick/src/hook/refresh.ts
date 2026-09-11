// Picking up a hosted index without being asked to (carrick#955).
//
// The first CI scan on a repo's default branch is what writes that repo's
// hosted index, and until a local `carrick refresh` reads it, a workspace
// indexed before that scan holds only what this machine could see on its own.
// Nothing tells the developer the moment it lands, so the step that connects
// the two was a command they had to remember.
//
// The session-start hook is where that can happen for free, with two
// constraints it must not break:
//
// - **A hook may not take time.** `carrick refresh` re-scans every repo in the
//   workspace and drives a type sidecar per repo; it is minutes, not
//   milliseconds. So it is started detached and not waited for. The session
//   gets its orientation line immediately, and the refreshed index is there
//   for the next one.
// - **A hook may not thrash.** The refresh is asked for only while some
//   service still reports `no_index_yet` — a repo connected to the workspace
//   whose hosted index has not appeared yet — and at most once per cooldown.
//   Once the hosted index is read, the state becomes `enriched` and this stops
//   on its own. `not_connected` and `not_signed_in` never trigger it: there is
//   nothing to wait for in either.
//
// Two sessions opening in one workspace would otherwise both see the same
// stale marker and both start a re-index over the same `.carrick` directory,
// so the marker is claimed by a link that only one of them can win.

import fs from "node:fs";
import path from "node:path";
import { spawn } from "node:child_process";
import { binary } from "../cli.ts";
import type { StatusResult } from "../contract.ts";

/** Default gap between unattended refreshes of one workspace. */
export const REFRESH_COOLDOWN_MS = 60 * 60_000;

/** The claim file, inside the ignored directory the index already owns. */
export function markerPath(root: string): string {
  return path.join(root, ".carrick", "last-hook-refresh");
}

export function cooldownMs(env: NodeJS.ProcessEnv = process.env): number {
  const raw = env["CARRICK_REFRESH_COOLDOWN_MS"];
  const parsed = raw ? Number.parseInt(raw, 10) : Number.NaN;
  return Number.isFinite(parsed) && parsed >= 0 ? parsed : REFRESH_COOLDOWN_MS;
}

/** Whether this workspace is waiting for a hosted index that may now exist. */
export function awaitingHostedIndex(status: StatusResult): boolean {
  return status.services.some((service) => service.hosted_state === "no_index_yet");
}

/** When the marker last recorded a claim, or null when there is none. */
function claimedAt(marker: string): number | null {
  try {
    const stamp = Date.parse(fs.readFileSync(marker, "utf8").trim());
    return Number.isFinite(stamp) ? stamp : null;
  } catch {
    return null;
  }
}

/** The last time anything read the hosted side, from the marker or the index. */
export function lastHostedRead(status: StatusResult, marker: string): number {
  const checked = status.hosted_checked_at ? Date.parse(status.hosted_checked_at) : Number.NaN;
  return Math.max(claimedAt(marker) ?? 0, Number.isFinite(checked) ? checked : 0);
}

/**
 * Take the right to refresh this workspace, or decline it.
 *
 * The claim is a hard link onto the marker path, which is the one file
 * operation two processes cannot both win: whoever links first holds it, and
 * the loser sees EEXIST. A marker older than the cooldown is replaced, and
 * replacing it is where the race is — so the timestamp it held is re-read
 * before it is removed, and a marker that changed under us belongs to the
 * session that changed it.
 */
export function claimRefresh(marker: string, now: number, cooldown: number): boolean {
  const held = claimedAt(marker);
  if (held !== null && now - held < cooldown) return false;
  const temporary = `${marker}.${process.pid}`;
  try {
    fs.mkdirSync(path.dirname(marker), { recursive: true });
    fs.writeFileSync(temporary, `${new Date(now).toISOString()}\n`, { flag: "wx" });
  } catch {
    return false;
  }
  try {
    for (let attempt = 0; attempt < 2; attempt += 1) {
      try {
        fs.linkSync(temporary, marker);
        return true;
      } catch (error) {
        if ((error as NodeJS.ErrnoException).code !== "EEXIST") return false;
      }
      const current = claimedAt(marker);
      if (current === null || current !== held) return false;
      if (now - current < cooldown) return false;
      try {
        fs.unlinkSync(marker);
      } catch {
        // Another session removed it first; the retry decides between us.
      }
    }
    return false;
  } finally {
    try {
      fs.unlinkSync(temporary);
    } catch {
      // Already gone, which is not ours to report.
    }
  }
}

export type RefreshOptions = {
  env?: NodeJS.ProcessEnv;
  now?: number;
  /** Injected in tests; the real one detaches a child and forgets it. */
  start?: (command: string, args: string[], env: NodeJS.ProcessEnv) => void;
};

function startDetached(command: string, args: string[], env: NodeJS.ProcessEnv): void {
  const child = spawn(command, args, { detached: true, stdio: "ignore", env });
  child.unref();
}

/**
 * Start a background `carrick refresh` when this workspace is waiting for a
 * hosted index and has not asked recently. Returns the line to print, or null.
 */
export function refreshInBackground(
  root: string,
  status: StatusResult,
  options: RefreshOptions = {},
): string | null {
  const env = options.env ?? process.env;
  const now = options.now ?? Date.now();
  if (!awaitingHostedIndex(status)) return null;
  const marker = markerPath(root);
  const cooldown = cooldownMs(env);
  if (now - lastHostedRead(status, marker) < cooldown) return null;
  if (!claimRefresh(marker, now, cooldown)) return null;
  (options.start ?? startDetached)(binary(env), ["refresh", "--workspace", root], env);
  return "Carrick: a repo here is connected with no hosted index yet, so a refresh is running in the background. Its rows arrive in the next session.";
}
