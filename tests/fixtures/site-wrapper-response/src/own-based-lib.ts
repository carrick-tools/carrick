import { http } from "@fixture/http";
export async function poke(base: string) {
  const res = await http.post<{ hits: number }>(`${base}/poke`);
  return res.hits > 0;
}
