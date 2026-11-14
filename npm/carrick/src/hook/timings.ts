// How long each hook held the agent, written down so a slow one is seen.
//
// Every hook invocation, and every background re-check the runner finishes, appends one line to `<root>/.carrick/hook-timings.jsonl`:
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
import { performance } from "node:perf_hooks";
import { initialisedRoot } from "../init/outdated.ts";
import { writeAtomic } from "./reuse.ts";

export type TimedHook =
  | "session-start"
  | "post-read"
  | "post-edit"
  | "post-bash"
  | "recheck"
  | "prompt-submit"
  | "stop"
  | "subagent-stop";

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
  /** Post-edit and post-bash: the edited file, relative to the workspace. */
  file?: string;
  /** Post-edit and post-read: time spent opening the fact file and matching (`facts/lines.ts`), inside `duration_ms`. */
  facts_ms?: number;
  /**
   * Post-edit only, and only when true: the probe's uses-count line was
   * printed because no fact file covered the edited file (counts the
   * fallback carrick#3966 retires).
   */
  uses_fallback?: true;
  /**
   * Post-edit and re-check rows, when the answer they read says: whether the
   * compiler engine was ready for the re-scan (`recheck.engine_ready`), so the
   * rate of re-checks that missed it can be read later (carrick#3948).
   */
  engine_ready?: boolean;
};

/** The file keeps this many of its last lines. */
export const MAX_LINES = 2000;
/** The window the status line and the session-start warning read. */
export const WINDOW_MS = 7 * 24 * 60 * 60_000;
/** A hook whose p90 is above this is named at session start. */
export const SLOW_HOOK_MS = 1000;

/** The hooks whose own duration `carrick status` and the session start report; a `recheck` row runs in the background, so the agent never waits on it. */
const ORDER: TimedHook[] = ["session-start", "post-read", "post-edit", "post-bash", "prompt-submit", "stop", "subagent-stop"];

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
  /** Time spent on fact lines, when the hook read the fact file. */
  factsMs?: number;
  /** The uses-count line was printed because no fact file covered the file. */
  usesFallback?: boolean;
  /** Whether the compiler engine was ready for the re-scan the answer came from, when it says. */
  engineReady?: boolean;
};

/** `recheck.engine_ready` of an answer, or undefined where no re-scan ran. */
export function engineReadyOf(result: { recheck?: { engine_ready?: boolean } } | null | undefined): boolean | undefined {
  const ready = result?.recheck?.engine_ready;
  return typeof ready === "boolean" ? ready : undefined;
}

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
 * Whether what a hook wrote gave the agent text: any text that is not an empty
 * `additionalContext` emission or whitespace.
 */
export function gaveText(output: string): boolean {
  const text = output.trim();
  if (!text) return false;
  try {
    const context = (JSON.parse(text) as { hookSpecificOutput?: { additionalContext?: unknown } })
      ?.hookSpecificOutput?.additionalContext;
    if (typeof context === "string") return context.trim().length > 0;
  } catch {
    // Not JSON: plain text, which is text.
  }
  return true;
}

/**
 * Run one hook's `main` and write its timing line, whether it printed, stayed
 * silent or threw (the error is rethrown). What it printed is read off stdout,
 * so no hook has to report it. The clock starts at `origin`, the process start
 * by default, so Node's start-up and module loading count: the agent waits on
 * them too.
 */
export async function timed(
  hook: TimedHook,
  run: (timing: Timing) => Promise<void>,
  origin: number = performance.timeOrigin,
): Promise<void> {
  const timing: Timing = { root: null, file: null, waitedMs: 0 };
  let output = "";
  const write = process.stdout.write;
  process.stdout.write = ((...args: Parameters<typeof write>) => {
    const chunk = args[0];
    output += typeof chunk === "string" ? chunk : Buffer.from(chunk).toString("utf8");
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
          start: new Date(origin).toISOString(),
          duration_ms: Math.max(0, Math.round(Date.now() - origin)),
          waited_ms: Math.round(timing.waitedMs),
          outcome: failed ? "error" : gaveText(output) ? "printed" : "silent",
        };
        if (timing.factsMs !== undefined) record.facts_ms = timing.factsMs;
        if (timing.usesFallback) record.uses_fallback = true;
        if (timing.engineReady !== undefined) record.engine_ready = timing.engineReady;
        if (timing.file) record.file = path.isAbsolute(timing.file) ? path.relative(root, timing.file) : timing.file;
        appendTiming(root, record);
      }
    } catch {
      // Dropped.
    }
  }
}
