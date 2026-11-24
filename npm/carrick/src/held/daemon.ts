// The held-compiler daemon (part 3): one small process per checkout root that
// keeps one compiler engine per service open, and answers "does this edit break
// the other side of a route" over a Unix socket.
//
// This file is the lifecycle and the routing: which service a file belongs to,
// when an engine starts, how much memory they may hold together, when they and
// the daemon itself leave. What an engine IS (shadow tree, language server,
// checks) is behind `ServiceRuntime`, supplied by the caller.

import fs from "node:fs";
import net from "node:net";
import path from "node:path";
import { realPath } from "../fs-util.ts";
import { createLogger } from "../log.ts";
import { readJson, releaseLock, takeLock, touchLock } from "../hook/lock.ts";
import { answerEarly, supersededEntry } from "../hook/rechecks.ts";
import { linkedCheckoutTop, mainCheckoutOf } from "../hook/worktree.ts";
import { heldBuildId } from "./build.ts";
import { heldDir, heldRootOf, helloPath, lockPath, LOCK_CAP_MS, logPath, placedPath, serviceFilesDir, socketPath, statePath } from "./paths.ts";
import { readState, writeState, type NotChecked, type ServiceState } from "./state.ts";
import { addSteps, tenth, type Steps } from "./steps.ts";
import {
  admits,
  ADMISSION_MB,
  EVICT_IDLE_MS,
  liveEntries,
  memoryBudgetMb,
  removeEntry,
  closeCeilingMb,
  isWorktreeEntry,
  worktreeSlotsMax,
  worktreesEnabled,
  writeEntry,
} from "./registry.ts";

const log = createLogger("carrick-held");

/** One service's program init, as `carrick held-programs --json` prints it. */
export type ServiceInit = {
  repo: string;
  service: string;
  repo_path: string;
  sidecar_root: string;
  tsconfig: string | null;
  scan_root: string;
  /** The real path of the linked worktree's top this init belongs to. Absent for every main-checkout service. */
  checkout?: string;
};

export type HeldRequest = {
  session: string;
  /** Absolute paths. */
  files: string[];
  kind: "read" | "edit";
  edited_at?: number;
  /** How long the asker waits; an answer after it is logged as late. */
  wait_ms?: number;
  /**
   * The asker's queue entry, part 2's own (B1): the answer is filed under it as
   * its early answer (`<seq>.early.json`), unless a newer entry of the same
   * file superseded this ask, which is then not run.
   */
  seq?: string;
};

/** One break the compiler found in an armed route's reply. */
export type HeldLine = {
  row_key: string;
  group: number;
  label: string;
  consumer: string;
  file: string;
  line: number;
  message: string;
  /** The group's named type, when the row has one (the detail says it in place of the check's alias). */
  symbol?: string | null;
  /** The contract row's own file, repo-relative, and its line: where the route is, never the send. */
  row_file: string;
  row_line: number | null;
  /** The row's file, absolute: what decides whether the row is in the edited file. */
  row_path: string;
  /** The group's places (the consumers that read the type) and the type they expect. */
  places: Array<{ service: string; file: string; line: number | null }>;
  expected: string;
  /** Present on a consumer-side line (a socket emit, later an HTTP call): the edited side is the consumer. Absent on a producer line. */
  side?: "consumer";
  /** True when the check form wraps this code's expectation (a receiver form); absent when it wraps what this code sends. */
  reads?: true;
  /** The row's protocol (`http`, `socket`, ...): names the other side in the sentence. */
  protocol?: string;
  /** A consumer line's call line on the current text, for joining part 2's call row. */
  call_line?: number | null;
  /**
   * The run this break belongs to: the edit answer at which its check began to
   * speak this message. A break that stopped and spoke again is a new run.
   */
  run?: string;
  /** Its check spoke the same message at the engine's previous edit answer: not this edit's break (T1 P1.1). */
  standing?: true;
  /**
   * The commit of main the other side's types were read from: the fact file's
   * commit for the places' repo, or null when it names none (T2 P2.1). The check
   * holds the other side fixed at that copy, whatever the session has edited since.
   */
  main_commit: string | null;
  /** The files that hold the other side as main has it: each place's file and the group's home file, absolute (T2 P2.2). */
  named_files: string[];
};

export type HeldState = "none" | "cold" | "ready" | "capped" | "no_compiler" | "error" | "worktree";

export type HeldAnswer = {
  state: HeldState;
  lines: HeldLine[];
  ms: number;
  /** The services whose engine did not check the edit, with why (T7): the hook says each once. */
  not_checked?: NotChecked[];
};

/** What serving a request found, and where its time went. */
type Served = { state: HeldState; lines: HeldLine[]; steps: Steps; note?: NotChecked; service?: string; engine?: string; commits?: Array<() => void> };

/** A started engine for one service. */
export interface ServiceEngine {
  /**
   * The lines for `files` as they are now. `after` runs once the answer has
   * been sent, still inside the engine's queue: no request reaches the engine
   * until it ends (the twin pass edits the engine's texts). `steps` is where
   * the answer's time went, in ms by step; `after` answers the same for its own.
   * `commit` is called once the answer is filed as an early answer: what its
   * lines spoke becomes what the next answer's are marked `standing` against
   * (T1 P1.1). An answer that was not filed is never committed. `engine` is
   * the engine's id, one per start: a break's run names it (`runOf`).
   */
  answer(files: string[], request: HeldRequest): Promise<{ lines: HeldLine[]; steps?: Steps; after?: () => Promise<Steps | void>; commit?: () => void; engine?: string }>;
  /** Resident memory in MB, or null. */
  rss(): number | null;
  /** False for a service that started with no compiler: it holds no memory, so it holds no registry slot. */
  holdsCompiler?: boolean;
  close(): Promise<void>;
}

/**
 * `refused`: a linked worktree's start found no clone of the main checkout's shadow it may use (#5038, T2):
 * the refusal code `cloneShadow` gave (`held/shadow.ts` `CloneRefusal`) and what it names.
 */
export type StartResult = { engine: ServiceEngine } | { state: "no_compiler" | "error"; reason: string } | { state: "refused"; refused: string; detail: string };

