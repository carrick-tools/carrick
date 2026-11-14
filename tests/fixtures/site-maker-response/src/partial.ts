import { http } from "@fixture/http";

function makeSweep(root: string) {
  return () => http.remove<{ swept: number }>(`${root}/sweep`, { headers: {} });
}
export const sweepLocal = makeSweep("/local");
export function sweepAny(root: string) { return makeSweep(root); }
