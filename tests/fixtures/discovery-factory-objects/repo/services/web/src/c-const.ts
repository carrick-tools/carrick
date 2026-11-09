import { loadC } from "./load";
type DepsC = { get: (id: string) => string };
const fixedC: DepsC = { get: loadC };
function makeC(): DepsC {
  return fixedC;
}
export function runCWith(deps: DepsC, id: string) {
  return deps.get(id);
}
export function runC(id: string) {
  return runCWith(makeC(), id);
}
