// The post-edit hook's one question to the binary: the probe.
//
// `check <file> --recheck --no-rescan --json` answers at once when no re-scan
// is needed (the file reads the same as the index, no service reads it, a
// known miss at the background budget) and otherwise says `recheck.deferred`.
// This file is the only place that knows that shape, so the binary's half
// (B3-R) can change its spelling in one place. A call that gives no answer,
// including a binary too old to know the flag, reads as deferred: the edit is
// queued for the background runner, which asks the full question.

import { check, checkEach } from "../cli.ts";
import type { CheckResult } from "../contract.ts";
import { backgroundCapMs } from "./rechecks.ts";

/** The limit on a probe: 5 s, and half a second more for each further file of a patch, to 40 s. */
export function probeTimeoutMs(files: number): number {
  return Math.min(5000 + 500 * Math.max(files - 1, 0), 40_000);
}

/** Whether the probe left this edit for the background re-check: no answer, or `deferred`. */
export function needsBackground(result: CheckResult | null): boolean {
  return result === null || result.recheck?.deferred === true;
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
    limitMs: probeTimeoutMs(relatives.length),
  };
  if (relatives.length === 1) {
    const outcome = await check(relatives[0]!, options);
    return { results: [outcome.result], failure: outcome.failure, ms: outcome.ms };
  }
  const outcome = await checkEach(relatives, options);
  return { results: outcome.result ?? [], failure: outcome.failure, ms: outcome.ms };
}
