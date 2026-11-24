// What `carrick doctor` and the edit hook say about a service the compiler
// engine did not check (T7, #4654). The daemon writes one small record per
// service beside its placed file (`paths.ts`) every time the engine's state
// changes; the doctor reads it, and an answer that did not check an edit
// carries the same facts as a `NotChecked` note for the hook to say once.

import fs from "node:fs";
import path from "node:path";
import { readJson } from "../hook/lock.ts";
import { writeAtomic } from "../hook/reuse.ts";
import { statePath } from "./paths.ts";

export type ServiceState = {
  at: string;
  service: string;
  /** The repo the service belongs to (`init.repo`); the doctor files the line under it. */
  repo?: string;
  state: "starting" | "ready" | "capped" | "no_compiler" | "failed";
  /** Why a start failed, one line. */
  reason?: string;
  /** `capped`: the machine-wide budget and how many other engines hold it. */
  budget_mb?: number;
  holders?: number;
  /** `ready`: how long the start took. `starting`: how long the previous one took, when there was one. */
  start_ms?: number;
  last_start_ms?: number;
  /** The engine's size in MB when its first gate finished; a new engine of this service is counted at it. */
  settled_mb?: number;
};

/** One edit the engine did not check, for the hook's line. `state` is the answer's state. */
export type NotChecked = {
  service: string;
  /** `busy`: no daemon took the ask (one is starting, backed off, or closed mid-ask), T5. */
  state: "cold" | "capped" | "no_compiler" | "error" | "worktree" | "busy";
  reason?: string;
  /** `worktree`: the top folder of the linked worktree the edited file sits in. */
  checkout?: string;
  budget_mb?: number;
  holders?: number;
  took_s?: number;
};

export function writeState(root: string, service: string, record: ServiceState, home?: string): void {
  try {
    fs.mkdirSync(path.dirname(statePath(root, service, home)), { recursive: true, mode: 0o700 });
    writeAtomic(statePath(root, service, home), `${JSON.stringify(record)}\n`);
  } catch {
    // A record nobody can write is not a reason to fail an answer.
  }
}

export function readState(root: string, service: string, home?: string): ServiceState | null {
  return readJson<ServiceState>(statePath(root, service, home));
}
