import { loadH } from "./load";
type DepsH = { get: (id: string) => string };
const fixedH: DepsH = { get: loadH };
export function runHWith(deps: DepsH, id: string) {
  return deps.get(id);
}
export function runH(id: string) {
  return runHWith(fixedH, id);
}
