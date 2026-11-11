import { onEvent, run } from "fixture-runner";

// A named function a library runner is handed: the runner returns what it
// returns, so it runs it.
export function handle(value: string): string {
  return value;
}

export function chained(pending: Promise<string>): Promise<string> {
  return pending.then(handle);
}

export function work(): number {
  return 1;
}

export function traced(): number {
  return run(work);
}

// A named listener a registration keeps: the call returns no result of it.
export function onTick(): void {}

export function subscribe(): void {
  onEvent("tick", onTick);
}

// A named function an in-repo callee runs: the callee's own call takes the
// edge, as before.
export function runLocal(job: () => number): number {
  return job();
}

export function viaLocal(): number {
  return runLocal(work);
}
