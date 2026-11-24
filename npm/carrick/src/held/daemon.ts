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
import { heldDir, heldRootOf, helloPath, lockPath, LOCK_CAP_MS, logPath, socketPath } from "./paths.ts";
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

export type StartResult = { engine: ServiceEngine } | { state: "no_compiler" | "error"; reason: string };

/** Starts a service's engine: the program, the compiler, the gate. */
export interface ServiceRuntime {
  start(init: ServiceInit): Promise<StartResult>;
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
  /** The id the daemon answers `hello` with; the hash of its own build files by default. */
  buildId?: string;
};

/**
 * First frame of a connection: who is on the other end of the socket. The
 * client asks only a daemon of its own build; `exit` retires one of another.
 */
export type HeldHello = { build_id: string; pid: number; root: string };
export type HeldFrame = { hello: true } | { exit: true };

const MIN = 60_000;
/** D2: an engine closes after this long with no request. */
const ENGINE_IDLE_MS = 15 * MIN;
/** D2: the daemon exits after this long with no request... */
const DAEMON_IDLE_MS = 30 * MIN;
/** ...or this long after its last engine closed. */
const EMPTY_EXIT_MS = 5 * MIN;
/** A start that failed is tried again after this long. */
const RETRY_MS = 5 * MIN;

type Slot = {
  init: ServiceInit;
  status: "starting" | "ready" | "no_compiler" | "failed";
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
  /** The newest edit ask per session and file answered `cold` while the engine started (N2); each is asked again once it is ready. */
  cold: Map<string, { request: HeldRequest; files: string[] }>;
};

/** A reason as one short line. */
const oneLine = (text: string): string => text.replace(/\s+/g, " ").trim().slice(0, 160);

const key = (init: ServiceInit): string => `${init.repo}\0${init.service}${init.checkout === undefined ? "" : `\0${init.checkout}`}`;

