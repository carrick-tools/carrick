import { loadF } from "./load";
type DepsF = { get: (id: string) => string };
let cachedF: DepsF | undefined;
function makeF(): DepsF {
  if (cachedF) return cachedF;
  cachedF = { get: loadF };
  return cachedF;
}
export function runF(id: string) {
  const deps = makeF();
  return deps.get(id);
}
