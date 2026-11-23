// Daily usage counts: how often Carrick's hooks ran on this machine, sent to Carrick (carrick#5083).
//
// Built from the hook-timings file (`timings.ts`) and nothing else, through an
// allowlist: a body is assembled field by field from counts, never by copying
// a row, so a field added to a timing row later cannot leave the machine by
// accident. What is sent: per hook and per UTC day, the runs, how many printed
// text, stayed silent or failed, the p50/p90 duration and the p50 wait, beside
// this machine's random install id, a random per-folder id and the CLI
// version. Never sent: file names or paths, code, repo, service or branch
// names, host or user names, session ids, environment values. The exact body
// of the last send is kept in `<root>/.carrick/usage-last.json`.
//
// The send runs in a detached child (`usage-run.ts`) started from the session
// start, so a hook never waits on it and a failed send costs nothing. A day is
// kept for a later try on every failure that could succeed later. Wire shape:
// carrick-cloud `docs/internal/reference/usage-wire.md`.

import fs from "node:fs";
import os from "node:os";
import path from "node:path";
import { randomUUID } from "node:crypto";
import { API_BASE, readCredential } from "../auth/credentials.ts";
import { createLogger } from "../log.ts";
import { hasMarker } from "../root.ts";
import { currentVersion } from "../update.ts";
import { ensureInstallId, INSTALL_ID_PATTERN, readInstallId } from "../init/install-id.ts";
import { claimedAt, claimRefresh, markerPath, scriptBeside, startDetached } from "./refresh.ts";
import { writeAtomic } from "./reuse.ts";
import { MAX_LINES, percentile, readTimings } from "./timings.ts";
import type { Outcome, TimedHook, TimingRecord } from "./timings.ts";

const log = createLogger("carrick-usage");

/** The hooks a day entry may name: the closed `TimedHook` set. */
export const USAGE_HOOKS: readonly TimedHook[] = [
  "session-start",
  "post-read",
  "post-edit",
  "post-bash",
  "recheck",
  "held",
  "prompt-submit",
  "stop",
  "subagent-stop",
];

export type HookUsage = {
  runs: number;
  printed: number;
  silent: number;
  error: number;
  p50_ms: number;
  p90_ms: number;
  waited_p50_ms: number;
};

export type DayUsage = {
  /** UTC day, `YYYY-MM-DD`. */
  day: string;
  /** False when the timings file was trimmed inside this day, so early runs are missing. */
  complete: boolean;
  hooks: Partial<Record<TimedHook, HookUsage>>;
};

export type UsageBody = {
  v: 1;
  install_id: string;
  folder_id: string;
  cli_version: string;
  days: DayUsage[];
};

/** The sentence a person is told once, by `carrick init` or the first session start. */
export const USAGE_NOTICE =
  "Carrick counts how often its hooks run on this machine (counts only: no code, file names or paths) and sends the daily totals to Carrick. Turn it off with `carrick telemetry off`.";

/** At most one send per folder in this time. */
export const USAGE_COOLDOWN_MS = 60 * 60_000;
const USAGE_MARKER = "usage-send";
const FINAL_DAY_MARKER = "last-usage-final-day";
const DAY_MS = 24 * 60 * 60_000;
const SEND_TIMEOUT_MS = 5_000;
const VERSION_PATTERN = /^\d+\.\d+\.\d+([-.][0-9A-Za-z.]+)?$/;

// --- The machine's setting -------------------------------------------------

export type TelemetryState = { state: "on" | "off"; noticed_at: string | null };

/** `~/.carrick/telemetry`, beside the install id. */
export function telemetryPath(home: string = os.homedir()): string {
  return path.join(home, ".carrick", "telemetry");
}

/** The saved setting; `on` and never told when there is no readable file. */
export function readTelemetry(home?: string): TelemetryState {
  try {
    const value = JSON.parse(fs.readFileSync(telemetryPath(home), "utf8")) as { state?: unknown; noticed_at?: unknown };
    const noticed = typeof value.noticed_at === "string" && Number.isFinite(Date.parse(value.noticed_at)) ? value.noticed_at : null;
    return { state: value.state === "off" ? "off" : "on", noticed_at: noticed };
  } catch {
    return { state: "on", noticed_at: null };
  }
}

/** Save the setting. Throws when the home directory will not take a file. */
export function writeTelemetry(next: TelemetryState, home?: string): void {
  const file = telemetryPath(home);
  fs.mkdirSync(path.dirname(file), { recursive: true, mode: 0o700 });
  writeAtomic(file, `${JSON.stringify(next)}\n`);
}

