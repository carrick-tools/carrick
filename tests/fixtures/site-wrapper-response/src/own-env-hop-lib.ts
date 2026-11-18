import { http } from "@fixture/http";
export async function inner(p: string) {
  const body = await http.post<{ e: number }>(p);
  return { status: 200, body };
}
export function outer(p: string) {
  return inner(p);
}
