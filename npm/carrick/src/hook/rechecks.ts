// The background re-check queue and its runner (carrick#3198, B3).
//
// A hook may not wait for a re-check that costs a scan of the edited file's
// repo. The post-edit hook queues the file instead, makes sure one runner is
// alive for the workspace, and a later carrier (the next hook, or Stop) picks
// up the answer. This file is the queue, the runner's loop and the per-session
// answer files; the hooks that queue and deliver are separate.
//
// Two places hold state:
//
// - The workspace, `<root>/.carrick/rechecks/`: `queue/<seq>.json` (one file per
//   edit, created exclusively because Claude Code runs tool hooks in parallel),
//   `taken/<pid>/<seq>.json` (what the runner has claimed, by rename) and
//   `runner.lock`. Not `.carrick/working/`: every new index deletes that.
// - The session, flat files beside its record in `~/.carrick/sessions/`:
//   `<id>.recheck.<seq>.pending.json` (the hook), `.answer.json` (the runner),
//   `.delivered` and `.told-running` (each claimed exclusively by the hook that
//   prints the answer or says it is running). Flat, so `prune` and
//   `removeSessions` need no change.
//
// Nothing here throws at its caller: an edit must not fail on a state file.
// One runner per workspace, one `carrick check` at a time, in queue order.

import fs from "node:fs";
import path from "node:path";
import { binary, check, checkChanged, refresh } from "../cli.ts";
import type { ChangedResult, CheckResult } from "../contract.ts";
import { createLogger } from "../log.ts";
import {
  alive,
  createExclusive,
  lockHeld,
  readJson,
  releaseLock,
  STALE_SLACK_MS,
  takeLock,
  touchLock,
  type Lock,
} from "./lock.ts";
import { scriptBeside, startDetached } from "./refresh.ts";
import { sessionsDir, writeAtomic } from "./reuse.ts";
import { safeId } from "./session-key.ts";
import { appendTiming, engineReadyOf } from "./timings.ts";
import type { HeldLine } from "../held/daemon.ts";
import { earlyResult } from "../held/early.ts";
import { compareEarly, hasEarlyBreak, heldRecord } from "./confirm.ts";
import type { HeldRecord } from "./confirm.ts";

const log = createLogger("carrick-recheck");

/**
 * How long a background re-check may run, in ms: what the binary is given as
 * `CARRICK_RECHECK_BUDGET_MS`. 180 s until the route package's uncapped time
 * is measured (C0), then min(300 s, 2 x its slowest uncapped E2): change this
 * one constant.
 */
export const BACKGROUND_CAP_MS = 180_000;

/**
 * The Stop entry's timeout, in seconds. Stop never waits for a re-check
 * (`stop-wait.ts`): this only bounds its own reads of the session's files.
 */
export const STOP_TIMEOUT_S = 10;

/** The runner's own deadline for one call: the binary's budget ends it first. */
const CALL_SLACK_MS = 15_000;

/** A lock or an entry this far past 2 x cap is not waited for any longer. */
export function backgroundCapMs(env: NodeJS.ProcessEnv = process.env): number {
  const raw = env["CARRICK_RECHECK_BACKGROUND_MS"];
  const parsed = raw ? Number.parseInt(raw, 10) : Number.NaN;
  return Number.isFinite(parsed) && parsed > 0 ? parsed : BACKGROUND_CAP_MS;
}

/** Entries older than this are answered "stopped" without being run. */
export function entryExpiryMs(cap: number): number {
  return 2 * cap + STALE_SLACK_MS;
}

export type AnswerState = "answered" | "superseded" | "stopped";

/** One queued edit, as the queue and the session's pending file hold it. */
export type QueueEntry = {
  seq: string;
  session: string;
  /** Absolute path of the edited file. */
  file: string;
  /** The path `carrick check` is asked about: relative to `root`. */
  relative: string;
  root: string;
  enqueued_at: string;
  /** How many runners died holding this entry. */
  attempts: number;
  /**
   * `changed`: one `carrick check --changed` for the session's whole change
   * in `root` (`file` is `root`), queued by a Stop that could not state the
   * check from the per-file answers (carrick#3466). It runs after every
   * per-file entry and Stop never waits for it. Absent: one edited file.
   */
  kind?: "changed";
  /**
   * The files the Stop that queued a `changed` entry named as not yet checked
   * (workspace-relative). A clean answer is delivered only for a named set
   * (`late.ts`); absent on an entry no Stop named.
   */
  named?: string[];
  /**
   * The edit-time fact lines answered this file's callers, so its answer
   * leaves out the uses-count line (`facts/lines.ts` `callersRead`).
   */
  facts_callers?: true;
  /**
   * Queued in place of the edit-time probe (a workspace with a fact file,
   * T12-2): its answer is rendered as the probe's would have been, even one
   * that does not answer the edit (`late.ts` `lateContext`).
   */
  instead_of_probe?: true;
  /** The agent's folder when it edited, so a late list prints paths from it (`similar.ts`). */
  cwd?: string;
};

export type Answer = {
  seq: string;
  session: string;
  file: string;
  relative: string;
  root: string;
  state: AnswerState;
  enqueued_at: string;
  started_at: string | null;
  finished_at: string;
  kind?: "changed";
  /** The files Stop named when it queued a `changed` entry, carried from the entry. */
  named?: string[];
  /** Carried from the entry: the late answer leaves out the uses-count line. */
  facts_callers?: true;
  /** Carried from the entry: rendered as the edit-time probe's answer would have been. */
  instead_of_probe?: true;
  /** Carried from the entry. */
  cwd?: string;
  result?: CheckResult;
  /** What a `changed` entry's `check --changed` answered. */
  changed?: ChangedResult;
  failure?: string;
};

