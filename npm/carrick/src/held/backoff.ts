// Crash back-off for the held daemon, the rule the scanner's type daemon uses
// (carrick#2904): two crashes of one build within an hour keep that build from
// starting for a day after the latest. A new build starts afresh.

import fs from "node:fs";
import path from "node:path";
import { crashesPath } from "./paths.ts";

/** Crashes of one build this close together back it off. */
export const CRASH_WINDOW_MS = 60 * 60_000;
/** How long a backed-off build is not started again. */
export const BACK_OFF_MS = 24 * 60 * 60_000;

/** Append one crash of `buildId`. Never throws. */
export function recordCrash(root: string, buildId: string, now: number = Date.now(), home?: string): void {
  try {
    const file = crashesPath(root, home);
    fs.mkdirSync(path.dirname(file), { recursive: true, mode: 0o700 });
    fs.appendFileSync(file, `${Math.floor(now / 1000)} ${buildId}\n`);
  } catch {
    // A crash nobody can record is not a reason to crash again.
  }
}

/** Whether `buildId` is backed off for this checkout at `now`. */
export function backedOff(root: string, buildId: string, now: number = Date.now(), home?: string): boolean {
  let text: string;
  try {
    text = fs.readFileSync(crashesPath(root, home), "utf8");
  } catch {
    return false;
  }
  const times = text
    .split("\n")
    .map((line) => line.split(" "))
    .filter(([secs, build]) => build?.trim() === buildId && /^\d+$/.test(secs ?? ""))
    .map(([secs]) => Number(secs) * 1000)
    .sort((a, b) => a - b);
  const latest = times[times.length - 1];
  if (latest === undefined) return false;
  const close = times.some((time, i) => i > 0 && time - times[i - 1]! <= CRASH_WINDOW_MS);
  return close && now < latest + BACK_OFF_MS;
}
