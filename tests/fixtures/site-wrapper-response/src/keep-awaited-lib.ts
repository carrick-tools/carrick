import { http } from "@fixture/http";
export async function fetchRows(path: string) {
  const res = await http.post<{ rows: number }>(path);
  return res;
}