/**
 * `edited`: the files (absolute) of the edit ask that started a linked worktree's engine. Its first gate judges their text
 * at the worktree's `HEAD`, so the edit itself is then judged as an edit (#5038, T2 F1). A main-checkout start gets none.
 */
export type StartOptions = { edited?: string[] };

/** Starts a service's engine: the program, the compiler, the gate. */
export interface ServiceRuntime {
  start(init: ServiceInit, options?: StartOptions): Promise<StartResult>;
  /**
   * How many fact rows `start` would read for the service. A service with none gets no
   * engine, so it asks for no admission and leaves no "not checked" note.
   */
  rowCount?(init: ServiceInit): Promise<number>;
}

export type DaemonDeps = {
  root: string;
  home?: string;
  env?: NodeJS.ProcessEnv;
  /** The machine's memory in bytes; `os.totalmem` by default. */
  totalmem?: () => number;
  clock?: () => number;
  programs: () => Promise<ServiceInit[]>;
  runtime: ServiceRuntime;
  /** How often a worktree ask waiting for a slot looks again, in ms (250 by default). */
  queuePollMs?: number;
  /** The id the daemon answers `hello` with; the hash of its own build files by default. */
  buildId?: string;
};

/**
 * First frame of a connection: who is on the other end of the socket. The
 * client asks only a daemon of its own build; `exit` retires one of another.
 */
export type HeldHello = { build_id: string; pid: number; root: string };
/**
 * `ask`: a request written with the hello, before its reply (T5, #5038), stamped with the asker's build. A busy
 * daemon reads it whenever its loop is free, even after the asker stopped waiting, and files its early answer;
 * a daemon of another build never answers it.
 */
export type HeldFrame = { hello: true } | { exit: true } | { ask: HeldRequest; build_id: string };

const MIN = 60_000;
/** D2: an engine closes after this long with no request. */
const ENGINE_IDLE_MS = 15 * MIN;
/** D2: the daemon exits after this long with no request... */
const DAEMON_IDLE_MS = 30 * MIN;
/** ...or this long after its last engine closed. */
const EMPTY_EXIT_MS = 5 * MIN;
/** A start that failed is tried again after this long. */
const RETRY_MS = 5 * MIN;
/**
 * A linked worktree's engine closes after this long with no request (#5038, T2): it starts again from a
 * clone of main's shadow in seconds, so it gives its memory back sooner than a main-checkout engine.
 */
export const WORKTREE_IDLE_MS = 60_000;
/** A worktree ask that waited out its `wait_ms` is kept this long, and answered late if a slot frees (#5038, T3). */
const WORKTREE_PARK_MS = 2 * MIN;
/** The note a worktree ask gets when every worktree slot is busy and the cap or budget has no room. */
const SLOTS_BUSY = "worktree slots busy";
/** A worktree clone refused for one of these is tried once more after the main checkout's service has started (it writes a fresh shadow and stamp). */
const STALE_MAIN = new Set(["no_saved_answer", "stale_build", "stale_main_inputs"]);
/** ...and for these the main checkout's engine is stale too: a ready one is closed and started again first (T2 F3). */
const RESTART_MAIN = new Set(["stale_build", "stale_main_inputs"]);
/** T2 F5: a main start waits at most this long for a worktree engine's ask in flight before it closes that engine. */
const MAIN_START_WAIT_MS = 2_000;

type Slot = {
  init: ServiceInit;
  /** `refused`: a worktree slot whose clone of main's shadow was refused (`reason` holds the refusal code). */
  status: "starting" | "ready" | "no_compiler" | "failed" | "refused";
  engine: ServiceEngine | null;
  lastUsed: number;
  failedAt: number;
  begunAt: number;
  /** MB the engine held when its first gate finished (or, before that, the service's last one). */
  settledMb?: number;
  /** Why the start failed, one line. */
  reason?: string;
  /** Engine calls run one at a time, in arrival order. */
  queue: Promise<void>;
  /** Engine calls asked of this slot and not yet finished: a slot with none is idle (T3). */
  busy: number;
  /** The newest edit ask per session and file answered `cold` while the engine started (N2); each is asked again once it is ready. */
  cold: Map<string, { request: HeldRequest; files: string[] }>;
  /** Settles once the start has given its result. */
  started: Promise<void>;
  /** A worktree slot whose start, inside the service's chain, has given an engine over its clone (set before `started` marks it ready). */
  cloned?: true;
  /** The files of the edit ask that began a worktree slot: its first gate reads them at the worktree's HEAD (T2 F1). */
  edited?: string[];
  settle: () => void;
};

/** A reason as one short line. */
const oneLine = (text: string): string => text.replace(/\s+/g, " ").trim().slice(0, 160);

const key = (init: ServiceInit): string => `${init.repo}\0${init.service}${init.checkout === undefined ? "" : `\0${init.checkout}`}`;
/** One service, whichever checkout: its starts (and its worktree engines' closes) run one at a time. */
const serviceKey = (init: ServiceInit): string => `${init.repo}\0${init.service}`;

const within = (file: string, dir: string): boolean => file === dir || file.startsWith(dir + path.sep);

/**
 * The main checkout's init for a service, re-pointed at a linked worktree (#5038, T2): the same service,
 * with its folders at the same place under the worktree's top, and `checkout` naming that top. Null when
 * a folder of the init is not under the main checkout's top.
 */
function initInWorktree(init: ServiceInit, mainTop: string, top: string): ServiceInit | null {
  const move = (p: string): string | null => {
    const rel = path.relative(mainTop, realPath(p));
    return rel.startsWith("..") || path.isAbsolute(rel) ? null : path.join(top, rel);
  };
  const repoPath = move(init.repo_path);
  const sidecarRoot = move(init.sidecar_root);
  const scanRoot = move(init.scan_root);
  if (repoPath === null || sidecarRoot === null || scanRoot === null) return null;
  const tsconfig = init.tsconfig !== null && path.isAbsolute(init.tsconfig) ? (move(init.tsconfig) ?? init.tsconfig) : init.tsconfig;
  return { ...init, repo_path: repoPath, sidecar_root: sidecarRoot, scan_root: scanRoot, tsconfig, checkout: top };
}

