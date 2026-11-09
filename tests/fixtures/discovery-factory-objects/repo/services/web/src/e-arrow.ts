import { loadE } from "./load";
type DepsE = { get: (id: string) => string };
let cachedE: DepsE | undefined;
function makeE(): DepsE {
  if (cachedE) return cachedE;
  cachedE = { get: (id: string) => loadE(id) };
  return cachedE;
}
export function runEWith(deps: DepsE, id: string) {
  return deps.get(id);
}
export function runE(id: string) {
  return runEWith(makeE(), id);
}