/** Whether an entry or answer is the session's whole-change check. */
export function isChanged(entry: { kind?: string }): boolean {
  return entry.kind === "changed";
}

/**
 * How long the background `check --changed` may run, in ms: the binary's own
 * default budget (`DEFAULT_BUDGET`, 300 s, in `src/local_mode/changed.rs`),
 * given to it as `CARRICK_CHECK_BUDGET_MS`. Longer than the per-file cap
 * because it re-reads every changed repo (measured 37-209 s, carrick#3466).
 */
export const CHANGED_BUDGET_MS = 300_000;

/** How often the runner touches its lock while one call runs. */
const LOCK_TOUCH_MS = 30_000;

export type Pending = QueueEntry;

type Options = { home?: string };
type ChangedOptions = Options & { named?: string[] };
/**
 * `factsCallers`: the files whose edit-time fact lines answered their callers.
 * `insteadOfProbe`: the entries stand in for the edit-time probe's verdict (the
 * probe still runs and its similar-function block prints now). Main session and
 * sub-agent alike.
 */
type EnqueueOptions = Options & { factsCallers?: ReadonlySet<string>; insteadOfProbe?: boolean; cwd?: string | null };

export function rechecksDir(root: string): string {
  return path.join(root, ".carrick", "rechecks");
}
export function queueDir(root: string): string {
  return path.join(rechecksDir(root), "queue");
}
export function takenDir(root: string, pid: number): string {
  return path.join(rechecksDir(root), "taken", String(pid));
}
export function lockPath(root: string): string {
  return path.join(rechecksDir(root), "runner.lock");
}
/**
 * The rebuilds a hook asks the runner for (`askJob`): `refresh` is
 * `carrick refresh`, `forward` is `carrick refresh --hosted-only`.
 */
export type Job = "refresh" | "forward";
/** Where a hook asks the runner for `job`. */
export function requestPath(root: string, job: Job): string {
  return path.join(rechecksDir(root), `${job}.request`);
}

/** `<id>.recheck.<seq>.<kind>` in the sessions directory, or null for an unusable id. */
function sessionPart(session: string, seq: string, kind: string, home?: string): string | null {
  if (safeId(session) === null) return null;
  return path.join(sessionsDir(home), `${session}.recheck.${seq}.${kind}`);
}

let counter = 0;

/** Zero-padded epoch ms + pid + counter: sorts by time, never repeats. */
function nextSeq(now: number): string {
  counter += 1;
  return `${String(now).padStart(13, "0")}-${process.pid}-${String(counter).padStart(4, "0")}`;
}


function names(dir: string): string[] {
  try {
    return fs.readdirSync(dir).sort();
  } catch {
    return [];
  }
}

/**
 * Queue one entry per file for a session. The session's pending file is
 * written first so that no entry in the queue is ever unknown to its session.
 * `files` are absolute paths; returns the entries queued, in order.
 */
export function enqueue(
  root: string,
  session: string,
  files: string[],
  now: number,
  options: EnqueueOptions = {},
): QueueEntry[] {
  const queued: QueueEntry[] = [];
  for (const file of files) {
    const seq = nextSeq(now);
    const entry: QueueEntry = {
      seq,
      session,
      file,
      relative: path.relative(root, file),
      root,
      enqueued_at: new Date(now).toISOString(),
      attempts: 0,
      ...(options.factsCallers?.has(file) ? { facts_callers: true as const } : {}),
      ...(options.insteadOfProbe ? { instead_of_probe: true as const } : {}),
      ...(options.cwd ? { cwd: options.cwd } : {}),
    };
    const pending = sessionPart(session, seq, "pending.json", options.home);
    if (pending === null) continue;
    const body = `${JSON.stringify(entry, null, 2)}\n`;
    if (!writeAtomic(pending, body)) continue;
    if (!createExclusive(path.join(queueDir(root), `${seq}.json`), body)) {
      try {
        fs.unlinkSync(pending);
      } catch {
        // Nothing to take back.
      }
      continue;
    }
    queued.push(entry);
  }
  return queued;
}

/**
 * Queue one `carrick check --changed` for this session's change in `root`,
 * behind every per-file entry (`nextEntry` runs it last). A newer one of the
 * same session supersedes an older one that has not started. Null when the
 * queue refused it.
 */
export function enqueueChanged(root: string, session: string, now: number, options: ChangedOptions = {}): QueueEntry | null {
  const seq = nextSeq(now);
  const entry: QueueEntry = {
    seq,
    session,
    file: root,
    relative: ".",
    root,
    enqueued_at: new Date(now).toISOString(),
    attempts: 0,
    kind: "changed",
    ...(options.named !== undefined && options.named.length > 0 ? { named: options.named } : {}),
  };
  const pending = sessionPart(session, seq, "pending.json", options.home);
  if (pending === null) return null;
  const body = `${JSON.stringify(entry, null, 2)}\n`;
  if (!writeAtomic(pending, body)) return null;
  if (!createExclusive(path.join(queueDir(root), `${seq}.json`), body)) {
    remove(pending);
    return null;
  }
  return entry;
}

/**
 * Record an answer the post-edit hook had in hand (no re-check queued) as the
 * session's answer for that edit, already delivered: the hook printed it. It
 * is what `sessionCoverage` (stop-wait.ts) reads for a file that was never
 * queued (carrick#3466); `pendingFor` and `finishedFor` skip it.
 */