/**
 * Whether a program serves a file: the file is inside the program's folder AND in the
 * same git checkout. A nested worktree's file sits under a repo-root program's folder
 * but is another checkout's text, so it is never that program's (T2, #5038).
 */
const serves = (file: string, dir: string): boolean => {
  if (!within(file, dir)) return false;
  const top = linkedCheckoutTop(file);
  return top === null || within(dir, realPath(top));
};

export class HeldDaemon {
  private readonly deps: DaemonDeps;
  private readonly root: string;
  private readonly slots = new Map<string, Slot>();
  /** Per service (`serviceKey`): the tail of its starts and worktree closes, run one at a time. */
  private readonly chains = new Map<string, Promise<unknown>>();
  private programs: Promise<Array<{ init: ServiceInit; dir: string }>> | null = null;
  private lastRequest: number;
  private emptySince: number | null = null;
  private closed = false;
  /** Worktree admissions run one at a time, so the cap and the budget are counted before each start, never beside it (T3). */
  private admitChain: Promise<unknown> = Promise.resolve();
  /** Worktree edit asks that waited out their `wait_ms`, per slot key: answered late once a slot frees (T3). */
  private readonly parked = new Map<string, { init: ServiceInit; asks: Array<{ files: string[]; request: HeldRequest }> }>();

  constructor(deps: DaemonDeps) {
    this.deps = deps;
    this.root = realPath(deps.root);
    this.lastRequest = this.now();
  }

  private now(): number {
    return (this.deps.clock ?? Date.now)();
  }

  private loadPrograms(): Promise<Array<{ init: ServiceInit; dir: string }>> {
    if (this.programs === null) {
      this.programs = this.deps
        .programs()
        .then((inits) => inits.map((init) => ({ init, dir: realPath(init.sidecar_root) })))
        .catch((error: unknown) => {
          // Not cached: the next request reads them again.
          this.programs = null;
          throw error;
        });
    }
    return this.programs;
  }

  async handle(request: HeldRequest): Promise<HeldAnswer> {
    const started = performance.now();
    this.lastRequest = this.now();
    let state: HeldState = "none";
    let lines: HeldLine[] = [];
    let steps: Steps = {};
    let notes: NotChecked[] = [];
    let service: string | undefined;
    let engine: string | undefined;
    let commits: Array<() => void> = [];
    try {
      ({ state, lines, steps, notes, service, engine, commits = [] } = await this.route(request));
    } catch (error) {
      log("request failed", String(error));
      state = "error";
    }
    const ms = Math.round(performance.now() - started);
    this.record(request, state, lines.length, ms, steps);
    if (this.file(request, state, lines, ms, { service, engine })) for (const commit of commits) commit();
    return { state, lines, ms, ...(notes.length > 0 ? { not_checked: notes } : {}) };
  }

  private async route(request: HeldRequest): Promise<Served & { notes: NotChecked[] }> {
    const started = performance.now();
    const programs = await this.loadPrograms();
    const files = request.files.map(realPath);
    const lines: HeldLine[] = [];
    const states: HeldState[] = [];
    const notes: NotChecked[] = [];
    const ready: string[] = [];
    const engines: string[] = [];
    const commits: Array<() => void> = [];
    const steps: Steps = { programs: tenth(performance.now() - started) };
    const served = new Set<string>();
    const take = (init: ServiceInit, answer: Served): void => {
      states.push(answer.state);
      if (answer.state === "ready") ready.push(`${init.repo}/${init.service}`);
      lines.push(...answer.lines);
      if (answer.note !== undefined) notes.push(answer.note);
      commits.push(...(answer.commits ?? []));
      if (answer.engine !== undefined) engines.push(answer.engine);
      addSteps(steps, answer.steps);
    };
    for (const { init, dir } of programs) {
      const mine = files.filter((f) => serves(f, dir));
      if (mine.length === 0) continue;
      for (const f of mine) served.add(f);
      take(init, await this.serveService(init, mine, request));
    }
    // A worktree file no program serves goes to that worktree's own slot of its twin's service: an engine
    // over a clone of main's shadow with the worktree's files (T2, #5038). Where the init cannot be
    // re-pointed at the worktree, it is not checked: say so, and start nothing.
    const worktrees = new Map<string, { init: ServiceInit; files: string[] }>();
    const told = new Set<string>();
    for (const file of files) {
      if (served.has(file)) continue;
      const top = linkedCheckoutTop(file);
      const main = top === null ? null : mainCheckoutOf(top);
      if (top === null || main === null) continue;
      const mainTop = realPath(main);
      const twin = path.join(mainTop, path.relative(realPath(top), file));
      for (const { init, dir } of programs) {
        if (!within(twin, dir)) continue;
        const theirs = worktreesEnabled(this.env()) ? initInWorktree(init, mainTop, realPath(top)) : null;
        if (theirs !== null) {
          const group = worktrees.get(key(theirs)) ?? { init: theirs, files: [] };
          if (!group.files.includes(file)) group.files.push(file);
          worktrees.set(key(theirs), group);
          continue;
        }
        if (told.has(`${init.service}\0${top}`)) continue;
        if (((await this.deps.runtime.rowCount?.(init)) ?? 1) === 0) continue;
        told.add(`${init.service}\0${top}`);
        notes.push({ service: init.service, state: "worktree", checkout: top });
        states.push("worktree");
      }
    }
    for (const { init, files: theirs } of worktrees.values()) take(init, await this.serveService(init, theirs, request));
    // One state for the whole request: the most useful one any service gave.
    const order: HeldState[] = ["ready", "cold", "capped", "no_compiler", "error", "worktree"];
    const state = order.find((s) => states.includes(s)) ?? "none";
    // The services whose engines answered, and those engines: what an early answer's rows are the whole list of (T1 P1.4).
    return { state, lines, steps, notes, commits, ...(ready.length > 0 ? { service: ready.join(", ") } : {}), ...(engines.length > 0 ? { engine: engines.join(", ") } : {}) };
  }

