import { http } from "@fixture/http";
export function makeGate(b: string) {
  return async () => http.post<{ g: number }>(`${b}/gate`);
}
export function gateFor(b: string) {
  return makeGate(b);
}
