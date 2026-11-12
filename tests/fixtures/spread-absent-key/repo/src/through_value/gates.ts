// A gate run only through a value: `runGate` is handed as `{ gate: runGate }`
// and called as `deps.gate(...)` (dispatch.ts), so the resolver finds no
// direct caller of it. Its `args` reach `check` only through spreads, and a
// caller elsewhere does write `log`: the key is unread, not absent.
export type Log = { warn(message: string): void };
export type GateArgs = { log: Log; tag?: string };

function check({ log }: GateArgs): number {
  log.warn('checked');
  return 1;
}

function viaSpread(args: GateArgs): number {
  return check({ ...args, tag: 'pool' });
}

function viaLocal(args: GateArgs): number {
  const base = { ...args, tag: 'base' };
  return checkLocal({ ...base, tag: 'pool' });
}

function checkLocal({ log }: GateArgs): number {
  log.warn('checked locally');
  return 2;
}

export function runGate(args: GateArgs): number {
  return viaSpread(args) + viaLocal(args);
}

// The twin: the same read through a spread whose source the resolver does
// not read on either side of the rule, so it states the walk's reason for an
// unread key both before and after.
function checkParsed({ log }: GateArgs): number {
  log.warn('checked parsed');
  return 3;
}

export function runParsed(raw: string): number {
  return checkParsed({ ...JSON.parse(raw), tag: 'pool' });
}
