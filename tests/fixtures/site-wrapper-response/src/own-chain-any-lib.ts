import { http } from "@fixture/http";
export async function isUp(p: string): Promise<boolean> {
  const r = await http.post<any>(p);
  return r.v > 0;
}
export function check(p: string) {
  return isUp(p);
}
