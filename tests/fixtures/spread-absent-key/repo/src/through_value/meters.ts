// A meter run only through a value: `meterPools` is handed as
// `{ pools: meterPools }` and never called by name, so the resolver finds no
// caller of it. `log` reaches `meterWith` through `{ ...base, scope }`, where
// `base` is a local object that writes `log` from the pool meter's own
// parameter: the spread source writes the key, its value is a parameter,
// and that parameter's callers come back empty.
import type { Log } from './gates';

export type MeterArgs = { log?: Log; tag?: string; scope?: string };
type Deps = { record(scope: string): boolean };

function meterWith(deps: Deps, { scope, log }: MeterArgs): boolean {
  const ok = deps.record(scope || 'none');
  if (!ok && log) {
    log.warn('meter write failed');
  }
  return ok;
}

function meterPoolsWith(deps: Deps, { log, tag }: MeterArgs): boolean[] {
  const base = { log, tag };
  return [
    meterWith(deps, { ...base, scope: 'day' }),
    meterWith(deps, { ...base, scope: 'month' }),
  ];
}

function recordSafely(scope: string): boolean {
  return scope.length > 0;
}

function meterPools(args: MeterArgs): boolean[] {
  return meterPoolsWith({ record: recordSafely }, args);
}

export function meters(): { pools(args: MeterArgs): boolean[] } {
  return { pools: meterPools };
}