export function answerInHand(
  root: string,
  session: string,
  file: string,
  result: CheckResult,
  now: number,
  options: Options = {},
): void {
  const seq = nextSeq(now);
  const at = new Date(now).toISOString();
  const entry: QueueEntry = { seq, session, file, relative: path.relative(root, file), root, enqueued_at: at, attempts: 0 };
  const pending = sessionPart(session, seq, "pending.json", options.home);
  const answer = sessionPart(session, seq, "answer.json", options.home);
  if (pending === null || answer === null) return;
  // Delivered, then answered, then pending: no carrier ever reads it as owed
  // to it, and no Stop ever waits for it.
  if (!claimDelivery(session, seq, options.home, now)) return;
  if (!writeAnswer(entry, "answered", now, now, { result }, options.home)) return;
  writeAtomic(pending, `${JSON.stringify(entry, null, 2)}\n`);
}

// --- the early answer (part 3, B1) ------------------------------------------
//
// The held compiler answers a per-file entry with `<seq>.early.json` beside it:
// never `answer.json` (which reads as "answered" to the coverage and the queue)
// and never a `.delivered` name (which reads as the seq delivered). Its claim
// is `<seq>.early.said`, holding the keys of the rows that were printed, and the
// differ record is written once per entry under `<seq>.early.recorded`.

/** One early answer: the entry it is for, when it landed, and its rows as a check answer (`held/early.ts`). */
export type EarlyAnswer = {
  seq: string;
  session: string;
  file: string;
  relative: string;
  root: string;
  enqueued_at: string;
  landed_at: string;
  /** How long the daemon took to answer. */
  held_ms: number;
  cwd?: string;
  /**
   * The service whose engine answered (`repo/service`): its early answer lists
   * every break its checks speak, so the newest one says which still stand
   * (T1 P1.4). Absent when the daemon named none.
   */
  service?: string;
  /**
   * The engine that answered (one id per engine start): its answer says which
   * of that engine's runs still stand, and nothing of an earlier engine's.
   */
  engine?: string;
  /** No items and no moved rows: the daemon answered and found no break. */
  result: CheckResult;
};

/** Whether a newer per-file entry of the same file was filed after `seq`: its answers are the ones to give. */
export function supersededEntry(session: string, seq: string, home?: string): boolean {
  const own = readJson<QueueEntry>(sessionPart(session, seq, "pending.json", home) ?? "");
  if (own === null) return false;
  for (const name of sessionNames(session, home).filter((n) => n.endsWith(".pending.json"))) {
    if (seqOf(session, name) <= seq) continue;
    const entry = readJson<QueueEntry>(path.join(sessionsDir(home), name));
    if (entry !== null && !isChanged(entry) && entry.file === own.file) return true;
  }
  return false;
}

/**
 * File the daemon's answer to the ask for entry `seq` as its early answer: for
 * every `ready` answer, with no rows when it found no break. Nothing is filed
 * for any other state, for an entry that is gone or is not a file's, or when a
 * newer entry of the same file was filed since. When the full answer is
 * already there, the differ record is written here. False when nothing was filed.
 */
export function answerEarly(
  session: string,
  seq: string,
  answer: { state: string; lines: readonly HeldLine[]; ms: number; service?: string; engine?: string },
  now: number,
  home?: string,
): boolean {
  if (answer.state !== "ready") return false;
  const entry = readJson<QueueEntry>(sessionPart(session, seq, "pending.json", home) ?? "");
  if (entry === null || isChanged(entry)) return false;
  if (supersededEntry(session, seq, home)) return false;
  const early: EarlyAnswer = {
    seq,
    session,
    file: entry.file,
    relative: entry.relative,
    root: entry.root,
    enqueued_at: entry.enqueued_at,
    landed_at: new Date(now).toISOString(),
    held_ms: answer.ms,
    ...(entry.cwd ? { cwd: entry.cwd } : {}),
    ...(answer.service === undefined ? {} : { service: answer.service }),
    ...(answer.engine === undefined ? {} : { engine: answer.engine }),
    result: earlyResult(entry, answer.lines),
  };
  const file = sessionPart(session, seq, "early.json", home);
  if (file === null || !writeAtomic(file, `${JSON.stringify(early, null, 2)}\n`)) return false;
  const full = readAnswer(session, seq, home);
  if (full !== null) {
    const record = claimRecord(session, seq, early, full.result, home);
    if (record !== null) {
      appendTiming(entry.root, {
        hook: "held",
        start: early.landed_at,
        duration_ms: answer.ms,
        waited_ms: 0,
        outcome: "silent",
        file: entry.relative,
        held_ms: answer.ms,
        ...record,
      });
    }
  }
  return true;
}

/** The full answer filed for entry `seq`, delivered or not, or null. */
export function readAnswer(session: string, seq: string, home?: string): Answer | null {
  return readJson<Answer>(sessionPart(session, seq, "answer.json", home) ?? "");
}

/** The early answer filed for entry `seq`, or null. */
export function readEarly(session: string, seq: string, home?: string): EarlyAnswer | null {
  return readJson<EarlyAnswer>(sessionPart(session, seq, "early.json", home) ?? "");
}

/**
 * The newest early answer of each service `session` filed, by entry order, keyed
 * by `EarlyAnswer.service`: the whole list of breaks that service's checks speak
 * now (T1 P1.4, D4). Only `ready` answers are filed, so each is a computed
 * answer. An answer that names no service is no evidence and is left out.
 */