/** Record that this person has been told, keeping an earlier time. Never throws. */
export function markNoticed(now: number = Date.now(), home?: string): void {
  try {
    const held = readTelemetry(home);
    if (held.noticed_at !== null) return;
    writeTelemetry({ state: held.state, noticed_at: new Date(now).toISOString() }, home);
  } catch {
    // Not told, then: the notice is said again next time.
  }
}

export type GateReason = "ok" | "env_off" | "ci" | "off" | "never_noticed";

/** Which environment switch is in force, or null. */
export function envSwitch(env: NodeJS.ProcessEnv): "CARRICK_TELEMETRY" | "DO_NOT_TRACK" | null {
  if (env["CARRICK_TELEMETRY"] === "0") return "CARRICK_TELEMETRY";
  const dnt = env["DO_NOT_TRACK"];
  if (dnt === "1" || dnt === "true") return "DO_NOT_TRACK";
  return null;
}

/**
 * Whether anything may be sent from this machine, and from when it counts.
 * Reads state only: it creates no file.
 */
export function usageGate(
  env: NodeJS.ProcessEnv = process.env,
  home?: string,
): { send: boolean; reason: GateReason; since: number } {
  if (envSwitch(env) !== null) return { send: false, reason: "env_off", since: 0 };
  if (env["CI"]) return { send: false, reason: "ci", since: 0 };
  const held = readTelemetry(home);
  if (held.state === "off") return { send: false, reason: "off", since: 0 };
  if (held.noticed_at === null) return { send: false, reason: "never_noticed", since: 0 };
  return { send: true, reason: "ok", since: Date.parse(held.noticed_at) };
}

// --- The counts ------------------------------------------------------------

function dayOf(ms: number): string {
  return new Date(ms).toISOString().slice(0, 10);
}

function count(values: number[], p: number): number {
  return values.length === 0 ? 0 : Math.round(percentile(values, p));
}

/** The day's counts from `rows`, or null when no row of that day started at or after `since`. Allowlist-built. */
export function buildDayUsage(rows: TimingRecord[], day: string, since: number): DayUsage | null {
  const from = Date.parse(`${day}T00:00:00Z`);
  const to = from + DAY_MS;
  const known = new Set<string>(USAGE_HOOKS);
  const byHook = new Map<TimedHook, TimingRecord[]>();
  for (const row of rows) {
    const at = Date.parse(row.start);
    if (!Number.isFinite(at) || at < from || at >= to || at < since || !known.has(row.hook)) continue;
    byHook.set(row.hook, [...(byHook.get(row.hook) ?? []), row]);
  }
  if (byHook.size === 0) return null;
  const hooks: Partial<Record<TimedHook, HookUsage>> = {};
  for (const hook of USAGE_HOOKS) {
    const list = byHook.get(hook);
    if (!list) continue;
    const outcomes = (outcome: Outcome) => list.filter((row) => row.outcome === outcome).length;
    const waited = list.map((row) => (Number.isFinite(row.waited_ms) && row.waited_ms > 0 ? row.waited_ms : 0));
    const durations = list.map((row) => Math.max(0, row.duration_ms));
    hooks[hook] = {
      runs: list.length,
      printed: outcomes("printed"),
      silent: outcomes("silent"),
      error: outcomes("error"),
      p50_ms: count(durations, 50),
      p90_ms: count(durations, 90),
      waited_p50_ms: count(waited, 50),
    };
  }
  // A full file has dropped its oldest lines: a day that begins after midnight in it may have lost runs.
  const first = rows.length > 0 ? Date.parse(rows[0]!.start) : Number.NaN;
  const trimmed = rows.length >= MAX_LINES && Number.isFinite(first) && first > from;
  return { day, complete: !trimmed, hooks };
}

// --- The send --------------------------------------------------------------

type Answer = { status: number };
export type UsageRequest = (url: string, init: { method: string; headers: Record<string, string>; body: string; redirect: "error"; signal: AbortSignal }) => Promise<Answer>;

export type SendOptions = {
  env?: NodeJS.ProcessEnv;
  now?: number;
  home?: string;
  /** Injected in tests. */
  request?: UsageRequest;
  /** Injected in tests: replaces the CLI's own version. */
  version?: string | null;
  timeoutMs?: number;
};

export function lastBodyPath(root: string): string {
  return path.join(root, ".carrick", "usage-last.json");
}

function folderIdPath(root: string): string {
  return path.join(root, ".carrick", "usage-folder-id");
}

