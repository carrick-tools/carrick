// The machine-wide registry of held compilers (D3). Each engine a daemon holds
// has one file under `~/.carrick/held/engines/`, so a second daemon, in another
// checkout, counts the memory the first already holds before it starts one.

import { createHash } from "node:crypto";
import fs from "node:fs";
import path from "node:path";
import { alive, readJson } from "../hook/lock.ts";
import { writeAtomic } from "../hook/reuse.ts";
import { heldDir } from "./paths.ts";

/** What a TypeScript 7 engine was measured to settle at, and so what a new one is counted as. */
export const ADMISSION_MB = 2048;
/** 1.5 x the plateau: an engine above this is restarted. */
export const RSS_CEILING_MB = 3072;
/** An engine idle this long may be closed to make room for another. */
export const EVICT_IDLE_MS = 2 * 60_000;

const DEFAULT_BUDGET_MB = 4096;

export type EngineEntry = {
  /** The daemon that owns the engine: the entry is live while this process is. */
  pid: number;
  root: string;
  service: string;
  rss_mb: number;
  last_used: number;
};

export function memoryBudgetMb(env: NodeJS.ProcessEnv = process.env): number {
  const parsed = Number.parseInt(env["CARRICK_HELD_MEMORY_MB"] ?? "", 10);
  return Number.isFinite(parsed) && parsed > 0 ? parsed : DEFAULT_BUDGET_MB;
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

/** Whether one more engine, counted at its plateau, keeps the live total within the budget. */
export function admits(entries: readonly EngineEntry[], budgetMb: number): boolean {
  return entries.reduce((sum, e) => sum + e.rss_mb, 0) + ADMISSION_MB <= budgetMb;
}
