import { http } from "@fixture/http";
export async function isUp(p: string) {
  const r = await http.post<{ v: number }>(p);
  return r.v > 0;
}
export function check(p: string) {
  return isUp(p);
}
