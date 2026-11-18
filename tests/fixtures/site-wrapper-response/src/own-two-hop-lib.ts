import { http } from "@fixture/http";
export function lift(path: string) {
  return http.post<{ v: number }>(path);
}
export async function lifted(path: string) {
  const r = await lift(path);
  return r.v > 0;
}