  private async serveService(init: ServiceInit, files: string[], request: HeldRequest): Promise<Served> {
    const now = this.now();
    // The off switch is read per request: no worktree slot starts, and a worktree file is answered `worktree` (T3).
    if (init.checkout !== undefined && !worktreesEnabled(this.env())) return this.notChecked(init, "worktree");
    let slot = this.slots.get(key(init));
    if ((slot?.status === "failed" || slot?.status === "refused") && now - slot.failedAt >= RETRY_MS) {
      this.slots.delete(key(init));
      slot = undefined;
    }
    if (slot === undefined) {
      if ((await this.deps.runtime.rowCount?.(init)) === 0) return { state: "none", lines: [], steps: {} };
      if (init.checkout !== undefined) {
        if (await this.admitWorktree(init, request.wait_ms ?? 0, request.kind === "edit" ? files : undefined)) return this.serveService(init, files, request);
        this.park(init, files, request);
        this.slotLog("capped", init);
        return this.notChecked(init, "capped", SLOTS_BUSY);
      }
      if (!(await this.makeRoom(init))) return this.notChecked(init, "capped");
      this.begin(init, now);
      this.remember(this.slots.get(key(init)), files, request);
      return this.notChecked(init, "cold");
    }
    slot.lastUsed = now;
    if (slot.status === "starting") {
      this.remember(slot, files, request);
      return this.notChecked(init, "cold");
    }
    if (slot.status === "no_compiler") return this.notChecked(init, "no_compiler");
    if (slot.status === "refused") return this.notChecked(init, "worktree", slot.reason);
    if (slot.status === "failed" || slot.engine === null) return this.notChecked(init, "error", slot.reason);
    const asked = await this.ask(slot, slot.engine, files, request);
    return asked === null ? this.notChecked(init, "error", "the engine could not answer") : { state: "ready", ...asked };
  }

  /** N2: keep an edit ask that named its entry and was answered `cold`; a read, or an ask with no entry, files nothing to answer later. */
  private remember(slot: Slot | undefined, files: string[], request: HeldRequest): void {
    if (slot === undefined || request.kind !== "edit" || request.seq === undefined) return;
    slot.cold.set(`${request.session}\0${files.join("\0")}`, { request, files });
  }

  /** N2: the engine is ready; answer each edit ask it missed, as `handle` files one. A newer edit's entry supersedes an older ask. */
  private async askAgain(slot: Slot, engine: ServiceEngine): Promise<void> {
    const owed = [...slot.cold.values()];
    slot.cold.clear();
    for (const { request, files } of owed) {
      try {
        const started = performance.now();
        const asked = await this.ask(slot, engine, files, request);
        if (asked === null) continue;
        const ms = Math.round(performance.now() - started);
        const filed = this.file(request, "ready", asked.lines, ms, { service: `${slot.init.repo}/${slot.init.service}`, engine: asked.engine });
        if (filed) for (const commit of asked.commits ?? []) commit();
      } catch (error) {
        log("could not answer an ask made during the start", String(error));
      }
    }
  }

  /** An answer that did not check the edit, with the facts the hook's line and the doctor's read (T7). */
  private notChecked(init: ServiceInit, state: Exclude<NotChecked["state"], "busy">, reason?: string): Served {
    const note: NotChecked = { service: init.service, state };
    if (reason !== undefined) note.reason = reason;
    if (state === "worktree" && init.checkout !== undefined) note.checkout = init.checkout;
    if (state === "capped") {
      const holders = liveEntries(this.deps.home).length;
      note.budget_mb = this.budget();
      note.holders = holders;
      this.keep(init, { state: "capped", budget_mb: note.budget_mb, holders });
    }
    if (state === "cold") {
      const took = readState(heldRootOf(init, this.root), init.service, this.deps.home)?.last_start_ms;
      if (took !== undefined) note.took_s = Math.round(took / 1000);
    }
    return { state, lines: [], steps: {}, note };
  }

  /** Write the service's state record (`state.ts`); a start's duration carries over to the next one. */
  private keep(init: ServiceInit, change: Pick<ServiceState, "state"> & Partial<ServiceState>): void {
    const before = readState(heldRootOf(init, this.root), init.service, this.deps.home);
    const lastStart = change.start_ms ?? before?.start_ms ?? before?.last_start_ms;
    const settled = change.settled_mb ?? before?.settled_mb;
    writeState(heldRootOf(init, this.root), init.service, { at: new Date(this.now()).toISOString(), service: init.service, repo: init.repo, ...(settled === undefined ? {} : { settled_mb: settled }), ...change, ...(lastStart === undefined || change.state === "ready" ? {} : { last_start_ms: lastStart }) }, this.deps.home);
  }

  /** Asked through the slot's queue; `null` when the engine could not answer. */
  private ask(slot: Slot, engine: ServiceEngine, files: string[], request: HeldRequest): Promise<{ lines: HeldLine[]; steps: Steps; engine?: string; commits?: Array<() => void> } | null> {
    const queued = performance.now();
    slot.busy += 1;
    return new Promise((resolve) => {
      slot.queue = slot.queue.then(async () => {
        try {
          await this.askNow(slot, engine, files, request, queued, resolve);
        } finally {
          slot.busy -= 1;
        }
      });
    });
  }

  private async askNow(
    slot: Slot,
    engine: ServiceEngine,
    files: string[],
    request: HeldRequest,
    queued: number,
    resolve: (value: { lines: HeldLine[]; steps: Steps; engine?: string; commits?: Array<() => void> } | null) => void,
  ): Promise<void> {
    const begun = performance.now();
    // A newer edit of the file asked again while this waited: its answer is
    // the one to give, and this one would only be stale.
    if (this.superseded(request)) {
      resolve({ lines: [], steps: { queue_wait: tenth(begun - queued) } });
      return;
    }
    let after: (() => Promise<Steps | void>) | undefined;
    try {
      const answered = await engine.answer(files, request);
      after = answered.after;
      const engineMs = performance.now() - begun;
      resolve({
        lines: answered.lines,
        steps: { queue_wait: tenth(begun - queued), engine: tenth(engineMs), ...answered.steps },
        ...(answered.commit ? { commits: [answered.commit] } : {}),
        ...(answered.engine === undefined ? {} : { engine: answered.engine }),
      });
    } catch (error) {
      log("engine could not answer", String(error));
      resolve(null);
    }
    const afterBegun = performance.now();
    try {
      const afterSteps = await after?.();
      if (afterSteps !== undefined) this.recordAfter(request, slot.init.service, Math.round(performance.now() - afterBegun), afterSteps);
    } catch (error) {
      log("after-answer step failed", String(error));
    }
  }

