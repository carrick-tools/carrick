// The hook's side of the held-compiler daemon: make sure one runs for the
// checkout (`ensureHeld`), and ask it within a hard wait (`askHeld`). Both do
// nothing unless the switch is on (`compilerLinesEnabled`).

import fs from "node:fs";
import net from "node:net";
import path from "node:path";
import { compilerLinesEnabled } from "../facts/enabled.ts";
import { createLogger } from "../log.ts";
import { lockHeld, readJson, releaseLock, takeLock, type Lock } from "../hook/lock.ts";
import { probeTimeoutMs } from "../hook/probe.ts";
import { scriptBeside, startDetached } from "../hook/refresh.ts";
import { backedOff } from "./backoff.ts";
import { heldBuildId } from "./build.ts";
import type { HeldAnswer, HeldHello, HeldRequest } from "./daemon.ts";
import type { NotChecked } from "./state.ts";
import { lockPath, LOCK_CAP_MS, socketPath } from "./paths.ts";

const log = createLogger("carrick-held-client");

/** How long a daemon has to answer the hello; one that does not is another build, and is replaced. */
export const HELLO_WAIT_MS = 200;

let ownBuildId: string | null = null;
/** This client's build id, hashed once per process unless a test names one. */
function clientBuildId(given: string | undefined): string {
  if (given !== undefined) return given;
  ownBuildId ??= heldBuildId();
  return ownBuildId;
}

export type EnsureOptions = {
  env?: NodeJS.ProcessEnv;
  home?: string;
  now?: number;
  /** Injected in tests; the real one detaches a child and forgets it. */
  start?: (command: string, args: string[], env: NodeJS.ProcessEnv) => void;
  /** This client's build id; the hash of the package's own files by default. */
  buildId?: string;
};

/**
 * Make sure one daemon is alive for this checkout. True when this call
 * started one; false when the switch is off or a live daemon holds the lock.
 * The lock names this hook until the daemon takes it over, so a second hook in
 * that window starts nothing. A build that crashed twice within an hour is not
 * started again for a day (`backoff.ts`).
 */
export function ensureHeld(root: string, options: EnsureOptions = {}): boolean {
  const env = options.env ?? process.env;
  if (!compilerLinesEnabled(env)) return false;
  const now = options.now ?? Date.now();
  const lock = lockPath(root, options.home);
  if (lockHeld(readJson<Lock>(lock), now, LOCK_CAP_MS)) return false;
  if (backedOff(root, clientBuildId(options.buildId), now, options.home)) return false;
  if (!takeLock(lock, process.pid, now, LOCK_CAP_MS)) return false;
  const { command, args } = scriptBeside(import.meta.url, "run", root);
  const daemonEnv = { ...env, CARRICK_NO_KEEP_AWAKE: "1", CARRICK_HELD_SPAWNER: String(process.pid) };
  try {
    (options.start ?? startDetached)(command, args, daemonEnv);
  } catch (error) {
    releaseLock(lock, process.pid);
    log("could not start the held-compiler daemon", String(error));
    return false;
  }
  return true;
}

export type AskOptions = {
  env?: NodeJS.ProcessEnv;
  home?: string;
  session: string;
  kind: "read" | "edit";
  editedAt?: number;
  waitMs?: number;
  /** Part 2's queue entry for this edit: the daemon files its early answer under it. */
  seq?: string;
  /** This client's build id; the hash of the package's own files by default. */
  buildId?: string;
  /** Injected in tests, as `EnsureOptions.start`. */
  start?: EnsureOptions["start"];
};

/**
 * What the asker got: the daemon's states, or why it got none. `off` is the
 * switch, `unavailable` is no daemon (or one that failed), `late` is an answer
 * that did not come within the wait.
 */
export type AskAnswer = Omit<HeldAnswer, "state"> & { state: HeldAnswer["state"] | "off" | "unavailable" | "late" };

const none = (state: AskAnswer["state"], started: number): AskAnswer => ({ state, lines: [], ms: Date.now() - started });

