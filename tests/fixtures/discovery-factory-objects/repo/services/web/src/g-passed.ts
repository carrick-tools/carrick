import { loadG } from "./load";
type DepsG = { get: (id: string) => string };
export function runGWith(deps: DepsG, id: string) {
  return deps.get(id);
}
export function runG(id: string) {
  return runGWith({ get: loadG }, id);
}
