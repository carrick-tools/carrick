import { http } from "@fixture/http";
export async function save(path: string, body: unknown) {
  const res = await http.post<{ ok: boolean; id: string }>(path, body);
  return res.ok;
}