export function newestEarlies(session: string, home?: string, listing?: readonly string[]): Map<string, EarlyAnswer> {
  const newest = new Map<string, EarlyAnswer>();
  for (const early of sessionEarlies(session, home, listing)) if (early.service !== undefined) newest.set(early.service, early);
  return newest;
}

/** Every early answer `session` filed, oldest entry first. */
export function sessionEarlies(session: string, home?: string, listing?: readonly string[]): EarlyAnswer[] {
  const names = sessionNames(session, home, listing)
    .filter((n) => n.endsWith(".early.json"))
    .sort((a, b) => seqOf(session, a).localeCompare(seqOf(session, b)));
  return names.flatMap((name) => readJson<EarlyAnswer>(path.join(sessionsDir(home), name)) ?? []);
}

/** Claim the right to print entry `seq`'s early answer, naming the rows printed: true for exactly one caller. */
export function claimEarly(session: string, seq: string, keys: readonly string[], home?: string, now: number = Date.now()): boolean {
  const file = sessionPart(session, seq, "early.said", home);
  if (file === null) return false;
  return createExclusive(file, `${JSON.stringify({ said_at: new Date(now).toISOString(), keys })}\n`);
}

/** The keys of entry `seq`'s early rows that were printed, or null when its early answer was never claimed. */
export function earlySaid(session: string, seq: string, home?: string): Set<string> | null {
  const said = readJson<{ keys?: string[] }>(sessionPart(session, seq, "early.said", home) ?? "");
  return said === null ? null : new Set(said.keys ?? []);
}

/**
 * The differ record for entry `seq`, for whichever of the two answers lands
 * second: null when the other one wrote it first.
 */
function claimRecord(session: string, seq: string, early: EarlyAnswer, full: CheckResult | undefined, home?: string): HeldRecord | null {
  const file = sessionPart(session, seq, "early.recorded", home);
  if (file === null || !createExclusive(file, `${JSON.stringify({ at: new Date().toISOString() })}\n`)) return null;
  return heldRecord(compareEarly(early.result, full));
}

/** An early answer with a break that `reader` has not printed, filed under the reader's own key or an ended sub-agent's. */
export type OwedEarly = { owner: string; early: EarlyAnswer };

/**
 * Every early answer with a break that `reader` is owed: its own, then each
 * ended sub-agent's (carrick#4074), oldest first. One a newer entry of the
 * same file superseded is claimed here unprinted, so it is never read again.
 */
export function earlyOwedFor(reader: string, home?: string, given?: readonly string[]): OwedEarly[] {
  const listing = given ?? sessionListing(home);
  const out: OwedEarly[] = [];
  for (const owner of [reader, ...endedAgents(reader, listing)]) {
    const all = sessionNames(owner, home, listing);
    const said = new Set(all.filter((n) => n.endsWith(".early.said")).map((n) => seqOf(owner, n)));
    for (const name of all.filter((n) => n.endsWith(".early.json"))) {
      const seq = seqOf(owner, name);
      if (said.has(seq)) continue;
      const early = readJson<EarlyAnswer>(path.join(sessionsDir(home), name));
      if (early === null || !hasEarlyBreak(early.result)) continue;
      if (supersededEntry(owner, seq, home)) {
        claimEarly(owner, seq, [], home);
        continue;
      }
      out.push({ owner, early });
    }
  }
  return out;
}

// --- the runner's lock -----------------------------------------------------

// The lock itself is `lock.ts`; this runner's lock file is `lockPath(root)`.

export type EnsureOptions = {
  env?: NodeJS.ProcessEnv;
  now?: number;
  /** Injected in tests; the real one detaches a child and forgets it. */
  start?: (command: string, args: string[], env: NodeJS.ProcessEnv) => void;
};

/** The command that runs `runQueue` for a workspace: the runner script beside this module. */
function runnerCommand(root: string): { command: string; args: string[] } {
  return scriptBeside(import.meta.url, "recheck-run", root);
}

/**
 * Make sure one runner is alive for this workspace. Returns whether one was
 * started: false when the lock is held by a live, recently touched process.
 *
 * The lock names this hook until the runner it spawns takes it over: a second
 * hook in that window sees a live holder and starts nothing.
 */
export function ensureRunner(root: string, options: EnsureOptions = {}): boolean {
  const env = options.env ?? process.env;
  const now = options.now ?? Date.now();
  const cap = backgroundCapMs(env);
  // A live, recently touched holder (a runner, or a hook that is starting
  // one) is the runner: this process's own earlier claim counts too.
  if (lockHeld(readJson<Lock>(lockPath(root)), now, cap)) return false;
  if (!takeLock(lockPath(root), process.pid, now, cap)) return false;
  const { command, args } = runnerCommand(root);
  // The runner is nobody's foreground work: it keeps no one awake (carrick#1889).
  const runnerEnv = {
    ...env,
    CARRICK_NO_KEEP_AWAKE: "1",
    CARRICK_RECHECK_SPAWNER: String(process.pid),
  };
  try {
    (options.start ?? startDetached)(command, args, runnerEnv);
  } catch (error) {
    releaseLock(lockPath(root), process.pid);
    log("could not start the re-check runner", String(error));
    return false;
  }
  return true;
}

