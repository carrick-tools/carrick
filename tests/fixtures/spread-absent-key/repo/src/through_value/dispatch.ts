import { runGate, type GateArgs } from './gates';

type Deps = { gate(args: GateArgs): number };

// Runs the gate through a member of `deps`, writing `log`.
export function dispatch(deps: Deps, args: GateArgs): number {
  return deps.gate({ log: args.log, tag: args.tag });
}

export function start(): number {
  return dispatch({ gate: runGate }, { log: console });
}
