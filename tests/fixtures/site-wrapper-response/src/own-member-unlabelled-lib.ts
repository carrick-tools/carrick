import { http } from "@fixture/http";
export async function clear(path: string) {
  const res = await http.purge<{ gone: boolean }>(path);
  return res.gone;
}