/**
 * Ask for a rebuild of the local read model, which the runner runs under its
 * lock between re-checks (READY-A2 P11): a rebuild rewrites `.carrick/repos`,
 * `judged/` and `working/`, which every re-check reads, so it never runs
 * beside one, nor beside another rebuild. Starts a runner when none is alive;
 * a live one takes the request before it releases the lock. Returns whether a
 * runner was started.
 */
export function askJob(root: string, job: Job, options: EnsureOptions = {}): boolean {
  const now = options.now ?? Date.now();
  fs.mkdirSync(rechecksDir(root), { recursive: true });
  writeAtomic(requestPath(root, job), `${new Date(now).toISOString()}\n`);
  return ensureRunner(root, options);
}

/**
 * Take a job's request, once. `stuck` is a request this runner cannot remove:
 * it is not run, and not waited for again, so the runner never spins on it.
 */
function takeJob(root: string, job: Job): "taken" | "none" | "stuck" {
  try {
    fs.unlinkSync(requestPath(root, job));
    return "taken";
  } catch (error) {
    return (error as NodeJS.ErrnoException).code === "ENOENT" ? "none" : "stuck";
  }
}

/**
 * The longest one rebuild may run under the lock: a full `carrick refresh`
 * scans every repo, which the CLI already bounds at three hours (its keep-awake
 * cap); the move is a read-in and a join, bounded like `check --changed`.
 */
const JOB_LIMIT_MS: Record<Job, number> = {
  refresh: 3 * 60 * 60_000,
  forward: CHANGED_BUDGET_MS,
};

// --- the runner ------------------------------------------------------------

export type RunOptions = {
  env?: NodeJS.ProcessEnv;
  home?: string;
  /** The hook that spawned this runner, which holds the lock until it takes over. */
  spawner?: number;
  /** The `carrick` to run; defaults to `CARRICK_BIN` or `carrick`. */
  bin?: string;
  now?: () => number;
  /** Called between the runner removing its lock and its last look at the queue (tests). */
  afterRelease?: () => void | Promise<void>;
};

function writeAnswer(
  entry: QueueEntry,
  state: AnswerState,
  startedAt: number | null,
  finishedAt: number,
  body: { result?: CheckResult; changed?: ChangedResult; failure?: string },
  home?: string,
): boolean {
  const answer: Answer = {
    seq: entry.seq,
    session: entry.session,
    file: entry.file,
    relative: entry.relative,
    root: entry.root,
    state,
    enqueued_at: entry.enqueued_at,
    started_at: startedAt === null ? null : new Date(startedAt).toISOString(),
    finished_at: new Date(finishedAt).toISOString(),
    ...(isChanged(entry) ? { kind: "changed" as const } : {}),
    ...(entry.named !== undefined ? { named: entry.named } : {}),
    ...(entry.facts_callers ? { facts_callers: true as const } : {}),
    ...(entry.instead_of_probe ? { instead_of_probe: true as const } : {}),
    ...(entry.cwd ? { cwd: entry.cwd } : {}),
    ...body,
  };
  const file = sessionPart(entry.session, entry.seq, "answer.json", home);
  return file !== null && writeAtomic(file, `${JSON.stringify(answer, null, 2)}\n`);
}

function remove(file: string): void {
  try {
    fs.unlinkSync(file);
  } catch {
    // Already gone.
  }
}

/** Entries a dead runner held: re-queued once, then answered "stopped". */
function recoverTaken(root: string, self: number, now: number, home?: string): void {
  const base = path.join(rechecksDir(root), "taken");
  for (const dir of names(base)) {
    const pid = Number.parseInt(dir, 10);
    if (!Number.isFinite(pid) || pid === self || alive(pid)) continue;
    const folder = path.join(base, dir);
    for (const name of names(folder)) {
      const file = path.join(folder, name);
      const entry = readJson<QueueEntry>(file);
      if (entry === null) {
        remove(file);
        continue;
      }
      if ((entry.attempts ?? 0) >= 1) {
        writeAnswer(entry, "stopped", null, now, { failure: "the background re-check stopped" }, home);
        remove(file);
        continue;
      }
      const retried: QueueEntry = { ...entry, attempts: (entry.attempts ?? 0) + 1 };
      if (createExclusive(path.join(queueDir(root), name), `${JSON.stringify(retried, null, 2)}\n`)) {
        remove(file);
      }
    }
    try {
      fs.rmdirSync(folder);
    } catch {
      // Something is still in it; the next runner looks again.
    }
  }
}

/**
 * Settle the queue's bookkeeping, then name the entry to run next: entries
 * past their expiry are answered "stopped" unrun, an older entry of the same
 * session and file is answered "superseded" (silently), and what is left runs
 * in queue order. Null when nothing is left. Entries in `skip` (a claim of
 * theirs failed for good) are left where they are and never chosen.
 */
function nextEntry(
  root: string,
  now: number,
  cap: number,
  skip: ReadonlySet<string>,
  home?: string,
): QueueEntry | null {
  const live: Array<{ name: string; entry: QueueEntry }> = [];
  for (const name of names(queueDir(root))) {
    if (!name.endsWith(".json")) continue;
    const file = path.join(queueDir(root), name);
    const entry = readJson<QueueEntry>(file);
    if (entry === null) continue;
    if (now - Date.parse(entry.enqueued_at) > entryExpiryMs(cap)) {
      writeAnswer(entry, "stopped", null, now, { failure: "the background re-check did not start in time" }, home);
      remove(file);
      continue;
    }
    live.push({ name, entry });
  }
  const newest = new Map<string, string>();
  for (const { entry } of live) newest.set(`${entry.session}\0${entry.file}`, entry.seq);
  let chosen: QueueEntry | null = null;
  // A `changed` entry runs only once no per-file entry is left: it never
  // delays an answer a Stop waits for (carrick#3466).
  let last: QueueEntry | null = null;
  for (const { name, entry } of live) {
    if (newest.get(`${entry.session}\0${entry.file}`) !== entry.seq) {
      writeAnswer(entry, "superseded", null, now, {}, home);
      remove(path.join(queueDir(root), name));
      continue;
    }
    if (skip.has(entry.seq)) continue;
    if (isChanged(entry)) last ??= entry;
    else chosen ??= entry;
  }
  return chosen ?? last;
}

