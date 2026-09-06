// Running the `carrick` binary, read-only, with a hard time limit.
//
// Both channels go through here. Nothing in this file may throw at its caller:
// a missing binary, a crash, a timeout and unparseable stdout all read as "no
// answer", because neither an edit nor a session start may fail on Carrick.

import { execFile } from "node:child_process";
import { parseCheckResult, type CheckResult } from "./contract.ts";

/** Binary to run. `carrick` on PATH unless CARRICK_BIN names another. */
export function binary(env: NodeJS.ProcessEnv = process.env): string {
  return env["CARRICK_BIN"] || "carrick";
}

/** Time limit for one CLI call. The budget for the hook is 300 ms of our own. */
export function timeoutMs(env: NodeJS.ProcessEnv = process.env): number {
  const raw = env["CARRICK_TIMEOUT_MS"];
  const parsed = raw ? Number.parseInt(raw, 10) : Number.NaN;
  return Number.isFinite(parsed) && parsed > 0 ? parsed : 5000;
}

export type RunOutcome = {
  result: CheckResult | null;
  /** Why there is no result, for the log. Null when there is one. */
  failure: string | null;
  ms: number;
};

export type RunOptions = {
  cwd: string;
  env?: NodeJS.ProcessEnv;
  /** Injectable for tests; defaults to the real `carrick` binary. */
  bin?: string;
};

async function run(args: string[], options: RunOptions): Promise<RunOutcome> {
  const env = options.env ?? process.env;
  const bin = options.bin ?? binary(env);
  const started = Date.now();
  return await new Promise<RunOutcome>((resolve) => {
    execFile(
      bin,
      args,
      {
        cwd: options.cwd,
        env,
        timeout: timeoutMs(env),
        maxBuffer: 32 * 1024 * 1024,
        encoding: "utf8",
      },
      (error, stdout) => {
        const ms = Date.now() - started;
        const parsed = parseCheckResult(stdout ?? "");
        if (parsed) {
          resolve({ result: parsed, failure: null, ms });
          return;
        }
        const failure = error
          ? `${bin} ${args.join(" ")} failed: ${error.message}`
          : `${bin} ${args.join(" ")} printed no carrick.check/0 payload`;
        resolve({ result: null, failure, ms });
      },
    );
  });
}

/** `carrick check <file> --json`, run from the workspace root. */
export async function check(file: string, options: RunOptions): Promise<RunOutcome> {
  return await run(["check", file, "--json"], options);
}

/** `carrick touch <file> --json`, or with no file for the whole workspace. */
export async function touch(file: string | null, options: RunOptions): Promise<RunOutcome> {
  const args = file ? ["touch", file, "--json"] : ["touch", "--json"];
  return await run(args, options);
}
