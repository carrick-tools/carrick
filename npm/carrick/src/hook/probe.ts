// The post-edit hook's one question to the binary: the probe.
//
// `check <file> --recheck --no-rescan --json` answers at once what the index,
// git and the one file can tell (the file reads the same as the index, a
// known miss at the background budget) and otherwise says `recheck.deferred`.
// It walks no service's imports, so "no service reads it" is the full
// re-check's answer, not the probe's.
// This file is the only place that knows that shape, so the binary's half
// (B3-R) can change its spelling in one place. A call that gives no answer,
// including a binary too old to know the flag, reads as deferred: the edit is
// queued for the background runner, which asks the full question.

import { check, checkEach } from "../cli.ts";
import type { CheckResult } from "../contract.ts";
import { backgroundCapMs } from "./rechecks.ts";

/**
 * The limit on a probe, whatever the file count: the agent is never made to
 * wait for the binary (David, 2026-11-11: "we can't ever stop the agent. We can
 * only deliver the results later"). A probe past it is killed and reads as
 * deferred, so the edit goes to the background runner and its answer arrives
 * late (`late.ts`), as Stop's does.
 */
export function probeTimeoutMs(): number {
  return 600;
}

/**
 * Whether an answer is one the probe leaves for the background re-check
 * rather than printing in hand: no answer, or `deferred`. The one test of "in
 * hand or queued", read by the edit hook for the probe's answer and by
 * `late.ts` for a runner's answer queued in place of the probe (R46.4). A
 * runner never answers `deferred`; a re-scan it started and did not finish is
 * the same case: it ran out of time (`deadline`), or it failed after deciding
 * what to re-read (`scope` on a `none`, which the probe, stopping before that
 * decision, never carries; `src/local_mode/recheck.rs` `run`).
 */
export function needsBackground(result: CheckResult | null): boolean {
  if (result === null) return true;
  const recheck = result.recheck;
  if (recheck === undefined) return false;
  if (recheck.deferred === true || recheck.deadline === true) return true;
  return recheck.ran === "none" && recheck.scope != null;
}

/**
 * One probe call for every edited file (a patch is one `check --each`), in the
 * order given. The binary reads misses at the background cap, so a service the
 * runner could not fit is answered at once instead of deferred forever.
 */
export async function probeEdits(
  relatives: string[],
  cwd: string,
  env: NodeJS.ProcessEnv = process.env,
): Promise<{ results: Array<CheckResult | null>; failure: string | null; ms: number }> {
  const probeEnv = { ...env, CARRICK_RECHECK_BUDGET_MS: String(backgroundCapMs(env)) };
  const options = {
    cwd,
    env: probeEnv,
    recheck: true as const,
    noRescan: true,
    limitMs: probeTimeoutMs(),
    // The limit ends the call for good: a probe that ignores SIGTERM is not left running.
    killSignal: "SIGKILL" as const,
  };
  if (relatives.length === 1) {
    const outcome = await check(relatives[0]!, options);
    return { results: [outcome.result], failure: outcome.failure, ms: outcome.ms };
  }
  const outcome = await checkEach(relatives, options);
  return { results: outcome.result ?? [], failure: outcome.failure, ms: outcome.ms };
}
