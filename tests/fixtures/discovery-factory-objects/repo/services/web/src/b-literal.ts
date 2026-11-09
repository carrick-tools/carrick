import { loadB } from "./load";
type DepsB = { get: (id: string) => string };
function makeB(): DepsB {
  return { get: loadB };
}
export function runBWith(deps: DepsB, id: string) {
  return deps.get(id);
}
export function runB(id: string) {
  return runBWith(makeB(), id);
}