  private superseded(request: HeldRequest): boolean {
    return request.seq !== undefined && supersededEntry(request.session, request.seq, this.deps.home);
  }

  /** File the early answer for an ask that named its entry, unless a newer edit superseded it: true when filed. Never throws. */
  private file(request: HeldRequest, state: HeldState, lines: HeldLine[], ms: number, by: { service?: string | undefined; engine?: string | undefined }): boolean {
    if (request.seq === undefined || request.kind !== "edit") return false;
    try {
      if (this.superseded(request)) {
        log("held ask superseded by a newer edit; nothing filed");
        return false;
      }
      const named = { ...(by.service === undefined ? {} : { service: by.service }), ...(by.engine === undefined ? {} : { engine: by.engine }) };
      return answerEarly(request.session, request.seq, { state, lines, ms, ...named }, this.now(), this.deps.home);
    } catch (error) {
      log("could not file the held answer", String(error));
      return false;
    }
  }

  private env(): NodeJS.ProcessEnv {
    return this.deps.env ?? process.env;
  }

  private budget(): number {
    return memoryBudgetMb(this.deps.env, this.deps.totalmem?.());
  }

  /**
   * What a new slot is counted at in the budget. A main-checkout slot: its service's last settled size, else
   * the plateau. A worktree slot: its own settled size when known, else the main checkout's for that service
   * (the same program), else the plateau (#5038, T3).
   */
  private admissionMb(init: ServiceInit): number {
    const own = readState(heldRootOf(init, this.root), init.service, this.deps.home)?.settled_mb;
    if (own !== undefined || init.checkout === undefined) return own ?? ADMISSION_MB;
    return readState(this.root, init.service, this.deps.home)?.settled_mb ?? ADMISSION_MB;
  }

  /**
   * Free room for one more engine, counted at its admission size. A main-checkout start closes idle engines,
   * least recently used first. A worktree start must also keep the live worktree slots on the machine under the
   * cap: it closes the least recently used idle worktree slot first (whatever its idle age), then, for the
   * budget only, an idle main slot by the usual rule; a busy slot is never closed for it (#5038, T3).
   * A service that alone exceeds the budget is never started.
   */
  private async makeRoom(init: ServiceInit): Promise<boolean> {
    const budget = this.budget();
    const now = this.now();
    const need = this.admissionMb(init);
    if (need > budget) return false;
    const worktree = init.checkout !== undefined;
    const cap = worktreeSlotsMax(this.env());
    for (;;) {
      const entries = liveEntries(this.deps.home);
      const overCap = worktree && entries.filter(isWorktreeEntry).length >= cap;
      if (!overCap && admits(entries, budget, need)) return true;
      const ready = [...this.slots.values()].filter((s) => s.status === "ready");
      const byUse = (a: Slot, b: Slot): number => a.lastUsed - b.lastUsed;
      let victim: Slot | undefined;
      if (!worktree) {
        victim = ready.filter((s) => now - s.lastUsed >= EVICT_IDLE_MS).sort(byUse)[0];
      } else {
        victim = ready.filter((s) => s.init.checkout !== undefined && s.busy === 0).sort(byUse)[0];
        if (victim === undefined && !overCap) victim = ready.filter((s) => s.init.checkout === undefined && s.busy === 0 && now - s.lastUsed >= EVICT_IDLE_MS).sort(byUse)[0];
      }
      if (victim === undefined) return false;
      if (worktree) this.slotLog("evict", init, { victim: victim.init.checkout ?? "main", victim_service: victim.init.service });
      await this.closeSlot(victim, "evicted");
    }
  }

  /** One row in the held log per worktree admit, evict, queue or capped decision, with the counts a soak test reads. */
  private slotLog(decision: "admit" | "evict" | "queue" | "capped", init: ServiceInit, more: Record<string, unknown> = {}): void {
    const entries = liveEntries(this.deps.home);
    this.write({
      at: new Date(this.now()).toISOString(),
      kind: "slot",
      decision,
      service: init.service,
      checkout: init.checkout,
      worktrees: entries.filter(isWorktreeEntry).length,
      cap: worktreeSlotsMax(this.env()),
      held_mb: Math.round(entries.reduce((sum, e) => sum + e.rss_mb, 0)),
      budget_mb: this.budget(),
      need_mb: this.admissionMb(init),
      ...more,
    });
  }

  /** One attempt to admit a worktree slot, after every earlier attempt: true when the slot exists (made here or before). */
  private admitOnce(init: ServiceInit, edited?: string[]): Promise<boolean> {
    const attempt = this.admitChain.then(async () => {
      if (this.closed) return false;
      if (this.slots.has(key(init))) return true;
      if (!(await this.makeRoom(init))) return false;
      this.begin(init, this.now(), edited);
      this.slotLog("admit", init);
      return true;
    });
    this.admitChain = attempt.catch(() => undefined);
    return attempt;
  }

  private sleep(ms: number): Promise<void> {
    return new Promise((resolve) => setTimeout(resolve, ms).unref());
  }

  /** Admit a worktree slot, waiting up to `waitMs` for the cap or the budget to free. False when it did not. */
  private async admitWorktree(init: ServiceInit, waitMs: number, edited?: string[]): Promise<boolean> {
    const began = performance.now();
    let queued = false;
    for (;;) {
      if (await this.admitOnce(init, edited)) return true;
      const left = waitMs - (performance.now() - began);
      if (left <= 0 || this.closed || this.admissionMb(init) > this.budget()) return false;
      if (!queued) {
        queued = true;
        this.slotLog("queue", init, { wait_ms: waitMs });
      }
      await this.sleep(Math.min(left, this.deps.queuePollMs ?? 250));
    }
  }