/** Claim an entry by renaming it into this runner's own folder. */
function take(root: string, entry: QueueEntry, pid: number): string | null {
  const from = path.join(queueDir(root), `${entry.seq}.json`);
  const to = path.join(takenDir(root, pid), `${entry.seq}.json`);
  try {
    fs.mkdirSync(path.dirname(to), { recursive: true });
    fs.renameSync(from, to);
    return to;
  } catch {
    return null;
  }
}

/** Return a taken entry to the queue, unanswered. */
function putBack(root: string, entry: QueueEntry, taken: string): void {
  try {
    fs.renameSync(taken, path.join(queueDir(root), `${entry.seq}.json`));
  } catch (error) {
    log("could not return an edit to the queue", String(error));
  }
}

/**
 * The runner's loop: one `carrick check <file> --recheck --json` at a time, in
 * queue order, each answer written when its call ends. Returns when the queue
 * is empty and the lock is released, or at once when another runner holds it.
 */
export async function runQueue(root: string, options: RunOptions = {}): Promise<void> {
  const env = options.env ?? process.env;
  const clock = options.now ?? Date.now;
  const cap = backgroundCapMs(env);
  const pid = process.pid;
  if (!takeLock(lockPath(root), pid, clock(), cap, options.spawner)) return;
  const callEnv: NodeJS.ProcessEnv = {
    ...env,
    CARRICK_RECHECK_BUDGET_MS: String(cap),
    CARRICK_TIMEOUT_MS: String(cap + CALL_SLACK_MS),
  };
  const { CARRICK_RECHECK_BUDGET_MS: _budget, ...base } = env;
  void _budget;
  const changedEnv: NodeJS.ProcessEnv = {
    ...base,
    CARRICK_CHECK_BUDGET_MS: String(CHANGED_BUDGET_MS),
    CARRICK_TIMEOUT_MS: String(CHANGED_BUDGET_MS + CALL_SLACK_MS),
  };
  const bin = options.bin ?? binary(env);
  // Entries this runner could not claim although their file stayed (a disk
  // that refuses the rename): answered once, then left alone for its life.
  const unclaimable = new Set<string>();
  const waiting = () => names(queueDir(root)).filter((name) => !unclaimable.has(name.replace(/\.json$/, "")));
  // A rebuild request this runner could not remove, the same way: left alone.
  const stuck = new Set<Job>();
  // An edit returned to the queue because the read model was being rebuilt:
  // this runner stops once the lock goes, rather than take it again at once.
  let requeued = false;
  const jobs: Job[] = ["refresh", "forward"];
  const jobWaiting = () => jobs.some((job) => !stuck.has(job) && fs.existsSync(requestPath(root, job)));
  for (;;) {
    recoverTaken(root, pid, clock(), options.home);
    for (;;) {
      const entry = nextEntry(root, clock(), cap, unclaimable, options.home);
      if (entry === null) break;
      const taken = take(root, entry, pid);
      if (taken === null) {
        if (fs.existsSync(path.join(queueDir(root), `${entry.seq}.json`))) {
          unclaimable.add(entry.seq);
          const failure = "the background re-check could not claim this edit";
          writeAnswer(entry, "stopped", null, clock(), { failure }, options.home);
        }
        continue;
      }
      touchLock(lockPath(root), pid, clock());
      const startedAt = clock();
      if (isChanged(entry)) {
        // Longer than the per-file cap, so the lock is kept fresh while it
        // runs: a lock older than cap + 60 s reads as dead to `ensureRunner`.
        const keep = setInterval(() => touchLock(lockPath(root), pid, clock()), LOCK_TOUCH_MS);
        const outcome = await checkChanged({ cwd: root, limitMs: CHANGED_BUDGET_MS + CALL_SLACK_MS, env: changedEnv, bin });
        clearInterval(keep);
        const failure = outcome.failure ?? "the check gave no answer";
        writeAnswer(
          entry,
          "answered",
          startedAt,
          clock(),
          outcome.result ? { changed: outcome.result } : { failure },
          options.home,
        );
        remove(taken);
        continue;
      }
      const outcome = await check(entry.relative, {
        cwd: root,
        workspace: root,
        recheck: true,
        limitMs: cap + CALL_SLACK_MS,
        env: callEnv,
        bin,
      });
      // The local read model was being rebuilt and nothing was judged: the
      // edit goes back in the queue unanswered, and this runner stops so it
      // does not take it again at once. The next hook's runner asks again.
      if (outcome.result?.recheck?.rebuilding === true) {
        putBack(root, entry, taken);
        requeued = true;
        break;
      }
      const finishedAt = clock();
      const body = outcome.result ? { result: outcome.result } : { failure: outcome.failure ?? "the re-check gave no answer" };
      writeAnswer(entry, "answered", startedAt, finishedAt, body, options.home);
      // The differ record (P12) when the early answer landed first.
      const early = readEarly(entry.session, entry.seq, options.home);
      const record = early === null ? null : claimRecord(entry.session, entry.seq, early, outcome.result ?? undefined, options.home);
      // One timing row per re-check, with whether the compiler engine was
      // ready for its re-scan, so the miss rate can be read later (carrick#3948).
      // Not in the hooks' p50/p90: the agent does not wait on this runner.
      const engineReady = engineReadyOf(outcome.result);
      appendTiming(root, {
        hook: "recheck",
        start: new Date(startedAt).toISOString(),
        duration_ms: Math.max(0, finishedAt - startedAt),
        waited_ms: Math.max(0, finishedAt - startedAt),
        outcome: outcome.result ? "silent" : "error",
        file: entry.relative,
        ...(engineReady === undefined ? {} : { engine_ready: engineReady }),
        ...(record === null ? {} : { held_ms: early!.held_ms, ...record }),
      });
      remove(taken);
    }
    // A rebuild asked for (`askJob`), run while this runner holds the lock and
    // no re-check is reading; edits queued while it runs are taken before the
    // lock goes. The full refresh first: the move then starts from it.
    let ran = false;
    for (const job of jobs) {
      const asked = stuck.has(job) ? "none" : takeJob(root, job);
      if (asked === "stuck") {
        stuck.add(job);
        log("a rebuild request could not be taken", requestPath(root, job));
      }
      if (asked !== "taken") continue;
      const limit = JOB_LIMIT_MS[job] + CALL_SLACK_MS;
      const keep = setInterval(() => touchLock(lockPath(root), pid, clock()), LOCK_TOUCH_MS);
      const done = await refresh({
        cwd: root,
        workspace: root,
        hostedOnly: job === "forward",
        limitMs: limit,
        env: { ...base, CARRICK_NO_KEEP_AWAKE: "1", CARRICK_TIMEOUT_MS: String(limit) },
        bin,
      });
      clearInterval(keep);
      if (done.failure) log(`the background ${job} failed`, done.failure);
      ran = true;
    }
    if (ran) continue;
    releaseLock(lockPath(root), pid);
    await options.afterRelease?.();
    // Anything queued between the last look and the release would otherwise
    // wait for the next hook to notice: look once more, and go on if so.
    if (requeued || (waiting().length === 0 && !jobWaiting())) return;
    if (!takeLock(lockPath(root), pid, clock(), cap)) return;
  }
}