const within = (file: string, dir: string): boolean => file === dir || file.startsWith(dir + path.sep);

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
  private programs: Promise<Array<{ init: ServiceInit; dir: string }>> | null = null;
  private lastRequest: number;
  private emptySince: number | null = null;
  private closed = false;

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
    for (const { init, dir } of programs) {
      const mine = files.filter((f) => serves(f, dir));
      if (mine.length === 0) continue;
      for (const f of mine) served.add(f);
      const answer = await this.serveService(init, mine, request);
      states.push(answer.state);
      if (answer.state === "ready") ready.push(`${init.repo}/${init.service}`);
      lines.push(...answer.lines);
      if (answer.note !== undefined) notes.push(answer.note);
      commits.push(...(answer.commits ?? []));
      if (answer.engine !== undefined) engines.push(answer.engine);
      addSteps(steps, answer.steps);
    }
    // A worktree file no program serves is not checked: say so, and start nothing (T2, #5038).
    const told = new Set<string>();
    for (const file of files) {
      if (served.has(file)) continue;
      const top = linkedCheckoutTop(file);
      const main = top === null ? null : mainCheckoutOf(top);
      if (top === null || main === null) continue;
      const twin = path.join(realPath(main), path.relative(realPath(top), file));
      for (const { init, dir } of programs) {
        if (!within(twin, dir) || told.has(`${init.service}\0${top}`)) continue;
        if (((await this.deps.runtime.rowCount?.(init)) ?? 1) === 0) continue;
        told.add(`${init.service}\0${top}`);
        notes.push({ service: init.service, state: "worktree", checkout: top });
        states.push("worktree");
      }
    }
    // One state for the whole request: the most useful one any service gave.
    const order: HeldState[] = ["ready", "cold", "capped", "no_compiler", "error", "worktree"];
    const state = order.find((s) => states.includes(s)) ?? "none";
    // The services whose engines answered, and those engines: what an early answer's rows are the whole list of (T1 P1.4).
    return { state, lines, steps, notes, commits, ...(ready.length > 0 ? { service: ready.join(", ") } : {}), ...(engines.length > 0 ? { engine: engines.join(", ") } : {}) };
  }

  private async serveService(init: ServiceInit, files: string[], request: HeldRequest): Promise<Served> {
    const now = this.now();
    let slot = this.slots.get(key(init));
    if (slot?.status === "failed" && now - slot.failedAt >= RETRY_MS) {
      this.slots.delete(key(init));
      slot = undefined;
    }
    if (slot === undefined) {
      if ((await this.deps.runtime.rowCount?.(init)) === 0) return { state: "none", lines: [], steps: {} };
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
  private notChecked(init: ServiceInit, state: NotChecked["state"], reason?: string): Served {
    const note: NotChecked = { service: init.service, state };
    if (reason !== undefined) note.reason = reason;
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
    return new Promise((resolve) => {
      slot.queue = slot.queue.then(async () => {
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
      });
    });
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

  private budget(): number {
    return memoryBudgetMb(this.deps.env, this.deps.totalmem?.());
  }

  /**
   * Free room for one more engine, counted at the service's last settled size: close idle
   * engines, least recently used first. A service that alone exceeds the budget is never started.
   */
  private async makeRoom(init: ServiceInit): Promise<boolean> {
    const budget = this.budget();
    const now = this.now();
    const need = readState(heldRootOf(init, this.root), init.service, this.deps.home)?.settled_mb ?? ADMISSION_MB;
    if (need > budget) return false;
    for (;;) {
      if (admits(liveEntries(this.deps.home), budget, need)) return true;
      const victim = [...this.slots.values()]
        .filter((s) => s.status === "ready" && now - s.lastUsed >= EVICT_IDLE_MS)
        .sort((a, b) => a.lastUsed - b.lastUsed)[0];
      if (victim === undefined) return false;
      await this.closeSlot(victim, "evicted");
    }
  }

  private begin(init: ServiceInit, now: number): void {
    const settledMb = readState(heldRootOf(init, this.root), init.service, this.deps.home)?.settled_mb;
    const slot: Slot = { init, status: "starting", engine: null, lastUsed: now, failedAt: 0, begunAt: now, queue: Promise.resolve(), cold: new Map(), ...(settledMb === undefined ? {} : { settledMb }) };
    this.slots.set(key(init), slot);
    this.keep(init, { state: "starting" });
    this.emptySince = null;
    this.entry(slot, slot.settledMb ?? ADMISSION_MB);
    // The next turn of the event loop: a start's synchronous work (it clears the old shadow folder before its
    // first await) must not hold back the `cold` answer this ask is about to return.
    setImmediate(() => {
      void Promise.resolve()
        .then(() => this.deps.runtime.start(init))
        .then(
          (result) => this.started(slot, result),
          (error: unknown) => this.started(slot, { state: "error", reason: String(error) }),
        );
    });
  }

  private started(slot: Slot, result: StartResult): void {
    if ("engine" in result) {
      if (this.closed || this.slots.get(key(slot.init)) !== slot) {
        void result.engine.close();
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

  private async closeSlot(slot: Slot, reason: "idle" | "size" | "evicted" | "exit"): Promise<void> {
    this.write({ at: new Date(this.now()).toISOString(), kind: "close", service: slot.init.service, reason });
    this.slots.delete(key(slot.init));
    removeEntry(this.deps.home, process.pid, heldRootOf(slot.init, this.root), slot.init.service);
    if (this.slots.size === 0) this.emptySince = this.now();
    const engine = slot.engine;
    // After the call in flight, never under it.
    await slot.queue.then(() => engine?.close()).catch((error: unknown) => log("engine close failed", String(error)));
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
      if (now - slot.lastUsed >= ENGINE_IDLE_MS || oversized) {
        await this.closeSlot(slot, oversized ? "size" : "idle");
      } else if (holdsEntry && rss !== null) {
        this.entry(slot, rss);
      }
    }
  }

  wantsExit(): boolean {
    const now = this.now();
    if (now - this.lastRequest >= DAEMON_IDLE_MS) return true;
    return this.slots.size === 0 && this.emptySince !== null && now - this.emptySince >= EMPTY_EXIT_MS;
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
  } catch {
    // Not a frame: a request line, answered (or failed) as one.
  }
  return null;
}

async function answer(daemon: HeldDaemon, text: string): Promise<HeldAnswer> {
  try {
    return await daemon.handle(JSON.parse(text) as HeldRequest);
  } catch {
    return { state: "error", lines: [], ms: 0 };
  }
}
