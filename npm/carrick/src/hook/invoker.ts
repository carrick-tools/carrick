// Who started a `carrick` process: a hook, or a person at a shell.
//
// A hook runs `carrick check` or `carrick status` on the user's machine, and
// the binary's cloud requests carry the answer in `x-carrick-invoker`, so a
// hook fire is a line in the cloud's log rather than a guess. The binary reads
// `CARRICK_INVOKER` (src/invoker.rs); absent or unknown means `cli`. A hook
// sets it once at its entry: every spawn after that, attached or detached,
// inherits the process environment.

/** The hooks that start the binary, and the value each sends. */
export type HookInvoker = "post-edit" | "stop" | "session-start" | "post-read";

export const INVOKER_ENV = "CARRICK_INVOKER";

/** The value the binary sends for a hook. */
export function invokerValue(hook: HookInvoker): string {
  return `hook:${hook}`;
}

/** Mark this process, and every child it spawns, as started by `hook`. */
export function markInvoker(hook: HookInvoker, env: NodeJS.ProcessEnv = process.env): void {
  env[INVOKER_ENV] = invokerValue(hook);
}
