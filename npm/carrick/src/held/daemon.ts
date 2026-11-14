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
import { releaseLock, takeLock, touchLock } from "../hook/lock.ts";
import { heldDir, lockPath, LOCK_CAP_MS, logPath, socketPath } from "./paths.ts";
import {
  admits,
  ADMISSION_MB,
  EVICT_IDLE_MS,
  liveEntries,
  memoryBudgetMb,
  removeEntry,
  RSS_CEILING_MB,
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
};

export type HeldRequest = {
  session: string;
  /** Absolute paths. */
  files: string[];
  kind: "read" | "edit";
  edited_at?: number;
  /** How long the asker waits; an answer after it is logged as late. */
  wait_ms?: number;
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
  /** The group's named type, when the row has one (the line says it in place of the check's alias). */
  symbol?: string | null;
};

export type HeldState = "none" | "cold" | "ready" | "capped" | "no_compiler" | "error";

export type HeldAnswer = { state: HeldState; lines: HeldLine[]; ms: number };

/** A started engine for one service. */
export interface ServiceEngine {
  /**
   * The lines for `files` as they are now. `after` runs once the answer has
   * been sent, still inside the engine's queue: no request reaches the engine
   * until it ends (the twin pass edits the engine's texts).
   */
  answer(files: string[], request: HeldRequest): Promise<{ lines: HeldLine[]; after?: () => Promise<void> }>;
  /** Resident memory in MB, or null. */
  rss(): number | null;
  close(): Promise<void>;
}

export type StartResult = { engine: ServiceEngine } | { state: "no_compiler" | "error"; reason: string };

/** Starts a service's engine: the program, the compiler, the gate. */
export interface ServiceRuntime {
  start(init: ServiceInit): Promise<StartResult>;
}

export type DaemonDeps = {
  root: string;
  home?: string;
  env?: NodeJS.ProcessEnv;
  clock?: () => number;
  programs: () => Promise<ServiceInit[]>;
  runtime: ServiceRuntime;
};

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
  /** Engine calls run one at a time, in arrival order. */
  queue: Promise<void>;
};

const key = (init: ServiceInit): string => `${init.repo}\0${init.service}`;

