import { loadA } from "./load";
type DepsA = { get: (id: string) => string };
let cachedA: DepsA | undefined;
function makeA(): DepsA {
  if (cachedA) return cachedA;
  cachedA = { get: loadA };
  return cachedA;
}
export function runAWith(deps: DepsA, id: string) {
  return deps.get(id);
}
export function runA(id: string) {
  return runAWith(makeA(), id);
}