export function askHeld(root: string, files: string[], options: AskOptions): Promise<AskAnswer> {
  const started = Date.now();
  const env = options.env ?? process.env;
  if (!compilerLinesEnabled(env)) return Promise.resolve(none("off", started));
  // At most the probe's budget (`probeTimeoutMs`); the edit hook passes what is
  // left of it since the edit. An answer ready within it prints in this action;
  // a slower one is filed by the daemon as the entry's early answer, which the
  // next carrier prints (`hook/late.ts`).
  const waitMs = options.waitMs ?? probeTimeoutMs();
  const request: HeldRequest = {
    session: options.session,
    files,
    kind: options.kind,
    wait_ms: waitMs,
    ...(options.editedAt === undefined ? {} : { edited_at: options.editedAt }),
    ...(options.seq === undefined ? {} : { seq: options.seq }),
  };
  const buildId = clientBuildId(options.buildId);
  return new Promise((resolve) => {
    let buffer = "";
    let settled = false;
    let greeted = false;
    let replacing = false;
    const finish = (answer: AskAnswer): void => {
      if (settled) return;
      settled = true;
      clearTimeout(timer);
      resolve(answer);
      if (options.seq === undefined) {
        clearTimeout(helloTimer);
        conn.destroy();
        return;
      }
      // An ask the daemon files an answer for outlives its wait: once the hello
      // matches, the request is written and the connection let go gracefully
      // (below); a daemon that never says hello is let go after a moment.
      if (greeted) conn.end();
      else setTimeout(() => conn.destroy(), 2_000).unref();
    };
    const conn = net.createConnection(socketPath(root, options.home));
    const timer = setTimeout(() => finish(none("late", started)), waitMs);
    /**
     * A daemon of another build, or one that says nothing a hello says, is never
     * asked: it is replaced and this ask answers cold. One that knows the
     * protocol is told to exit over the socket; one that does not is ended
     * through the lock's pid.
     */
    const replace = (viaSocket: boolean): void => {
      if (replacing) return;
      replacing = true;
      clearTimeout(helloTimer);
      const startNew = (): void => {
        ensureHeld(root, { env, buildId, ...(options.home === undefined ? {} : { home: options.home }), ...(options.start === undefined ? {} : { start: options.start }) });
        finish(none("cold", started));
      };
      if (viaSocket) {
        conn.write(`${JSON.stringify({ exit: true })}\n`);
        // The reply `bye` (below) starts the new daemon; a daemon that does not reply is ended instead.
        byeTimer = setTimeout(() => {
          endStale(root, options.home);
          startNew();
        }, HELLO_WAIT_MS);
        startNewAfterBye = startNew;
      } else {
        endStale(root, options.home);
        startNew();
      }
    };
    let byeTimer: NodeJS.Timeout | undefined;
    let startNewAfterBye: (() => void) | undefined;
    // The hello comes first and the request only after it matched: a daemon of
    // another build is never asked, so its rules never reach an agent. One that
    // does not answer it within a short fixed wait is another build by definition.
    const helloTimer = setTimeout(() => replace(false), HELLO_WAIT_MS);
    helloTimer.unref();
    conn.write(`${JSON.stringify({ hello: true })}\n`);
    conn.on("data", (chunk) => {
      buffer += chunk.toString("utf8");
      const end = buffer.indexOf("\n");
      if (end < 0) return;
      const line = buffer.slice(0, end);
      buffer = buffer.slice(end + 1);
      let parsed: { hello?: Partial<HeldHello>; bye?: boolean } & Partial<AskAnswer>;
      try {
        parsed = JSON.parse(line) as typeof parsed;
      } catch {
        replace(false);
        return;
      }
      if (parsed.bye === true) {
        clearTimeout(byeTimer);
        startNewAfterBye?.();
        return;
      }
      if (greeted) {
        finish(parsed as AskAnswer);
        return;
      }
      const theirs = parsed.hello?.build_id;
      if (typeof theirs !== "string") replace(false);
      else if (theirs !== buildId) replace(true);
      else {
        greeted = true;
        clearTimeout(helloTimer);
        conn.write(`${JSON.stringify(request)}\n`);
        if (settled) conn.end();
      }
    });
    /**
     * No daemon said hello: none listens (it left after its idle time,
     * `daemon.ts` `wantsExit`, or it crashed), or one closed before its hello.
     * One is started for the checkout (`ensureHeld`; the lock keeps it to one),
     * so the check comes back at the next ask, not the next session (#4939).
     * When this ask started it, the answer is `cold` with a note for the edit's
     * line, naming the edited file because no daemon is there to name its
     * service; otherwise (a daemon is starting or backed off) `unavailable`.
     * An ask whose wait ran out first (`late`) still starts it.
     */
    const noDaemon = (): void => {
      if (greeted || replacing) {
        finish(none("unavailable", started));
        return;
      }
      // Nothing is there to say hello, so the hello timeout must not take this
      // for a silent daemon of another build and end the one started here.
      replacing = true;
      clearTimeout(helloTimer);
      const startedOne = ensureHeld(root, { env, buildId, ...(options.home === undefined ? {} : { home: options.home }), ...(options.start === undefined ? {} : { start: options.start }) });
      if (!startedOne) {
        finish(none("unavailable", started));
        return;
      }
      const notes: NotChecked[] = files.map((file) => ({ service: path.relative(root, file), state: "cold" }));
      finish({ ...none("cold", started), not_checked: notes });
    };
    conn.on("error", noDaemon);
    conn.on("close", noDaemon);
  });
}

/**
 * End the daemon behind this checkout's socket when it cannot be told over it:
 * the lock's own pid, when the lock is live and is not this process (a daemon
 * holds the lock once its socket exists). Never a pattern kill. Its lock and
 * socket go with it, so the replacement can take them.
 */
function endStale(root: string, home: string | undefined): void {
  const lock = lockPath(root, home);
  const held = readJson<Lock>(lock);
  if (held === null || held.pid === process.pid || !lockHeld(held, Date.now(), LOCK_CAP_MS)) return;
  try {
    process.kill(held.pid, "SIGTERM");
  } catch {
    // Already gone.
  }
  releaseLock(lock, held.pid);
  fs.rmSync(socketPath(root, home), { force: true });
}
