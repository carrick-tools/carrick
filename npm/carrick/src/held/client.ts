// The hook's side of the held-compiler daemon: make sure one runs for the
// checkout (`ensureHeld`), and ask it within a hard wait (`askHeld`). Both do
// nothing unless the switch is on (`compilerLinesEnabled`).

import net from "node:net";
import { compilerLinesEnabled } from "../facts/enabled.ts";
import { createLogger } from "../log.ts";
import { lockHeld, readJson, releaseLock, takeLock, type Lock } from "../hook/lock.ts";
import { probeTimeoutMs } from "../hook/probe.ts";
import { scriptBeside, startDetached } from "../hook/refresh.ts";
import { backedOff } from "./backoff.ts";
import { heldBuildId } from "./build.ts";
import type { HeldAnswer, HeldHello, HeldRequest } from "./daemon.ts";
import { lockPath, LOCK_CAP_MS, socketPath } from "./paths.ts";

const log = createLogger("carrick-held-client");

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
    const finish = (answer: AskAnswer): void => {
      if (settled) return;
      settled = true;
      clearTimeout(timer);
      resolve(answer);
      if (options.seq === undefined) {
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
    // The hello comes first and the request only after it matched: a daemon of
    // another build is never asked, so its rules never reach an agent.
    conn.write(`${JSON.stringify({ hello: true })}\n`);
    conn.on("data", (chunk) => {
      buffer += chunk.toString("utf8");
      const end = buffer.indexOf("\n");
      if (end < 0) return;
      const line = buffer.slice(0, end);
      buffer = buffer.slice(end + 1);
      try {
        const parsed = JSON.parse(line) as { hello?: HeldHello; bye?: boolean } & Partial<AskAnswer>;
        if (!greeted && parsed.bye !== true) {
          if (parsed.hello?.build_id === buildId) {
            greeted = true;
            conn.write(`${JSON.stringify(request)}\n`);
            if (settled) conn.end();
          } else {
            // Another build (or one that does not say): told to exit, then
            // replaced; this ask answers cold at once.
            conn.write(`${JSON.stringify({ exit: true })}\n`);
          }
          return;
        }
        if (parsed.bye === true) {
          ensureHeld(root, { env, buildId, ...(options.home === undefined ? {} : { home: options.home }), ...(options.start === undefined ? {} : { start: options.start }) });
          finish(none("cold", started));
          return;
        }
        finish(parsed as AskAnswer);
      } catch {
        finish(none("unavailable", started));
      }
    });
    conn.on("error", () => finish(none("unavailable", started)));
    conn.on("close", () => finish(none("unavailable", started)));
  });
}
