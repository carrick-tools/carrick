// Running the `carrick` binary, read-only, with a hard time limit.
//
// Both channels go through here. Nothing in this file may throw at its caller:
// a missing binary, a crash, a timeout and unparseable stdout all read as "no
// answer", because neither an edit nor a session start may fail on Carrick.

import { execFile } from "node:child_process";
import {
  parseChangedResult,
  parseCheckResult,
  parseCheckResults,
  parseStatusResult,
  type ChangedResult,
  type CheckResult,
  type StatusResult,
} from "./contract.ts";

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

export type RunOutcome<T = CheckResult> = {
  result: T | null;
  /** Why there is no result, for the log. Null when there is one. */
  failure: string | null;
  ms: number;
};

export type RunOptions = {
  cwd: string;
  env?: NodeJS.ProcessEnv;
  /** Injectable for tests; defaults to the real `carrick` binary. */
  bin?: string;
  /**
   * The limit for this call in ms, used when `CARRICK_TIMEOUT_MS` is not set
   * (the environment's own value always wins). Absent: five seconds.
   */
  limitMs?: number;
};

/**
 * What a `--recheck` call must state: its own limit. There is no default,
 * because the right one depends on who is asking (a hook's probe, or the
 * background runner), and a wrong one kills the call before it answers.
 * `noRescan` is the probe: it answers at once, or says `recheck.deferred`.
 */
export type RecheckOptions = RunOptions & { recheck: true; limitMs: number; noRescan?: boolean };

async function run<T>(
  args: string[],
  options: RunOptions,
  parse: (stdout: string, exit: number) => T | null,
): Promise<RunOutcome<T>> {
  const env = options.env ?? process.env;
  const bin = options.bin ?? binary(env);
  const started = Date.now();
  return await new Promise<RunOutcome<T>>((resolve) => {
    execFile(
      bin,
      args,
      {
        cwd: options.cwd,
        env,
        timeout: env["CARRICK_TIMEOUT_MS"] ? timeoutMs(env) : (options.limitMs ?? timeoutMs(env)),
        maxBuffer: 32 * 1024 * 1024,
        encoding: "utf8",
      },
      (error, stdout) => {
        const ms = Date.now() - started;
        // A non-zero exit sets `error.code` to the number; a kill leaves it unset.
        const code = (error as { code?: unknown } | null)?.code;
        const exit = error ? (typeof code === "number" ? code : -1) : 0;
        const parsed = parse(stdout ?? "", exit);
        if (parsed) {
          resolve({ result: parsed, failure: null, ms });
          return;
        }
        const failure = error
          ? `${bin} ${args.join(" ")} failed: ${error.message}`
          : `${bin} ${args.join(" ")} printed no payload this reader knows`;
        resolve({ result: null, failure, ms });
      },
    );
  });
}

/**
 * `carrick check <file> --json`, run from the workspace root.
 *
 * `recheck` asks the binary to re-extract the file and re-judge it against the
 * index before answering, which costs a scan of that file's repo. It is passed
 * by the post-edit hook, by the background runner and by nothing else: the
 * language server runs this on every save, and a file being edited is always
 * newer than the index (carrick#1036). A re-check states its own `limitMs`:
 * one cut off by this timeout prints nothing at all, where one cut off by the
 * binary's own budget still answers, so the caller sizes the limit above it.
 */
export async function check(
  file: string,
  options: RunOptions | RecheckOptions,
): Promise<RunOutcome<CheckResult>> {
  const args = ["check", file, "--json"];
  if ("recheck" in options && options.recheck) {
    args.push("--recheck");
    if (options.noRescan) args.push("--no-rescan");
  }
  return await run(args, options, parseCheckResult);
}

/**
 * `carrick check --each <files...> --json --recheck`: every file of one patch
 * in one binary call (each answered as `check <file> --recheck` would), so a
 * patch that touches six files starts one process rather than six. The
 * answers come back in the order asked; `null` for a file the binary refused.
 */
export async function checkEach(
  files: string[],
  options: Omit<RecheckOptions, "recheck">,
): Promise<RunOutcome<Array<CheckResult | null>>> {
  const args = ["check", "--each", ...files, "--json", "--recheck"];
  if (options.noRescan) args.push("--no-rescan");
  return await run(args, options, parseCheckResults);
}

/**
 * `carrick status --json`: the workspace, with no file in the question.
 *
 * `check` and `touch` each take exactly one file, so this is the command a
 * surface opening a session asks. `--workspace` is passed only when the caller
 * names one; without it the CLI finds `.carrick/` from the working directory.
 */
export async function status(
  options: RunOptions & { workspace?: string | null },
): Promise<RunOutcome<StatusResult>> {
  const args = options.workspace
    ? ["status", "--workspace", options.workspace, "--json"]
    : ["status", "--json"];
  return await run(args, options, parseStatusResult);
}

/**
 * `carrick check --changed --json`, run from the workspace root: the whole
 * change against the main branch. Only the background runner calls it, for a
 * Stop that could not state the session's check from the per-file re-checks
 * (carrick#3466). The binary leaves its marker (`hook/checked.ts`) itself.
 */
export async function checkChanged(options: RunOptions): Promise<RunOutcome<ChangedResult>> {
  return await run(["check", "--changed", "--json"], options, parseChangedResult);
}
