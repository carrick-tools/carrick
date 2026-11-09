import { loadJ } from "./load";
function makeJ(): (id: string) => string {
  return loadJ;
}
export function runJWith(get: (id: string) => string, id: string) {
  return get(id);
}
export function runJ(id: string) {
  return runJWith(makeJ(), id);
}