// --- the session's side ----------------------------------------------------

/** The sessions folder's file names, listed once for a hook call that reads several keys. */
export function sessionListing(home?: string): string[] {
  return names(sessionsDir(home));
}

function sessionNames(session: string, home?: string, listing?: readonly string[]): string[] {
  if (safeId(session) === null) return [];
  const prefix = `${session}.recheck.`;
  return (listing ?? names(sessionsDir(home))).filter((name) => name.startsWith(prefix));
}

// --- an ended sub-agent (carrick#4074) -------------------------------------
//
// A sub-agent's re-checks are filed under `<session>.<agent>`, and none of its
// hooks runs once it has ended, so an answer landing after that was never read.
// `carrick hook subagent-stop` leaves `<session>.<agent>.ended` (not `.json`:
// `sessionRecords` counts those); the main session's readers then take what
// the ended agent is owed. A sub-agent resumed later deletes its own marker
// with its next hook, so its own late answers stay its own.

const ENDED = ".ended";

function endedFile(key: string, home?: string): string | null {
  return safeId(key) === null ? null : path.join(sessionsDir(home), `${key}${ENDED}`);
}

/** Mark the sub-agent `key` (`<session>.<agent>`) as ended. Never throws. */
export function markEnded(key: string, home?: string, now: number = Date.now()): void {
  const file = endedFile(key, home);
  if (file === null) return;
  try {
    writeAtomic(file, `${JSON.stringify({ ended_at: new Date(now).toISOString() })}\n`);
  } catch (error) {
    log("could not mark the sub-agent ended", String(error));
  }
}

/**
 * The keys (`<session>.<agent>`) of the sub-agents of `session` that have
 * ended. An agent id is one flat segment, so a marker with a dot after the
 * session belongs to a deeper key (a sub-agent's own, or a session id that
 * itself holds a dot) and is not this reader's to adopt.
 */
function endedAgents(session: string, listing: readonly string[]): string[] {
  const prefix = `${session}.`;
  return listing
    .filter((name) => name.startsWith(prefix) && name.endsWith(ENDED))
    .map((name) => name.slice(0, -ENDED.length))
    .filter((key) => key.length > prefix.length && !key.slice(prefix.length).includes("."));
}

function seqOf(session: string, name: string): string {
  return name.slice(`${session}.recheck.`.length).split(".")[0] ?? "";
}

/** Entries this session queued that have no answer yet, oldest first. */
export function pendingFor(session: string, home?: string): Pending[] {
  const all = sessionNames(session, home);
  const answered = new Set(all.filter((n) => n.endsWith(".answer.json")).map((n) => seqOf(session, n)));
  const out: Pending[] = [];
  for (const name of all.filter((n) => n.endsWith(".pending.json"))) {
    const seq = seqOf(session, name);
    if (answered.has(seq)) continue;
    const entry = readJson<QueueEntry>(path.join(sessionsDir(home), name));
    if (entry !== null) out.push(entry);
  }
  return out;
}

