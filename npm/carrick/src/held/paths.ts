// Where the held-compiler daemon keeps its files. Everything is under
// `~/.carrick/held/`, outside every checkout, keyed by a hash of the checkout's
// real path (a socket path is capped near 104 bytes on macOS, so no names).

import { createHash } from "node:crypto";
import os from "node:os";
import path from "node:path";
import { realPath } from "../fs-util.ts";

/** The daemon touches its lock every tick; it is stale this long (plus the lock's slack) after. */
export const LOCK_CAP_MS = 30_000;

export function heldDir(home: string = os.homedir()): string {
  return path.join(home, ".carrick", "held");
}

/** The first 12 hex characters of the sha-256 of the checkout's real path. */
export function rootKey(root: string): string {
  return createHash("sha256").update(realPath(root)).digest("hex").slice(0, 12);
}

export function socketPath(root: string, home?: string): string {
  return path.join(heldDir(home), `${rootKey(root)}.sock`);
}

export function lockPath(root: string, home?: string): string {
  return path.join(heldDir(home), `${rootKey(root)}.lock`);
}

/** One JSON line per request. */
export function logPath(root: string, home?: string): string {
  return path.join(heldDir(home), `${rootKey(root)}.log`);
}
