// How long each hook held the agent, written down so a slow one is seen.
//
// Every hook invocation appends one line to `<root>/.carrick/hook-timings.jsonl`:
// which hook, when it started, how long it ran, how much of that it spent
// blocking on a check, and what it did. No code, no file contents, no path
// beyond the workspace-relative edited file. The file keeps its last
// `MAX_LINES` lines. `carrick status` prints the p50/p90 per hook from it, and
// the session start names a hook whose p90 over the last 7 days is above
// `SLOW_HOOK_MS`.
//
// A write that fails is dropped: a hook never fails because its own timing
// could not be kept.

import fs from "node:fs";
import path from "node:path";
import { initialisedRoot } from "../init/outdated.ts";
import { writeAtomic } from "./reuse.ts";

export type TimedHook = "session-start" | "post-read" | "post-edit" | "prompt-submit" | "stop";

export type Outcome = "printed" | "silent" | "error";

/** One line of the file. */
export type TimingRecord = {
  hook: TimedHook;
  /** ISO time the hook started. */
  start: string;
  duration_ms: number;
  /** Time spent blocking on a check or re-check, inside `duration_ms`. */
  waited_ms: number;
  outcome: Outcome;
  /** Post-edit only: the edited file, relative to the workspace. */
  file?: string;
};

/** The file keeps this many of its last lines. */
export const MAX_LINES = 2000;
/** The window the status line and the session-start warning read. */
export const WINDOW_MS = 7 * 24 * 60 * 60_000;
/** A hook whose p90 is above this is named at session start. */
export const SLOW_HOOK_MS = 1000;

const ORDER: TimedHook[] = ["session-start", "post-read", "post-edit", "prompt-submit", "stop"];

export function timingsPath(root: string): string {
  return path.join(root, ".carrick", "hook-timings.jsonl");
}

/** What a hook fills in while it runs. */
export type Timing = {
  /** Where to write; found from the environment when the hook leaves it null. */
  root: string | null;
  /** Post-edit: the edited file, absolute or relative to `root`. */
  file: string | null;
  waitedMs: number;
};

/** Append `record` to the file under `root` and trim it to `MAX_LINES`. Never throws. */
export function appendTiming(root: string, record: TimingRecord): void {
  const file = timingsPath(root);
  try {
    fs.mkdirSync(path.dirname(file), { recursive: true });
    fs.appendFileSync(file, `${JSON.stringify(record)}\n`);
    const lines = fs.readFileSync(file, "utf8").split("\n").filter(Boolean);
    if (lines.length > MAX_LINES) writeAtomic(file, `${lines.slice(-MAX_LINES).join("\n")}\n`);
  } catch {
    // Dropped, as above.
  }
}

/** The records in the file, oldest first; unreadable lines are skipped. */
export function readTimings(root: string): TimingRecord[] {
  let text: string;
  try {
    text = fs.readFileSync(timingsPath(root), "utf8");
  } catch {
    return [];
  }
  const out: TimingRecord[] = [];
  for (const line of text.split("\n")) {
    if (!line) continue;
    try {
      const row = JSON.parse(line) as Partial<TimingRecord>;
      if (typeof row.hook === "string" && typeof row.duration_ms === "number" && typeof row.start === "string") {
        out.push(row as TimingRecord);
      }
    } catch {
      // A torn line.
    }
  }
  return out;
}

/** Nearest-rank percentile of `values` (not empty). */
export function percentile(values: number[], p: number): number {
  const sorted = [...values].sort((a, b) => a - b);
  return sorted[Math.max(0, Math.ceil((p / 100) * sorted.length) - 1)]!;
}

export type HookSpeed = { hook: TimedHook; runs: number; p50: number; p90: number };

/** Each hook's own duration over the window, for the hooks that ran in it. */
export function hookSpeeds(root: string, now: number = Date.now()): HookSpeed[] {
  const byHook = new Map<TimedHook, number[]>();
  for (const row of readTimings(root)) {
    const at = Date.parse(row.start);
    if (!Number.isFinite(at) || now - at > WINDOW_MS) continue;
    byHook.set(row.hook, [...(byHook.get(row.hook) ?? []), row.duration_ms]);
  }
  return ORDER.filter((hook) => byHook.has(hook)).map((hook) => {
    const values = byHook.get(hook)!;
    return { hook, runs: values.length, p50: percentile(values, 50), p90: percentile(values, 90) };
  });
}

function seconds(ms: number): string {
  return (ms / 1000).toFixed(1);
}

/** The `carrick status` line, or null where no hook has run in the window. */
export function statusTimingsLine(from: string, now: number = Date.now()): string | null {
  const root = initialisedRoot(from);
  if (!root) return null;
  const speeds = hookSpeeds(root, now);
  if (speeds.length === 0) return null;
  const each = speeds.map((s) => `${s.hook} ${seconds(s.p50)}/${seconds(s.p90)}`);
  return `Hook run time, last 7 days (p50/p90, seconds): ${each.join(", ")}`;
}

/** Session-start lines: one per hook whose p90 is above `SLOW_HOOK_MS`. Empty when none is. */
export function slowHookLines(root: string, now: number = Date.now()): string[] {
  return hookSpeeds(root, now)
    .filter((s) => s.p90 > SLOW_HOOK_MS)
    .map((s) => `Carrick's ${s.hook} hook has taken ${seconds(s.p90)} s (p90) in recent turns; run \`carrick status\`.`);
}

/**
 * Run one hook's `main` and write its timing line, whether it printed, stayed
 * silent or threw (the error is rethrown). What it printed is read off stdout,
 * so no hook has to report it.
 */
export async function timed(hook: TimedHook, run: (timing: Timing) => Promise<void>): Promise<void> {
  const started = Date.now();
  const timing: Timing = { root: null, file: null, waitedMs: 0 };
  let printed = false;
  const write = process.stdout.write;
  process.stdout.write = ((...args: Parameters<typeof write>) => {
    printed = true;
    return write.apply(process.stdout, args);
  }) as typeof write;
  let failed = false;
  try {
    await run(timing);
  } catch (error) {
    failed = true;
    throw error;
  } finally {
    process.stdout.write = write;
    try {
      const root = timing.root ?? initialisedRoot(process.env["CLAUDE_PROJECT_DIR"] ?? process.cwd());
      if (root) {
        const record: TimingRecord = {
          hook,
          start: new Date(started).toISOString(),
          duration_ms: Date.now() - started,
          waited_ms: Math.round(timing.waitedMs),
          outcome: failed ? "error" : printed ? "printed" : "silent",
        };
        if (timing.file) record.file = path.isAbsolute(timing.file) ? path.relative(root, timing.file) : timing.file;
        appendTiming(root, record);
      }
    } catch {
      // Dropped.
    }
  }
}