  /** Keep a capped worktree edit ask, so its answer still lands late through the early-answer path once a slot frees. */
  private park(init: ServiceInit, files: string[], request: HeldRequest): void {
    if (request.kind !== "edit" || request.seq === undefined) return;
    const k = key(init);
    const held = this.parked.get(k);
    if (held !== undefined) {
      held.asks.push({ files, request });
      return;
    }
    const parked = { init, asks: [{ files, request }] };
    this.parked.set(k, parked);
    void this.drain(k, parked);
  }

  private async drain(k: string, parked: { init: ServiceInit; asks: Array<{ files: string[]; request: HeldRequest }> }): Promise<void> {
    const until = performance.now() + WORKTREE_PARK_MS;
    try {
      while (!this.closed && performance.now() < until) {
        await this.sleep(this.deps.queuePollMs ?? 250);
        const edited = [...new Set(parked.asks.flatMap((a) => a.files))];
        if (!(await this.admitOnce(parked.init, edited))) continue;
        const slot = this.slots.get(k);
        for (const { files, request } of parked.asks) this.remember(slot, files, request);
        if (slot?.status === "ready" && slot.engine !== null) void this.askAgain(slot, slot.engine);
        return;
      }
    } finally {
      this.parked.delete(k);
    }
  }

  private begin(init: ServiceInit, now: number, edited?: string[]): void {
    const settledMb = readState(heldRootOf(init, this.root), init.service, this.deps.home)?.settled_mb;
    let settle: () => void = () => undefined;
    const started = new Promise<void>((resolve) => (settle = resolve));
    const slot: Slot = { init, status: "starting", engine: null, lastUsed: now, failedAt: 0, begunAt: now, queue: Promise.resolve(), busy: 0, cold: new Map(), started, settle, ...(settledMb === undefined ? {} : { settledMb }), ...(edited === undefined ? {} : { edited }) };
    this.slots.set(key(init), slot);
    this.keep(init, { state: "starting" });
    this.emptySince = null;
    this.entry(slot, init.checkout === undefined ? (slot.settledMb ?? ADMISSION_MB) : this.admissionMb(init));
    // The next turn of the event loop: a start's synchronous work (it clears the old shadow folder before its
    // first await) must not hold back the `cold` answer this ask is about to return.
    setImmediate(() => {
      void Promise.resolve()
        .then(() => this.run(slot))
        .then(
          (result) => this.started(slot, result),
          (error: unknown) => this.started(slot, { state: "error", reason: String(error) }),
        )
        .finally(() => slot.settle());
    });
  }

  /** Run `fn` after every earlier start (and worktree close) of the same service has ended. */
  private serial<T>(init: ServiceInit, fn: () => Promise<T>): Promise<T> {
    const k = serviceKey(init);
    const run = (this.chains.get(k) ?? Promise.resolve()).then(fn, fn);
    this.chains.set(k, run.catch(() => undefined));
    return run;
  }

  /**
   * A start, one at a time per service. A main-checkout start rewrites the shadow its service's worktree
   * clones read packages from (`v/`), so it first closes those worktree engines; they clone again on their
   * next ask. A worktree start whose clone was refused for a stale or missing main shadow starts the main
   * checkout's service first, through its normal path, and clones once more (#5038, T2).
   */
  private async run(slot: Slot): Promise<StartResult> {
    const init = slot.init;
    if (init.checkout === undefined) {
      return this.serial(init, async () => {
        // A worktree slot whose start already gave an engine is closed, ready or not yet marked so: it read the `v/` this start rewrites.
        for (const other of [...this.slots.values()]) {
          if (other.init.checkout !== undefined && serviceKey(other.init) === serviceKey(init) && (other.status === "ready" || other.cloned)) await this.closeSlot(other, "main_start", true, MAIN_START_WAIT_MS);
        }
        return this.deps.runtime.start(init);
      });
    }
    const clone = (): Promise<StartResult> =>
      this.serial(init, async () => {
        const result = await this.deps.runtime.start(init, slot.edited === undefined ? {} : { edited: slot.edited });
        if ("engine" in result) slot.cloned = true;
        return result;
      });
    const first = await clone();
    if (!("refused" in first) || !STALE_MAIN.has(first.refused)) return first;
    log(`worktree clone of ${init.service} refused (${first.refused}); starting the main checkout's engine first`);
    if (!(await this.startMain(init, first.refused))) return first;
    return clone();
  }

  /** Start the main checkout's slot of a worktree init's service, unless it has one, and wait for its start to end. False when it cannot be admitted. */
  private async startMain(worktree: ServiceInit, refused: string): Promise<boolean> {
    const main = (await this.loadPrograms()).find(({ init }) => init.checkout === undefined && serviceKey(init) === serviceKey(worktree))?.init;
    if (main === undefined) return false;
    let slot = this.slots.get(key(main));
    // Main's own engine read the same stale inputs or build: close it and start it again (T2 F3).
    if (slot?.status === "ready" && RESTART_MAIN.has(refused)) {
      log(`restarting ${main.service} in the main checkout: its shadow is stale (${refused})`);
      await this.closeSlot(slot, "stale");
      slot = undefined;
    }
    if (slot === undefined) {
      if (!(await this.makeRoom(main))) return false;
      this.begin(main, this.now());
      slot = this.slots.get(key(main));
    }
    await slot?.started;
    return true;
  }

