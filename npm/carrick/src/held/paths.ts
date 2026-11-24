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

/**
 * The checkout a held engine's files are named by: a linked worktree's top when the service's init names
 * one (`checkout`), else the daemon's own checkout. One answer for every per-engine key and file (slot key,
 * shadow folder, placed file, state file, registry entry), so a worktree's engine never shares or deletes
 * the main checkout's. A main-checkout init has no `checkout`: its keys and files are unchanged.
 */
export function heldRootOf(init: { checkout?: string }, root: string): string {
  return init.checkout ?? root;
}

export function socketPath(root: string, home?: string): string {
  return path.join(heldDir(home), `${rootKey(root)}.sock`);
}

export function lockPath(root: string, home?: string): string {
  return path.join(heldDir(home), `${rootKey(root)}.lock`);
}

/** `{ pid, build_id }`, written once the daemon listens: a busy daemon of this build is told from another by it. */
export function helloPath(root: string, home?: string): string {
  return path.join(heldDir(home), `${rootKey(root)}.hello`);
}

/** A service name as one safe path segment (a hash keeps two names that sanitise alike apart). */
function serviceSegment(service: string): string {
  const hash = createHash("sha256").update(service).digest("hex").slice(0, 6);
  return `${service.replace(/[^A-Za-z0-9._-]/g, "_")}-${hash}`;
}

/** D4: the service's shadow tree, `w/` (its own files) and `v/` (the vendored program). */
export function shadowDir(root: string, service: string, home?: string): string {
  return path.join(heldDir(home), rootKey(root), serviceSegment(service));
}

/** The rows the gate armed and the rows it saved with a reason. */
export function placedPath(root: string, service: string, home?: string): string {
  return path.join(heldDir(home), rootKey(root), `${serviceSegment(service)}.placed.json`);
}

/** What the daemon knows of the service's engine: starting, ready, capped, no compiler, failed (`state.ts`). */
export function statePath(root: string, service: string, home?: string): string {
  return path.join(heldDir(home), rootKey(root), `${serviceSegment(service)}.state.json`);
}

/** The folder holding every service's placed and state files for one checkout. */
export function serviceFilesDir(root: string, home?: string): string {
  return path.join(heldDir(home), rootKey(root));
}

/** One JSON line per request. */
export function logPath(root: string, home?: string): string {
  return path.join(heldDir(home), `${rootKey(root)}.log`);
}

/** One `<unix seconds> <build id>` line per daemon crash (the back-off reads it). */
export function crashesPath(root: string, home?: string): string {
  return path.join(heldDir(home), `${rootKey(root)}.crashes`);
}
