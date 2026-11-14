// The hook's side of the held-compiler daemon: make sure one runs for the
// checkout (`ensureHeld`), and ask it within a hard wait (`askHeld`). Both do
// nothing unless the switch is on (`compilerLinesEnabled`).

import net from "node:net";
import { compilerLinesEnabled } from "../facts/enabled.ts";
import { createLogger } from "../log.ts";
import { lockHeld, readJson, releaseLock, takeLock, type Lock } from "../hook/lock.ts";
import { scriptBeside, startDetached } from "../hook/refresh.ts";
import type { HeldAnswer, HeldRequest } from "./daemon.ts";
import { lockPath, LOCK_CAP_MS, socketPath } from "./paths.ts";

const log = createLogger("carrick-held-client");

/** D10: the longest a hook waits for the daemon. */
export const HELD_WAIT_MS = 1000;

export type EnsureOptions = {
  env?: NodeJS.ProcessEnv;
  home?: string;
  now?: number;
  /** Injected in tests; the real one detaches a child and forgets it. */
  start?: (command: string, args: string[], env: NodeJS.ProcessEnv) => void;
};

/**
 * Make sure one daemon is alive for this checkout. True when this call
 * started one; false when the switch is off or a live daemon holds the lock.
 * The lock names this hook until the daemon takes it over, so a second hook in
 * that window starts nothing.
 */
export function ensureHeld(root: string, options: EnsureOptions = {}): boolean {
  const env = options.env ?? process.env;
  if (!compilerLinesEnabled(env)) return false;
  const now = options.now ?? Date.now();
  const lock = lockPath(root, options.home);
  if (lockHeld(readJson<Lock>(lock), now, LOCK_CAP_MS)) return false;
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
  const waitMs = options.waitMs ?? HELD_WAIT_MS;
  const request: HeldRequest = {
    session: options.session,
    files,
    kind: options.kind,
    wait_ms: waitMs,
    ...(options.editedAt === undefined ? {} : { edited_at: options.editedAt }),
  };
  return new Promise((resolve) => {
    let buffer = "";
    let settled = false;
    const finish = (answer: AskAnswer): void => {
      if (settled) return;
      settled = true;
      clearTimeout(timer);
      conn.destroy();
      resolve(answer);
    };
    const conn = net.createConnection(socketPath(root, options.home));
    const timer = setTimeout(() => finish(none("late", started)), waitMs);
    conn.on("connect", () => conn.write(`${JSON.stringify(request)}\n`));
    conn.on("data", (chunk) => {
      buffer += chunk.toString("utf8");
      const end = buffer.indexOf("\n");
      if (end < 0) return;
      try {
        finish(JSON.parse(buffer.slice(0, end)) as AskAnswer);
      } catch {
        finish(none("unavailable", started));
      }
    });
    conn.on("error", () => finish(none("unavailable", started)));
    conn.on("close", () => finish(none("unavailable", started)));
  });
}