  private started(slot: Slot, result: StartResult): void {
    if ("engine" in result) {
      if (this.closed || this.slots.get(key(slot.init)) !== slot) {
        // A worktree engine's close (it removes the clone) runs in the service's chain, before any new start of that worktree clones.
        const engine = result.engine;
        void (slot.init.checkout === undefined ? engine.close() : this.serial(slot.init, () => engine.close())).catch((error: unknown) => log("engine close failed", String(error)));
        return;
      }
      slot.engine = result.engine;
      slot.status = "ready";
      // The start returns once the first gate has run: the engine's size now is its settled size.
      // An engine with no compiler holds no memory: it settles at 0 MB, so a later daemon admits it free.
      const holdsNone = result.engine.holdsCompiler === false;
      const size = holdsNone ? null : result.engine.rss();
      const settled = holdsNone ? 0 : size === null ? undefined : Math.round(size);
      if (settled !== undefined) slot.settledMb = settled;
      this.keep(slot.init, { state: "ready", start_ms: this.now() - slot.begunAt, ...(settled === undefined ? {} : { settled_mb: settled }) });
      // Give back the admission slot; the slot stays ready so its asks still answer.
      if (result.engine.holdsCompiler === false) removeEntry(this.deps.home, process.pid, heldRootOf(slot.init, this.root), slot.init.service);
      void this.askAgain(slot, result.engine);
      return;
    }
    if (result.state === "refused") {
      log(`no engine for ${slot.init.service} in ${slot.init.checkout ?? "the main checkout"}: clone refused (${result.refused}): ${result.detail}`);
      slot.status = "refused";
      slot.failedAt = this.now();
      slot.reason = result.refused;
      this.keep(slot.init, { state: "failed", reason: oneLine(`${result.refused}: ${result.detail}`) });
      removeEntry(this.deps.home, process.pid, heldRootOf(slot.init, this.root), slot.init.service);
      // A refused slot holds nothing: it does not keep the daemon from its empty exit (T2 F4).
      if (this.holding() === 0) this.emptySince ??= this.now();
      return;
    }
    log(`no engine for ${slot.init.service}: ${result.reason}`);
    slot.status = result.state === "no_compiler" ? "no_compiler" : "failed";
    slot.failedAt = this.now();
    slot.reason = oneLine(result.reason);
    this.keep(slot.init, { state: slot.status === "no_compiler" ? "no_compiler" : "failed", reason: slot.reason });
    removeEntry(this.deps.home, process.pid, heldRootOf(slot.init, this.root), slot.init.service);
  }

  private entry(slot: Slot, rssMb: number): void {
    writeEntry(this.deps.home, {
      pid: process.pid,
      root: heldRootOf(slot.init, this.root),
      service: slot.init.service,
      rss_mb: rssMb,
      last_used: slot.lastUsed,
    });
  }

  /**
   * `chained`: the caller already runs in the service's start chain. A worktree engine's close otherwise
   * joins that chain, so a new start of the same worktree never clones while the old clone is removed.
   */
  private async closeSlot(slot: Slot, reason: "idle" | "size" | "evicted" | "exit" | "main_start" | "stale", chained = false, waitMs?: number): Promise<void> {
    this.write({ at: new Date(this.now()).toISOString(), kind: "close", service: slot.init.service, reason });
    this.slots.delete(key(slot.init));
    removeEntry(this.deps.home, process.pid, heldRootOf(slot.init, this.root), slot.init.service);
    if (this.holding() === 0) this.emptySince = this.now();
    const engine = slot.engine;
    // After the call in flight, never under it; with `waitMs`, no longer than that (T2 F5).
    const done = slot.queue.catch(() => undefined);
    const waited = waitMs === undefined ? done : Promise.race([done, new Promise<void>((resolve) => setTimeout(resolve, waitMs).unref())]);
    const close = (): Promise<void> =>
      waited
        .then(() => engine?.close())
        .catch((error: unknown) => log("engine close failed", String(error)))
        .then(() => this.forget(slot));
    await (slot.init.checkout === undefined || chained ? close() : this.serial(slot.init, close));
  }

  /** A closed worktree slot's placed and state files go with its clone, unless a new slot of that worktree holds them now (T2 F4). */
  private forget(slot: Slot): void {
    if (slot.init.checkout === undefined || this.slots.has(key(slot.init))) return;
    const root = heldRootOf(slot.init, this.root);
    for (const file of [placedPath(root, slot.init.service, this.deps.home), statePath(root, slot.init.service, this.deps.home)]) fs.rmSync(file, { force: true });
    try {
      fs.rmdirSync(serviceFilesDir(root, this.deps.home));
    } catch {
      // Another service of that worktree still has files there.
    }
  }

  /** Slots that hold or wait for an engine: a refused worktree slot holds nothing. */
  private holding(): number {
    return [...this.slots.values()].filter((s) => s.status !== "refused").length;
  }

  /** Housekeeping: close idle or oversized engines, refresh the registry. */
  async tick(): Promise<void> {
    const now = this.now();
    for (const slot of [...this.slots.values()]) {
      if (slot.status !== "ready" || slot.engine === null) continue;
      const holdsEntry = slot.engine.holdsCompiler !== false;
      const rss = slot.engine.rss();
      const ceiling = closeCeilingMb(slot.settledMb);
      const oversized = rss !== null && rss > ceiling;
      if (oversized) log(`closing ${slot.init.service} for size: ${Math.round(rss)} MB is above ${ceiling} MB (1.5 x ${slot.settledMb ?? "the floor"})`);
      const idleMs = slot.init.checkout === undefined ? ENGINE_IDLE_MS : WORKTREE_IDLE_MS;
      if (now - slot.lastUsed >= idleMs || oversized) {
        await this.closeSlot(slot, oversized ? "size" : "idle");
      } else if (holdsEntry && rss !== null) {
        this.entry(slot, rss);
      }
    }
  }

  wantsExit(): boolean {
    const now = this.now();
    if (now - this.lastRequest >= DAEMON_IDLE_MS) return true;
    return this.holding() === 0 && this.emptySince !== null && now - this.emptySince >= EMPTY_EXIT_MS;
  }

  async close(): Promise<void> {
    this.closed = true;
    for (const slot of [...this.slots.values()]) await this.closeSlot(slot, "exit");
  }

  private record(request: HeldRequest, state: HeldState, lines: number, ms: number, steps: Steps): void {
    this.write({
      at: new Date(this.now()).toISOString(),
      session: request.session,
      kind: request.kind,
      files: request.files.length,
      state,
      lines,
      ms,
      late: request.wait_ms !== undefined && ms > request.wait_ms ? true : undefined,
      steps: Object.keys(steps).length > 0 ? steps : undefined,
    });
  }

