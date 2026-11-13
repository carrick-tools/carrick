import { runGate, type GateArgs } from './gates';

type Deps = { gate(args: GateArgs): void };

// Runs the gate through a member of `deps`, handing it `args` whole.
export function dispatch(deps: Deps, args: GateArgs): void {
  return deps.gate(args);
}

export function start(): void {
  return dispatch({ gate: runGate }, { log: console });
}