const within = (file: string, dir: string): boolean => file === dir || file.startsWith(dir + path.sep);

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
    try {
      ({ state, lines } = await this.route(request));
    } catch (error) {
      log("request failed", String(error));
      state = "error";
    }
    const ms = Math.round(performance.now() - started);
    this.record(request, state, lines.length, ms);
    return { state, lines, ms };
  }

  private async route(request: HeldRequest): Promise<{ state: HeldState; lines: HeldLine[] }> {
    const programs = await this.loadPrograms();
    const files = request.files.map(realPath);
    const lines: HeldLine[] = [];
    const states: HeldState[] = [];
    for (const { init, dir } of programs) {
      const mine = files.filter((f) => within(f, dir));
      if (mine.length === 0) continue;
      const answer = await this.serveService(init, mine, request);
      states.push(answer.state);
      lines.push(...answer.lines);
    }
    // One state for the whole request: the most useful one any service gave.
    const order: HeldState[] = ["ready", "cold", "capped", "no_compiler", "error"];
    const state = order.find((s) => states.includes(s)) ?? "none";
    return { state, lines };
  }

  private async serveService(
    init: ServiceInit,
    files: string[],
    request: HeldRequest,
  ): Promise<{ state: HeldState; lines: HeldLine[] }> {
    const now = this.now();
    let slot = this.slots.get(key(init));
    if (slot?.status === "failed" && now - slot.failedAt >= RETRY_MS) {
      this.slots.delete(key(init));
      slot = undefined;
    }
    if (slot === undefined) {
      if (!(await this.makeRoom())) return { state: "capped", lines: [] };
      this.begin(init, now);
      return { state: "cold", lines: [] };
    }
    slot.lastUsed = now;
    if (slot.status === "starting") return { state: "cold", lines: [] };
    if (slot.status === "no_compiler") return { state: "no_compiler", lines: [] };
    if (slot.status === "failed" || slot.engine === null) return { state: "error", lines: [] };
    const lines = await this.ask(slot, slot.engine, files, request);
    return lines === null ? { state: "error", lines: [] } : { state: "ready", lines };
  }

  /** Asked through the slot's queue; `null` when the engine could not answer. */
  private ask(slot: Slot, engine: ServiceEngine, files: string[], request: HeldRequest): Promise<HeldLine[] | null> {
    return new Promise((resolve) => {
      slot.queue = slot.queue.then(async () => {
        let after: (() => Promise<void>) | undefined;
        try {
          const answered = await engine.answer(files, request);
          after = answered.after;
          resolve(answered.lines);
        } catch (error) {
          log("engine could not answer", String(error));
          resolve(null);
        }
        try {
          await after?.();
        } catch (error) {
          log("after-answer step failed", String(error));
        }
      });
    });
  }

  /** Free room for one more engine: close idle engines, least recently used first. */
  private async makeRoom(): Promise<boolean> {
    const budget = memoryBudgetMb(this.deps.env);
    const now = this.now();
    for (;;) {
      if (admits(liveEntries(this.deps.home), budget)) return true;
      const victim = [...this.slots.values()]
        .filter((s) => s.status === "ready" && now - s.lastUsed >= EVICT_IDLE_MS)
        .sort((a, b) => a.lastUsed - b.lastUsed)[0];
      if (victim === undefined) return false;
      await this.closeSlot(victim);
    }
  }

  private begin(init: ServiceInit, now: number): void {
    const slot: Slot = { init, status: "starting", engine: null, lastUsed: now, failedAt: 0, queue: Promise.resolve() };
    this.slots.set(key(init), slot);
    this.emptySince = null;
    this.entry(slot, ADMISSION_MB);
    void this.deps.runtime.start(init).then(
      (result) => this.started(slot, result),
      (error: unknown) => this.started(slot, { state: "error", reason: String(error) }),
    );
  }

  private started(slot: Slot, result: StartResult): void {
    if ("engine" in result) {
      if (this.closed || this.slots.get(key(slot.init)) !== slot) {
        void result.engine.close();
        return;
      }
      slot.engine = result.engine;
      slot.status = "ready";
      return;
    }
    log(`no engine for ${slot.init.service}: ${result.reason}`);
    slot.status = result.state === "no_compiler" ? "no_compiler" : "failed";
    slot.failedAt = this.now();
    removeEntry(this.deps.home, process.pid, this.root, slot.init.service);
  }

  private entry(slot: Slot, rssMb: number): void {
    writeEntry(this.deps.home, {
      pid: process.pid,
      root: this.root,
      service: slot.init.service,
      rss_mb: rssMb,
      last_used: slot.lastUsed,
    });
  }

  private async closeSlot(slot: Slot): Promise<void> {
    this.slots.delete(key(slot.init));
    removeEntry(this.deps.home, process.pid, this.root, slot.init.service);
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
      const rss = slot.engine.rss();
      if (now - slot.lastUsed >= ENGINE_IDLE_MS || (rss !== null && rss > RSS_CEILING_MB)) {
        await this.closeSlot(slot);
      } else if (rss !== null) {
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
    for (const slot of [...this.slots.values()]) await this.closeSlot(slot);
  }

  private record(request: HeldRequest, state: HeldState, lines: number, ms: number): void {
    const row = {
      at: new Date(this.now()).toISOString(),
      session: request.session,
      kind: request.kind,
      files: request.files.length,
      state,
      lines,
      ms,
      late: request.wait_ms !== undefined && ms > request.wait_ms ? true : undefined,
    };
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
  // The lock is ours, so a socket file left here belongs to a dead daemon.
  fs.rmSync(sock, { force: true });
  const server = net.createServer((conn) => {
    let buffer = "";
    conn.on("error", () => undefined);
    conn.on("data", (chunk) => {
      buffer += chunk.toString("utf8");
      const end = buffer.indexOf("\n");
      if (end < 0) return;
      const text = buffer.slice(0, end);
      buffer = "";
      void answer(daemon, text).then((reply) => conn.end(`${JSON.stringify(reply)}\n`));
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
  await new Promise<void>((resolve) => {
    const timer = setInterval(() => {
      touchLock(lock, process.pid, now());
      void daemon.tick().then(() => {
        if (daemon.wantsExit()) resolve();
      });
    }, options.tickMs ?? 10_000);
    const done = (): void => {
      clearInterval(timer);
      resolve();
    };
    if (options.signal?.aborted) done();
    options.signal?.addEventListener("abort", done, { once: true });
  });
  server.close();
  fs.rmSync(sock, { force: true });
  await daemon.close();
  releaseLock(lock, process.pid);
  return true;
}

async function answer(daemon: HeldDaemon, text: string): Promise<HeldAnswer> {
  try {
    return await daemon.handle(JSON.parse(text) as HeldRequest);
  } catch {
    return { state: "error", lines: [], ms: 0 };
  }
}