  /** The after-answer work of a request, once it ends: its own row, `kind: "after"`. */
  private recordAfter(request: HeldRequest, service: string, ms: number, steps: Steps): void {
    this.write({ at: new Date(this.now()).toISOString(), session: request.session, kind: "after", service, ms, steps });
  }

  private write(row: Record<string, unknown>): void {
    try {
      fs.mkdirSync(heldDir(this.deps.home), { recursive: true, mode: 0o700 });
      fs.appendFileSync(logPath(this.root, this.deps.home), `${JSON.stringify(row)}\n`);
    } catch {
      // A log nobody can write is not a reason to fail an answer.
    }
  }
}

export type ServeOptions = {
  /** The hook that started this process and holds the lock until it takes over. */
  spawner?: number;
  signal?: AbortSignal;
  tickMs?: number;
};

/**
 * Run the daemon for `deps.root` until it wants to exit or `signal` aborts.
 * False when another daemon holds the root's lock: nothing is started then.
 */
export async function serve(deps: DaemonDeps, options: ServeOptions = {}): Promise<boolean> {
  const now = deps.clock ?? Date.now;
  const lock = lockPath(deps.root, deps.home);
  if (!takeLock(lock, process.pid, now(), LOCK_CAP_MS, options.spawner)) return false;
  const daemon = new HeldDaemon(deps);
  const sock = socketPath(deps.root, deps.home);
  const hello: HeldHello = { build_id: deps.buildId ?? heldBuildId(), pid: process.pid, root: realPath(deps.root) };
  // The lock is ours, so a socket file left here belongs to a dead daemon.
  fs.rmSync(sock, { force: true });
  let retired = false;
  let exitReason: "signal" | "idle" | "retired" | undefined;
  let leave: () => void = () => undefined;
  // A daemon of another build was told to exit: its socket and lock go at once,
  // so the replacement can take them, and only then does it reply and wind down.
  const retire = (): void => {
    if (retired) return;
    retired = true;
    exitReason = "retired";
    removeHelloFile();
    fs.rmSync(sock, { force: true });
    releaseLock(lock, process.pid);
  };
  const server = net.createServer((conn) => {
    let buffer = "";
    let chain: Promise<void> = Promise.resolve();
    conn.on("error", () => undefined);
    conn.on("data", (chunk) => {
      buffer += chunk.toString("utf8");
      for (let end = buffer.indexOf("\n"); end >= 0; end = buffer.indexOf("\n")) {
        const text = buffer.slice(0, end);
        buffer = buffer.slice(end + 1);
        chain = chain.then(async () => {
          const frame = parseFrame(text);
          if (frame !== null && "hello" in frame) {
            conn.write(`${JSON.stringify({ hello })}\n`);
          } else if (frame !== null && "ask" in frame) {
            // Another build's ask is never answered, and the connection stays open for the `exit` its asker sends next.
            if (frame.build_id === hello.build_id) conn.end(`${JSON.stringify(await answerRequest(daemon, frame.ask))}\n`);
          } else if (frame !== null) {
            retire();
            conn.end(`${JSON.stringify({ bye: true })}\n`);
            leave();
          } else {
            conn.end(`${JSON.stringify(await answer(daemon, text))}\n`);
          }
        });
      }
    });
  });
  try {
    await new Promise<void>((resolve, reject) => {
      server.once("error", reject);
      server.listen(sock, resolve);
    });
  } catch (error) {
    log("could not listen", String(error));
    releaseLock(lock, process.pid);
    return false;
  }
  fs.chmodSync(sock, 0o600);
  // Beside the lock: a client whose hello wait ran out tells a busy daemon of its own build from another.
  const helloFile = helloPath(deps.root, deps.home);
  const removeHelloFile = (): void => {
    if (readJson<{ pid?: number }>(helloFile)?.pid === process.pid) fs.rmSync(helloFile, { force: true });
  };
  fs.writeFileSync(helloFile, `${JSON.stringify({ pid: process.pid, build_id: hello.build_id })}\n`, { mode: 0o600 });
  await new Promise<void>((resolve) => {
    const timer = setInterval(() => {
      if (retired) return;
      touchLock(lock, process.pid, now());
      void daemon.tick().then(() => {
        if (daemon.wantsExit()) {
          exitReason ??= "idle";
          resolve();
        }
      });
    }, options.tickMs ?? 10_000);
    const done = (): void => {
      exitReason ??= options.signal?.aborted === true ? "signal" : "idle";
      clearInterval(timer);
      resolve();
    };
    leave = done;
    if (options.signal?.aborted) done();
    options.signal?.addEventListener("abort", done, { once: true });
  });
  server.close();
  log(`daemon exiting: ${exitReason ?? "idle"}`);
  removeHelloFile();
  // A retired daemon's socket and lock may be the replacement's by now.
  if (!retired) fs.rmSync(sock, { force: true });
  await daemon.close();
  if (!retired) releaseLock(lock, process.pid);
  return true;
}

function parseFrame(text: string): HeldFrame | null {
  try {
    const parsed = JSON.parse(text) as Record<string, unknown>;
    if (parsed["hello"] === true) return { hello: true };
    if (parsed["exit"] === true) return { exit: true };
    if (typeof parsed["build_id"] === "string" && typeof parsed["ask"] === "object" && parsed["ask"] !== null) {
      return { ask: parsed["ask"] as HeldRequest, build_id: parsed["build_id"] };
    }
  } catch {
    // Not a frame: a request line, answered (or failed) as one.
  }
  return null;
}

async function answer(daemon: HeldDaemon, text: string): Promise<HeldAnswer> {
  try {
    return await answerRequest(daemon, JSON.parse(text) as HeldRequest);
  } catch {
    return { state: "error", lines: [], ms: 0 };
  }
}

async function answerRequest(daemon: HeldDaemon, request: HeldRequest): Promise<HeldAnswer> {
  try {
    return await daemon.handle(request);
  } catch {
    return { state: "error", lines: [], ms: 0 };
  }
}
