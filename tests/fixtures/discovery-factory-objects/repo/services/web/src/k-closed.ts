import { loadK } from "./load";
type DepsK = { get: (id: string) => string; other: (id: string) => string };
function makeK(): DepsK {
  return { other: loadK } as DepsK;
}
export function runKWith(deps: DepsK, id: string) {
  return deps.get(id);
}
export function runK(id: string) {
  return runKWith(makeK(), id);
}