/**
 * Answers this session has not been given, oldest first. A superseded answer
 * is silent; an answer is held back while the session holds a newer entry for
 * the same file (its newer answer, or the wait for it, is what to say).
 *
 * Only what may still be owed is read: the names are
 * listed once, an answer is owed while it has no `.delivered`, and pending
 * entries are read only from the oldest owed seq on (seqs sort by time, so
 * nothing older can be newer than an owed answer). An owed answer no carrier
 * would ever print (superseded, or outdated by a newer entry for its file) is
 * claimed here, so the next call finds nothing owed and reads no file.
 */
export function finishedFor(session: string, home?: string, listing?: readonly string[]): Answer[] {
  const all = sessionNames(session, home, listing);
  const delivered = new Set(all.filter((n) => n.endsWith(".delivered")).map((n) => seqOf(session, n)));
  const owed = all
    .filter((n) => n.endsWith(".answer.json"))
    .map((n) => seqOf(session, n))
    .filter((seq) => !delivered.has(seq));
  if (owed.length === 0) return [];
  const oldest = owed[0]!;
  const newest = new Map<string, string>();
  const fileOf = new Map<string, string>();
  for (const name of all.filter((n) => n.endsWith(".pending.json"))) {
    if (seqOf(session, name) < oldest) continue;
    const entry = readJson<QueueEntry>(path.join(sessionsDir(home), name));
    if (entry === null) continue;
    fileOf.set(entry.seq, entry.file);
    const held = newest.get(entry.file);
    if (held === undefined || held < entry.seq) newest.set(entry.file, entry.seq);
  }
  const out: Answer[] = [];
  for (const seq of owed) {
    const file = fileOf.get(seq);
    if (file !== undefined && (newest.get(file) ?? seq) > seq) {
      claimDelivery(session, seq, home);
      continue;
    }
    const answer = readJson<Answer>(path.join(sessionsDir(home), `${session}.recheck.${seq}.answer.json`));
    if (answer === null) continue;
    if (answer.state === "superseded" || (newest.get(answer.file) ?? seq) > seq) {
      claimDelivery(session, seq, home);
      continue;
    }
    out.push(answer);
  }
  return out;
}

/** A finished answer and the key it is filed under: the reader's own, or an ended sub-agent's. */
export type Owed = { owner: string; answer: Answer };

/**
 * Every finished answer `reader` is owed: its own, then each ended sub-agent's
 * (carrick#4074). First of all it deletes the reader's own `.ended` marker:
 * a hook of a sub-agent's own means it was resumed, and its late answers are
 * its own again (a main session never has one). A sub-agent reader adopts
 * nothing, since no marker is filed under its key. The caller claims each
 * answer under `owner`, so an adopted answer is never said twice. One listing
 * of the sessions folder serves the reader and every ended agent.
 */
export function owedFor(reader: string, home?: string, given?: readonly string[]): Owed[] {
  let listing = given ?? sessionListing(home);
  const own = endedFile(reader, home);
  if (own !== null && listing.includes(`${reader}${ENDED}`)) {
    try {
      fs.unlinkSync(own);
    } catch {
      // Another hook of the same agent deleted it first.
    }
    listing = listing.filter((name) => name !== `${reader}${ENDED}`);
  }
  const out: Owed[] = finishedFor(reader, home, listing).map((answer) => ({ owner: reader, answer }));
  for (const owner of endedAgents(reader, listing)) {
    for (const answer of finishedFor(owner, home, listing)) out.push({ owner, answer });
  }
  return out;
}

/**
 * The newest entry this session holds for each edited file, with its answer
 * when there is one, delivered or not: what `sessionCoverage` reads. `changed`
 * entries are not files and are left out.
 */
export function newestPerFile(session: string, home?: string): Map<string, { entry: QueueEntry; answer: Answer | null }> {
  const all = sessionNames(session, home);
  const out = new Map<string, { entry: QueueEntry; answer: Answer | null }>();
  for (const name of all.filter((n) => n.endsWith(".pending.json"))) {
    const entry = readJson<QueueEntry>(path.join(sessionsDir(home), name));
    if (entry === null || isChanged(entry)) continue;
    const held = out.get(entry.file);
    if (held !== undefined && held.entry.seq > entry.seq) continue;
    const answer = readJson<Answer>(path.join(sessionsDir(home), `${session}.recheck.${entry.seq}.answer.json`));
    out.set(entry.file, { entry, answer });
  }
  return out;
}

/**
 * When the session's newest answered `check --changed` for `root` was queued,
 * in epoch ms, or 0 when none answered. That check re-read every changed file
 * of the workspace, so an edit made before it was queued is covered by it.
 */
export function changedAnsweredAt(session: string, root: string, home?: string): number {
  let newest = 0;
  for (const name of sessionNames(session, home).filter((n) => n.endsWith(".answer.json"))) {
    const answer = readJson<Answer>(path.join(sessionsDir(home), name));
    if (answer === null || !isChanged(answer) || answer.root !== root) continue;
    if (answer.state !== "answered" || answer.changed === undefined) continue;
    const at = Date.parse(answer.enqueued_at);
    if (Number.isFinite(at) && at > newest) newest = at;
  }
  return newest;
}

/** Claim the right to print one answer: true for exactly one caller. */
export function claimDelivery(session: string, seq: string, home?: string, now: number = Date.now()): boolean {
  const file = sessionPart(session, seq, "delivered", home);
  if (file === null) return false;
  return createExclusive(file, `${JSON.stringify({ delivered_at: new Date(now).toISOString() })}\n`);
}
