// One lock file for one background process: the re-check runner and the
// held-compiler daemon both use it. A lock names a pid and the time it was last
// touched; it is held while that process is alive and has touched it lately.

import fs from "node:fs";
import path from "node:path";
import { writeAtomic } from "./reuse.ts";

/** A lock is stale this long after its holder's cap, whatever its pid says. */
export const STALE_SLACK_MS = 60_000;

export type Lock = { pid: number; started: string; touched: string };

let tempCounter = 0;

export function alive(pid: number): boolean {
  try {
    process.kill(pid, 0);
    return true;
  } catch (error) {
    // EPERM: the process exists and is somebody else's.
    return (error as NodeJS.ErrnoException).code === "EPERM";
  }
}

export function readJson<T>(file: string): T | null {
  try {
    return JSON.parse(fs.readFileSync(file, "utf8")) as T;
  } catch {
    return null;
  }
}

/**
 * Create `file` holding `body`, only if it does not exist yet, and complete
 * when it does: written beside it, then hard-linked into place (the one
 * operation two processes cannot both win).
 */
export function createExclusive(file: string, body: string): boolean {
  tempCounter += 1;
  const temp = `${file}.${process.pid}.${tempCounter}.tmp`;
  try {
    fs.mkdirSync(path.dirname(file), { recursive: true, mode: 0o700 });
    fs.writeFileSync(temp, body, { flag: "wx", mode: 0o600 });
  } catch {
    return false;
  }
  try {
    fs.linkSync(temp, file);
    return true;
  } catch {
    return false;
  } finally {
    try {
      fs.unlinkSync(temp);
    } catch {
      // Already gone.
    }
  }
}

function lockBody(pid: number, started: string, now: number): string {
  return `${JSON.stringify({ pid, started, touched: new Date(now).toISOString() })}\n`;
}

/** Whether the lock's holder is a live process that has touched it lately. */
export function lockHeld(lock: Lock | null, now: number, cap: number): boolean {
  if (lock === null || typeof lock.pid !== "number") return false;
  if (!alive(lock.pid)) return false;
  const touched = Date.parse(lock.touched);
  return Number.isFinite(touched) && now - touched <= cap + STALE_SLACK_MS;
}

/**
 * Take the lock at `file` for `pid`, replacing one nobody holds. The dead lock
 * is re-read before it is removed, so a lock another process just replaced is
 * never unlinked from under it. `takeOver` names the process that started
 * `pid` and holds the lock until `pid` takes it.
 */
export function takeLock(file: string, pid: number, now: number, cap: number, takeOver?: number): boolean {
  const body = lockBody(pid, new Date(now).toISOString(), now);
  for (let attempt = 0; attempt < 2; attempt += 1) {
    if (createExclusive(file, body)) return true;
    const seen = readJson<Lock>(file);
    if (seen !== null && seen.pid === pid) {
      writeAtomic(file, body);
      return true;
    }
    // The process that started this one holds the lock until it takes over.
    if (seen !== null && takeOver !== undefined && seen.pid === takeOver) {
      writeAtomic(file, body);
      return true;
    }
    if (lockHeld(seen, now, cap)) return false;
    const again = readJson<Lock>(file);
    if ((again?.pid ?? null) !== (seen?.pid ?? null)) return false;
    try {
      fs.unlinkSync(file);
    } catch {
      // Another process removed it first; the retry decides between us.
    }
  }
  return false;
}

export function releaseLock(file: string, pid: number): void {
  if (readJson<Lock>(file)?.pid !== pid) return;
  try {
    fs.unlinkSync(file);
  } catch {
    // Already gone.
  }
}

export function touchLock(file: string, pid: number, now: number): void {
  const held = readJson<Lock>(file);
  if (held?.pid !== pid) return;
  writeAtomic(file, lockBody(pid, held.started, now));
}
