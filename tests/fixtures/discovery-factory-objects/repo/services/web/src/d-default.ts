import { loadD } from "./load";
type DepsD = { get: (id: string) => string };
let cachedD: DepsD | undefined;
function makeD(): DepsD {
  if (cachedD) return cachedD;
  cachedD = { get: loadD };
  return cachedD;
}
export function runD(id: string, deps: DepsD = makeD()) {
  return deps.get(id);
}
