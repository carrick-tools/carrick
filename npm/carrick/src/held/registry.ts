// The machine-wide registry of held compilers (D3). Each engine a daemon holds
// has one file under `~/.carrick/held/engines/`, so a second daemon, in another
// checkout, counts the memory the first already holds before it starts one.
//
// Settings (environment, read when a request arrives, so a developer changes them
// without restarting anything but their session):
// - `CARRICK_HELD_MEMORY_MB`: the machine-wide memory budget in MB. Default: a
//   quarter of the machine's RAM, at least 2,048 and at most 8,192.
// - `CARRICK_HELD_WORKTREES`: `off`, `0` or `false` stops the daemon starting a
//   compiler slot for a linked git worktree; a worktree file is then answered
//   `worktree`, as it was before worktree slots. Default: on.
// - `CARRICK_HELD_WORKTREE_SLOTS`: the most worktree slots live on the machine
//   at once (a positive integer). Default: 3. It holds beside the memory budget.

import { createHash } from "node:crypto";
import fs from "node:fs";
import os from "node:os";
import path from "node:path";
import { alive, readJson } from "../hook/lock.ts";
import { linkedCheckoutTop } from "../hook/worktree.ts";
import { writeAtomic } from "../hook/reuse.ts";
import { heldDir } from "./paths.ts";

/** What a TypeScript 7 engine was measured to settle at, and so what a new one is counted as. */
export const ADMISSION_MB = 2048;
/** The least an engine may grow to before it is restarted. */
export const RSS_CEILING_MB = 3072;
/** An engine is restarted above this many times the size it settled at (when that is above the floor). */
const CEILING_FACTOR = 1.5;
/** An engine idle this long may be closed to make room for another. */
export const EVICT_IDLE_MS = 2 * 60_000;

/** The most compiler slots linked worktrees may hold at once on one machine (`CARRICK_HELD_WORKTREE_SLOTS` overrides). */
export const WORKTREE_SLOTS_MAX = 3;

/** The budget is a quarter of the machine's memory, within these bounds (D8). */
const BUDGET_SHARE = 0.25;
const MIN_BUDGET_MB = 2048;
const MAX_BUDGET_MB = 8192;

export type EngineEntry = {
  /** The daemon that owns the engine: the entry is live while this process is. */
  pid: number;
  root: string;
  service: string;
  rss_mb: number;
  last_used: number;
};

/** `CARRICK_HELD_MEMORY_MB` when set, else 25% of the machine's RAM (`totalBytes`), at least 2,048 and at most 8,192 MB. */
export function memoryBudgetMb(env: NodeJS.ProcessEnv = process.env, totalBytes: number = os.totalmem()): number {
  const parsed = Number.parseInt(env["CARRICK_HELD_MEMORY_MB"] ?? "", 10);
  if (Number.isFinite(parsed) && parsed > 0) return parsed;
  const share = Math.floor((totalBytes / (1024 * 1024)) * BUDGET_SHARE);
  return Math.min(MAX_BUDGET_MB, Math.max(MIN_BUDGET_MB, share));
}

/** The size above which an engine is closed: 1.5 x the size its first gate settled at, never under the floor. */
export function closeCeilingMb(settledMb: number | undefined): number {
  return settledMb === undefined ? RSS_CEILING_MB : Math.max(RSS_CEILING_MB, Math.round(settledMb * CEILING_FACTOR));
}

export function registryDir(home?: string): string {
  return path.join(heldDir(home), "engines");
}

function entryFile(home: string | undefined, pid: number, root: string, service: string): string {
  const key = createHash("sha256").update(`${root}\0${service}`).digest("hex").slice(0, 8);
  return path.join(registryDir(home), `${pid}-${key}.json`);
}

export function writeEntry(home: string | undefined, entry: EngineEntry): void {
  writeAtomic(entryFile(home, entry.pid, entry.root, entry.service), `${JSON.stringify(entry)}\n`);
}

export function removeEntry(home: string | undefined, pid: number, root: string, service: string): void {
  try {
    fs.unlinkSync(entryFile(home, pid, root, service));
  } catch {
    // Already gone.
  }
}

/** Every entry whose owner is alive. An entry whose owner is gone is deleted here. */
export function liveEntries(home?: string): EngineEntry[] {
  const dir = registryDir(home);
  let names: string[];
  try {
    names = fs.readdirSync(dir);
  } catch {
    return [];
  }
  const out: EngineEntry[] = [];
  for (const name of names) {
    if (!name.endsWith(".json")) continue;
    const file = path.join(dir, name);
    const entry = readJson<EngineEntry>(file);
    if (entry === null || typeof entry.pid !== "number" || !alive(entry.pid)) {
      try {
        fs.unlinkSync(file);
      } catch {
        // Another process removed it first.
      }
      continue;
    }
    out.push(entry);
  }
  return out;
}

/** Whether one more engine, counted at `newMb` (its last settled size, else the plateau), keeps the live total within the budget. */
export function admits(entries: readonly EngineEntry[], budgetMb: number, newMb: number = ADMISSION_MB): boolean {
  return entries.reduce((sum, e) => sum + e.rss_mb, 0) + newMb <= budgetMb;
}

/** False when `CARRICK_HELD_WORKTREES` is `off`, `0` or `false` (any case): no worktree slot is started. */
export function worktreesEnabled(env: NodeJS.ProcessEnv = process.env): boolean {
  const value = (env["CARRICK_HELD_WORKTREES"] ?? "").trim().toLowerCase();
  return !(value === "off" || value === "0" || value === "false");
}

/** `CARRICK_HELD_WORKTREE_SLOTS` when it is a positive integer, else `WORKTREE_SLOTS_MAX`. */
export function worktreeSlotsMax(env: NodeJS.ProcessEnv = process.env): number {
  const text = (env["CARRICK_HELD_WORKTREE_SLOTS"] ?? "").trim();
  const parsed = /^\d+$/.test(text) ? Number.parseInt(text, 10) : Number.NaN;
  return parsed > 0 ? parsed : WORKTREE_SLOTS_MAX;
}

/** Whether an entry's root is a linked git worktree. */
export function isWorktreeEntry(entry: EngineEntry): boolean {
  return linkedCheckoutTop(path.join(entry.root, "x")) !== null;
}
