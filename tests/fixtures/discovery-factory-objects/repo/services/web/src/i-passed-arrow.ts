import { loadI } from "./load";
type DepsI = { get: (id: string) => string };
export function runIWith(deps: DepsI, id: string) {
  return deps.get(id);
}
export function runI(id: string) {
  return runIWith({ get: (x: string) => loadI(x) }, id);
}