/** This folder's random id, made on first use. Never a path or a name. */
function ensureFolderId(root: string): string {
  const file = folderIdPath(root);
  const read = (): string | null => {
    try {
      const id = fs.readFileSync(file, "utf8").trim();
      return INSTALL_ID_PATTERN.test(id) ? id : null;
    } catch {
      return null;
    }
  };
  const held = read();
  if (held !== null) return held;
  const id = randomUUID();
  fs.mkdirSync(path.dirname(file), { recursive: true });
  try {
    fs.writeFileSync(file, `${id}\n`, { flag: "wx" });
  } catch (error) {
    if ((error as NodeJS.ErrnoException).code !== "EEXIST") throw error;
    const written = read();
    if (written !== null) return written;
    fs.writeFileSync(file, `${id}\n`);
  }
  return id;
}

function readFinalDay(root: string): string {
  try {
    return fs.readFileSync(markerPath(root, FINAL_DAY_MARKER), "utf8").trim();
  } catch {
    return "";
  }
}

export type SendResult = "sent" | "kept" | "none";

/**
 * Send this folder's counts once. "sent": the cloud took them (or refused the
 * body for good). "kept": a failure that may pass, so nothing is marked and a
 * later send carries the day again. "none": nothing to send or not allowed.
 * Never throws.
 */
export async function sendUsage(root: string, options: SendOptions = {}): Promise<SendResult> {
  try {
    const env = options.env ?? process.env;
    const gate = usageGate(env, options.home);
    if (!gate.send) return "none";
    const credential = readCredential(env);
    if (!credential) return "none";
    const version = options.version === undefined ? currentVersion() : options.version;
    if (!version || !VERSION_PATTERN.test(version)) return "none";
    const now = options.now ?? Date.now();
    const today = dayOf(now);
    const yesterday = dayOf(now - DAY_MS);
    const rows = readTimings(root);
    const final = readFinalDay(root);
    const days: DayUsage[] = [];
    const earlier = final < yesterday ? buildDayUsage(rows, yesterday, gate.since) : null;
    if (earlier) days.push(earlier);
    const latest = buildDayUsage(rows, today, gate.since);
    if (latest) days.push(latest);
    if (days.length === 0) return "none";
    const body: UsageBody = {
      v: 1,
      install_id: readInstallId(options.home) ?? ensureInstallId(options.home),
      folder_id: ensureFolderId(root),
      cli_version: version,
      days,
    };
    // Claimed only now that there is something to send, so a child with no
    // counted rows does not use up the hour for the next hook's child.
    if (!claimRefresh(markerPath(root, USAGE_MARKER), now, USAGE_COOLDOWN_MS)) return "none";
    const text = JSON.stringify(body);
    fs.mkdirSync(path.dirname(lastBodyPath(root)), { recursive: true });
    writeAtomic(lastBodyPath(root), `${JSON.stringify(body, null, 2)}\n`);
    const request: UsageRequest = options.request ?? ((url, init) => fetch(url, init));
    let status: number;
    try {
      ({ status } = await request(`${API_BASE}/mcp/usage`, {
        method: "POST",
        redirect: "error",
        signal: AbortSignal.timeout(options.timeoutMs ?? SEND_TIMEOUT_MS),
        headers: { Authorization: `Bearer ${credential.token}`, "Content-Type": "application/json" },
        body: text,
      }));
    } catch (error) {
      log("usage not sent", String(error));
      return "kept";
    }
    // A 400 will not improve on a retry; every other refusal may pass.
    if ((status >= 200 && status < 300) || status === 400) {
      if (earlier) writeAtomic(markerPath(root, FINAL_DAY_MARKER), `${yesterday}\n`);
      return "sent";
    }
    log(`usage kept: HTTP ${status}`);
    return "kept";
  } catch (error) {
    log("usage not sent", String(error));
    return "kept";
  }
}

export type StartOptions = {
  env?: NodeJS.ProcessEnv;
  now?: number;
  home?: string;
  start?: (command: string, args: string[], env: NodeJS.ProcessEnv) => void;
};

/**
 * Start the detached send for this folder unless one went out within the hour:
 * one read of the marker, so the session start, Stop and prompt-submit hooks can
 * all call it. The child claims the hour (`sendUsage`). True when it was started.
 * Never throws, never waits.
 */
export function startUsageSend(root: string, options: StartOptions = {}): boolean {
  const env = options.env ?? process.env;
  try {
    if (!hasMarker(root)) return false;
    if (!usageGate(env, options.home).send) return false;
    const held = claimedAt(markerPath(root, USAGE_MARKER));
    if (held !== null && (options.now ?? Date.now()) - held < USAGE_COOLDOWN_MS) return false;
    const { command, args } = scriptBeside(import.meta.url, "usage-run", root);
    (options.start ?? startDetached)(command, args, { ...env, CARRICK_NO_KEEP_AWAKE: "1" });
    return true;
  } catch (error) {
    log("usage send not started", String(error));
    return false;
  }
}
