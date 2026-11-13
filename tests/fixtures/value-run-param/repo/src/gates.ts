// The same shape across modules: `runGate` is imported by `wiring.ts`,
// handed there as `{ gate: runGate }` and run only as `deps.gate(args)`.
export type Log = { warn(message: string): void };
export type GateArgs = { log: Log };

function check({ log }: GateArgs): void {
  log.warn('checked');
}

export function runGate(args: GateArgs): void {
  return check(args);
}
